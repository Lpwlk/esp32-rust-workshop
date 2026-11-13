//! Markdown reference pages generated from the firmware's own tables.
//!
//! The documentation site (Zensical, `docs/book`, published on GitHub Pages)
//! includes these pages; they are never written by hand.
//! The host test `tests/docs.rs` regenerates them and fails if the committed
//! files are out of date (run `UPDATE_DOCS=1 cargo test-host` to refresh them).

use core::fmt::{self, Write};

use crate::cli::{COMMANDS, Group};
use crate::config::{KEYS, Link, LinkKind, MAX_GPIO, MISSING_PINS, NodeConfig, RESERVED_PINS, shared};
use crate::fx::PATTERNS;
use crate::net::{NET_KEYS, PHY_PINS};
use crate::protocol::{MAX_FRAME, Payload};
use crate::test_vectors::FRAMES;

const HEADER: &str = "<!-- Generated from the source by `workshop_core::docgen`. Do not edit: run `UPDATE_DOCS=1 cargo test-host`. -->\n\n";

/// Why a pin is in [`RESERVED_PINS`] (ESP32-S31 datasheet, section 2.3.5).
pub fn reserved_reason(gpio: u8) -> Option<&'static str> {
    Some(match gpio {
        26..=28 | 30..=32 => "SPI0/1: in-package flash",
        33 => "USB Serial/JTAG D- (CLI, espflash, probe-rs)",
        34 => "USB Serial/JTAG D+ (CLI, espflash, probe-rs)",
        58 => "UART0 TX: ROM / bootloader console",
        59 => "UART0 RX: ROM / bootloader console",
        60 => "on-board RGB LED (strapping pin)",
        50..=57 => "on-board audio codec and amplifier (not on the header)",
        61 => "BOOT button, read by the firmware like the jumper (strapping pin)",
        _ => return None,
    })
}

/// The default config of a link kind.
fn defaults(kind: LinkKind) -> NodeConfig {
    match kind {
        LinkKind::Uart => NodeConfig::DEFAULT,
        LinkKind::Net => NodeConfig::DEFAULT_NET,
    }
}

/// `docs/book/reference/cli.md` (`eth-cli.md` for Ethernet): every console command.
pub fn cli_reference(out: &mut impl Write, kind: LinkKind) -> fmt::Result {
    match kind {
        LinkKind::Uart => {
            write!(out, "{HEADER}# CLI commands\n\n")?;
            out.write_str("Type these in the USB console (`cargo run --release --bin node-a`).\n")?;
        }
        LinkKind::Net => {
            write!(out, "{HEADER}# CLI commands (Ethernet app)\n\n")?;
            out.write_str("Type these in the USB console (`cd eth-lab && cargo run --release --bin node-a`).\n")?;
        }
    }
    out.write_str("Changes are applied immediately and lost on reboot.\n")?;
    for (group, title) in [
        (Group::Settings, "Settings"),
        (Group::Shortcuts, "Shortcuts"),
        (Group::Link, "Link tests"),
        (Group::Info, "Information"),
        (Group::System, "Saved settings and restart"),
    ] {
        write!(out, "\n## {title}\n\n| Command | Aliases | Summary |\n|---|---|---|\n")?;
        for c in COMMANDS.iter().filter(|c| c.group == group && c.on(kind)) {
            write!(out, "| `{}` | ", c.usage)?;
            for (i, a) in c.aliases.iter().enumerate() {
                write!(out, "{}`{a}`", if i == 0 { "" } else { ", " })?;
            }
            writeln!(out, " | {} |", c.summary)?;
        }
        for c in COMMANDS.iter().filter(|c| c.group == group && c.on(kind)) {
            write!(out, "\n### `{}`\n\n", c.name)?;
            if !c.details.is_empty() {
                out.write_str("```text\n")?;
                for line in c.details {
                    writeln!(out, "{line}")?;
                }
                out.write_str("```\n\n")?;
            }
            out.write_str("Examples:\n\n```text\n")?;
            for e in c.examples {
                writeln!(out, "{e}")?;
            }
            out.write_str("```\n")?;
        }
    }
    Ok(())
}

