//! **UA-lite**: a very small address space, in the spirit of OPC UA, on top of the same frames.
//!
//! A node *describes* what it has (LEDs, buttons, its statistics, its settings) as a tree of
//! named nodes, and the peer can **browse** it, **read** and **write** values, and **subscribe**
//! to changes. It travels in the same checksummed lines as the other frames
//! ([`crate::protocol`]), so it works unchanged over the UART, UDP datagrams and TCP.
//!
//! ```text
//! /                       Object
//! ├── node                Object      name (text, R)   uptime (u32 ms, R)
//! ├── leds                Object      1, 2 (text RW: "off", "on", "blink 200", "peer"...)
//! ├── buttons             Object      jumper, boot (bool, R)
//! ├── rgb                 Object      pattern (text RW)   brightness (u32 0-100, RW)
//! ├── stats               Object      tx, rx, lost (u32, R)
//! └── config              Object      one text variable per runtime setting (RW)
//! ```
//!
//! What is borrowed from OPC UA: nodes with a class, a data type and an access level; browsing
//! by parent; numeric status codes (`Good` = 0); subscriptions that report a value when it
//! changes. What is left out, on purpose: sessions, security, namespaces, a type system, binary
//! encoding, method calls. A frame is one short ASCII line.
//!
//! ```text
//! $BRW,<node>,<seq>,<t_ms>,<id>,<index>*CS                          browse: child #index of <id>
//! $BRR,<node>,<seq>,<t_ms>,<req>,<id>,<name>,<kind>,<more>*CS      one child (id 65535 = no more)
//! $RED,<node>,<seq>,<t_ms>,<id>*CS                                  read
//! $WRT,<node>,<seq>,<t_ms>,<id>,<value>*CS                          write
//! $SUB,<node>,<seq>,<t_ms>,<id>,<interval_ms>*CS                    subscribe (0 cancels; id 65535: all)
//! $VAL,<node>,<seq>,<t_ms>,<req>,<id>,<status>,<value>*CS           answer to read / write / subscribe,
//!                                                                   and every later change
//! ```
//!
//! `<req>` is the `seq` of the request an answer belongs to; `<kind>` is three letters: class
//! (`O`bject, `V`ariable), data type (`B`ool, `U`32, `T`ext, `-` for an object), access
//! (`R`ead, `W`rite).
//!
//! The whole thing is plain logic, so a request can be answered and checked on the computer:
//!
//! ```
//! use workshop_core::config::NodeConfig;
//! use workshop_core::space::{self, Body, Frame, NodeSpace, Status, Subscriptions};
//!
//! let mut cfg = NodeConfig::DEFAULT;
//! let mut space = NodeSpace { cfg: &mut cfg, uptime_ms: 5000, jumper: true, boot: false, tx: 3, rx: 2, lost: 0 };
//! let mut subs = Subscriptions::<4>::new();
//!
//! // "read /buttons/jumper", as it arrives on the wire:
//! let id = space::find("/buttons/jumper").unwrap();
//! let request = Frame { node: "node-B", seq: 7, t_ms: 1000, body: Body::Read { id } };
//! let line = space::encode(&request);
//! let parsed = space::parse(line.trim_end()).unwrap();
//!
//! let answer = space::serve(&parsed, &mut space, &mut subs, 1000).unwrap();
//! assert!(matches!(answer, Body::Value { req: 7, status: Status::Good, ref value, .. } if value == "1"));
//! ```

use core::fmt::{self, Write as _};

use heapless::String;

use crate::{
    config::{KEYS, NodeConfig, SetError},
    protocol::{self, MAX_FRAME, ParseError},
};

/// Identifier of a node in the address space.
pub type NodeId = u16;

/// The root object: `/`.
pub const ROOT: NodeId = 0;
/// "No node": the end of a browse, and the parent of the root.
pub const NONE: NodeId = 0xFFFF;
/// Longest value, in characters. Fits the longest setting (`blink 60000`...) and keeps every frame
/// under [`MAX_FRAME`].
pub const VALUE_LEN: usize = 24;
/// A value as it is carried in a frame: text, whatever its data type.
pub type Value = String<VALUE_LEN>;
/// Longest node name.
pub const NAME_LEN: usize = 16;

/// First [`NodeId`] of the `/config` variables: `CONFIG_BASE + n` is the n-th runtime setting.
pub const CONFIG_BASE: NodeId = 100;
/// The `/config` object.
pub const CONFIG: NodeId = 17;

/// What a node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Has children, no value.
    Object,
    /// Has a value.
    Variable,
}

/// How a variable's value is read and written (it is always carried as text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataType {
    /// An object has no value.
    None,
    /// `0` or `1`.
    Bool,
    /// A whole number, 0 to 4294967295.
    U32,
    /// A short text without `,` `*` `$`.
    Text,
}

/// A node of the address space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeDesc {
    /// Its identifier.
    pub id: NodeId,
    /// Its parent ([`NONE`] for the root).
    pub parent: NodeId,
    /// Its name: one segment of its path.
    pub name: &'static str,
    /// Object or variable.
    pub class: Class,
    /// Data type of a variable.
    pub ty: DataType,
    /// Whether a write is accepted.
    pub writable: bool,
    /// One line: what it is. Not sent on the wire; it feeds the generated reference.
    pub doc: &'static str,
}

const fn object(id: NodeId, parent: NodeId, name: &'static str, doc: &'static str) -> NodeDesc {
    NodeDesc { id, parent, name, class: Class::Object, ty: DataType::None, writable: false, doc }
}

