//! Network frame dissector, for the Ethernet firmware's `trace`: Ethernet, ARP, IPv4, ICMP, UDP and
//! TCP, then the node's own frames inside them.
//!
//! [`dissect`] reads the bytes of one frame as they crossed the port and returns a [`Packet`]
//! that borrows them. Nothing is allocated and nothing is printed here: the firmware formats the
//! result with [`Packet`]'s `Display` (one line), [`Packet::layers`] (one line per layer),
//! [`App`] (the node's frame in the payload) and [`hexdump`] (the raw bytes). So all of it runs and
//! is tested on the host.
//!
//! ```
//! use workshop_core::dissect::dissect;
//!
//! // A broadcast ARP request: who has 192.168.77.12? tell 192.168.77.11.
//! let mut frame = vec![0xFF; 6];
//! frame.extend([0x02, 0, 0, 0, 0, 0x11, 0x08, 0x06]); // source MAC, ethertype ARP
//! frame.extend([0, 1, 8, 0, 6, 4, 0, 1]); // Ethernet / IPv4, request
//! frame.extend([0x02, 0, 0, 0, 0, 0x11, 192, 168, 77, 11]); // sender
//! frame.extend([0, 0, 0, 0, 0, 0, 192, 168, 77, 12]); // target
//!
//! let packet = dissect(&frame, frame.len()).unwrap();
//! assert_eq!(packet.to_string(), "who-has 192.168.77.12 tell 192.168.77.11");
//! assert!(packet.is_control());
//! ```
//!
//! Checksums are checked when the whole frame was captured (a frame cut short can't be verified:
//! the result says so instead of guessing).

use core::fmt;

use crate::{
    net::Addr,
    protocol::{self, Edge, ParseError, Payload},
    space,
};

/// Bytes of the Ethernet header, without a VLAN tag.
pub const ETHERNET_HEADER: usize = 14;

/// A MAC address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mac(pub [u8; 6]);

impl Mac {
    /// `ff:ff:ff:ff:ff:ff`.
    pub const BROADCAST: Mac = Mac([0xFF; 6]);

    /// Whether this is the broadcast address.
    pub fn is_broadcast(self) -> bool {
        self == Self::BROADCAST
    }

    /// Whether the address is a group (multicast or broadcast) one: low bit of the first byte.
    pub fn is_group(self) -> bool {
        self.0[0] & 1 != 0
    }
}

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}

fn mac(bytes: &[u8]) -> Mac {
    Mac(bytes[..6].try_into().expect("6 bytes"))
}

fn addr(bytes: &[u8]) -> Addr {
    Addr(bytes[..4].try_into().expect("4 bytes"))
}

fn be16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// The Ethernet header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ethernet {
    /// Destination MAC.
    pub dst: Mac,
    /// Source MAC.
    pub src: Mac,
    /// What follows: `0x0800` IPv4, `0x0806` ARP... (after the VLAN tag, if there is one).
    pub ethertype: u16,
    /// The VLAN id, when the frame has an 802.1Q tag.
    pub vlan: Option<u16>,
}

impl Ethernet {
    /// Bytes of the header, with the VLAN tag if any.
    pub const fn header_len(&self) -> usize {
        if self.vlan.is_some() { ETHERNET_HEADER + 4 } else { ETHERNET_HEADER }
    }
}

/// `0x0800`.
pub const ETHERTYPE_IPV4: u16 = 0x0800;
/// `0x0806`.
pub const ETHERTYPE_ARP: u16 = 0x0806;
const ETHERTYPE_VLAN: u16 = 0x8100;

fn ethertype_name(t: u16) -> &'static str {
    match t {
        ETHERTYPE_IPV4 => "IPv4",
        ETHERTYPE_ARP => "ARP",
        0x86DD => "IPv6",
        0x88CC => "LLDP",
        0x88F7 => "PTP",
        0x8808 => "pause",
        _ => "?",
    }
}

/// What an ARP packet asks or says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArpOp {
    /// `who-has`.
    Request,
    /// `is-at`.
    Reply,
    /// Any other operation code.
    Other(u16),
}

/// An ARP packet (IPv4 over Ethernet).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arp {
    /// Request or reply.
    pub op: ArpOp,
    /// Who asks or answers.
    pub sender_mac: Mac,
    /// Its address.
    pub sender_ip: Addr,
    /// Who is asked (all zeros in a request) or answered.
    pub target_mac: Mac,
    /// The address asked about.
    pub target_ip: Addr,
}

/// An IPv4 header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4 {
    /// Source address.
    pub src: Addr,
    /// Destination address.
    pub dst: Addr,
    /// Protocol number: 1 ICMP, 6 TCP, 17 UDP.
    pub protocol: u8,
    /// Time to live.
    pub ttl: u8,
    /// Type of service (DSCP and ECN).
    pub tos: u8,
    /// Identification, to put fragments back together.
    pub id: u16,
    /// Header plus payload, bytes.
    pub total_len: u16,
    /// Header bytes (20 plus options).
    pub header_len: usize,
    /// Don't-fragment flag.
    pub dont_fragment: bool,
    /// More-fragments flag.
    pub more_fragments: bool,
    /// Where this fragment starts in the datagram, bytes.
    pub fragment_offset: u16,
    /// Whether the header checksum is right.
    pub checksum_ok: bool,
}

/// A UDP header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Udp {
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// Header plus payload, bytes.
    pub len: u16,
    /// `Some` when the checksum could be verified; `None` when the sender left it at 0 (allowed in
    /// IPv4) or the frame was captured only in part.
    pub checksum_ok: Option<bool>,
}

/// TCP flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpFlags(pub u8);

impl TcpFlags {
    /// No more data from the sender.
    pub const FIN: u8 = 0x01;
    /// Opens a connection.
    pub const SYN: u8 = 0x02;
    /// Resets the connection.
    pub const RST: u8 = 0x04;
    /// Hands the data to the application now.
    pub const PSH: u8 = 0x08;
    /// The acknowledgement number is valid.
    pub const ACK: u8 = 0x10;
    /// Urgent pointer valid.
    pub const URG: u8 = 0x20;

    /// Whether every bit of `mask` is set.
    pub const fn has(self, mask: u8) -> bool {
        self.0 & mask == mask
    }

