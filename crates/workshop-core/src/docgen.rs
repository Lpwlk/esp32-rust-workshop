//! Markdown reference pages generated from the firmware's own tables.
//!
//! The documentation site (Zensical, `docs/book`, published on GitHub Pages)
//! includes these pages; they are never written by hand.
//! The host test `tests/docs.rs` regenerates them and fails if the committed
//! files are out of date (run `UPDATE_DOCS=1 cargo test-host` to refresh them).

use core::fmt::{self, Write};

use crate::cli::{COMMANDS, Group};
use crate::config::{KEYS, MAX_GPIO, MISSING_PINS, NodeConfig, READ_ONLY, RESERVED_PINS, shared};
use crate::fx::PATTERNS;
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
        61 => "BOOT button (strapping pin)",
        _ => return None,
    })
}

/// `docs/book/reference/cli.md`: every console command.
pub fn cli_reference(out: &mut impl Write) -> fmt::Result {
    write!(out, "{HEADER}# CLI commands\n\n")?;
    out.write_str("Type these in the USB console (`cargo run --release --bin node-a`).\n")?;
    out.write_str("Changes are applied immediately and lost on reboot.\n")?;
    for (group, title) in [
        (Group::Settings, "Settings"),
        (Group::Shortcuts, "Shortcuts"),
        (Group::Link, "Link tests"),
        (Group::Info, "Information"),
        (Group::System, "Saved settings and restart"),
    ] {
        write!(out, "\n## {title}\n\n| Command | Aliases | Summary |\n|---|---|---|\n")?;
        for c in COMMANDS.iter().filter(|c| c.group == group) {
            write!(out, "| `{}` | ", c.usage)?;
            for (i, a) in c.aliases.iter().enumerate() {
                write!(out, "{}`{a}`", if i == 0 { "" } else { ", " })?;
            }
            writeln!(out, " | {} |", c.summary)?;
        }
        for c in COMMANDS.iter().filter(|c| c.group == group) {
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

/// `docs/book/reference/config.md`: every setting, its range and default.
pub fn config_reference(out: &mut impl Write) -> fmt::Result {
    let d = NodeConfig::DEFAULT;
    write!(out, "{HEADER}# Configuration\n\n")?;
    out.write_str("Build values come from `NodeConfig::DEFAULT`, overridden per node in `src/bin/node_*.rs`.\n")?;
    out.write_str("Invalid values fail the build; at runtime `set` applies the same checks.\n\n")?;
    out.write_str(
        "## Runtime settings (`set <key> <value>`)\n\n| Key | Meaning | Accepted | Default |\n|---|---|---|---|\n",
    )?;
    for (name, key) in KEYS {
        let (what, range) = key.describe();
        writeln!(out, "| `{name}` | {what} | {range} | `{}` |", d.value(key))?;
    }
    out.write_str("\n## Fixed at build time\n\n")?;
    for k in READ_ONLY {
        writeln!(out, "- `{k}`")?;
    }
    write!(out, "\n`shared.baud_rate` = {} baud, 8N1: must be identical on both nodes.\n", shared::BAUD_RATE)?;
    Ok(())
}

/// `docs/book/reference/pins.md`: default pins and the ones to avoid.
pub fn pins_reference(out: &mut impl Write) -> fmt::Result {
    let p = NodeConfig::DEFAULT.pins;
    write!(out, "{HEADER}# Pins\n\n## Default assignment\n\n| Function | GPIO |\n|---|---|\n")?;
    for (what, n) in [
        ("UART1 TX", p.uart_tx),
        ("UART1 RX", p.uart_rx),
        ("button", p.button),
        ("LED 1", p.led1),
        ("LED 2", p.led2),
        ("RGB LED (fixed)", 60),
    ] {
        writeln!(out, "| {what} | {n} |")?;
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

/// `docs/book/reference/protocol.md`: frame format, with examples that are the
/// test vectors (so their checksums are checked by the tests).
pub fn protocol_reference(out: &mut impl Write) -> fmt::Result {
    write!(out, "{HEADER}# Frame protocol\n\n")?;
    writeln!(
        out,
        "UART1, {} baud, 8N1. One frame = one ASCII line, at most {MAX_FRAME} bytes with its `\\n`.",
        shared::BAUD_RATE
    )?;
    out.write_str("The checksum `CS` is the XOR of every byte between `$` and `*`, as 2 hex digits (NMEA style).\n\n")?;
    out.write_str("| Frame | Format | Sent |\n|---|---|---|\n")?;
    out.write_str("| `DAT` | `$DAT,<node>,<seq>,<t_ms>,<btn>,<events>,<period_us>*CS` | every `data_period_ms` |\n")?;
    out.write_str("| `EVT` | `$EVT,<node>,<seq>,<t_ms>,<RISE\\|FALL>,<key>*CS` | on each debounced button edge |\n")?;
    out.write_str(
        "| `BAU` | `$BAU,<node>,<seq>,<t_ms>,<baud>,<REQ\\|ACK>*CS` | CLI `baud`: speed change handshake |\n",
    )?;
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
        writeln!(out, "| `{field}` | {what} |")?;
    }
    out.write_str("\n## Examples\n\n")?;
    for (frame, line) in FRAMES {
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

/// Every generated page: `(file name in docs/book/reference, generator)`.
pub const PAGES: [(&str, Generator); 5] = [
    ("cli.md", |o| cli_reference(&mut Dyn(o))),
    ("config.md", |o| config_reference(&mut Dyn(o))),
    ("pins.md", |o| pins_reference(&mut Dyn(o))),
    ("patterns.md", |o| patterns_reference(&mut Dyn(o))),
    ("protocol.md", |o| protocol_reference(&mut Dyn(o))),
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
