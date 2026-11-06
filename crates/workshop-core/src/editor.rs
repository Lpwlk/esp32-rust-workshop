//! Line editor for the USB console: cursor movement and command history.
//!
//! `espflash monitor` puts the terminal in raw mode and forwards every key press,
//! the special keys as ANSI escape sequences (`ESC [ A` for ↑...). The firmware
//! feeds those bytes to a [`LineEditor`] and redraws the prompt from
//! [`LineEditor::line`] and [`LineEditor::cursor`].
//!
//! | Keys | Action |
//! |---|---|
//! | ← → | move the cursor; typing inserts at the cursor |
//! | Home / End, Ctrl+A / Ctrl+E | start / end of the line |
//! | Backspace / Delete | erase before / under the cursor |
//! | Ctrl+U / Ctrl+K | clear the line / erase to its end |
//! | ↑ ↓ | previous / next command of the history (↓ at the bottom: back to the line being typed) |
//!
//! ```
//! use workshop_core::editor::{EditorEvent, LineEditor};
//!
//! let mut ed = LineEditor::new();
//! let feed = |ed: &mut LineEditor, bytes: &[u8]| bytes.iter().filter_map(|&b| ed.feed(b)).last();
//! feed(&mut ed, b"lst");
//! feed(&mut ed, b"\x1b[D\x1b[D"); // ← ←: cursor after `l`
//! feed(&mut ed, b"i");            // insert
//! assert_eq!((ed.line(), ed.cursor()), ("list", 2));
//! let Some(EditorEvent::Submitted(line)) = feed(&mut ed, b"\r") else { panic!() };
//! ed.remember(&line);
//! feed(&mut ed, b"\x1b[A"); // ↑ brings it back
//! assert_eq!(ed.line(), "list");
//! ```

use heapless::{Deque, String};

use crate::cli::MAX_INPUT;

/// Commands kept in the history.
pub const HISTORY: usize = 16;

/// What a key press did to the line being edited.
#[derive(Debug, PartialEq, Eq)]
pub enum EditorEvent {
    /// The line or the cursor changed: redraw the prompt.
    Changed,
    /// Enter on a non-empty line: here it is (the editor is empty again).
    Submitted(String<MAX_INPUT>),
}

/// Where we are in an escape sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Escape {
    None,
    /// After `ESC`.
    Esc,
    /// After `ESC [`, with the numeric parameter read so far (`ESC [ 3 ~`).
    Csi(u8),
    /// After `ESC O` (some terminals send ↑ as `ESC O A`).
    Ss3,
}

/// A key, once its escape sequence is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
}