const fn variable(
    id: NodeId,
    parent: NodeId,
    name: &'static str,
    ty: DataType,
    writable: bool,
    doc: &'static str,
) -> NodeDesc {
    NodeDesc { id, parent, name, class: Class::Variable, ty, writable, doc }
}

/// The fixed part of the address space, in browse order. The `/config` variables come after it
/// ([`describe`]).
pub const NODES: [NodeDesc; 18] = [
    object(ROOT, NONE, "", "the root of the address space"),
    object(1, ROOT, "node", "this node"),
    variable(2, 1, "name", DataType::Text, false, "the node's name, as sent in every frame (fixed at build time)"),
    variable(3, 1, "uptime", DataType::U32, false, "time since boot, ms"),
    object(4, ROOT, "leds", "the two breadboard LEDs"),
    variable(5, 4, "1", DataType::Text, true, "mode of LED 1: off, on, blink [ms], button, peer, link, rx, tx"),
    variable(6, 4, "2", DataType::Text, true, "mode of LED 2, same values as LED 1"),
    object(7, ROOT, "buttons", "the two buttons"),
    variable(8, 7, "jumper", DataType::Bool, false, "1 while the jumper button is pressed (debounced)"),
    variable(9, 7, "boot", DataType::Bool, false, "1 while the board's BOOT button is pressed (debounced)"),
    object(10, ROOT, "rgb", "the on-board RGB LED"),
    variable(11, 10, "pattern", DataType::Text, true, "fade pattern: viridis, sunset, ocean, aurora, sakura"),
    variable(12, 10, "brightness", DataType::U32, true, "0-100 %"),
    object(13, ROOT, "stats", "frame counters since boot"),
    variable(14, 13, "tx", DataType::U32, false, "frames sent"),
    variable(15, 13, "rx", DataType::U32, false, "frames received"),
    variable(16, 13, "lost", DataType::U32, false, "frames lost (gaps in the peer's sequence numbers)"),
    object(CONFIG, ROOT, "config", "the runtime settings: one text variable each, as `set` takes them"),
];

/// Describes a node: from [`NODES`], or one of the `/config` variables.
pub fn describe(id: NodeId) -> Option<NodeDesc> {
    if let Some(n) = NODES.iter().find(|n| n.id == id) {
        return Some(*n);
    }
    let index = usize::from(id.checked_sub(CONFIG_BASE)?);
    KEYS.get(index).map(|(name, key)| variable(id, CONFIG, name, DataType::Text, true, key.describe().0))
}

/// The children of `parent`, in browse order.
fn children_of(parent: NodeId) -> impl Iterator<Item = NodeDesc> {
    let config = (parent == CONFIG).then(|| (0..KEYS.len()).filter_map(|i| describe(CONFIG_BASE + i as NodeId)));
    NODES.iter().copied().filter(move |n| n.parent == parent).chain(config.into_iter().flatten())
}

/// The `index`-th child of `parent`, and whether more follow it. `None` when there is no such child.
pub fn child(parent: NodeId, index: usize) -> Option<(NodeDesc, bool)> {
    if parent == NONE {
        return None; // "no node" has no children (the root's parent is NONE, but isn't browsed from it)
    }
    let mut children = children_of(parent);
    let node = children.nth(index)?;
    Some((node, children.next().is_some()))
}

/// Finds a node by path: `/leds/1`, `leds/1`, `/`.
pub fn find(path: &str) -> Option<NodeId> {
    let mut id = ROOT;
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        id = children_of(id).find(|n| n.name == segment)?.id;
    }
    Some(id)
}

/// Writes a node's path (`/buttons/jumper`); `/` for the root. Writes `?` for an unknown id.
pub fn path(id: NodeId, out: &mut impl fmt::Write) -> fmt::Result {
    let Some(node) = describe(id) else { return out.write_char('?') };
    if node.parent == NONE {
        return out.write_char('/');
    }
    if node.parent != ROOT {
        path(node.parent, out)?;
    }
    write!(out, "/{}", node.name)
}

/// The node's path as a string.
pub fn path_string(id: NodeId) -> String<48> {
    let mut s = String::new();
    let _ = path(id, &mut s);
    s
}

/// What a request came to: UA-style status codes, `Good` = 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    /// Done.
    Good = 0,
    /// No such node.
    BadNodeIdUnknown = 1,
    /// The node can't be written.
    BadNotWritable = 2,
    /// The node has no value, or the value isn't of its type.
    BadTypeMismatch = 3,
    /// Of the right type, refused by the node's own checks (range, name...).
    BadOutOfRange = 4,
    /// All subscription slots are taken.
    BadTooManySubscriptions = 5,
}

impl Status {
    /// From the number on the wire.
    pub const fn from_code(code: u8) -> Option<Status> {
        Some(match code {
            0 => Status::Good,
            1 => Status::BadNodeIdUnknown,
            2 => Status::BadNotWritable,
            3 => Status::BadTypeMismatch,
            4 => Status::BadOutOfRange,
            5 => Status::BadTooManySubscriptions,
            _ => return None,
        })
    }