/// `docs/book/reference/config.md` (`eth-config.md` for Ethernet): every setting, its range and default.
pub fn config_reference(out: &mut impl Write, kind: LinkKind) -> fmt::Result {
    let d = defaults(kind);
    write!(out, "{HEADER}# Configuration{}\n\n", if kind == LinkKind::Net { " (Ethernet app)" } else { "" })?;
    match kind {
        LinkKind::Uart => {
            out.write_str("Build values come from `NodeConfig::DEFAULT`, overridden per node in `src/bin/node_*.rs`.\n")?
        }
        LinkKind::Net => out.write_str(
            "Build values come from `NodeConfig::DEFAULT_NET`, overridden per node by a preset (`eth-lab/presets/*.toml`).\n",
        )?,
    }
    out.write_str("Invalid values fail the build; at runtime `set` applies the same checks.\n\n")?;
    out.write_str(
        "## Runtime settings (`set <key> <value>`)\n\n| Key | Meaning | Accepted | Default |\n|---|---|---|---|\n",
    )?;
    for (name, key) in KEYS {
        let (what, range) = key.describe();
        writeln!(out, "| `{name}` | {what} | {range} | `{}` |", d.value(key))?;
    }
    out.write_str("\n## Fixed at build time\n\n")?;
    for k in d.read_only() {
        writeln!(out, "- `{k}`")?;
    }
    match d.link {
        Link::Uart(_) => {
            write!(out, "\n`shared.baud_rate` = {} baud, 8N1: must be identical on both nodes.\n", shared::BAUD_RATE)?
        }
        Link::Net(n) => {
            out.write_str("\n## Ethernet link (`[net]` in a preset)\n\n")?;
            out.write_str("Identical on both nodes, except the two addresses, which are each other's.\n\n")?;
            out.write_str("| Key | Meaning | Accepted | Default |\n|---|---|---|---|\n")?;
            for (name, what, range) in NET_KEYS {
                let mut value: heapless::String<24> = heapless::String::new();
                let _ = n.write_value(name, &mut value);
                writeln!(out, "| `{name}` | {what} | {range} | `{value}` |")?;
            }
        }
    }
    Ok(())
}

/// `docs/book/reference/pins.md` (`eth-pins.md` for Ethernet): default pins and the ones to avoid.
pub fn pins_reference(out: &mut impl Write, kind: LinkKind) -> fmt::Result {
    let d = defaults(kind);
    let p = d.pins;
    write!(
        out,
        "{HEADER}# Pins{}\n\n## Default assignment\n\n| Function | GPIO |\n|---|---|\n",
        if kind == LinkKind::Net { " (Ethernet app)" } else { "" }
    )?;
    if let Link::Uart(u) = d.link {
        writeln!(out, "| UART1 TX | {} |\n| UART1 RX | {} |", u.tx, u.rx)?;
    }
    for (what, n) in [("button", p.button), ("LED 1", p.led1), ("LED 2", p.led2), ("RGB LED (fixed)", 60)] {
        writeln!(out, "| {what} | {n} |")?;
    }
    if kind == LinkKind::Net {
        out.write_str(
            "\n## Used by the Ethernet PHY\n\nWired on the board, not configurable. Nothing else may use them:\n\n",
        )?;
        out.write_str("| GPIO | Signal |\n|---|---|\n")?;
        for (n, what) in PHY_PINS {
            writeln!(out, "| {n} | {what} |")?;
        }
    }
    write!(
        out,
        "\n## Refused by the config check\n\nGPIO numbers go from 0 to {MAX_GPIO}.\n\n| GPIO | Why |\n|---|---|\n"
    )?;
    for n in RESERVED_PINS {
        writeln!(out, "| {n} | {} |", reserved_reason(n).unwrap_or("reserved"))?;
    }
    for n in MISSING_PINS {
        writeln!(out, "| {n} | does not exist on the ESP32-S31 |")?;
    }
    if kind == LinkKind::Net {
        writeln!(out, "| 5-19 | used by the Ethernet PHY (see above) |")?;
    }
    Ok(())
}

/// `docs/book/reference/patterns.md`: RGB patterns with color swatches.
pub fn patterns_reference(out: &mut impl Write) -> fmt::Result {
    write!(out, "{HEADER}# RGB patterns\n\n| # | Name | Keys | Description |\n|---|---|---|---|\n")?;
    for (i, pat) in PATTERNS.iter().enumerate() {
        write!(out, "| {} | `{}` | ", i + 1, pat.name)?;
        for c in pat.keys {
            write!(
                out,
                "<span style=\"background:#{:02X}{:02X}{:02X}\" title=\"#{0:02X}{1:02X}{2:02X}\">&nbsp;&nbsp;&nbsp;</span>",
                c.r, c.g, c.b
            )?;
        }
        writeln!(out, " | {} |", pat.description)?;
    }
    Ok(())
}

