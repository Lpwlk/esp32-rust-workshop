//! Settings of the Ethernet link (the counterpart of the UART link): addresses, port,
//! transport. They are **build-time** settings: both nodes must agree on them, and the
//! network stack is configured once at boot.
//!
//! The frames are the same as on the UART link ([`crate::protocol`]), one frame per
//! UDP datagram, or one line per frame on a TCP connection:
//!
//! | `net.transport` | How frames travel |
//! |---|---|
//! | `udp` | one frame per datagram, node to `net.peer:net.port`. Frames can be lost (the `seq` check shows it) |
//! | `tcp` | one connection (`net.tcp_role`: `server` listens, `client` connects), frames are lines in the stream. Nothing is lost, but a stall delays everything behind it |
//!
//! ```
//! use workshop_core::config::{Link, NodeConfig};
//!
//! const CONFIG: NodeConfig = NodeConfig { name: "node-A", ..NodeConfig::DEFAULT_NET };
//! const _: () = CONFIG.check();
//! let Link::Net(net) = CONFIG.link else { unreachable!() };
//! assert_eq!(net.address, [192, 168, 77, 11]);
//! assert!(net.same_subnet(net.peer));
//! ```

use core::fmt;

/// First GPIO used by the Ethernet PHY on the ESP32-S31-Function-CoreBoard-1
/// (MDC 5, MDIO 6, PHY reset 7, then RGMII data and clocks on 8-19).
pub const PHY_FIRST_PIN: u8 = 5;
/// Last GPIO used by the Ethernet PHY (RGMII RXD0, GPIO19).
pub const PHY_LAST_PIN: u8 = 19;

/// What each PHY pin does, for the pin reference.
pub const PHY_PINS: [(u8, &str); 15] = [
    (5, "MDC (PHY management clock)"),
    (6, "MDIO (PHY management data)"),
    (7, "PHY reset"),
    (8, "RGMII TXD0"),
    (9, "RGMII TXD1"),
    (10, "RGMII TXD2"),
    (11, "RGMII TXD3"),
    (12, "RGMII TX_CTL"),
    (13, "RGMII TX_CLK"),
    (14, "RGMII RX_CLK"),
    (15, "RGMII RX_CTL"),
    (16, "RGMII RXD3"),
    (17, "RGMII RXD2"),
    (18, "RGMII RXD1"),
    (19, "RGMII RXD0"),
];

/// How frames travel between the nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// One frame per UDP datagram.
    Udp,
    /// Frames are lines in one TCP connection.
    Tcp,
}

impl Transport {
    /// Name used in the presets and shown by the CLI.
    pub const fn name(self) -> &'static str {
        match self {
            Transport::Udp => "udp",
            Transport::Tcp => "tcp",
        }
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

/// Which node opens the TCP connection (`net.transport = "tcp"` only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TcpRole {
    /// Listens on `net.port`.
    Server,
    /// Connects to `net.peer:net.port`, and retries until the server is there.
    Client,
}

impl TcpRole {
    /// Name used in the presets and shown by the CLI.
    pub const fn name(self) -> &'static str {
        match self {
            TcpRole::Server => "server",
            TcpRole::Client => "client",
        }
    }
}

impl fmt::Display for TcpRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

/// An IPv4 address as it is written in presets and the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Addr(pub [u8; 4]);

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{a}.{b}.{c}.{d}")
    }
}

impl Addr {
    /// Parses `192.168.77.11`.
    pub fn parse(text: &str) -> Option<Addr> {
        text.trim().parse::<core::net::Ipv4Addr>().ok().map(|a| Addr(a.octets()))
    }
}

/// Network settings of a node (`link = Link::Net`). All of them are fixed at build time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetLink {
    /// UDP datagrams or a TCP connection.
    pub transport: Transport,
    /// This node's static IPv4 address (there is no DHCP).
    pub address: [u8; 4],
    /// Subnet size: 24 = `255.255.255.0`.
    pub prefix_len: u8,
    /// The other node's address, same subnet.
    pub peer: [u8; 4],
    /// Port of the frames: this node listens on it (UDP, or TCP server) and the peer's
    /// is the same.
    pub port: u16,
    /// TCP only: who opens the connection.
    pub tcp_role: TcpRole,
    /// Fastest link speed to allow, Mbit/s: 1000 or 100. The RGMII clock comes from the
    /// MPLL through an integer divider: 1000 Mbit/s needs it at 500 MHz, the HAL default of
    /// 400 MHz (what 200 MHz PSRAM needs) caps Ethernet at 100.
    pub max_speed: u16,
}