    /// Whether any of `mask`'s bits is set.
    pub const fn any(self, mask: u8) -> bool {
        self.0 & mask != 0
    }
}

impl fmt::Display for TcpFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (bit, name) in [
            (Self::SYN, "SYN"),
            (Self::FIN, "FIN"),
            (Self::RST, "RST"),
            (Self::PSH, "PSH"),
            (Self::ACK, "ACK"),
            (Self::URG, "URG"),
        ] {
            if self.any(bit) {
                f.write_str(if first { "" } else { "," })?;
                f.write_str(name)?;
                first = false;
            }
        }
        if first { f.write_str("none") } else { Ok(()) }
    }
}

/// A TCP header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tcp {
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// Sequence number of the first payload byte (of the `SYN`, in a `SYN`).
    pub seq: u32,
    /// Next byte the sender expects (valid with `ACK`).
    pub ack: u32,
    /// The flags.
    pub flags: TcpFlags,
    /// Receive window the sender offers, bytes.
    pub window: u16,
    /// Header bytes (20 plus options).
    pub header_len: usize,
    /// Maximum segment size option, when present (in a `SYN`).
    pub mss: Option<u16>,
    /// Whether the checksum is right; `None` when the frame was captured only in part.
    pub checksum_ok: Option<bool>,
}

/// An ICMP message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Icmp {
    /// Type: 8 echo request, 0 echo reply, 3 destination unreachable...
    pub kind: u8,
    /// Code, in the type.
    pub code: u8,
    /// The four bytes after the checksum: identifier and sequence number in an echo.
    pub rest: [u8; 4],
    /// Whether the checksum is right; `None` when the frame was captured only in part.
    pub checksum_ok: Option<bool>,
}

impl Icmp {
    fn name(&self) -> &'static str {
        match (self.kind, self.code) {
            (0, _) => "echo reply",
            (3, 0) => "network unreachable",
            (3, 1) => "host unreachable",
            (3, 3) => "port unreachable",
            (3, _) => "destination unreachable",
            (8, _) => "echo request",
            (11, _) => "time exceeded",
            _ => "message",
        }
    }
}

/// What an IPv4 packet carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum L4<'a> {
    /// A UDP datagram and its payload (as much as was captured).
    Udp(Udp, &'a [u8]),
    /// A TCP segment and its payload (as much as was captured).
    Tcp(Tcp, &'a [u8]),
    /// An ICMP message.
    Icmp(Icmp),
    /// A fragment other than the first one: nothing to decode.
    Fragment,
    /// Another protocol, by number.
    Other(u8),
    /// A header that doesn't hold together.
    Malformed(&'static str),
}

/// What the Ethernet frame carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Net<'a> {
    /// ARP.
    Arp(Arp),
    /// IPv4 and what it carries.
    Ipv4(Ipv4, L4<'a>),
    /// Another ethertype (IPv6, LLDP...).
    Other(u16),
    /// A header that doesn't hold together.
    Malformed(&'static str),
}

/// One dissected frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet<'a> {
    /// Size of the frame on the wire (without its checksum), bytes.
    pub wire_len: usize,
    /// Bytes that were captured (the driver keeps the first ones only).
    pub captured: usize,
    /// The Ethernet header.
    pub eth: Ethernet,
    /// What it carries.
    pub net: Net<'a>,
}

/// Why nothing could be decoded.
pub type Malformed = &'static str;

/// Dissects `data`, the first bytes of a frame that is `wire_len` bytes on the wire. Only a frame
/// shorter than an Ethernet header is refused: anything wrong further in is reported in the
/// [`Packet`] (as `Malformed`), so the rest is still shown.
pub fn dissect(data: &[u8], wire_len: usize) -> Result<Packet<'_>, Malformed> {
    if data.len() < ETHERNET_HEADER {
        return Err("shorter than an Ethernet header");
    }
    let mut eth = Ethernet { dst: mac(&data[0..]), src: mac(&data[6..]), ethertype: be16(&data[12..]), vlan: None };
    if eth.ethertype == ETHERTYPE_VLAN {
        if data.len() < ETHERNET_HEADER + 4 {
            return Err("shorter than an Ethernet header with a VLAN tag");
        }
        eth.vlan = Some(be16(&data[14..]) & 0x0FFF);
        eth.ethertype = be16(&data[16..]);
    }
    let body = &data[eth.header_len()..];
    let complete = data.len() >= wire_len;
    let net = match eth.ethertype {
        ETHERTYPE_ARP => arp(body),
        ETHERTYPE_IPV4 => ipv4(body, complete),
        other => Net::Other(other),
    };
    Ok(Packet { wire_len, captured: data.len(), eth, net })
}

fn arp(b: &[u8]) -> Net<'_> {
    if b.len() < 28 {
        return Net::Malformed("ARP packet cut short");
    }
    // Hardware type 1 (Ethernet), protocol 0x0800 (IPv4), sizes 6 and 4: anything else isn't what we print.
    if be16(b) != 1 || be16(&b[2..]) != ETHERTYPE_IPV4 || b[4] != 6 || b[5] != 4 {
        return Net::Malformed("ARP for something other than IPv4 over Ethernet");
    }
    let op = match be16(&b[6..]) {
        1 => ArpOp::Request,
        2 => ArpOp::Reply,
        n => ArpOp::Other(n),
    };
    Net::Arp(Arp {
        op,
        sender_mac: mac(&b[8..]),
        sender_ip: addr(&b[14..]),
        target_mac: mac(&b[18..]),
        target_ip: addr(&b[24..]),
    })
}