    /// The name shown in the console.
    pub const fn name(self) -> &'static str {
        match self {
            Status::Good => "Good",
            Status::BadNodeIdUnknown => "BadNodeIdUnknown",
            Status::BadNotWritable => "BadNotWritable",
            Status::BadTypeMismatch => "BadTypeMismatch",
            Status::BadOutOfRange => "BadOutOfRange",
            Status::BadTooManySubscriptions => "BadTooManySubscriptions",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

/// The three-letter `<kind>` of a node, as it goes in a `BRR` frame: class, data type, access.
pub fn kind_of(node: &NodeDesc) -> [u8; 3] {
    [
        match node.class {
            Class::Object => b'O',
            Class::Variable => b'V',
        },
        match node.ty {
            DataType::None => b'-',
            DataType::Bool => b'B',
            DataType::U32 => b'U',
            DataType::Text => b'T',
        },
        if node.writable { b'W' } else { b'R' },
    ]
}

/// What a `<kind>` says, spelled out: `variable text read-write`.
pub fn describe_kind(kind: [u8; 3]) -> String<32> {
    let mut s = String::new();
    let class = if kind[0] == b'O' { "object" } else { "variable" };
    let ty = match kind[1] {
        b'B' => " bool",
        b'U' => " u32",
        b'T' => " text",
        _ => "",
    };
    let _ = write!(s, "{class}{ty}{}", if kind[2] == b'W' { " read-write" } else { "" });
    s
}

/// What a frame carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// `BRW`: asks for child number `index` of `id`.
    Browse {
        /// The parent.
        id: NodeId,
        /// Which child, from 0.
        index: u16,
    },
    /// `BRR`: one child, or the end of the list.
    BrowseResult {
        /// `seq` of the `BRW` this answers.
        req: u32,
        /// The child ([`NONE`]: there is no such child).
        id: NodeId,
        /// Its name.
        name: String<NAME_LEN>,
        /// Its [`kind_of`].
        kind: [u8; 3],
        /// Whether a next child exists.
        more: bool,
    },
    /// `RED`: reads a variable.
    Read {
        /// The variable.
        id: NodeId,
    },
    /// `WRT`: writes a variable.
    Write {
        /// The variable.
        id: NodeId,
        /// The new value, as text.
        value: Value,
    },
    /// `SUB`: reports changes of a variable, checking every `interval_ms` (0: stop).
    Subscribe {
        /// The variable.
        id: NodeId,
        /// Sampling interval, ms.
        interval_ms: u32,
    },
    /// `VAL`: a value, or why there is none. The answer to a read, a write, a subscribe, and
    /// every notification that follows a subscribe.
    Value {
        /// `seq` of the request (for a notification: of the `SUB`).
        req: u32,
        /// The variable.
        id: NodeId,
        /// How it went.
        status: Status,
        /// The value (empty when `status` isn't `Good`, or for a write's answer when ...).
        value: Value,
    },
}

/// A frame of the address space protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Sender's name.
    pub node: &'a str,
    /// Sequence number, shared with the other frames of the sender.
    pub seq: u32,
    /// Sender's uptime, ms.
    pub t_ms: u32,
    /// What it says.
    pub body: Body,
}

/// Frame tags of this protocol.
pub const TAGS: [&str; 6] = ["BRW", "BRR", "RED", "WRT", "SUB", "VAL"];

/// Whether a received line is a frame of this protocol (by its tag, before any other check).
pub fn is_space(line: &str) -> bool {
    line.strip_prefix('$')
        .and_then(|rest| rest.get(..4))
        .is_some_and(|head| head.ends_with(',') && TAGS.contains(&&head[..3]))
}

/// Whether a value can travel in a frame: printable ASCII without `,` `*` `$`.
pub fn text_ok(text: &str) -> bool {
    text.len() <= VALUE_LEN && text.bytes().all(|b| (0x20..0x7f).contains(&b) && !matches!(b, b',' | b'*' | b'$'))
}

/// Encodes a frame into a line terminated by `\n`.
pub fn encode(frame: &Frame<'_>) -> String<MAX_FRAME> {
    let mut body: String<MAX_FRAME> = String::new();
    let Frame { node, seq, t_ms, body: b } = frame;
    let _ = match b {
        Body::Browse { id, index } => write!(body, "BRW,{node},{seq},{t_ms},{id},{index}"),
        Body::BrowseResult { req, id, name, kind, more } => write!(
            body,
            "BRR,{node},{seq},{t_ms},{req},{id},{name},{}{}{},{}",
            kind[0] as char,
            kind[1] as char,
            kind[2] as char,
            u8::from(*more)
        ),
        Body::Read { id } => write!(body, "RED,{node},{seq},{t_ms},{id}"),
        Body::Write { id, value } => write!(body, "WRT,{node},{seq},{t_ms},{id},{value}"),
        Body::Subscribe { id, interval_ms } => write!(body, "SUB,{node},{seq},{t_ms},{id},{interval_ms}"),
        Body::Value { req, id, status, value } => {
            write!(body, "VAL,{node},{seq},{t_ms},{req},{id},{},{value}", *status as u8)
        }
    };
    let mut line: String<MAX_FRAME> = String::new();
    let _ = writeln!(line, "${body}*{:02X}", protocol::checksum(body.as_bytes()));
    line
}