/// Line being typed, cursor, and history (see the [module docs](self)).
pub struct LineEditor {
    /// Printable ASCII only, so byte index = character index.
    line: String<MAX_INPUT>,
    cursor: usize,
    escape: Escape,
    /// Oldest first.
    history: Deque<String<MAX_INPUT>, HISTORY>,
    /// While browsing with ↑ ↓: how far back (0 = most recent).
    browsing: Option<usize>,
    /// The line being typed when ↑ was first pressed, restored by ↓.
    draft: String<MAX_INPUT>,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl LineEditor {
    /// An empty line and an empty history.
    pub const fn new() -> Self {
        Self {
            line: String::new(),
            cursor: 0,
            escape: Escape::None,
            history: Deque::new(),
            browsing: None,
            draft: String::new(),
        }
    }

    /// The line typed so far.
    pub fn line(&self) -> &str {
        &self.line
    }

    /// Cursor position, in characters from the start of the line.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Adds a submitted command to the history (not called for answers to
    /// questions, which shouldn't come back with ↑). Repeats of the latest entry
    /// are kept once.
    pub fn remember(&mut self, line: &str) {
        if line.is_empty() || self.history.back().is_some_and(|last| last == line) {
            return;
        }
        if self.history.is_full() {
            self.history.pop_front();
        }
        let _ = self.history.push_back(String::try_from(line).unwrap_or_default());
    }

    /// Processes one received byte.
    pub fn feed(&mut self, byte: u8) -> Option<EditorEvent> {
        match self.escape {
            Escape::Esc => match byte {
                b'[' => {
                    self.escape = Escape::Csi(0);
                    return None;
                }
                b'O' => {
                    self.escape = Escape::Ss3;
                    return None;
                }
                // A lone ESC (the Esc key) followed by a normal key: handle the key.
                _ => self.escape = Escape::None,
            },
            Escape::Csi(param) => {
                return match byte {
                    b'0'..=b'9' => {
                        self.escape = Escape::Csi(param.saturating_mul(10).saturating_add(byte - b'0'));
                        None
                    }
                    0x40..=0x7e => {
                        self.escape = Escape::None;
                        let key = match (byte, param) {
                            (b'A', _) => Key::Up,
                            (b'B', _) => Key::Down,
                            (b'C', _) => Key::Right,
                            (b'D', _) => Key::Left,
                            (b'H', _) | (b'~', 1 | 7) => Key::Home,
                            (b'F', _) | (b'~', 4 | 8) => Key::End,
                            (b'~', 3) => Key::Delete,
                            _ => return None, // Insert, F-keys...: ignored
                        };
                        self.key(key)
                    }
                    _ => None, // `;` and other intermediate bytes
                };
            }
            Escape::Ss3 => {
                self.escape = Escape::None;
                let key = match byte {
                    b'A' => Key::Up,
                    b'B' => Key::Down,
                    b'C' => Key::Right,
                    b'D' => Key::Left,
                    b'H' => Key::Home,
                    b'F' => Key::End,
                    _ => return None,
                };
                return self.key(key);
            }
            Escape::None => {}
        }

        match byte {
            0x1b => {
                self.escape = Escape::Esc;
                None
            }
            b'\r' | b'\n' if !self.line.is_empty() => {
                self.cursor = 0;
                self.browsing = None;
                Some(EditorEvent::Submitted(core::mem::take(&mut self.line)))
            }
            0x08 | 0x7f if self.cursor > 0 => {
                self.cursor -= 1;
                self.line.remove(self.cursor);
                self.edited()
            }
            0x01 => self.key(Key::Home),  // Ctrl+A
            0x05 => self.key(Key::End),   // Ctrl+E
            0x02 => self.key(Key::Left),  // Ctrl+B
            0x06 => self.key(Key::Right), // Ctrl+F
            0x10 => self.key(Key::Up),    // Ctrl+P
            0x0e => self.key(Key::Down),  // Ctrl+N
            0x15 => {
                // Ctrl+U: clear the line.
                self.line.clear();
                self.cursor = 0;
                self.edited()
            }
            0x0b => {
                // Ctrl+K: erase to the end of the line.
                self.line.truncate(self.cursor);
                self.edited()
            }
            0x20..=0x7e if self.line.len() < MAX_INPUT => {
                // Rebuild the line with the character inserted at the cursor.
                let mut line: String<MAX_INPUT> = String::new();
                let _ = line.push_str(&self.line[..self.cursor]);
                let _ = line.push(byte as char);
                let _ = line.push_str(&self.line[self.cursor..]);
                self.line = line;
                self.cursor += 1;
                self.edited()
            }
            _ => None,
        }
    }

    /// The line was edited by hand: it's no longer a history entry.
    fn edited(&mut self) -> Option<EditorEvent> {
        self.browsing = None;
        Some(EditorEvent::Changed)
    }

    fn key(&mut self, key: Key) -> Option<EditorEvent> {
        match key {
            Key::Left if self.cursor > 0 => self.cursor -= 1,
            Key::Right if self.cursor < self.line.len() => self.cursor += 1,
            Key::Home if self.cursor > 0 => self.cursor = 0,
            Key::End if self.cursor < self.line.len() => self.cursor = self.line.len(),
            Key::Delete if self.cursor < self.line.len() => {
                self.line.remove(self.cursor);
                return self.edited();
            }
            Key::Up => {
                let back = match self.browsing {
                    None if !self.history.is_empty() => {
                        self.draft = self.line.clone();
                        0
                    }
                    Some(b) if b + 1 < self.history.len() => b + 1,
                    _ => return None, // nothing older
                };
                self.browsing = Some(back);
                self.show(self.history.len() - 1 - back);
            }
            Key::Down => match self.browsing {
                Some(0) => {
                    self.browsing = None;
                    self.line = self.draft.clone();
                    self.cursor = self.line.len();
                }
                Some(b) => {
                    self.browsing = Some(b - 1);
                    self.show(self.history.len() - b);
                }
                None => return None,
            },
            _ => return None,
        }
        Some(EditorEvent::Changed)
    }

    /// Loads history entry `index` (0 = oldest), cursor at its end.
    fn show(&mut self, index: usize) {
        if let Some(entry) = self.history.iter().nth(index) {
            self.line = entry.clone();
            self.cursor = self.line.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP: &[u8] = b"\x1b[A";
    const DOWN: &[u8] = b"\x1b[B";
    const LEFT: &[u8] = b"\x1b[D";
    const RIGHT: &[u8] = b"\x1b[C";

    fn feed(ed: &mut LineEditor, bytes: &[u8]) -> Option<EditorEvent> {
        bytes.iter().filter_map(|&b| ed.feed(b)).last()
    }

    fn submit(ed: &mut LineEditor, text: &str) {
        feed(ed, text.as_bytes());
        let Some(EditorEvent::Submitted(line)) = feed(ed, b"\r") else { panic!("{text}") };
        ed.remember(&line);
    }

    #[test]
    fn cursor_moves_and_edits_in_the_middle() {
        let mut ed = LineEditor::new();
        feed(&mut ed, b"set led1 on");
        feed(&mut ed, LEFT);
        feed(&mut ed, LEFT);
        feed(&mut ed, b"\x7f"); // backspace before the cursor: removes the space
        assert_eq!((ed.line(), ed.cursor()), ("set led1on", 8));
        feed(&mut ed, b" ");
        feed(&mut ed, b"\x1b[3~"); // Delete under the cursor: removes `o`
        assert_eq!((ed.line(), ed.cursor()), ("set led1 n", 9));
        feed(&mut ed, b"\x1b[H"); // Home
        assert_eq!(ed.cursor(), 0);
        feed(&mut ed, b"\x7f"); // nothing before the cursor
        assert_eq!(ed.line(), "set led1 n");
        feed(&mut ed, b"\x1b[F"); // End
        assert_eq!(ed.cursor(), 10);
        feed(&mut ed, RIGHT); // already at the end
        assert_eq!(ed.cursor(), 10);
    }

    #[test]
    fn control_keys() {
        let mut ed = LineEditor::new();
        feed(&mut ed, b"burst 100 max");
        feed(&mut ed, b"\x01"); // Ctrl+A
        assert_eq!(ed.cursor(), 0);
        feed(&mut ed, b"\x05"); // Ctrl+E
        assert_eq!(ed.cursor(), 13);
        for _ in 0..4 {
            feed(&mut ed, LEFT);
        }
        feed(&mut ed, b"\x0b"); // Ctrl+K
        assert_eq!(ed.line(), "burst 100");
        feed(&mut ed, b"\x15"); // Ctrl+U
        assert_eq!((ed.line(), ed.cursor()), ("", 0));
    }

    #[test]
    fn history_browsing_keeps_the_draft() {
        let mut ed = LineEditor::new();
        submit(&mut ed, "status");
        submit(&mut ed, "list");
        submit(&mut ed, "list"); // repeated: kept once
        feed(&mut ed, b"bau");
        feed(&mut ed, UP);
        assert_eq!(ed.line(), "list");
        feed(&mut ed, UP);
        assert_eq!((ed.line(), ed.cursor()), ("status", 6));
        assert_eq!(feed(&mut ed, UP), None); // nothing older
        assert_eq!(ed.line(), "status");
        feed(&mut ed, DOWN);
        assert_eq!(ed.line(), "list");
        feed(&mut ed, DOWN);
        assert_eq!((ed.line(), ed.cursor()), ("bau", 3)); // the draft is back
        assert_eq!(feed(&mut ed, DOWN), None);
    }

    #[test]
    fn editing_a_recalled_command() {
        let mut ed = LineEditor::new();
        submit(&mut ed, "baud 1M");
        feed(&mut ed, UP);
        feed(&mut ed, b"\x7f\x7f2M");
        let Some(EditorEvent::Submitted(line)) = feed(&mut ed, b"\r") else { panic!() };
        assert_eq!(line, "baud 2M");
    }

    #[test]
    fn history_drops_the_oldest() {
        let mut ed = LineEditor::new();
        for i in 0..HISTORY + 3 {
            submit(&mut ed, &format!("cmd {i}"));
        }
        for _ in 0..HISTORY + 5 {
            feed(&mut ed, UP);
        }
        assert_eq!(ed.line(), "cmd 3");
    }

    #[test]
    fn escape_sequences_are_never_typed() {
        let mut ed = LineEditor::new();
        // Insert, F5, Ctrl+→ (`ESC [ 1 ; 5 C`), `ESC O A`: none must leak into the line.
        feed(&mut ed, b"\x1b[2~\x1b[15~\x1b[1;5C\x1bOA");
        assert_eq!(ed.line(), "");
        // A lone Esc then a letter: the letter is typed.
        feed(&mut ed, b"\x1bx");
        assert_eq!(ed.line(), "x");
    }

    #[test]
    fn line_never_overflows() {
        let mut ed = LineEditor::new();
        for _ in 0..MAX_INPUT + 10 {
            feed(&mut ed, b"a");
        }
        feed(&mut ed, b"\x1b[H");
        feed(&mut ed, b"b"); // full: refused even in the middle
        assert_eq!(ed.line().len(), MAX_INPUT);
        assert!(ed.line().bytes().all(|b| b == b'a'));
    }
}