/// Sum of 16-bit big-endian words, carries folded back in, starting from `sum`.
fn ones_complement_sum(data: &[u8], mut sum: u32) -> u16 {
    let (words, rest) = data.as_chunks::<2>();
    for w in words {
        sum += u32::from(u16::from_be_bytes(*w));
    }
    if let [last] = rest {
        sum += u32::from(*last) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    sum as u16
}

/// The checksum of a TCP, UDP or ICMP message verifies when the sum over it (with the pseudo header for
/// TCP and UDP) is all ones.
fn l4_checksum_ok(ip: &Ipv4, segment: &[u8], pseudo: bool) -> bool {
    let start = if pseudo {
        let words =
            |a: [u8; 4]| u32::from(u16::from_be_bytes([a[0], a[1]])) + u32::from(u16::from_be_bytes([a[2], a[3]]));
        words(ip.src.0) + words(ip.dst.0) + u32::from(ip.protocol) + segment.len() as u32
    } else {
        0
    };
    ones_complement_sum(segment, start) == 0xFFFF
}

fn ipv4(b: &[u8], complete: bool) -> Net<'_> {
    if b.len() < 20 {
        return Net::Malformed("IPv4 header cut short");
    }
    if b[0] >> 4 != 4 {
        return Net::Malformed("not IPv4 (version field)");
    }
    let header_len = usize::from(b[0] & 0x0F) * 4;
    if header_len < 20 || header_len > b.len() {
        return Net::Malformed("IPv4 header length is wrong");
    }
    let total_len = be16(&b[2..]);
    if usize::from(total_len) < header_len {
        return Net::Malformed("IPv4 total length is shorter than its header");
    }
    let flags_fragment = be16(&b[6..]);
    let ip = Ipv4 {
        src: addr(&b[12..]),
        dst: addr(&b[16..]),
        protocol: b[9],
        ttl: b[8],
        tos: b[1],
        id: be16(&b[4..]),
        total_len,
        header_len,
        dont_fragment: flags_fragment & 0x4000 != 0,
        more_fragments: flags_fragment & 0x2000 != 0,
        fragment_offset: (flags_fragment & 0x1FFF) * 8,
        checksum_ok: ones_complement_sum(&b[..header_len], 0) == 0xFFFF,
    };
    // The IP length bounds the payload: the Ethernet frame may be padded to 60 bytes, or carry its CRC.
    let end = usize::from(total_len).min(b.len());
    let whole = complete && usize::from(total_len) <= b.len();
    let l4 = if ip.fragment_offset != 0 {
        L4::Fragment
    } else {
        transport(&ip, &b[header_len..end], whole && !ip.more_fragments)
    };
    Net::Ipv4(ip, l4)
}

fn transport<'a>(ip: &Ipv4, seg: &'a [u8], whole: bool) -> L4<'a> {
    match ip.protocol {
        17 => {
            if seg.len() < 8 {
                return L4::Malformed("UDP header cut short");
            }
            let len = be16(&seg[4..]);
            if usize::from(len) < 8 {
                return L4::Malformed("UDP length is shorter than its header");
            }
            let stated = be16(&seg[6..]);
            let datagram = &seg[..usize::from(len).min(seg.len())];
            let checksum_ok =
                (stated != 0 && whole && usize::from(len) <= seg.len()).then(|| l4_checksum_ok(ip, datagram, true));
            L4::Udp(Udp { src_port: be16(seg), dst_port: be16(&seg[2..]), len, checksum_ok }, &datagram[8..])
        }
        6 => {
            if seg.len() < 20 {
                return L4::Malformed("TCP header cut short");
            }
            let header_len = usize::from(seg[12] >> 4) * 4;
            if header_len < 20 || header_len > seg.len() {
                return L4::Malformed("TCP header length is wrong");
            }
            let mut mss = None;
            let mut options = &seg[20..header_len];
            while let [kind, rest @ ..] = options {
                match *kind {
                    0 => break,
                    1 => options = rest,
                    k => {
                        let Some(&len) = rest.first() else { break };
                        let len = usize::from(len);
                        if len < 2 || len > options.len() {
                            break;
                        }
                        if k == 2 && len == 4 {
                            mss = Some(be16(&options[2..]));
                        }
                        options = &options[len..];
                    }
                }
            }
            let tcp = Tcp {
                src_port: be16(seg),
                dst_port: be16(&seg[2..]),
                seq: be32(&seg[4..]),
                ack: be32(&seg[8..]),
                flags: TcpFlags(seg[13] & 0x3F),
                window: be16(&seg[14..]),
                header_len,
                mss,
                checksum_ok: whole.then(|| l4_checksum_ok(ip, seg, true)),
            };
            L4::Tcp(tcp, &seg[header_len..])
        }
        1 => {
            if seg.len() < 8 {
                return L4::Malformed("ICMP header cut short");
            }
            let rest = [seg[4], seg[5], seg[6], seg[7]];
            L4::Icmp(Icmp {
                kind: seg[0],
                code: seg[1],
                rest,
                checksum_ok: whole.then(|| l4_checksum_ok(ip, seg, false)),
            })
        }
        other => L4::Other(other),
    }
}

