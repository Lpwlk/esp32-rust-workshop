//! Splits the received byte stream into `\n`-terminated lines.
//!
//! The UART delivers bytes in arbitrary chunks: a frame can arrive in several
//! pieces, several frames in one piece. Lines longer than the buffer are dropped
//! as a whole (reported once, at their `\n`), never truncated into something that
//! could look like a valid frame.
//!
//! ```
//! use workshop_core::line::{LineBuffer, LineEvent};
//!
//! let mut rx = LineBuffer::<16>::new();
//! let mut lines = Vec::new();
//! for &b in b"$A*41\n$B*" {
//!     if let Some(LineEvent::Line(l)) = rx.push(b) {
//!         lines.push(l.to_vec());
//!     }
//! }
//! assert_eq!(lines, [b"$A*41".to_vec()]); // `$B*` waits for the rest
//! ```

use heapless::Vec;

/// What a byte completed.
#[derive(Debug, PartialEq, Eq)]
pub enum LineEvent<'a> {
    /// A complete line, without its `\n`.
    Line(&'a [u8]),
    /// A line that didn't fit in the buffer was dropped.
    TooLong,
}

/// Line assembler with room for `N` bytes per line.
#[derive(Default)]
pub struct LineBuffer<const N: usize> {
    buf: Vec<u8, N>,
    overflow: bool,
    /// The previous byte ended a line: start a new one on the next push.
    done: bool,
}

impl<const N: usize> LineBuffer<N> {
    /// An empty buffer.
    pub const fn new() -> Self {
        Self { buf: Vec::new(), overflow: false, done: false }
    }

    /// Adds one received byte; returns an event when it is a `\n`.
    pub fn push(&mut self, byte: u8) -> Option<LineEvent<'_>> {
        if self.done {
            self.buf.clear();
            self.overflow = false;
            self.done = false;
        }
        if byte != b'\n' {
            self.overflow |= self.buf.push(byte).is_err();
            return None;
        }
        self.done = true;
        Some(if self.overflow { LineEvent::TooLong } else { LineEvent::Line(&self.buf) })
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[derive(Debug, PartialEq)]
    enum Ev {
        Line(std::vec::Vec<u8>),
        TooLong,
    }

    fn feed<const N: usize>(rx: &mut LineBuffer<N>, bytes: &[u8]) -> std::vec::Vec<Ev> {
        let mut out = std::vec::Vec::new();
        for &b in bytes {
            match rx.push(b) {
                Some(LineEvent::Line(l)) => out.push(Ev::Line(l.to_vec())),
                Some(LineEvent::TooLong) => out.push(Ev::TooLong),
                None => {}
            }
        }
        out
    }

    #[test]
    fn empty_lines_are_reported() {
        assert_eq!(feed(&mut LineBuffer::<8>::new(), b"\n\n"), [Ev::Line(vec![]), Ev::Line(vec![])]);
    }

    #[test]
    fn exactly_full_line_fits_one_more_does_not() {
        let mut rx = LineBuffer::<4>::new();
        assert_eq!(
            feed(&mut rx, b"abcd\nabcde\nok\n"),
            [Ev::Line(b"abcd".to_vec()), Ev::TooLong, Ev::Line(b"ok".to_vec())]
        );
    }

    proptest! {
        /// However the stream is chunked, the same lines come out.
        #[test]
        fn chunking_does_not_matter(
            lines in prop::collection::vec("[ -~]{0,20}", 0..10),
            cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..10),
        ) {
            let stream: std::vec::Vec<u8> = lines.iter().flat_map(|l| l.bytes().chain(*b"\n")).collect();
            let mut points: std::vec::Vec<usize> = cuts.iter().map(|c| c.index(stream.len() + 1)).collect();
            points.sort();
            let mut rx = LineBuffer::<16>::new();
            let mut got = std::vec::Vec::new();
            let mut start = 0;
            for end in points.into_iter().chain([stream.len()]) {
                got.extend(feed(&mut rx, &stream[start..end]));
                start = end;
            }
            let expected: std::vec::Vec<Ev> = lines
                .iter()
                .map(|l| if l.len() <= 16 { Ev::Line(l.as_bytes().to_vec()) } else { Ev::TooLong })
                .collect();
            prop_assert_eq!(got, expected);
        }
    }
}