/// Ports the firmware accepts: not the well-known ones.
pub const PORT_RANGE: (u16, u16) = (1024, 65535);
/// Speeds accepted by [`NetLink::max_speed`].
pub const SPEEDS: [u16; 2] = [100, 1000];

impl NetLink {
    /// Two boards on a direct cable: `192.168.77.11` talking to `192.168.77.12`, UDP, port 7000.
    pub const DEFAULT: NetLink = NetLink {
        transport: Transport::Udp,
        address: [192, 168, 77, 11],
        prefix_len: 24,
        peer: [192, 168, 77, 12],
        port: 7000,
        tcp_role: TcpRole::Client,
        max_speed: 1000,
    };

    /// `255.255.255.0` for 24, as a number.
    pub const fn netmask(&self) -> u32 {
        if self.prefix_len == 0 { 0 } else { u32::MAX << (32 - self.prefix_len as u32) }
    }

    /// Whether `other` is in this node's subnet.
    pub const fn same_subnet(&self, other: [u8; 4]) -> bool {
        let mask = self.netmask();
        (u32::from_be_bytes(self.address) & mask) == (u32::from_be_bytes(other) & mask)
    }

    /// Checks the settings (usable in `const`, see [`crate::config::NodeConfig::check`]).
    pub const fn validate(&self) -> Result<(), &'static str> {
        if self.prefix_len < 8 || self.prefix_len > 30 {
            return Err("config: `net.prefix_len` must be 8-30");
        }
        if !self.same_subnet(self.peer) {
            return Err("config: `net.peer` is not in the subnet of `net.address`");
        }
        let (a, p) = (u32::from_be_bytes(self.address), u32::from_be_bytes(self.peer));
        if a == p {
            return Err("config: `net.peer` must differ from `net.address`");
        }
        let host_bits = !self.netmask();
        if (a & host_bits) == 0 || (a & host_bits) == host_bits || (p & host_bits) == 0 || (p & host_bits) == host_bits
        {
            return Err("config: `net.address` and `net.peer` can't be the network or broadcast address");
        }
        if self.port < PORT_RANGE.0 {
            return Err("config: `net.port` must be 1024-65535");
        }
        if self.max_speed != SPEEDS[0] && self.max_speed != SPEEDS[1] {
            return Err("config: `net.max_speed` must be 100 or 1000");
        }
        Ok(())
    }

    /// Sets one `net.*` key from text (build-time settings, see
    /// [`crate::config::NodeConfig::set_build`]).
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), &'static str> {
        let value = value.trim();
        match key {
            "net.transport" => {
                self.transport = match value {
                    "udp" => Transport::Udp,
                    "tcp" => Transport::Tcp,
                    _ => return Err("`net.transport`: expected udp or tcp"),
                }
            }
            "net.address" => self.address = Addr::parse(value).ok_or("`net.address`: expected an IPv4 address")?.0,
            "net.peer" => self.peer = Addr::parse(value).ok_or("`net.peer`: expected an IPv4 address")?.0,
            "net.prefix_len" => self.prefix_len = value.parse().map_err(|_| "`net.prefix_len`: expected 8-30")?,
            "net.port" => self.port = value.parse().map_err(|_| "`net.port`: expected 1024-65535")?,
            "net.tcp_role" => {
                self.tcp_role = match value {
                    "server" => TcpRole::Server,
                    "client" => TcpRole::Client,
                    _ => return Err("`net.tcp_role`: expected server or client"),
                }
            }
            "net.max_speed" => self.max_speed = value.parse().map_err(|_| "`net.max_speed`: expected 100 or 1000")?,
            _ => return Err("unknown `net.*` key"),
        }
        Ok(())
    }

    /// Writes the value of one `net.*` key as [`Self::set`] reads it back.
    pub fn write_value(&self, key: &str, out: &mut impl fmt::Write) -> fmt::Result {
        match key {
            "net.transport" => write!(out, "{}", self.transport),
            "net.address" => write!(out, "{}", Addr(self.address)),
            "net.peer" => write!(out, "{}", Addr(self.peer)),
            "net.prefix_len" => write!(out, "{}", self.prefix_len),
            "net.port" => write!(out, "{}", self.port),
            "net.tcp_role" => write!(out, "{}", self.tcp_role),
            "net.max_speed" => write!(out, "{}", self.max_speed),
            _ => Ok(()),
        }
    }
}