impl<'a> Packet<'a> {
    /// The UDP or TCP payload (what was captured of it), empty for anything else.
    pub fn payload(&self) -> &'a [u8] {
        match self.net {
            Net::Ipv4(_, L4::Udp(_, p) | L4::Tcp(_, p)) => p,
            _ => &[],
        }
    }

    /// Short name of the top protocol: `ARP`, `ICMP`, `IGMP`, `UDP`, `TCP`, `PTP`, `IPv4`, or the ethertype's name.
    pub fn protocol(&self) -> &'static str {
        match self.net {
            Net::Arp(_) => "ARP",
            Net::Ipv4(_, L4::Udp(u, _)) if matches!(u.dst_port, 319 | 320) => "PTP",
            Net::Ipv4(_, L4::Udp(..)) => "UDP",
            Net::Ipv4(_, L4::Tcp(..)) => "TCP",
            Net::Ipv4(_, L4::Icmp(_)) => "ICMP",
            Net::Ipv4(_, L4::Other(2)) => "IGMP",
            Net::Ipv4(..) => "IPv4",
            Net::Other(t) => ethertype_name(t),
            Net::Malformed(_) => "bad",
        }
    }

    /// Whether this frame is part of the network's own life rather than the node's traffic: ARP, ICMP,
    /// TCP that opens, closes or resets a connection, fragments, and what can't be decoded. This is
    /// what `trace events` shows.
    pub fn is_control(&self) -> bool {
        match self.net {
            Net::Arp(_) | Net::Other(_) | Net::Malformed(_) => true,
            Net::Ipv4(_, L4::Tcp(t, _)) => t.flags.any(TcpFlags::SYN | TcpFlags::FIN | TcpFlags::RST),
            Net::Ipv4(_, L4::Udp(..)) => false,
            Net::Ipv4(..) => true,
        }
    }

    /// Whether anything in the frame is wrong: a bad checksum, or a header that doesn't hold together.
    pub fn is_damaged(&self) -> bool {
        match self.net {
            Net::Malformed(_) => true,
            Net::Ipv4(ip, l4) => {
                !ip.checksum_ok
                    || match l4 {
                        L4::Udp(u, _) => u.checksum_ok == Some(false),
                        L4::Tcp(t, _) => t.checksum_ok == Some(false),
                        L4::Icmp(i) => i.checksum_ok == Some(false),
                        L4::Malformed(_) => true,
                        _ => false,
                    }
            }
            _ => false,
        }
    }

    /// Calls `line(layer, text)` once per layer, outermost first: what is in each header, one line
    /// each (a layer with something more to say, like TCP's options, may use a second line).
    pub fn layers(&self, mut line: impl FnMut(Layer, fmt::Arguments<'_>)) {
        let e = &self.eth;
        let note = match (e.dst.is_broadcast(), e.dst.is_group()) {
            (true, _) => "  broadcast",
            (_, true) => "  multicast",
            _ => "",
        };
        match e.vlan {
            Some(v) => line(
                Layer::Eth,
                format_args!(
                    "{} → {}  vlan {v}  type 0x{:04x} ({}){note}",
                    e.src,
                    e.dst,
                    e.ethertype,
                    ethertype_name(e.ethertype)
                ),
            ),
            None => line(
                Layer::Eth,
                format_args!(
                    "{} → {}  type 0x{:04x} ({}){note}",
                    e.src,
                    e.dst,
                    e.ethertype,
                    ethertype_name(e.ethertype)
                ),
            ),
        }
        match self.net {
            Net::Arp(a) => line(Layer::Arp, format_args!("{}", ArpText(a, true))),
            Net::Other(_) => {}
            Net::Malformed(why) => line(Layer::Bad, format_args!("{why}")),
            Net::Ipv4(ip, l4) => {
                line(
                    Layer::Ipv4,
                    format_args!(
                        "{} → {}  ttl {}  proto {} ({})  total {}  id 0x{:04x}{}  cs {}",
                        ip.src,
                        ip.dst,
                        ip.ttl,
                        ip.protocol,
                        protocol_name(ip.protocol),
                        ip.total_len,
                        ip.id,
                        IpFlags(ip.dont_fragment, ip.more_fragments),
                        Check(Some(ip.checksum_ok))
                    ),
                );
                match l4 {
                    L4::Udp(u, p) => line(
                        Layer::Udp,
                        format_args!(
                            "{} → {}  len {} (payload {} B)  cs {}",
                            u.src_port,
                            u.dst_port,
                            u.len,
                            p.len(),
                            Check(u.checksum_ok)
                        ),
                    ),
                    L4::Tcp(t, p) => {
                        line(
                            Layer::Tcp,
                            format_args!(
                                "{} → {}  {}  seq {}{}  win {}{}  hdr {} B  payload {} B  cs {}",
                                t.src_port,
                                t.dst_port,
                                t.flags,
                                t.seq,
                                AckText(t.flags.has(TcpFlags::ACK).then_some(t.ack)),
                                t.window,
                                MssText(t.mss),
                                t.header_len,
                                p.len(),
                                Check(t.checksum_ok)
                            ),
                        );
                    }
                    L4::Icmp(i) => line(Layer::Icmp, format_args!("{}  cs {}", i, Check(i.checksum_ok))),
                    L4::Fragment => {
                        line(Layer::Ipv4, format_args!("fragment at byte {}: no header to decode", ip.fragment_offset))
                    }
                    L4::Other(n) => line(Layer::Ipv4, format_args!("protocol {n}: not decoded")),
                    L4::Malformed(why) => line(Layer::Bad, format_args!("{why}")),
                }
            }
        }
    }
}

fn protocol_name(n: u8) -> &'static str {
    match n {
        1 => "ICMP",
        2 => "IGMP",
        6 => "TCP",
        17 => "UDP",
        _ => "?",
    }
}

/// `  mss 1460` (the option of a `SYN`), or nothing.
struct MssText(Option<u16>);

impl fmt::Display for MssText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(mss) => write!(f, "  mss {mss}"),
            None => Ok(()),
        }
    }
}

/// `  DF`, `  MF`, `  DF,MF`, or nothing when neither flag is set.
struct IpFlags(bool, bool);

impl fmt::Display for IpFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.0, self.1) {
            (false, false) => Ok(()),
            (true, false) => f.write_str("  DF"),
            (false, true) => f.write_str("  MF"),
            (true, true) => f.write_str("  DF,MF"),
        }
    }
}

/// `  ack 2000`, or nothing when the segment doesn't acknowledge.
struct AckText(Option<u32>);

impl fmt::Display for AckText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(ack) => write!(f, "  ack {ack}"),
            None => Ok(()),
        }
    }
}

/// `ok`, `BAD` or `-` (could not be checked).
struct Check(Option<bool>);

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            Some(true) => "ok",
            Some(false) => "BAD",
            None => "-",
        })
    }
}

/// `who-has 192.168.77.12 tell 192.168.77.11` / `192.168.77.12 is-at 02:..`; with `true`, the MACs too.
struct ArpText(Arp, bool);

impl fmt::Display for ArpText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Arp { op, sender_mac, sender_ip, target_mac, target_ip } = self.0;
        match (op, self.1) {
            (ArpOp::Request, false) => write!(f, "who-has {target_ip} tell {sender_ip}"),
            (ArpOp::Request, true) => write!(f, "who-has {target_ip} tell {sender_ip} ({sender_mac})"),
            (ArpOp::Reply, false) => write!(f, "{sender_ip} is-at {sender_mac}"),
            (ArpOp::Reply, true) => write!(f, "{sender_ip} is-at {sender_mac}  to {target_ip} ({target_mac})"),
            (ArpOp::Other(n), _) => write!(f, "operation {n}: {sender_ip} → {target_ip}"),
        }
    }
}

/// `type 8 code 0 (echo request)  id 1 seq 2`.
impl fmt::Display for Icmp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let i = *self;
        write!(f, "type {} code {} ({})", i.kind, i.code, i.name())?;
        if matches!(i.kind, 0 | 8) {
            write!(f, "  id {} seq {}", be16(&i.rest), be16(&i.rest[2..]))?;
        }
        Ok(())
    }
}