/// Parses a received line (without its `\n`). Same checksum rules as [`protocol::parse`].
pub fn parse(line: &str) -> Result<Frame<'_>, ParseError> {
    let body = protocol::verify(line)?;
    let mut fields = body.split(',');
    let mut next = || fields.next().ok_or(ParseError::BadField);
    let tag = next()?;
    let node = next()?;
    let seq = next()?.parse().map_err(|_| ParseError::BadField)?;
    let t_ms = next()?.parse().map_err(|_| ParseError::BadField)?;
    fn num<T: core::str::FromStr>(text: &str) -> Result<T, ParseError> {
        text.parse().map_err(|_| ParseError::BadField)
    }
    let text = |s: &str| -> Result<Value, ParseError> {
        if text_ok(s) { Ok(String::try_from(s).unwrap_or_default()) } else { Err(ParseError::BadField) }
    };

    let parsed = match tag {
        "BRW" => Body::Browse { id: num(next()?)?, index: num(next()?)? },
        "BRR" => {
            let req = num(next()?)?;
            let id = num(next()?)?;
            let name = String::try_from(next()?).map_err(|_| ParseError::BadField)?;
            let kind: [u8; 3] = next()?.as_bytes().try_into().map_err(|_| ParseError::BadField)?;
            let node_kind = matches!(kind[0], b'O' | b'V')
                && matches!(kind[1], b'-' | b'B' | b'U' | b'T')
                && matches!(kind[2], b'R' | b'W');
            // `---` is what the end of a browse carries.
            let valid = node_kind || kind == *b"---";
            if !valid {
                return Err(ParseError::BadField);
            }
            let more = match next()? {
                "0" => false,
                "1" => true,
                _ => return Err(ParseError::BadField),
            };
            Body::BrowseResult { req, id, name, kind, more }
        }
        "RED" => Body::Read { id: num(next()?)? },
        "WRT" => Body::Write { id: num(next()?)?, value: text(next()?)? },
        "SUB" => Body::Subscribe { id: num(next()?)?, interval_ms: num(next()?)? },
        "VAL" => {
            let req = num(next()?)?;
            let id = num(next()?)?;
            let status = Status::from_code(num(next()?)?).ok_or(ParseError::BadField)?;
            // An empty value is a valid (text) value: `split` gives it as "".
            Body::Value { req, id, status, value: text(next()?)? }
        }
        _ => return Err(ParseError::BadField),
    };
    if fields.next().is_some() {
        return Err(ParseError::BadField);
    }
    Ok(Frame { node, seq, t_ms, body: parsed })
}

// --- The values behind the nodes -----------------------------------------------------------------

/// Whatever holds the values of an address space.
pub trait Space {
    /// The value of a variable, as text.
    fn read(&mut self, id: NodeId) -> Result<Value, Status>;
    /// Writes a variable from text; the status says how it went.
    fn write(&mut self, id: NodeId, value: &str) -> Status;
}

/// The address space of a node: what the firmware knows about itself, over its [`NodeConfig`].
///
/// The firmware builds one from its live state each time a request arrives; a write to a setting
/// goes through [`NodeConfig::set`], so it is checked exactly like the CLI's `set` and a preset
/// are. The caller applies the change afterwards (`sync` in the firmware).
pub struct NodeSpace<'a> {
    /// The live settings.
    pub cfg: &'a mut NodeConfig,
    /// Uptime, ms.
    pub uptime_ms: u32,
    /// Whether the jumper button is pressed.
    pub jumper: bool,
    /// Whether the BOOT button is pressed.
    pub boot: bool,
    /// Frames sent.
    pub tx: u32,
    /// Frames received.
    pub rx: u32,
    /// Frames lost.
    pub lost: u32,
}

impl NodeSpace<'_> {
    fn set(&mut self, key: &str, value: &str) -> Status {
        if !text_ok(value) {
            return Status::BadTypeMismatch;
        }
        match self.cfg.set(key, value) {
            Ok(_) => Status::Good,
            Err(SetError::BadValue(_)) => Status::BadTypeMismatch,
            Err(SetError::Invalid(_)) => Status::BadOutOfRange,
            Err(SetError::ReadOnly) => Status::BadNotWritable,
            Err(SetError::UnknownKey) => Status::BadNodeIdUnknown,
        }
    }
}

fn number(n: u32) -> Value {
    let mut v = Value::new();
    let _ = write!(v, "{n}");
    v
}

fn text(s: &str) -> Value {
    String::try_from(s).unwrap_or_default()
}

impl Space for NodeSpace<'_> {
    fn read(&mut self, id: NodeId) -> Result<Value, Status> {
        use crate::config::Key;
        let node = describe(id).ok_or(Status::BadNodeIdUnknown)?;
        if node.class == Class::Object {
            return Err(Status::BadTypeMismatch);
        }
        Ok(match id {
            2 => text(self.cfg.name),
            3 => number(self.uptime_ms),
            5 => self.cfg.value(Key::Led1),
            6 => self.cfg.value(Key::Led2),
            8 => text(if self.jumper { "1" } else { "0" }),
            9 => text(if self.boot { "1" } else { "0" }),
            11 => self.cfg.value(Key::RgbPattern),
            12 => self.cfg.value(Key::RgbBrightness),
            14 => number(self.tx),
            15 => number(self.rx),
            16 => number(self.lost),
            _ => self.cfg.value(KEYS[usize::from(id - CONFIG_BASE)].1),
        })
    }

    fn write(&mut self, id: NodeId, value: &str) -> Status {
        let Some(node) = describe(id) else { return Status::BadNodeIdUnknown };
        if node.class == Class::Object {
            return Status::BadTypeMismatch;
        }
        if !node.writable {
            return Status::BadNotWritable;
        }
        match id {
            5 => self.set("led1", value),
            6 => self.set("led2", value),
            11 => self.set("rgb.pattern", value),
            12 => self.set("rgb.brightness", value),
            _ => self.set(KEYS[usize::from(id - CONFIG_BASE)].0, value),
        }
    }
}

// --- Subscriptions ---------------------------------------------------------------------------------