/// `docs/book/reference/protocol.md` (`eth-protocol.md` for Ethernet): frame format, with
/// examples that are the test vectors (so their checksums are checked by the tests).
pub fn protocol_reference(out: &mut impl Write, kind: LinkKind) -> fmt::Result {
    match kind {
        LinkKind::Uart => {
            write!(out, "{HEADER}# Frame protocol\n\n")?;
            writeln!(
                out,
                "UART1, {} baud, 8N1. One frame = one ASCII line, at most {MAX_FRAME} bytes with its `\\n`.",
                shared::BAUD_RATE
            )?;
        }
        LinkKind::Net => {
            write!(out, "{HEADER}# Frame protocol (Ethernet app)\n\n")?;
            writeln!(
                out,
                "The frames are the same as on the UART link. One frame = one ASCII line, at most {MAX_FRAME} bytes with its `\\n`."
            )?;
            out.write_str("With `net.transport = udp`, **each frame is one datagram** (line included), sent to the peer's `net.port`:\n")?;
            out.write_str("a lost frame shows as a gap in `seq`. With `tcp`, the lines are the stream of one connection, nothing is lost.\n")?;
            out.write_str("`BAU` frames are for the UART link: an Ethernet node ignores them.\n\n")?;
        }
    }
    out.write_str("The checksum `CS` is the XOR of every byte between `$` and `*`, as 2 hex digits (NMEA style).\n\n")?;
    out.write_str("| Frame | Format | Sent |\n|---|---|---|\n")?;
    out.write_str("| `DAT` | `$DAT,<node>,<seq>,<t_ms>,<btn>,<events>,<period_us>*CS` | every `data_period_ms` |\n")?;
    out.write_str("| `EVT` | `$EVT,<node>,<seq>,<t_ms>,<RISE\\|FALL>,<key>*CS` | on each debounced button edge |\n")?;
    if kind == LinkKind::Uart {
        out.write_str(
            "| `BAU` | `$BAU,<node>,<seq>,<t_ms>,<baud>,<REQ\\|ACK>*CS` | CLI `baud`: speed change handshake |\n",
        )?;
    }
    out.write_str(
        "| `ECH` | `$ECH,<node>,<seq>,<t_ms>,<echo_seq>*CS` | in `tx_mode on-rx`, for each received frame |\n",
    )?;
    out.write_str("| `BST` | `$BST,<node>,<burst>,<t_ms>,<index>,<count>,<fill>*CS` | CLI `burst` |\n\n")?;
    out.write_str("| Field | Meaning |\n|---|---|\n")?;
    for (field, what) in [
        ("node", "sender's name, 1-12 printable ASCII characters without `,` `*` `$`"),
        ("seq", "sequence number, shared by DAT and EVT, wraps after 4294967295: gaps = lost frames"),
        ("t_ms", "sender's uptime when the frame was built, ms"),
        ("btn", "`1` if the sender's button is pressed, else `0`"),
        ("events", "EVT frames sent since boot"),
        ("period_us", "sender's DAT period in µs (12.5 ms = 12500): the receiver uses it for its link timeout"),
        ("RISE / FALL", "jumper removed (released) / plugged to GND (pressed)"),
        ("key", "64-bit random number from the sender's TRNG, 16 hex digits"),
        (
            "baud / REQ / ACK",
            "requested speed; the peer answers `ACK` then switches, the requester switches on the `ACK`",
        ),
        ("echo_seq", "`seq` of the frame answered: its sender gets the round-trip time"),
        ("burst", "in `BST`, replaces `seq`: burst number"),
        ("index / count", "position in the burst (from 0) and its length"),
        ("fill", "value derived from `index`, checked on reception on top of the checksum"),
    ] {
        if kind == LinkKind::Net && field.starts_with("baud") {
            continue; // UART only
        }
        writeln!(out, "| `{field}` | {what} |")?;
    }
    out.write_str("\n## Examples\n\n")?;
    for (frame, line) in FRAMES {
        if kind == LinkKind::Net && matches!(frame.payload, Payload::Baud { .. }) {
            continue;
        }
        let what = match frame.payload {
            Payload::Data { .. } => "DAT",
            Payload::Event { .. } => "EVT",
            Payload::Baud { .. } => "BAU",
            Payload::Echo { .. } => "ECH",
            Payload::Burst { .. } => "BST",
        };
        writeln!(
            out,
            "- {what} from `{}`, seq {}:\n\n    ```text\n    {}\n    ```\n",
            frame.node,
            frame.seq,
            line.trim_end()
        )?;
    }
    Ok(())
}

/// Writes one reference page.
pub type Generator = fn(&mut dyn Write) -> fmt::Result;

/// Every generated page: `(file name in docs/book/reference, generator)`. The `eth-*` pages
/// are the Ethernet app's versions of the UART ones.
pub const PAGES: [(&str, Generator); 9] = [
    ("cli.md", |o| cli_reference(&mut Dyn(o), LinkKind::Uart)),
    ("config.md", |o| config_reference(&mut Dyn(o), LinkKind::Uart)),
    ("pins.md", |o| pins_reference(&mut Dyn(o), LinkKind::Uart)),
    ("patterns.md", |o| patterns_reference(&mut Dyn(o))),
    ("protocol.md", |o| protocol_reference(&mut Dyn(o), LinkKind::Uart)),
    ("eth-cli.md", |o| cli_reference(&mut Dyn(o), LinkKind::Net)),
    ("eth-config.md", |o| config_reference(&mut Dyn(o), LinkKind::Net)),
    ("eth-pins.md", |o| pins_reference(&mut Dyn(o), LinkKind::Net)),
    ("eth-protocol.md", |o| protocol_reference(&mut Dyn(o), LinkKind::Net)),
];

/// Adapter so the generators (generic over `impl Write`) fit in [`PAGES`].
struct Dyn<'a>(&'a mut dyn Write);

impl Write for Dyn<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reserved_pin_has_a_reason() {
        for n in RESERVED_PINS {
            assert!(reserved_reason(n).is_some(), "GPIO{n}");
        }
    }
}