/// The one-line summary, without colors: `192.168.77.11:49152 → 192.168.77.12:7000  [SYN]  len 78`.
impl fmt::Display for Packet<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.net {
            Net::Arp(a) => write!(f, "{}", ArpText(a, false)),
            Net::Other(t) => write!(
                f,
                "{} → {}  type 0x{t:04x} ({})  len {}",
                self.eth.src,
                self.eth.dst,
                ethertype_name(t),
                self.wire_len
            ),
            Net::Malformed(why) => write!(f, "{why}  len {}", self.wire_len),
            Net::Ipv4(ip, L4::Udp(u, p)) => {
                write!(
                    f,
                    "{}:{} → {}:{}  len {}  payload {} B",
                    ip.src,
                    u.src_port,
                    ip.dst,
                    u.dst_port,
                    self.wire_len,
                    p.len()
                )
            }
            Net::Ipv4(ip, L4::Tcp(t, p)) => {
                write!(
                    f,
                    "{}:{} → {}:{}  [{}]  len {}  payload {} B",
                    ip.src,
                    t.src_port,
                    ip.dst,
                    t.dst_port,
                    t.flags,
                    self.wire_len,
                    p.len()
                )
            }
            Net::Ipv4(ip, L4::Icmp(i)) => write!(f, "{} → {}  {}", ip.src, ip.dst, i),
            Net::Ipv4(ip, _) => write!(f, "{} → {}  proto {}  len {}", ip.src, ip.dst, ip.protocol, self.wire_len),
        }
    }
}

// --- The node's frames, inside the payload ---------------------------------------------------------

/// A line of the payload: the node's own frame (`$DAT,...*CS`), an address space one (`$RED,...`), or
/// any text. Shown as one short description.
pub struct App<'a>(pub &'a [u8]);

impl fmt::Display for App<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Ok(text) = core::str::from_utf8(self.0) else {
            return write!(f, "{} bytes, not text", self.0.len());
        };
        let text = text.trim_end();
        if space::is_space(text) {
            return match space::parse(text) {
                Ok(frame) => {
                    write!(f, "address space  {} #{} t={} ms  {:?}", frame.node, frame.seq, frame.t_ms, frame.body)
                }
                Err(e) => write!(f, "address space frame, {}", ParseErrorText(e)),
            };
        }
        match protocol::parse(text) {
            Ok(frame) => {
                let p = frame.payload;
                write!(f, "{} {} #{} t={} ms  ", tag(&p), frame.node, frame.seq, frame.t_ms)?;
                match p {
                    Payload::Data { pressed, events, period_us } => write!(
                        f,
                        "button {}  events {events}  period {}.{:03} ms",
                        if pressed { "pressed" } else { "released" },
                        period_us / 1000,
                        period_us % 1000
                    ),
                    Payload::Event { edge, key } => {
                        write!(
                            f,
                            "{}  key {key:016X}",
                            if edge == Edge::Falling { "press (FALL)" } else { "release (RISE)" }
                        )
                    }
                    Payload::Baud { baud, ack } => write!(f, "{} {baud} baud", if ack { "ACK" } else { "REQ" }),
                    Payload::Echo { echo_seq } => write!(f, "echo of #{echo_seq}"),
                    Payload::Burst { index, count } => write!(f, "{}/{count}", index + 1),
                }
            }
            Err(e) if text.starts_with('$') => write!(f, "invalid frame, {}", ParseErrorText(e)),
            Err(_) if text.bytes().all(|b| (0x20..0x7F).contains(&b)) => write!(f, "text {text:?}"),
            Err(_) => write!(f, "{} bytes, not a frame", self.0.len()),
        }
    }
}

fn tag(p: &Payload) -> &'static str {
    match p {
        Payload::Data { .. } => "DAT",
        Payload::Event { .. } => "EVT",
        Payload::Baud { .. } => "BAU",
        Payload::Echo { .. } => "ECH",
        Payload::Burst { .. } => "BST",
    }
}

struct ParseErrorText(ParseError);

impl fmt::Display for ParseErrorText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ParseError::Malformed => f.write_str("not of the form $...*CS"),
            ParseError::Checksum { expected, received } => {
                write!(f, "bad checksum: computed 0x{expected:02X}, found 0x{received:02X}")
            }
            ParseError::BadField => f.write_str("unknown type or bad field"),
        }
    }
}

/// The lines of a payload, without their `\n` (and without empty ones): a TCP segment can hold many.
pub fn app_lines(payload: &[u8]) -> impl Iterator<Item = &[u8]> {
    payload.split(|&b| b == b'\n').map(<[u8]>::trim_ascii_end).filter(|l| !l.is_empty())
}

// --- Raw bytes ----------------------------------------------------------------------------------------

/// One row of a hex dump: 16 bytes at an offset. Formatted whole (`0010  48 65 ...  Hello`), or by part
/// ([`Self::offset`], [`Self::hex`], [`Self::ascii`]) to put separators or colors between them.
#[derive(Clone, Copy)]
pub struct HexRow<'a> {
    offset: usize,
    bytes: &'a [u8],
}

impl<'a> HexRow<'a> {
    /// Where the row starts in the dump, bytes.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// The bytes in hex, always the same width (a short last row is padded), with a wider gap in the middle.
    pub fn hex(&self) -> impl fmt::Display + use<'a> {
        Hex(self.bytes)
    }

    /// The bytes as characters, `.` for what isn't printable ASCII.
    pub fn ascii(&self) -> impl fmt::Display + use<'a> {
        Ascii(self.bytes)
    }
}

struct Hex<'a>(&'a [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for i in 0..16 {
            if i == 8 {
                f.write_str(" ")?;
            }
            match self.0.get(i) {
                Some(b) => write!(f, "{b:02x}{}", if i == 15 { "" } else { " " })?,
                None => f.write_str(if i == 15 { "  " } else { "   " })?,
            }
        }
        Ok(())
    }
}

struct Ascii<'a>(&'a [u8]);

impl fmt::Display for Ascii<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.0 {
            f.write_str(if (0x20..0x7F).contains(&b) { as_char(b) } else { "." })?;
        }
        Ok(())
    }
}

impl fmt::Display for HexRow<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}  {}  {}", self.offset, self.hex(), self.ascii())
    }
}