/// One watched variable.
#[derive(Clone, Debug)]
struct Subscription {
    id: NodeId,
    /// `seq` of the `SUB`: the `req` of every notification.
    req: u32,
    interval_ms: u32,
    next_check_ms: u32,
    last: Value,
}

/// The subscriptions a node serves: up to `N` variables, each reported when its value changes.
#[derive(Clone, Debug)]
pub struct Subscriptions<const N: usize> {
    slots: [Option<Subscription>; N],
}

impl<const N: usize> Default for Subscriptions<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Subscriptions<N> {
    /// No subscription.
    pub const fn new() -> Self {
        Self { slots: [const { None }; N] }
    }

    /// How many are active.
    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Whether none is active.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Starts (or changes) watching `id`, whose current value is `current`. `Err` if all slots are
    /// taken. An `interval_ms` of 0 stops watching instead.
    pub fn subscribe(
        &mut self,
        id: NodeId,
        req: u32,
        interval_ms: u32,
        now_ms: u32,
        current: Value,
    ) -> Result<(), Status> {
        if interval_ms == 0 {
            self.cancel(id);
            return Ok(());
        }
        let sub = Subscription { id, req, interval_ms, next_check_ms: now_ms.wrapping_add(interval_ms), last: current };
        // The slot already watching this variable, else a free one.
        let index = self
            .slots
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.id == id))
            .or_else(|| self.slots.iter().position(Option::is_none));
        match index {
            Some(i) => {
                self.slots[i] = Some(sub);
                Ok(())
            }
            None => Err(Status::BadTooManySubscriptions),
        }
    }

    /// Stops watching `id`; whether it was watched.
    pub fn cancel(&mut self, id: NodeId) -> bool {
        let slot = self.slots.iter_mut().find(|s| s.as_ref().is_some_and(|s| s.id == id));
        slot.map(Option::take).is_some_and(|s| s.is_some())
    }

    /// Stops watching everything.
    pub fn clear(&mut self) {
        self.slots = [const { None }; N];
    }

    /// Checks the subscriptions that are due; returns a notification for the first whose value
    /// changed since it was last reported (call again for the next one).
    pub fn poll(&mut self, now_ms: u32, space: &mut impl Space) -> Option<Body> {
        for slot in self.slots.iter_mut().flatten() {
            // Wrap-safe "now >= next_check_ms".
            if now_ms.wrapping_sub(slot.next_check_ms) > u32::MAX / 2 {
                continue;
            }
            slot.next_check_ms = now_ms.wrapping_add(slot.interval_ms);
            let Ok(value) = space.read(slot.id) else { continue };
            if value != slot.last {
                slot.last = value.clone();
                return Some(Body::Value { req: slot.req, id: slot.id, status: Status::Good, value });
            }
        }
        None
    }
}

