//! Frames on the UART link: build one, encode it, parse what arrives, catch a
//! corrupted byte, and count lost frames.
//!
//! cargo run -p workshop-core --example frames --target host-tuple
//!
//! The `// --8<--` markers delimit the parts included in the documentation
//! (docs/book/examples.md): what you read there is this file, compiled and run.

use workshop_core::line::{LineBuffer, LineEvent};
use workshop_core::protocol::{self, Edge, Frame, MAX_FRAME, ParseError, Payload};
use workshop_core::seq::{self, SeqCheck};

fn main() {
    // --8<-- [start:encode]
    let frame = Frame {
        node: "node-A",
        seq: 42,
        t_ms: 12_345,
        payload: Payload::Data { pressed: true, events: 3, period_us: 2_500_000 },
    };
    let line = protocol::encode(&frame); // heapless::String: no allocator needed
    println!("on the wire: {}", line.trim_end());
    assert!(line.starts_with("$DAT,node-A,42,12345,"));
    assert!(line.ends_with('\n'));
    // --8<-- [end:encode]

    // --8<-- [start:parse]
    match protocol::parse(line.trim_end()) {
        Ok(Frame { node, seq, payload: Payload::Data { pressed, .. }, .. }) => {
            println!("DAT #{seq} from {node}, button {}", if pressed { "pressed" } else { "released" });
        }
        Ok(other) => println!("another frame: {other:?}"),
        Err(e) => println!("refused: {e:?}"),
    }
    // --8<-- [end:parse]

    // --8<-- [start:corrupt]
    // One byte flipped on the wire: the XOR checksum no longer matches.
    let corrupted = line.trim_end().replacen("node-A", "node-B", 1);
    match protocol::parse(&corrupted) {
        Err(ParseError::Checksum { expected, received }) => {
            println!("corrupted: checksum 0x{received:02X}, expected 0x{expected:02X}");
        }
        other => panic!("a corrupted line must be refused, got {other:?}"),
    }
    // --8<-- [end:corrupt]

    // --8<-- [start:stream]
    // Bytes arrive in arbitrary chunks (the UART interrupt drains its FIFO): the
    // line buffer gives back whole lines, whatever the chunking.
    let event =
        Frame { node: "node-B", seq: 7, t_ms: 900, payload: Payload::Event { edge: Edge::Falling, key: 0xC0FFEE } };
    let mut stream = protocol::encode(&event).as_bytes().to_vec();
    stream.extend_from_slice(line.as_bytes());

    let mut rx: LineBuffer<MAX_FRAME> = LineBuffer::new();
    let mut last_seq = None;
    for chunk in stream.chunks(5) {
        for &byte in chunk {
            if let Some(LineEvent::Line(bytes)) = rx.push(byte) {
                let text = core::str::from_utf8(bytes).unwrap();
                let frame = protocol::parse(text).unwrap();
                let check = seq::check(last_seq, frame.seq);
                last_seq = Some(frame.seq);
                println!("{} #{} from {}: {check:?}", &text[1..4], frame.seq, frame.node);
            }
        }
    }
    // 7 then 42 from "different" nodes in this demo: 34 frames counted as lost.
    assert_eq!(seq::check(Some(7), 42), SeqCheck::Lost(34));
    // --8<-- [end:stream]
}