/// Which header a line of [`Packet::layers`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    /// Ethernet.
    Eth,
    /// ARP.
    Arp,
    /// IPv4.
    Ipv4,
    /// UDP.
    Udp,
    /// TCP.
    Tcp,
    /// ICMP.
    Icmp,
    /// A header that doesn't hold together.
    Bad,
}

impl Layer {
    /// Short name, `eth`, `arp`, `ipv4`, `udp`, `tcp`, `icmp` or `bad`.
    pub const fn name(self) -> &'static str {
        match self {
            Layer::Eth => "eth",
            Layer::Arp => "arp",
            Layer::Ipv4 => "ipv4",
            Layer::Udp => "udp",
            Layer::Tcp => "tcp",
            Layer::Icmp => "icmp",
            Layer::Bad => "bad",
        }
    }
}

/// A printable ASCII byte as a one-character `&str` (no allocation).
fn as_char(b: u8) -> &'static str {
    const PRINTABLE: &str =
        " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
    let i = usize::from(b - 0x20);
    &PRINTABLE[i..=i]
}

/// Calls `row` for each 16 bytes of `data`.
pub fn hexdump(data: &[u8], mut row: impl FnMut(HexRow<'_>)) {
    for (i, chunk) in data.chunks(16).enumerate() {
        row(HexRow { offset: i * 16, bytes: chunk });
    }
}

#[cfg(test)]
mod tests {
    use std::{string::ToString, vec::Vec};

    use super::*;

    const A: [u8; 4] = [192, 168, 77, 11];
    const B: [u8; 4] = [192, 168, 77, 12];
    const MAC_A: [u8; 6] = [0x02, 0, 0, 0, 0, 0x11];
    const MAC_B: [u8; 6] = [0x02, 0, 0, 0, 0, 0x12];

    /// Writes the checksum of `bytes` (whose checksum field, at `at`, is zero) into it.
    fn fix(bytes: &mut [u8], at: usize, pseudo: Option<(u8, [u8; 4], [u8; 4])>) {
        let mut start = 0u32;
        if let Some((proto, src, dst)) = pseudo {
            for w in src.chunks(2).chain(dst.chunks(2)) {
                start += u32::from(be16(w));
            }
            start += u32::from(proto) + bytes.len() as u32;
        }
        let sum = !ones_complement_sum(bytes, start);
        bytes[at..at + 2].copy_from_slice(&sum.to_be_bytes());
    }

    fn ipv4_frame(proto: u8, l4: &[u8], flags_fragment: u16) -> Vec<u8> {
        let mut ip = vec![0x45, 0, 0, 0, 0x1c, 0x46, 0, 0, 64, proto, 0, 0];
        ip.extend(A);
        ip.extend(B);
        ip[2..4].copy_from_slice(&((20 + l4.len()) as u16).to_be_bytes());
        ip[6..8].copy_from_slice(&flags_fragment.to_be_bytes());
        fix(&mut ip, 10, None);
        let mut frame = MAC_B.to_vec();
        frame.extend(MAC_A);
        frame.extend([8, 0]);
        frame.extend(ip);
        frame.extend(l4);
        frame
    }

    fn udp(payload: &[u8]) -> Vec<u8> {
        let mut u = vec![0x1b, 0x58, 0x1b, 0x58, 0, 0, 0, 0];
        u[4..6].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        u.extend(payload);
        fix(&mut u, 6, Some((17, A, B)));
        u
    }

    fn tcp(flags: u8, options: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut t = vec![0xC0, 0x00, 0x1b, 0x58, 0, 0, 0x03, 0xE8, 0, 0, 0x07, 0xD0];
        t.push(((20 + options.len()) as u8 / 4) << 4);
        t.push(flags);
        t.extend([0x10, 0x00, 0, 0, 0, 0]);
        t.extend(options);
        t.extend(payload);
        fix(&mut t, 16, Some((6, A, B)));
        t
    }

    #[test]
    fn known_ipv4_header_checksum() {
        // The example of the IPv4 article of Wikipedia: the checksum field is 0xb861.
        let header = [0x45, 0, 0, 0x73, 0, 0, 0x40, 0, 0x40, 0x11, 0xb8, 0x61, 0xc0, 0xa8, 0, 1, 0xc0, 0xa8, 0, 0xc7];
        assert_eq!(ones_complement_sum(&header, 0), 0xFFFF);
        let mut bad = header;
        bad[8] = 0x3F;
        assert_ne!(ones_complement_sum(&bad, 0), 0xFFFF);
    }

    #[test]
    fn arp_request_and_reply() {
        let mut f = Mac::BROADCAST.0.to_vec();
        f.extend(MAC_A);
        f.extend([8, 6, 0, 1, 8, 0, 6, 4, 0, 1]);
        f.extend(MAC_A);
        f.extend(A);
        f.extend([0; 6]);
        f.extend(B);
        let p = dissect(&f, f.len()).unwrap();
        assert_eq!(p.to_string(), "who-has 192.168.77.12 tell 192.168.77.11");
        assert_eq!((p.protocol(), p.is_control(), p.is_damaged()), ("ARP", true, false));

        // 46 bytes of padding after the packet change nothing.
        f[21] = 2;
        f.extend([0; 18]);
        let p = dissect(&f, f.len()).unwrap();
        assert_eq!(p.to_string(), "192.168.77.11 is-at 02:00:00:00:00:11");
        let mut lines = Vec::new();
        p.layers(|layer, text| lines.push(std::format!("{} {text}", layer.name())));
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("eth 02:00:00:00:00:11 → ff:ff:ff:ff:ff:ff  type 0x0806 (ARP)  broadcast"));
        assert_eq!(lines[1], "arp 192.168.77.11 is-at 02:00:00:00:00:11  to 192.168.77.12 (00:00:00:00:00:00)");
    }

    #[test]
    fn udp_datagram_with_a_node_frame() {
        let line = b"$DAT,node-A,7,12500,1,2,2500000*08";
        let f = ipv4_frame(17, &udp(line), 0x4000);
        let p = dissect(&f, f.len()).unwrap();
        assert_eq!(
            p.to_string(),
            std::format!("192.168.77.11:7000 → 192.168.77.12:7000  len {}  payload 34 B", f.len())
        );
        assert_eq!(p.payload(), line);
        assert!(!p.is_control() && !p.is_damaged());
        let Net::Ipv4(ip, L4::Udp(u, _)) = p.net else { panic!("{p:?}") };
        assert!(ip.checksum_ok && ip.dont_fragment && !ip.more_fragments);
        assert_eq!((ip.ttl, ip.protocol, ip.id, usize::from(ip.total_len)), (64, 17, 0x1c46, 20 + 8 + 34));
        assert_eq!((u.src_port, u.dst_port, u.checksum_ok), (7000, 7000, Some(true)));
        assert_eq!(
            App(p.payload()).to_string(),
            "DAT node-A #7 t=12500 ms  button pressed  events 2  period 2500.000 ms"
        );
        let mut labels = Vec::new();
        p.layers(|layer, _| labels.push(layer));
        assert_eq!(labels, [Layer::Eth, Layer::Ipv4, Layer::Udp]);
    }

    #[test]
    fn padding_is_not_payload() {
        // A short datagram sent as a 60-byte frame: the 8 extra bytes are padding.
        let mut f = ipv4_frame(17, &udp(b"hi"), 0);
        f.extend([0; 8]);
        let p = dissect(&f, f.len()).unwrap();
        assert_eq!(p.payload(), b"hi");
        let Net::Ipv4(_, L4::Udp(u, _)) = p.net else { panic!() };
        assert_eq!(u.checksum_ok, Some(true), "the padding is outside the checksum");
    }

    #[test]
    fn a_corrupted_byte_is_reported() {
        let mut f = ipv4_frame(17, &udp(b"$ECH,node-B,3,900,2*00"), 0);
        let last = f.len() - 1;
        f[last] ^= 0x01;
        let p = dissect(&f, f.len()).unwrap();
        assert!(p.is_damaged());
        let Net::Ipv4(ip, L4::Udp(u, _)) = p.net else { panic!() };
        assert_eq!((ip.checksum_ok, u.checksum_ok), (true, Some(false)));

        let mut f = ipv4_frame(17, &udp(b"x"), 0);
        f[14 + 8] = 1; // TTL
        let p = dissect(&f, f.len()).unwrap();
        assert!(p.is_damaged(), "IPv4 header checksum");
    }

    #[test]
    fn a_frame_cut_short_is_not_verified() {
        let payload = [0x55u8; 300];
        let f = ipv4_frame(17, &udp(&payload), 0);
        let p = dissect(&f[..128], f.len()).unwrap();
        assert_eq!((p.captured, p.wire_len), (128, f.len()));
        let Net::Ipv4(_, L4::Udp(u, got)) = p.net else { panic!() };
        assert_eq!(u.checksum_ok, None, "not enough bytes to check, so no verdict");
        assert_eq!(got.len(), 128 - 14 - 20 - 8);
        assert_eq!(u.len, 308);
        assert!(!p.is_damaged());
    }

    #[test]
    fn udp_checksum_zero_means_none() {
        let mut u = udp(b"abc");
        u[6] = 0;
        u[7] = 0;
        let f = ipv4_frame(17, &u, 0);
        let p = dissect(&f, f.len()).unwrap();
        let Net::Ipv4(_, L4::Udp(u, _)) = p.net else { panic!() };
        assert_eq!(u.checksum_ok, None);
    }

    #[test]
    fn tcp_handshake_flags_and_options() {
        let syn = ipv4_frame(6, &tcp(TcpFlags::SYN, &[2, 4, 0x05, 0xB4, 1, 3, 3, 7], b""), 0x4000);
        let p = dissect(&syn, syn.len()).unwrap();
        let Net::Ipv4(_, L4::Tcp(t, payload)) = p.net else { panic!("{p:?}") };
        assert_eq!((t.src_port, t.dst_port, t.seq, t.ack, t.window), (49152, 7000, 1000, 2000, 4096));
        assert_eq!((t.mss, t.header_len, t.checksum_ok, payload.len()), (Some(1460), 28, Some(true), 0));
        assert!(p.is_control(), "a SYN is an event");
        assert!(p.to_string().contains("[SYN]  len 62  payload 0 B"), "{p}");
        let mut lines = Vec::new();
        p.layers(|layer, text| lines.push(std::format!("{}: {text}", layer.name())));
        assert_eq!(lines.len(), 3, "one line per layer");
        assert!(
            lines[2].starts_with("tcp: 49152 → 7000  SYN  seq 1000  win 4096  mss 1460  hdr 28 B  payload 0 B  cs ok"),
            "{}",
            lines[2]
        );

        let data = ipv4_frame(6, &tcp(TcpFlags::PSH | TcpFlags::ACK, &[], b"$ECH,node-B,3,900,2*00\n"), 0);
        let p = dissect(&data, data.len()).unwrap();
        assert!(!p.is_control(), "data isn't");
        assert!(p.to_string().contains("[PSH,ACK]"), "{p}");
        assert!(p.layers_text().contains("ack 2000"), "{}", p.layers_text());

        for flag in [TcpFlags::FIN | TcpFlags::ACK, TcpFlags::RST] {
            let f = ipv4_frame(6, &tcp(flag, &[], b""), 0);
            assert!(dissect(&f, f.len()).unwrap().is_control());
        }
        let f = ipv4_frame(6, &tcp(TcpFlags::ACK, &[], b""), 0);
        assert!(!dissect(&f, f.len()).unwrap().is_control(), "a pure ACK is traffic");
    }

    #[test]
    fn tcp_flags_display() {
        assert_eq!(TcpFlags(0).to_string(), "none");
        assert_eq!(TcpFlags(TcpFlags::SYN | TcpFlags::ACK).to_string(), "SYN,ACK");
        assert_eq!(TcpFlags(0x3F).to_string(), "SYN,FIN,RST,PSH,ACK,URG");
    }

    #[test]
    fn icmp_echo() {
        let mut icmp = vec![8, 0, 0, 0, 0, 1, 0, 2, 0xAA, 0xBB];
        fix(&mut icmp, 2, None);
        let f = ipv4_frame(1, &icmp, 0);
        let p = dissect(&f, f.len()).unwrap();
        assert_eq!(p.to_string(), "192.168.77.11 → 192.168.77.12  type 8 code 0 (echo request)  id 1 seq 2");
        assert!(p.is_control() && !p.is_damaged());
        icmp[8] ^= 1;
        let f = ipv4_frame(1, &icmp, 0);
        assert!(dissect(&f, f.len()).unwrap().is_damaged());
    }

    #[test]
    fn igmp_and_ptp_have_their_own_names() {
        let name = |frame: &[u8]| dissect(frame, frame.len()).unwrap().protocol();
        assert_eq!(name(&ipv4_frame(2, &[0x11, 0, 0, 0, 0, 0, 0, 0], 0)), "IGMP");
        assert_eq!(name(&ipv4_frame(89, &[0; 8], 0)), "IPv4", "another IP protocol");
        let mut ptp = udp(b"x");
        ptp[2..4].copy_from_slice(&319u16.to_be_bytes());
        fix(&mut ptp, 6, Some((17, A, B)));
        assert_eq!(name(&ipv4_frame(17, &ptp, 0)), "PTP", "event messages go to UDP port 319");
        ptp[2..4].copy_from_slice(&320u16.to_be_bytes());
        fix(&mut ptp, 6, Some((17, A, B)));
        assert_eq!(name(&ipv4_frame(17, &ptp, 0)), "PTP");
        assert_eq!(name(&ipv4_frame(17, &udp(b"x"), 0)), "UDP");
        let mut layer2 = MAC_B.to_vec();
        layer2.extend(MAC_A);
        layer2.extend([0x88, 0xF7, 0, 2, 0, 0]);
        assert_eq!(name(&layer2), "PTP", "PTP straight over Ethernet");
    }

    #[test]
    fn what_is_not_understood_is_still_shown() {
        // IPv6: only the ethertype is known.
        let mut f = MAC_B.to_vec();
        f.extend(MAC_A);
        f.extend([0x86, 0xDD, 0x60, 0, 0, 0]);
        let p = dissect(&f, 60).unwrap();
        assert_eq!(p.protocol(), "IPv6");
        assert!(p.is_control());

        // An IPv4 header that lies about its length.
        let mut g = ipv4_frame(17, &udp(b"x"), 0);
        g[14 + 2] = 0;
        g[14 + 3] = 5;
        let p = dissect(&g, g.len()).unwrap();
        assert!(matches!(p.net, Net::Malformed(_)) && p.is_damaged());
        assert!(p.to_string().contains("total length"), "{p}");

        // A fragment after the first: no UDP header there.
        let g = ipv4_frame(17, b"later bytes", 0x00B9);
        let p = dissect(&g, g.len()).unwrap();
        let Net::Ipv4(ip, L4::Fragment) = p.net else { panic!("{p:?}") };
        assert_eq!(ip.fragment_offset, 185 * 8);
        assert!(p.payload().is_empty());

        // Unknown protocol, and too short.
        let g = ipv4_frame(89, b"ospf", 0);
        assert!(matches!(dissect(&g, g.len()).unwrap().net, Net::Ipv4(_, L4::Other(89))));
        assert_eq!(dissect(&[0; 10], 10), Err("shorter than an Ethernet header"));
    }

    #[test]
    fn vlan_tag_is_skipped() {
        let plain = ipv4_frame(17, &udp(b"vlan"), 0);
        let mut f = plain[..12].to_vec();
        f.extend([0x81, 0x00, 0x00, 0x2A]); // 802.1Q, VLAN 42
        f.extend(&plain[12..]);
        let p = dissect(&f, f.len()).unwrap();
        assert_eq!(p.eth.vlan, Some(42));
        assert_eq!(p.payload(), b"vlan");
    }

    #[test]
    fn the_nodes_frames_in_a_payload() {
        let show = |s: &str| App(s.as_bytes()).to_string();
        assert_eq!(
            show("$EVT,node-B,3,900,FALL,00000000DEADBEEF*09"),
            "EVT node-B #3 t=900 ms  press (FALL)  key 00000000DEADBEEF"
        );
        assert_eq!(
            show("$EVT,node-B,3,900,FALL,00000000DEADBEEE*09"),
            "invalid frame, bad checksum: computed 0x0A, found 0x09"
        );
        assert_eq!(show("hello"), "text \"hello\"");
        assert_eq!(App(&[0xFF, 0xFE, 0x00]).to_string(), "3 bytes, not text");
        assert!(show("$RED,node-B,5,1200,3*00").starts_with("address space frame, bad checksum"));
        let two: Vec<_> = app_lines(b"$A*00\n$B*00\n\n").collect();
        assert_eq!(two, [b"$A*00".as_slice(), b"$B*00".as_slice()]);
    }

    #[test]
    fn hex_row_parts_have_a_fixed_width() {
        let data: Vec<u8> = (0..20).collect();
        let mut rows = Vec::new();
        hexdump(&data, |r| rows.push((r.offset(), r.hex().to_string(), r.ascii().to_string())));
        assert_eq!(rows[0].0, 0);
        assert_eq!(rows[1].0, 16);
        // Same width whatever the row holds: a bar between the parts lines up.
        assert_eq!(rows[0].1.chars().count(), rows[1].1.chars().count());
        assert_eq!(rows[0].1.chars().count(), 16 * 3 - 1 + 1);
        assert_eq!(rows[0].1, "00 01 02 03 04 05 06 07  08 09 0a 0b 0c 0d 0e 0f");
        assert_eq!(rows[1].2, "....");
    }

    #[test]
    fn hex_dump_rows() {
        let data: Vec<u8> = (0x41..0x41 + 20).collect();
        let mut rows = Vec::new();
        hexdump(&data, |r| rows.push(r.to_string()));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], "0000  41 42 43 44 45 46 47 48  49 4a 4b 4c 4d 4e 4f 50  ABCDEFGHIJKLMNOP");
        // The short last row is padded, so the text lines up.
        assert_eq!(rows[1], "0010  51 52 53 54                                       QRST");
        assert_eq!(rows[0].find("ABC"), rows[1].find("QRS"));
        let mut none = 0;
        hexdump(&[], |_| none += 1);
        assert_eq!(none, 0);
        // Non-printable bytes are dots.
        let mut row = Vec::new();
        hexdump(&[0x00, 0x7F, 0x20, 0x7E], |r| row.push(r.to_string()));
        assert!(row[0].ends_with("  .. ~"), "{}", row[0]);
    }

    impl Packet<'_> {
        fn layers_text(&self) -> std::string::String {
            let mut out = std::string::String::new();
            self.layers(|layer, text| out.push_str(&std::format!("{}: {text}\n", layer.name())));
            out
        }
    }
}
