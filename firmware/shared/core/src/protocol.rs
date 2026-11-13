//! Line-based frame format sent over UART1.
//!
//! Every frame is one ASCII line, NMEA style, with an XOR checksum of the bytes
//! between `$` and `*`:
//!
//! ```text
//! $DAT,<node>,<seq>,<t_ms>,<btn>,<events>,<period_us>*CS  periodic data
//! $EVT,<node>,<seq>,<t_ms>,<RISE|FALL>,<key>*CS  button edge, key = 64-bit random hex
//! $BAU,<node>,<seq>,<t_ms>,<baud>,<REQ|ACK>*CS     link speed change handshake
//! $ECH,<node>,<seq>,<t_ms>,<echo_seq>*CS           reply to a frame (round-trip time)
//! $BST,<node>,<burst>,<t_ms>,<index>,<count>,<fill>*CS  one frame of a burst
//! ```
//!
//! `seq` is shared by `DAT`, `EVT`, `BAU` and `ECH` so the receiver can detect lost
//! frames. In a `BST` frame it holds the burst number instead, and `index`/`count`
//! tell the position in the burst; `fill` is a value derived from `index`
//! ([`burst_fill`]), checked on reception on top of the checksum.
//! `period_us` is the sender's DAT period (µs, so 12.5 ms is exact): nodes may use different periods, and the
//! receiver needs the peer's one to decide when the link is down.
//!
//! ```
//! use workshop_core::protocol::{encode, parse, Frame, Payload};
//!
//! let frame = Frame {
//!     node: "node-A",
//!     seq: 7,
//!     t_ms: 12_500,
//!     payload: Payload::Data { pressed: true, events: 2, period_us: 2_500_000 },
//! };
//! let line = encode(&frame);
//! assert_eq!(line, "$DAT,node-A,7,12500,1,2,2500000*08\n");
//!
//! // The receiver gets the line without its `\n`.
//! assert_eq!(parse(line.trim_end()), Ok(frame));
//! ```

use core::fmt::Write as _;

use heapless::String;

/// Longest frame line, including `$`, `*CS` and `\n`.
pub const MAX_FRAME: usize = 96;

/// Direction of a button edge, as seen on the GPIO line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Jumper removed: the pull-up brings the line high (button released).
    Rising,
    /// Jumper to GND: the line goes low (button pressed).
    Falling,
}

impl Edge {
    /// Tag used on the wire (`RISE` / `FALL`).
    pub fn tag(self) -> &'static str {
        match self {
            Edge::Rising => "RISE",
            Edge::Falling => "FALL",
        }
    }
}

/// Frame-type specific fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Payload {
    /// `DAT`: periodic data.
    Data {
        /// Sender's button state.
        pressed: bool,
        /// `EVT` frames sent since boot.
        events: u32,
        /// Sender's DAT period, ms.
        period_us: u32,
    },
    /// `EVT`: button edge.
    Event {
        /// Which edge.
        edge: Edge,
        /// 64-bit random key from the sender's TRNG.
        key: u64,
    },
    /// `BAU`: link speed change. The requester sends `REQ` at the current speed,
    /// the peer answers `ACK` then switches; the requester switches on the `ACK`.
    Baud {
        /// New baud rate.
        baud: u32,
        /// `false`: request (`REQ`), `true`: acknowledgement (`ACK`).
        ack: bool,
    },
    /// `ECH`: reply sent for each received frame by a node in `on-rx` TX mode.
    Echo {
        /// `seq` of the frame this answers: the sender of that frame gets the
        /// round-trip time from it.
        echo_seq: u32,
    },
    /// `BST`: one frame of a burst (the frame's `seq` is the burst number).
    Burst {
        /// Position in the burst, from 0.
        index: u32,
        /// Number of frames in the burst (> `index`).
        count: u32,
    },
}

