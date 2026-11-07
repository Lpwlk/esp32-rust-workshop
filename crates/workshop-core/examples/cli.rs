//! The console: keys go into the line editor, finished lines become commands.
//!
//! cargo run -p workshop-core --example cli --target host-tuple

use workshop_core::cli::{self, Command};
use workshop_core::editor::{EditorEvent, LineEditor};

fn main() {
    // --8<-- [start:editor]
    let mut editor = LineEditor::new();
    // "patern" with a typo: ← ← ← (ESC [ D), insert the missing "t", End (ESC [ F),
    // then the value, Enter. Exactly the bytes the terminal sends.
    let keys = b"patern\x1b[D\x1b[D\x1b[Dt\x1b[F ocean\r";
    let mut submitted = None;
    for &key in keys {
        if let Some(EditorEvent::Submitted(line)) = editor.feed(key) {
            submitted = Some(line);
        }
    }
    let line = submitted.unwrap();
    println!("submitted: {line:?}");
    assert_eq!(line, "pattern ocean");
    // --8<-- [end:editor]

    // --8<-- [start:parse]
    // Shortcuts are just `set <key> <value>`:
    match cli::parse(&line) {
        Ok(Command::Set { key, value }) => println!("set {key} = {value}"),
        other => panic!("unexpected: {other:?}"),
    }
    for input in ["help set", "baud 1M", "burst 1000 max", "led 3 on", "frobnicate"] {
        match cli::parse(input) {
            Ok(cmd) => println!("{input:<16} → {cmd:?}"),
            Err(e) => println!("{input:<16} → error: {e}"),
        }
    }
    // --8<-- [end:parse]
}