/// The `net.*` settings, in the order the CLI and the reference list them:
/// `(key, what it is, accepted values)`.
pub const NET_KEYS: [(&str, &str, &str); 7] = [
    ("net.transport", "how frames travel between the nodes", "udp, tcp"),
    ("net.address", "this node's static IPv4 address (no DHCP)", "a.b.c.d"),
    ("net.prefix_len", "subnet size (24 = 255.255.255.0)", "8-30"),
    ("net.peer", "the other node's address, same subnet", "a.b.c.d"),
    ("net.port", "port of the frames (UDP, or TCP server), same on both nodes", "1024-65535"),
    ("net.tcp_role", "TCP only: who opens the connection", "server, client"),
    ("net.max_speed", "fastest link speed allowed, Mbit/s (1000 needs the MPLL at 500 MHz)", "100, 1000"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        assert_eq!(NetLink::DEFAULT.validate(), Ok(()));
        assert_eq!(NetLink::DEFAULT.netmask(), 0xFFFF_FF00);
    }

    #[test]
    fn address_rules() {
        let d = NetLink::DEFAULT;
        let bad = |f: &dyn Fn(&mut NetLink)| {
            let mut n = d;
            f(&mut n);
            n.validate()
        };
        assert!(bad(&|n| n.peer = n.address).is_err());
        assert!(bad(&|n| n.peer = [192, 168, 78, 12]).is_err(), "other subnet");
        assert!(bad(&|n| n.peer = [192, 168, 77, 0]).is_err(), "network address");
        assert!(bad(&|n| n.peer = [192, 168, 77, 255]).is_err(), "broadcast");
        assert!(bad(&|n| n.prefix_len = 31).is_err());
        assert!(bad(&|n| n.prefix_len = 7).is_err());
        assert!(bad(&|n| n.port = 80).is_err());
        assert!(bad(&|n| n.max_speed = 10).is_err());
        assert_eq!(bad(&|n| (n.prefix_len, n.peer) = (16, [192, 168, 99, 5])), Ok(()));
    }

    #[test]
    fn set_and_write_round_trip() {
        let mut n = NetLink::DEFAULT;
        for (key, value) in [
            ("net.transport", "tcp"),
            ("net.address", "10.0.0.2"),
            ("net.prefix_len", "16"),
            ("net.peer", "10.0.5.9"),
            ("net.port", "9000"),
            ("net.tcp_role", "server"),
            ("net.max_speed", "100"),
        ] {
            n.set(key, value).unwrap();
            let mut text = std::string::String::new();
            n.write_value(key, &mut text).unwrap();
            assert_eq!(text, value, "{key}");
        }
        assert_eq!(n.validate(), Ok(()));
        assert!(n.set("net.address", "10.0.0").is_err());
        assert!(n.set("net.address", "10.0.0.256").is_err());
        assert!(n.set("net.nope", "1").is_err());
        for (key, _, _) in NET_KEYS {
            let mut text = std::string::String::new();
            n.write_value(key, &mut text).unwrap();
            assert!(!text.is_empty(), "{key} has no value");
        }
    }

    #[test]
    fn phy_pins_are_the_documented_range() {
        assert_eq!(PHY_PINS.len(), usize::from(PHY_LAST_PIN - PHY_FIRST_PIN) + 1);
        for (i, (pin, _)) in PHY_PINS.iter().enumerate() {
            assert_eq!(usize::from(*pin), usize::from(PHY_FIRST_PIN) + i);
        }
    }
}