/// Filler of a `BST` frame: a value derived from `index` (golden-ratio hash), so
/// the receiver can check the payload on top of the checksum, and consecutive
/// frames differ in every byte.
pub const fn burst_fill(index: u32) -> u64 {
    (index as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// One frame, borrowing the node name from the line it was parsed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Sender's name (see [`crate::config::NodeConfig::name`]).
    pub node: &'a str,
    /// Sequence number, shared by both frame types.
    pub seq: u32,
    /// Sender's uptime when the frame was built, ms.
    pub t_ms: u32,
    /// Frame-type specific fields.
    pub payload: Payload,
}

/// Why a received line was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Line does not look like `$...*CS`.
    Malformed,
    /// Checksum mismatch: the line was corrupted on the wire.
    Checksum {
        /// Checksum computed from the received body.
        expected: u8,
        /// Checksum found after `*`.
        received: u8,
    },
    /// Unknown frame type, invalid, missing or extra field.
    BadField,
}

/// XOR of all bytes: the NMEA checksum of a frame body.
pub fn checksum(body: &[u8]) -> u8 {
    body.iter().fold(0, |acc, b| acc ^ b)
}

/// Checks the envelope of a received line, `$<body>*<CS>`, and returns the body: what is between
/// `$` and `*`, once its checksum is right. Shared by every frame family ([`parse`], and the
/// address space frames of [`crate::space`]).
pub fn verify(line: &str) -> Result<&str, ParseError> {
    let line = line.trim_end_matches('\r');
    let rest = line.strip_prefix('$').ok_or(ParseError::Malformed)?;
    let (body, cs) = rest.rsplit_once('*').ok_or(ParseError::Malformed)?;
    // Exactly the 2 uppercase hex digits `encode` writes: accepting `6a` for `6A`
    // would let a corrupted byte (case flip) in the checksum go unnoticed.
    if cs.len() != 2 || !cs.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)) {
        return Err(ParseError::Malformed);
    }
    let received = u8::from_str_radix(cs, 16).map_err(|_| ParseError::Malformed)?;
    let expected = checksum(body.as_bytes());
    if expected != received {
        return Err(ParseError::Checksum { expected, received });
    }
    Ok(body)
}

/// Encodes a frame into a ready-to-send line terminated by `\n`.
pub fn encode(frame: &Frame<'_>) -> String<MAX_FRAME> {
    let mut body: String<MAX_FRAME> = String::new();
    let Frame { node, seq, t_ms, payload } = frame;
    // Writes can only fail on overflow; node names are short so it never happens
    // (the longest possible frame is checked by a unit test).
    let _ = match payload {
        Payload::Data { pressed, events, period_us } => {
            write!(body, "DAT,{node},{seq},{t_ms},{},{events},{period_us}", u8::from(*pressed))
        }
        Payload::Event { edge, key } => write!(body, "EVT,{node},{seq},{t_ms},{},{key:016X}", edge.tag()),
        Payload::Baud { baud, ack } => {
            write!(body, "BAU,{node},{seq},{t_ms},{baud},{}", if *ack { "ACK" } else { "REQ" })
        }
        Payload::Echo { echo_seq } => write!(body, "ECH,{node},{seq},{t_ms},{echo_seq}"),
        Payload::Burst { index, count } => {
            write!(body, "BST,{node},{seq},{t_ms},{index},{count},{:016X}", burst_fill(*index))
        }
    };

    let mut line: String<MAX_FRAME> = String::new();
    let _ = writeln!(line, "${body}*{:02X}", checksum(body.as_bytes()));
    line
}

