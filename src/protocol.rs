//! Line-based frame format sent over UART1.
//!
//! Every frame is one ASCII line, NMEA style, with an XOR checksum of the bytes
//! between `$` and `*`:
//!
//! ```text
//! $DAT,<node>,<seq>,<t_ms>,<btn>,<events>,<period_ms>*CS  periodic data
//! $EVT,<node>,<seq>,<t_ms>,<RISE|FALL>,<key>*CS  button edge, key = 64-bit random hex
//! ```
//!
//! `seq` is shared by both frame types so the receiver can detect lost frames.
//! `period_ms` is the sender's DAT period: nodes may use different periods, and the
//! receiver needs the peer's one to decide when the link is down.

use core::fmt::Write as _;

use heapless::String;

/// Longest frame line, including `$`, `*CS` and `\n`.
pub const MAX_FRAME: usize = 96;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Jumper removed: the pull-up brings the line high (button released).
    Rising,
    /// Jumper to GND: the line goes low (button pressed).
    Falling,
}

impl Edge {
    fn tag(self) -> &'static str {
        match self {
            Edge::Rising => "RISE",
            Edge::Falling => "FALL",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Payload {
    Data { pressed: bool, events: u32, period_ms: u32 },
    Event { edge: Edge, key: u64 },
}

#[derive(Clone, Copy, Debug)]
pub struct Frame<'a> {
    pub node: &'a str,
    pub seq: u32,
    pub t_ms: u32,
    pub payload: Payload,
}

#[derive(Clone, Copy, Debug)]
pub enum ParseError {
    /// Line does not look like `$...*CS`.
    Malformed,
    /// Checksum mismatch: the line was corrupted on the wire.
    Checksum { expected: u8, received: u8 },
    /// Unknown frame type or invalid field.
    BadField,
}

fn checksum(body: &[u8]) -> u8 {
    body.iter().fold(0, |acc, b| acc ^ b)
}

/// Encodes a frame into a ready-to-send line terminated by `\n`.
pub fn encode(frame: &Frame<'_>) -> String<MAX_FRAME> {
    let mut body: String<MAX_FRAME> = String::new();
    let Frame { node, seq, t_ms, payload } = frame;
    // Writes can only fail on overflow; node names are short so it never happens.
    let _ = match payload {
        Payload::Data { pressed, events, period_ms } => {
            write!(body, "DAT,{node},{seq},{t_ms},{},{events},{period_ms}", u8::from(*pressed))
        }
        Payload::Event { edge, key } => write!(body, "EVT,{node},{seq},{t_ms},{},{key:016X}", edge.tag()),
    };

    let mut line: String<MAX_FRAME> = String::new();
    let _ = writeln!(line, "${body}*{:02X}", checksum(body.as_bytes()));
    line
}

/// Parses one received line (without the trailing `\n`).
pub fn parse(line: &str) -> Result<Frame<'_>, ParseError> {
    let line = line.trim_end_matches('\r');
    let rest = line.strip_prefix('$').ok_or(ParseError::Malformed)?;
    let (body, cs) = rest.rsplit_once('*').ok_or(ParseError::Malformed)?;
    let received = u8::from_str_radix(cs, 16).map_err(|_| ParseError::Malformed)?;
    let expected = checksum(body.as_bytes());
    if expected != received {
        return Err(ParseError::Checksum { expected, received });
    }

    let mut fields = body.split(',');
    let mut next = || fields.next().ok_or(ParseError::BadField);
    let kind = next()?;
    let node = next()?;
    let seq = next()?.parse().map_err(|_| ParseError::BadField)?;
    let t_ms = next()?.parse().map_err(|_| ParseError::BadField)?;

    let payload = match kind {
        "DAT" => Payload::Data {
            pressed: next()? == "1",
            events: next()?.parse().map_err(|_| ParseError::BadField)?,
            period_ms: next()?.parse().map_err(|_| ParseError::BadField)?,
        },
        "EVT" => Payload::Event {
            edge: match next()? {
                "RISE" => Edge::Rising,
                "FALL" => Edge::Falling,
                _ => return Err(ParseError::BadField),
            },
            key: u64::from_str_radix(next()?, 16).map_err(|_| ParseError::BadField)?,
        },
        _ => return Err(ParseError::BadField),
    };

    Ok(Frame { node, seq, t_ms, payload })
}