/// Answers a request. `None` for frames that are answers themselves (`BRR`, `VAL`).
///
/// The reply is a [`Body`] for the caller to number and send (its `req` is the request's `seq`).
pub fn serve<const N: usize>(
    request: &Frame<'_>,
    space: &mut impl Space,
    subs: &mut Subscriptions<N>,
    now_ms: u32,
) -> Option<Body> {
    let req = request.seq;
    Some(match &request.body {
        Body::Browse { id, index } => match child(*id, usize::from(*index)) {
            Some((node, more)) => Body::BrowseResult {
                req,
                id: node.id,
                name: String::try_from(node.name).unwrap_or_default(),
                kind: kind_of(&node),
                more,
            },
            // Unknown parent, or past the last child: the end of the list.
            None => Body::BrowseResult { req, id: NONE, name: String::new(), kind: *b"---", more: false },
        },
        Body::Read { id } => match space.read(*id) {
            Ok(value) => Body::Value { req, id: *id, status: Status::Good, value },
            Err(status) => Body::Value { req, id: *id, status, value: Value::new() },
        },
        Body::Write { id, value } => {
            let status = space.write(*id, value);
            // The answer carries the value now in force: what was accepted, or what stayed.
            let now = space.read(*id).unwrap_or_default();
            Body::Value { req, id: *id, status, value: now }
        }
        // `SUB` with no node and no interval: stop watching everything.
        Body::Subscribe { id: NONE, interval_ms: 0 } => {
            subs.clear();
            Body::Value { req, id: NONE, status: Status::Good, value: Value::new() }
        }
        Body::Subscribe { id, interval_ms } => match space.read(*id) {
            Ok(value) => match subs.subscribe(*id, req, *interval_ms, now_ms, value.clone()) {
                Ok(()) => Body::Value { req, id: *id, status: Status::Good, value },
                Err(status) => Body::Value { req, id: *id, status, value: Value::new() },
            },
            Err(status) => Body::Value { req, id: *id, status, value: Value::new() },
        },
        Body::BrowseResult { .. } | Body::Value { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Verbosity;

    fn space_of(cfg: &mut NodeConfig) -> NodeSpace<'_> {
        NodeSpace { cfg, uptime_ms: 12_345, jumper: false, boot: true, tx: 10, rx: 9, lost: 1 }
    }

    #[test]
    fn the_table_is_consistent() {
        for (i, n) in NODES.iter().enumerate() {
            assert!(NODES[i + 1..].iter().all(|m| m.id != n.id), "id {} twice", n.id);
            assert!(!n.name.contains(['/', ',', '*', '$']) && n.name.len() <= NAME_LEN, "{}", n.name);
            if n.parent != NONE {
                let parent = describe(n.parent).unwrap_or_else(|| panic!("{}: no parent", n.name));
                assert_eq!(parent.class, Class::Object, "{}'s parent isn't an object", n.name);
            }
            assert_eq!(n.class == Class::Object, n.ty == DataType::None, "{}", n.name);
            assert!(!(n.class == Class::Object && n.writable));
        }
        assert_eq!(describe(CONFIG).map(|n| n.name), Some("config"));
        assert!(describe(CONFIG_BASE + KEYS.len() as NodeId).is_none());
        assert!(describe(50).is_none() && describe(NONE).is_none());
    }

    #[test]
    fn every_node_has_a_path_that_finds_it_back() {
        let ids = NODES.iter().map(|n| n.id).chain((0..KEYS.len()).map(|i| CONFIG_BASE + i as NodeId));
        for id in ids {
            let p = path_string(id);
            assert_eq!(find(&p), Some(id), "{p}");
        }
        assert_eq!(path_string(ROOT), "/");
        assert_eq!(path_string(8), "/buttons/jumper");
        assert_eq!(find("leds/1"), Some(5), "the leading slash is optional");
        assert_eq!(find("/leds/9"), None);
        assert_eq!(find("/"), Some(ROOT));
        assert_eq!(path_string(CONFIG_BASE), "/config/data_period_ms");
    }

    #[test]
    fn browsing_lists_every_child_once() {
        let names = |parent| {
            let mut out = std::vec::Vec::new();
            let mut i = 0;
            while let Some((node, more)) = child(parent, i) {
                out.push(node.name);
                assert_eq!(more, child(parent, i + 1).is_some(), "{parent} #{i}");
                i += 1;
            }
            out
        };
        assert_eq!(names(ROOT), ["node", "leds", "buttons", "rgb", "stats", "config"]);
        assert_eq!(names(4), ["1", "2"]);
        assert_eq!(names(CONFIG).len(), KEYS.len());
        assert!(names(2).is_empty(), "a variable has no children");
        assert!(names(NONE).is_empty());
    }

    #[test]
    fn frames_round_trip() {
        let value = |s: &str| Value::try_from(s).unwrap();
        for body in [
            Body::Browse { id: 4, index: 1 },
            Body::BrowseResult { req: 3, id: 5, name: String::try_from("1").unwrap(), kind: *b"VTW", more: true },
            Body::BrowseResult { req: 3, id: NONE, name: String::new(), kind: *b"---", more: false },
            Body::Read { id: 8 },
            Body::Write { id: 5, value: value("blink 200") },
            Body::Subscribe { id: 8, interval_ms: 250 },
            Body::Value { req: 9, id: 8, status: Status::Good, value: value("1") },
            Body::Value { req: 9, id: 8, status: Status::BadNotWritable, value: Value::new() },
        ] {
            let frame = Frame { node: "node-A", seq: 41, t_ms: 123_456, body };
            let line = encode(&frame);
            assert!(is_space(&line), "{line}");
            assert_eq!(parse(line.trim_end()), Ok(frame));
        }
    }

    #[test]
    fn known_line() {
        let frame = Frame {
            node: "node-B",
            seq: 7,
            t_ms: 1000,
            body: Body::Write { id: 5, value: Value::try_from("peer").unwrap() },
        };
        let line = encode(&frame);
        assert_eq!(line, "$WRT,node-B,7,1000,5,peer*13\n");
    }

    #[test]
    fn the_longest_frames_fit() {
        let long = Value::try_from("x".repeat(VALUE_LEN).as_str()).unwrap();
        for body in [
            Body::BrowseResult {
                req: u32::MAX,
                id: u16::MAX - 1,
                name: String::try_from("0123456789ABCDEF").unwrap(),
                kind: *b"VTW",
                more: true,
            },
            Body::Write { id: u16::MAX, value: long.clone() },
            Body::Value { req: u32::MAX, id: u16::MAX, status: Status::BadTooManySubscriptions, value: long },
            Body::Subscribe { id: u16::MAX, interval_ms: u32::MAX },
        ] {
            let frame = Frame { node: "ABCDEFGHIJKL", seq: u32::MAX, t_ms: u32::MAX, body };
            let line = encode(&frame);
            assert!(line.ends_with('\n') && line.len() <= MAX_FRAME, "{} bytes: {line}", line.len());
            assert_eq!(parse(line.trim_end()), Ok(frame));
        }
    }

    #[test]
    fn bad_frames_are_refused() {
        let signed = |body: &str| format!("${body}*{:02X}", protocol::checksum(body.as_bytes()));
        for body in [
            "RED,n,1,2",             // no id
            "RED,n,1,2,x",           // not a number
            "RED,n,1,2,3,4",         // too many fields
            "WRT,n,1,2,5,a$b",       // a forbidden character in the value
            "VAL,n,1,2,3,4,9,x",     // unknown status
            "BRR,n,1,2,3,4,x,QQQ,0", // not a kind
            "BRR,n,1,2,3,4,x,VTW,2", // not a flag
            "BRW,n,1,2,4,70000",     // index out of range
            "XYZ,n,1,2",             // not a tag of this protocol
        ] {
            assert_eq!(parse(&signed(body)), Err(ParseError::BadField), "{body}");
        }
        let good = encode(&Frame { node: "n", seq: 1, t_ms: 2, body: Body::Read { id: 3 } });
        let corrupt = good.trim_end().replace(",3*", ",4*");
        assert!(matches!(parse(&corrupt), Err(ParseError::Checksum { .. })));
        assert!(!is_space("$DAT,n,1,2,0,0,10*00") && !is_space("hello") && !is_space("$RE"));
    }

    #[test]
    fn reading_the_node() {
        let mut cfg = NodeConfig { name: "node-A", ..NodeConfig::DEFAULT };
        let mut s = space_of(&mut cfg);
        let read = |s: &mut NodeSpace<'_>, path: &str| s.read(find(path).unwrap());
        assert_eq!(read(&mut s, "/node/name").unwrap(), "node-A");
        assert_eq!(read(&mut s, "/node/uptime").unwrap(), "12345");
        assert_eq!(read(&mut s, "/buttons/jumper").unwrap(), "0");
        assert_eq!(read(&mut s, "/buttons/boot").unwrap(), "1");
        assert_eq!(read(&mut s, "/stats/lost").unwrap(), "1");
        assert_eq!(read(&mut s, "/leds/1").unwrap(), "peer");
        assert_eq!(read(&mut s, "/leds/2").unwrap(), "rx");
        assert_eq!(read(&mut s, "/rgb/brightness").unwrap(), "40");
        assert_eq!(read(&mut s, "/config/verbose").unwrap(), "live");
        assert_eq!(read(&mut s, "/config/tx_mode").unwrap(), "periodic");
        assert_eq!(s.read(4), Err(Status::BadTypeMismatch), "an object has no value");
        assert_eq!(s.read(50), Err(Status::BadNodeIdUnknown));
    }

    #[test]
    fn every_variable_reads_and_every_value_fits_a_frame() {
        let mut cfg = NodeConfig::DEFAULT;
        let mut s = space_of(&mut cfg);
        let ids = NODES.iter().map(|n| n.id).chain((0..KEYS.len()).map(|i| CONFIG_BASE + i as NodeId));
        for id in ids {
            let node = describe(id).unwrap();
            match (node.class, s.read(id)) {
                (Class::Variable, Ok(v)) => assert!(text_ok(&v), "{}: {v:?}", path_string(id)),
                (Class::Object, Err(Status::BadTypeMismatch)) => {}
                (class, other) => panic!("{}: {class:?} -> {other:?}", path_string(id)),
            }
        }
    }

    #[test]
    fn writing_goes_through_the_same_checks_as_set() {
        let mut cfg = NodeConfig::DEFAULT;
        let mut s = space_of(&mut cfg);
        assert_eq!(s.write(5, "blink 200"), Status::Good);
        assert_eq!(s.write(12, "70"), Status::Good);
        assert_eq!(s.write(11, "ocean"), Status::Good);
        assert_eq!(s.write(CONFIG_BASE + 11, "wire"), Status::Good); // /config/verbose
        assert_eq!(s.write(5, "blinkety"), Status::BadTypeMismatch, "not a LED mode");
        assert_eq!(s.write(12, "170"), Status::BadOutOfRange, "a number, but not 0-100");
        assert_eq!(s.write(12, "lots"), Status::BadTypeMismatch);
        assert_eq!(s.write(14, "0"), Status::BadNotWritable, "a statistic");
        assert_eq!(s.write(2, "x"), Status::BadNotWritable, "the name is fixed at build time");
        assert_eq!(s.write(4, "x"), Status::BadTypeMismatch, "an object");
        assert_eq!(s.write(99, "x"), Status::BadNodeIdUnknown);
        assert_eq!(s.write(5, "a,b"), Status::BadTypeMismatch, "a comma would break the frame");
        assert_eq!(s.cfg.rgb.brightness, 70);
        assert_eq!(s.cfg.rgb.pattern, "ocean");
        assert_eq!(s.cfg.verbose, Verbosity::Wire);
        assert!(matches!(s.cfg.led1, crate::config::LedMode::Blink { period_ms } if period_ms == 200.0));
    }

    #[test]
    fn subscriptions_report_changes_only() {
        let mut cfg = NodeConfig::DEFAULT;
        let mut subs = Subscriptions::<2>::new();
        let jumper = find("/buttons/jumper").unwrap();
        let mut now = 1000;
        let mut space = space_of(&mut cfg);
        let current = space.read(jumper).unwrap();
        subs.subscribe(jumper, 5, 100, now, current).unwrap();
        // Nothing changed: nothing to report, however often it is polled.
        for _ in 0..5 {
            now += 100;
            assert_eq!(subs.poll(now, &mut space), None);
        }
        space.jumper = true;
        // Not due yet: the value is only sampled every 100 ms.
        assert_eq!(subs.poll(now + 50, &mut space), None);
        now += 100;
        let Some(Body::Value { req, id, status, value }) = subs.poll(now, &mut space) else {
            panic!("no notification")
        };
        assert_eq!((req, id, status, value.as_str()), (5, jumper, Status::Good, "1"));
        // Reported once.
        now += 100;
        assert_eq!(subs.poll(now, &mut space), None);
    }

    #[test]
    fn sub_with_no_node_and_no_interval_cancels_everything() {
        let mut cfg = NodeConfig::DEFAULT;
        let mut subs = Subscriptions::<4>::new();
        let mut space = space_of(&mut cfg);
        for (seq, id) in [(1, 8), (2, 9), (3, 14)] {
            let sub = Frame { node: "a", seq, t_ms: 0, body: Body::Subscribe { id, interval_ms: 100 } };
            assert!(matches!(serve(&sub, &mut space, &mut subs, 0), Some(Body::Value { status: Status::Good, .. })));
        }
        assert_eq!(subs.len(), 3);
        let stop = Frame { node: "a", seq: 4, t_ms: 0, body: Body::Subscribe { id: NONE, interval_ms: 0 } };
        assert!(matches!(
            serve(&stop, &mut space, &mut subs, 0),
            Some(Body::Value { id: NONE, status: Status::Good, .. })
        ));
        assert!(subs.is_empty());
    }

    #[test]
    fn subscription_slots_and_cancel() {
        let mut subs = Subscriptions::<2>::new();
        let v = Value::new;
        assert!(subs.subscribe(8, 1, 100, 0, v()).is_ok());
        assert!(subs.subscribe(9, 2, 100, 0, v()).is_ok());
        assert_eq!(subs.subscribe(14, 3, 100, 0, v()), Err(Status::BadTooManySubscriptions));
        assert!(subs.subscribe(8, 4, 50, 0, v()).is_ok(), "the same variable again replaces its slot");
        assert_eq!(subs.len(), 2);
        assert!(subs.cancel(8) && !subs.cancel(8));
        assert!(subs.subscribe(14, 5, 100, 0, v()).is_ok());
        assert!(subs.subscribe(14, 6, 0, 0, v()).is_ok(), "an interval of 0 stops watching");
        assert_eq!(subs.len(), 1);
        subs.clear();
        assert!(subs.is_empty());
    }

    #[test]
    fn subscription_timing_survives_the_clock_wrapping() {
        let mut cfg = NodeConfig::DEFAULT;
        let mut subs = Subscriptions::<1>::new();
        let mut space = space_of(&mut cfg);
        let start = u32::MAX - 150;
        subs.subscribe(8, 1, 100, start, text("0")).unwrap();
        space.jumper = true;
        assert_eq!(subs.poll(start.wrapping_add(50), &mut space), None);
        assert!(subs.poll(start.wrapping_add(100), &mut space).is_some(), "due 100 ms later, across the wrap");
    }

    /// Two nodes talking through the encoded lines, as on a link.
    #[test]
    fn a_node_browses_reads_writes_and_watches_its_peer() {
        let mut cfg_b = NodeConfig { name: "node-B", ..NodeConfig::DEFAULT };
        let mut subs_b = Subscriptions::<4>::new();
        let mut seq_a = 0u32;
        let mut seq_b = 0u32;

        // A sends a request, B answers: both sides only ever see lines.
        let mut ask = |cfg_b: &mut NodeConfig, subs_b: &mut Subscriptions<4>, jumper: bool, body: Body, now: u32| {
            seq_a += 1;
            let line = encode(&Frame { node: "node-A", seq: seq_a, t_ms: now, body });
            let request = parse(line.trim_end()).unwrap();
            let mut space = NodeSpace { cfg: cfg_b, uptime_ms: now, jumper, boot: false, tx: 0, rx: 0, lost: 0 };
            let reply = serve(&request, &mut space, subs_b, now).expect("a request gets a reply");
            seq_b += 1;
            let line = encode(&Frame { node: "node-B", seq: seq_b, t_ms: now, body: reply });
            parse(line.trim_end()).unwrap().body
        };

        // Browse /leds: two children, the second is the last.
        let leds = find("/leds").unwrap();
        let mut found = std::vec::Vec::new();
        for index in 0.. {
            let Body::BrowseResult { name, kind, more, .. } =
                ask(&mut cfg_b, &mut subs_b, false, Body::Browse { id: leds, index }, 0)
            else {
                panic!()
            };
            found.push((name.as_str().to_owned(), std::str::from_utf8(&kind).unwrap().to_owned()));
            if !more {
                break;
            }
        }
        assert_eq!(found, [("1".to_owned(), "VTW".to_owned()), ("2".to_owned(), "VTW".to_owned())]);

        // Write a LED of B, and read it back (as the console prints a LED mode: `blink 300 ms`).
        let led1 = find("/leds/1").unwrap();
        let answer = ask(&mut cfg_b, &mut subs_b, false, Body::Write { id: led1, value: text("blink 300") }, 10);
        assert!(
            matches!(answer, Body::Value { status: Status::Good, ref value, .. } if value == "blink 300 ms"),
            "{answer:?}"
        );
        assert!(matches!(cfg_b.led1, crate::config::LedMode::Blink { period_ms } if period_ms == 300.0));
        // A refused write leaves the value alone, and the answer says what is in force.
        let answer = ask(&mut cfg_b, &mut subs_b, false, Body::Write { id: led1, value: text("nope") }, 20);
        assert!(
            matches!(answer, Body::Value { status: Status::BadTypeMismatch, ref value, .. } if value == "blink 300 ms"),
            "{answer:?}"
        );

        // Watch B's jumper: the first answer is the current value, then a notification on change.
        let jumper = find("/buttons/jumper").unwrap();
        let answer = ask(&mut cfg_b, &mut subs_b, false, Body::Subscribe { id: jumper, interval_ms: 50 }, 100);
        assert!(matches!(answer, Body::Value { status: Status::Good, ref value, .. } if value == "0"));
        let mut space = NodeSpace { cfg: &mut cfg_b, uptime_ms: 0, jumper: true, boot: false, tx: 0, rx: 0, lost: 0 };
        let Some(Body::Value { req, value, .. }) = subs_b.poll(160, &mut space) else { panic!("no notification") };
        // The notification belongs to the subscription: `req` is the `seq` of the SUB (the 5th request).
        assert_eq!((req, value.as_str()), (5, "1"));
    }
}