/// Parses one received line (without the trailing `\n`; a trailing `\r` is ignored).
///
/// ```
/// use workshop_core::protocol::{parse, ParseError};
///
/// assert!(parse("$EVT,node-B,3,900,FALL,00000000DEADBEEF*09").is_ok());
/// // One flipped bit on the wire:
/// assert!(matches!(parse("$EVT,node-B,3,900,FALL,00000000DEADBEEE*09"), Err(ParseError::Checksum { .. })));
/// assert_eq!(parse("hello"), Err(ParseError::Malformed));
/// ```
pub fn parse(line: &str) -> Result<Frame<'_>, ParseError> {
    let body = verify(line)?;
    let mut fields = body.split(',');
    let mut next = || fields.next().ok_or(ParseError::BadField);
    let kind = next()?;
    let node = next()?;
    let seq = next()?.parse().map_err(|_| ParseError::BadField)?;
    let t_ms = next()?.parse().map_err(|_| ParseError::BadField)?;

    let payload = match kind {
        "DAT" => Payload::Data {
            pressed: match next()? {
                "0" => false,
                "1" => true,
                _ => return Err(ParseError::BadField),
            },
            events: next()?.parse().map_err(|_| ParseError::BadField)?,
            period_us: next()?.parse().map_err(|_| ParseError::BadField)?,
        },
        "EVT" => Payload::Event {
            edge: match next()? {
                "RISE" => Edge::Rising,
                "FALL" => Edge::Falling,
                _ => return Err(ParseError::BadField),
            },
            key: u64::from_str_radix(next()?, 16).map_err(|_| ParseError::BadField)?,
        },
        "BAU" => Payload::Baud {
            baud: next()?.parse().map_err(|_| ParseError::BadField)?,
            ack: match next()? {
                "REQ" => false,
                "ACK" => true,
                _ => return Err(ParseError::BadField),
            },
        },
        "ECH" => Payload::Echo { echo_seq: next()?.parse().map_err(|_| ParseError::BadField)? },
        "BST" => {
            let index: u32 = next()?.parse().map_err(|_| ParseError::BadField)?;
            let count: u32 = next()?.parse().map_err(|_| ParseError::BadField)?;
            let fill = u64::from_str_radix(next()?, 16).map_err(|_| ParseError::BadField)?;
            if index >= count || fill != burst_fill(index) {
                return Err(ParseError::BadField);
            }
            Payload::Burst { index, count }
        }
        _ => return Err(ParseError::BadField),
    };
    if fields.next().is_some() {
        return Err(ParseError::BadField);
    }

    Ok(Frame { node, seq, t_ms, payload })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::test_vectors;

    fn data(seq: u32) -> Frame<'static> {
        Frame {
            node: "node-A",
            seq,
            t_ms: 1000,
            payload: Payload::Data { pressed: false, events: 0, period_us: 2_500_000 },
        }
    }

    /// Re-signs a hand-edited body so only the field under test is wrong.
    fn signed(body: &str) -> std::string::String {
        format!("${body}*{:02X}", checksum(body.as_bytes()))
    }

    #[test]
    fn known_frames_encode_to_known_lines() {
        for (frame, line) in test_vectors::FRAMES {
            assert_eq!(encode(frame), *line);
            assert_eq!(parse(line.trim_end()), Ok(*frame));
        }
    }

    #[test]
    fn longest_possible_frame_fits() {
        // 12-char name (the limit enforced by config) and every number at its maximum.
        let name = "ABCDEFGHIJKL";
        for payload in [
            Payload::Data { pressed: true, events: u32::MAX, period_us: u32::MAX },
            Payload::Event { edge: Edge::Falling, key: u64::MAX },
            Payload::Baud { baud: u32::MAX, ack: true },
            Payload::Echo { echo_seq: u32::MAX },
            Payload::Burst { index: u32::MAX - 1, count: u32::MAX },
        ] {
            let frame = Frame { node: name, seq: u32::MAX, t_ms: u32::MAX, payload };
            let line = encode(&frame);
            assert!(line.ends_with('\n'), "truncated: {line:?}");
            assert_eq!(parse(line.trim_end()), Ok(frame));
        }
    }

    #[test]
    fn burst_fill_differs_between_consecutive_frames() {
        for i in 0..1000 {
            let (a, b) = (burst_fill(i).to_be_bytes(), burst_fill(i + 1).to_be_bytes());
            assert!(a.iter().zip(b).filter(|(x, y)| *x != y).count() >= 4, "{i}");
        }
    }

    /// Found by `single_byte_corruption_is_detected`: a case flip in the checksum.
    #[test]
    fn lowercase_checksum_is_rejected() {
        let line = encode(&data(1));
        let line = line.trim_end();
        let (body, cs) = line.rsplit_once('*').unwrap();
        assert!(cs.bytes().any(|b| b.is_ascii_uppercase()), "pick a frame whose checksum has a letter: {line}");
        assert_eq!(parse(&format!("{body}*{}", cs.to_ascii_lowercase())), Err(ParseError::Malformed));
    }

    #[test]
    fn crlf_is_accepted() {
        let line = encode(&data(1));
        let crlf = format!("{}\r", line.trim_end());
        assert_eq!(parse(&crlf), Ok(data(1)));
    }

    #[test]
    fn checksum_error_reports_both_values() {
        let line = encode(&data(1));
        let bad = line.trim_end().replace("node-A", "node-B");
        let Err(ParseError::Checksum { expected, received }) = parse(&bad) else { panic!() };
        assert_ne!(expected, received);
    }

    #[test]
    fn malformed_lines() {
        for line in ["", "DAT,a,1,2,0,0,10*00", "$DAT,a,1,2,0,0,10", "$DAT*ZZ", "$*", "$DAT*1", "$DAT*001"] {
            assert!(matches!(parse(line), Err(ParseError::Malformed | ParseError::Checksum { .. })), "{line:?}");
        }
    }

    #[test]
    fn bad_fields_are_rejected_even_with_a_valid_checksum() {
        for body in [
            "XYZ,a,1,2,0,0,10",                 // unknown type
            "DAT,a,1,2,0,0",                    // missing field
            "DAT,a,1,2,0,0,10,99",              // extra field
            "DAT,a,x,2,0,0,10",                 // seq not a number
            "DAT,a,1,2,7,0,10",                 // button neither 0 nor 1
            "DAT,a,1,2,0,-1,10",                // negative
            "EVT,a,1,2,DOWN,00",                // unknown edge
            "EVT,a,1,2,RISE,XYZ",               // key not hex
            "EVT,a,1,2,RISE,1FFFFFFFFFFFFFFFF", // key > 64 bits
            "BAU,a,1,2,9600,MAYBE",             // neither REQ nor ACK
            "BAU,a,1,2,fast,REQ",               // baud not a number
            "ECH,a,1,2",                        // missing echoed seq
            "BST,a,1,2,5,5,0000000000000000",   // index not below count
            "BST,a,1,2,0,5,0000000000000000",   // wrong fill for index 0
        ] {
            assert_eq!(parse(&signed(body)), Err(ParseError::BadField), "{body}");
        }
    }

    fn any_frame() -> impl Strategy<Value = (std::string::String, u32, u32, Payload)> {
        let name = "[!-~&&[^,*$]]{1,12}"; // the names `NodeConfig::validate` accepts
        let payload = prop_oneof![
            (any::<bool>(), any::<u32>(), any::<u32>()).prop_map(|(pressed, events, period_us)| Payload::Data {
                pressed,
                events,
                period_us
            }),
            (any::<bool>(), any::<u64>())
                .prop_map(|(rise, key)| Payload::Event { edge: if rise { Edge::Rising } else { Edge::Falling }, key }),
            (any::<u32>(), any::<bool>()).prop_map(|(baud, ack)| Payload::Baud { baud, ack }),
            any::<u32>().prop_map(|echo_seq| Payload::Echo { echo_seq }),
            (1u32.., any::<prop::sample::Index>())
                .prop_map(|(count, i)| Payload::Burst { index: i.index(count as usize) as u32, count }),
        ];
        (name, any::<u32>(), any::<u32>(), payload)
    }

    proptest! {
        /// Whatever a node sends, the peer decodes exactly the same frame.
        #[test]
        fn encode_then_parse_is_identity((name, seq, t_ms, payload) in any_frame()) {
            let frame = Frame { node: &name, seq, t_ms, payload };
            let line = encode(&frame);
            prop_assert!(line.len() <= MAX_FRAME && line.ends_with('\n'));
            prop_assert_eq!(parse(line.trim_end()), Ok(frame));
        }

        /// Any single corrupted byte is caught: XOR checksum detects every 1-byte error.
        #[test]
        fn single_byte_corruption_is_detected(
            (name, seq, t_ms, payload) in any_frame(),
            pos in any::<prop::sample::Index>(),
            flip in 1u8..=0x7f,
        ) {
            let frame = Frame { node: &name, seq, t_ms, payload };
            let mut bytes = encode(&frame).trim_end().as_bytes().to_vec();
            let i = pos.index(bytes.len());
            bytes[i] ^= flip;
            if let Ok(text) = core::str::from_utf8(&bytes) {
                prop_assert_ne!(parse(text), Ok(frame));
            }
        }

        /// The parser never panics, whatever arrives on the wire.
        #[test]
        fn parse_never_panics(line in ".{0,120}") {
            let _ = parse(&line);
        }
    }
}
