//! Colored terminal output (USB console, shown by `espflash monitor`). Same look as the UART
//! firmware's; only what concerns the link differs (Ethernet instead of baud rate).

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use critical_section::Mutex;

use workshop_core::{
    burst::{BurstSummary, Throughput},
    config::{self, Link, NodeConfig},
    net::{Addr, PHY_PINS},
    protocol::{Edge, Frame, ParseError, Payload},
    rtt::RttStats,
    units::{Freq, Period},
    wire::{DatDelta, Dir},
};

use crate::{cli, ext_leds::ExtLeds, fx, net::PhyLink};

/// Prints one line above the CLI prompt: erase the prompt line, print, redraw the prompt.
/// The line also goes to RTT with the `rtt` feature; the prompt itself is drawn on
/// the USB console only (RTT viewers show lines, not an editable prompt).
macro_rules! out {
    ($($arg:tt)*) => {{
        $crate::term::prompt_clear();
        crate::con_println!($($arg)*);
        $crate::term::prompt_draw();
    }};
}

static PROMPT_ON: AtomicBool = AtomicBool::new(false);
/// Cursor position in the line being typed (characters from its start).
static PROMPT_CURSOR: AtomicUsize = AtomicUsize::new(0);
static PROMPT: Mutex<RefCell<heapless::String<{ crate::cli::MAX_INPUT }>>> =
    Mutex::new(RefCell::new(heapless::String::new()));
/// Question shown instead of `cli` while the `burst` wizard runs.
static PROMPT_LABEL: Mutex<RefCell<Option<heapless::String<48>>>> = Mutex::new(RefCell::new(None));

/// The current `verbose` level (`workshop_core::config::Verbosity` as a number).
static VERBOSITY: AtomicU8 = AtomicU8::new(0);
/// The live line: the last frame event, redrawn in place just above the prompt (verbosity 0).
static LIVE: Mutex<RefCell<Option<heapless::String<512>>>> = Mutex::new(RefCell::new(None));

/// Prints formatted text on the serial console (the prompt and the live line are serial only).
fn raw(args: core::fmt::Arguments<'_>) {
    crate::console::print(args)
}

/// Sets how much the console prints. Leaving the live level removes the live line.
pub fn set_verbosity(level: workshop_core::config::Verbosity) {
    VERBOSITY.store(level as u8, Ordering::Relaxed);
    if level != workshop_core::config::Verbosity::Live {
        live_clear();
    }
}

/// Frames go to the live line: verbosity 0, and the serial console is on (an RTT viewer has no
/// editable prompt to keep the line under, so it gets every line).
fn live_mode() -> bool {
    VERBOSITY.load(Ordering::Relaxed) == 0 && crate::rtt_console::serial_on()
}

/// Shows `text` as the live line, replacing the previous one: the prompt block (live line, then
/// prompt) is erased, then drawn again with the new text.
fn live_set(text: &str) {
    prompt_clear();
    critical_section::with(|cs| {
        let mut live = LIVE.borrow_ref_mut(cs);
        let mut line = heapless::String::new();
        let _ = line.push_str(text);
        *live = Some(line);
    });
    prompt_draw();
}

/// Removes the live line.
pub fn live_clear() {
    prompt_clear();
    critical_section::with(|cs| *LIVE.borrow_ref_mut(cs) = None);
    prompt_draw();
}

pub fn prompt_enable() {
    PROMPT_ON.store(true, Ordering::Relaxed);
    prompt_draw();
}

#[doc(hidden)]
pub fn prompt_clear() {
    if PROMPT_ON.load(Ordering::Relaxed) && crate::rtt_console::serial_on() {
        // The prompt line; then, if there is a live line above it, that one too (up one row, erase).
        let live = critical_section::with(|cs| LIVE.borrow_ref(cs).is_some());
        raw(format_args!("\r\x1b[2K{}", if live { "\x1b[1A\r\x1b[2K" } else { "" }));
    }
}

#[doc(hidden)]
pub fn prompt_draw() {
    if PROMPT_ON.load(Ordering::Relaxed) && crate::rtt_console::serial_on() {
        // Copy, then print outside the critical section: printing can take a while,
        // and interrupts (buttons, the network) must keep running meanwhile.
        let (text, label) =
            critical_section::with(|cs| (PROMPT.borrow_ref(cs).clone(), PROMPT_LABEL.borrow_ref(cs).clone()));
        // The live line first, above the prompt; no auto-wrap, so a long line is cut, not wrapped
        // onto a second row (which the erase above wouldn't know about).
        if let Some(live) = critical_section::with(|cs| LIVE.borrow_ref(cs).clone()) {
            raw(format_args!("\x1b[?7l{live}\x1b[?7h\n"));
        }
        match label {
            Some(q) => crate::console::print(format_args!("\x1b[1;33m{q} ›\x1b[0m {text}")),
            None => crate::console::print(format_args!("\x1b[1;35mcli ›\x1b[0m {text}")),
        }
        // Put the terminal cursor where the editor's cursor is (ESC [ n D = n left).
        let back = text.len().saturating_sub(PROMPT_CURSOR.load(Ordering::Relaxed));
        if back > 0 {
            crate::console::print(format_args!("\x1b[{back}D"));
        }
    }
}

/// Shows a question instead of `cli` in the prompt (`None`: back to `cli`).
pub fn prompt_label(question: Option<&str>) {
    critical_section::with(|cs| {
        *PROMPT_LABEL.borrow_ref_mut(cs) = question.map(|q| {
            let mut s = heapless::String::new();
            let _ = s.push_str(&q[..q.len().min(48)]);
            s
        });
    });
    prompt_clear();
    prompt_draw();
}

/// Replaces the text being typed (and the cursor position in it), redraws the prompt.
pub fn prompt_set(text: &str, cursor: usize) {
    PROMPT_CURSOR.store(cursor, Ordering::Relaxed);
    critical_section::with(|cs| {
        let mut p = PROMPT.borrow_ref_mut(cs);
        p.clear();
        let _ = p.push_str(text);
    });
    prompt_clear();
    prompt_draw();
}

const RST: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const GRAY: &str = "\x1b[90m";
/// Black on yellow badge for button events.
const EVT_BADGE: &str = "\x1b[30;43m";
/// Black on gray badge for periodic data.
const DAT_BADGE: &str = "\x1b[30;47m";
/// Black on magenta badge for link control frames (baud handshake, echo).
const LNK_BADGE: &str = "\x1b[30;45m";
const MAGENTA: &str = "\x1b[35m";

/// Link counters shown in the periodic status line.
#[derive(Default)]
pub struct Stats {
    pub tx: u32,
    pub rx: u32,
    pub lost: u32,
    pub bad: u32,
}

fn stamp(t_ms: u64) -> (u64, u64) {
    (t_ms / 1000, t_ms % 1000)
}

/// Inner width of the banner box, in characters.
const BOX: usize = 62;

/// Hardware facts read at boot, shown in the "chip" card.
pub struct ChipInfo {
    pub rev_major: u8,
    pub rev_minor: u8,
    pub cpu_mhz: u32,
    pub mac: [u8; 6],
    pub reset: heapless::String<32>,
    pub rng_samples: [u32; 3],
}

/// Compiled-in features and saved settings, shown in the "system" card.
pub struct SystemInfo {
    /// Cargo features of this build, and whether each one is on.
    pub features: [(&'static str, bool); 3],
    /// What the boot did with the settings saved in flash.
    pub boot_load: crate::settings::BootLoad,
    /// Settings that currently differ from the build config.
    pub unsaved: usize,
    /// Watchdog timeout, if the `watchdog` feature is on.
    pub watchdog_ms: Option<u64>,
}

/// Width of the label column in cards.
const LABEL: usize = 10;

fn card_top(title: &str) {
    // "╭─ title " then fill: 3 chars + title + 1 space already used.
    out!("{CYAN}╭─ {BOLD}{title}{RST}{CYAN} {:─<w$}╮{RST}", "", w = BOX - 3 - title.chars().count());
}

fn card_split(title: &str) {
    out!("{CYAN}├─ {BOLD}{title}{RST}{CYAN} {:─<w$}┤{RST}", "", w = BOX - 3 - title.chars().count());
}

/// One `label  value` row. `value` must be plain text (no ANSI codes), since padding
/// and truncation count characters.
fn card_row(label: &str, args: core::fmt::Arguments<'_>) {
    let mut value: heapless::String<128> = heapless::String::new();
    let _ = core::fmt::Write::write_fmt(&mut value, args);
    // Pad to the card width, or cut with "…" so the right border never moves.
    let w = BOX - 2 - LABEL;
    if value.chars().count() > w {
        let cut = value.char_indices().nth(w - 1).map_or(value.len(), |(i, _)| i);
        value.truncate(cut);
        let _ = value.push('…');
    }
    out!("{CYAN}│{RST}  {GRAY}{label:<LABEL$}{RST}{value:<w$}{CYAN}│{RST}");
}

/// Prints `items` (separated by " · ") on as many rows as needed to fit the card.
fn card_list(label: &str, items: &str) {
    let width = BOX - 2 - LABEL;
    let mut row: heapless::String<128> = heapless::String::new();
    let mut label = label;
    for item in items.split(" · ") {
        let needed = if row.is_empty() { item.chars().count() } else { row.chars().count() + 3 + item.chars().count() };
        if needed > width && !row.is_empty() {
            card_row(label, format_args!("{row}"));
            label = "";
            row.clear();
        }
        if !row.is_empty() {
            let _ = row.push_str(" · ");
        }
        let _ = row.push_str(item);
    }
    if !row.is_empty() {
        card_row(label, format_args!("{row}"));
    }
}

/// `1000` -> `1 Gbit/s`, `100` -> `100 Mbit/s`.
pub struct Speed(pub u16);

impl core::fmt::Display for Speed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            0 => f.write_str("no link"),
            m if m >= 1000 => write!(f, "{} Gbit/s", m / 1000),
            m => write!(f, "{m} Mbit/s"),
        }
    }
}

/// How the transport behaves, for the boot card.
fn transport_note(n: &config::NetLink) -> &'static str {
    match n.transport {
        config::Transport::Udp => "one frame per datagram",
        config::Transport::Tcp => match n.tcp_role {
            config::TcpRole::Server => "listens for the peer",
            config::TcpRole::Client => "connects to the peer",
        },
    }
}

/// MPLL frequency the firmware runs at, for the boot card (500 MHz is what 1 Gbit/s needs).
fn mpll_mhz(n: &config::NetLink) -> u32 {
    if n.max_speed >= 1000 { 500 } else { 400 }
}

/// The boot card: the link, this node's config, build, chip, LEDs and CLI.
pub fn boot_card(cfg: &NodeConfig, chip: &ChipInfo, sys: &SystemInfo) {
    let p = cfg.pins;
    let n = cfg.net();
    out!();
    card_top("link · ethernet, must match on both nodes");
    card_row("transport", format_args!("{} · port {} · {}", n.transport, n.port, transport_note(&n)));
    card_row("phy", format_args!("YT8531 · RGMII · up to {} · MPLL {} MHz", Speed(n.max_speed), mpll_mhz(&n)));
    card_row("format", format_args!("$TYPE,node,seq,t_ms,…*XOR  (NMEA-style)"));
    card_row("frames", format_args!("DAT (periodic) · EVT + 64-bit key per edge"));

    card_split("node · this board's config");
    card_row("name", format_args!("{}", cfg.name));
    card_row("board", format_args!("ESP32-S31-Function-CoreBoard-1"));
    card_row("address", format_args!("{}/{} (static, no DHCP)", Addr(n.address), n.prefix_len));
    card_row("peer", format_args!("{}", Addr(n.peer)));
    card_row(
        "phy io",
        format_args!("GPIO{}-{} (MDC, MDIO, reset, RGMII)", PHY_PINS[0].0, PHY_PINS[PHY_PINS.len() - 1].0),
    );
    card_row(
        "DAT",
        format_args!(
            "every {} ({}) · tx {}",
            Period(cfg.data_period_ms),
            Freq(1000.0 / cfg.data_period_ms),
            cfg.tx_mode
        ),
    );
    card_row("status", format_args!("stats line every {}", Period(cfg.status_period_ms)));
    card_row("timeout", format_args!("link DOWN after {} peer periods", cfg.link_timeout_periods));
    card_row("button", format_args!("jumper GPIO{} + BOOT GPIO61 · IRQ on both edges", p.button));
    card_row("filter", format_args!("{} ms debounce · {} ms lockout", cfg.debounce_ms, cfg.lockout_ms));

    card_split("build");
    card_row("firmware", format_args!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")));
    card_row("git", format_args!("{}", env!("BUILD_GIT")));
    card_row("built", format_args!("{}", env!("BUILD_DATE")));
    card_row("profile", format_args!("{} · opt-level {}", env!("BUILD_PROFILE"), env!("BUILD_OPT_LEVEL")));
    card_row("rustc", format_args!("{} · edition {}", env!("BUILD_RUSTC"), env!("BUILD_EDITION")));
    card_row("cargo", format_args!("{}", env!("BUILD_CARGO")));
    card_row("target", format_args!("{}", env!("BUILD_TARGET")));
    card_row("host", format_args!("{}", env!("BUILD_HOST")));
    card_list("crates", env!("BUILD_DEPS"));

    card_split("chip");
    card_row("chip", format_args!("ESP32-S31 rev v{}.{} · CPU {} MHz", chip.rev_major, chip.rev_minor, chip.cpu_mhz));
    let m = chip.mac;
    card_row("MAC", format_args!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}", m[0], m[1], m[2], m[3], m[4], m[5]));
    card_row("reset", format_args!("{}", chip.reset));
    let [a, b, c] = chip.rng_samples;
    card_row("TRNG", format_args!("0x{a:08X} 0x{b:08X} 0x{c:08X}"));

    card_split("rgb led");
    let rgb = cfg.rgb;
    let pattern = &fx::PATTERNS[fx::find(rgb.pattern).unwrap_or(0)];
    card_swatch_row("pattern", pattern, format_args!("{}", pattern.name));
    card_row(
        "timing",
        format_args!("{} refresh · {} cycle · {}%", Freq(rgb.freq_hz), Period(rgb.cycle_ms), rgb.brightness),
    );
    card_row("driver", format_args!("WS2812 on GPIO60 · RMT channel 0"));

    card_split("breadboard leds");
    card_row("LED1", format_args!("GPIO{} · {}", p.led1, cfg.led1));
    card_row("LED2", format_args!("GPIO{} · {}", p.led2, cfg.led2));

    card_split("system · cargo features and saved settings");
    let mut features: heapless::String<64> = heapless::String::new();
    for (i, (name, on)) in sys.features.iter().enumerate() {
        let _ = core::fmt::Write::write_fmt(
            &mut features,
            format_args!("{}{name} {}", if i == 0 { "" } else { " · " }, if *on { "✓" } else { "✗" }),
        );
    }
    card_row("features", format_args!("{features}"));
    use crate::settings::BootLoad;
    match sys.boot_load {
        BootLoad::Disabled => card_row("saved", format_args!("off (built without `persist`)")),
        BootLoad::NoStorage => card_row("saved", format_args!("unavailable: no `nvs` partition")),
        BootLoad::Empty => card_row("saved", format_args!("nothing saved: build config")),
        BootLoad::Applied(l) if l.skipped > 0 => {
            card_row("saved", format_args!("{} applied · {} skipped (unknown / refused)", l.applied, l.skipped))
        }
        BootLoad::Applied(l) => card_row("saved", format_args!("{} setting(s) applied from flash", l.applied)),
        BootLoad::Corrupt => card_row("saved", format_args!("damaged record ignored: build config")),
        BootLoad::OtherNode => card_row("saved", format_args!("saved by another node: ignored")),
    }
    card_row("changed", format_args!("{} setting(s) differ from the build · `config`", sys.unsaved));
    match sys.watchdog_ms {
        Some(ms) => card_row("watchdog", format_args!("TIMG0 · reset if the loop stalls {} s", ms as f32 / 1000.0)),
        None => card_row("watchdog", format_args!("off (built without `watchdog`)")),
    }
    card_row("console", format_args!("{}", cfg.console));

    card_split("cli");
    card_row("port", format_args!("USB Serial/JTAG (this console)"));
    card_row("usage", format_args!("type `help` then Enter"));
    out!("{CYAN}╰{:─<BOX$}╯{RST}", "");
    out!();
}

/// Width of the node column of frame lines (names are up to 12 chars; longer
/// than this only shifts that one line).
const NODE_W: usize = 8;
/// Visible width of the body column of frame lines: a `DAT` body exactly, so
/// the `Δ` column lines up (`EVT` bodies are longer, but carry no `Δ`).
const BODY_W: usize = 24;

/// Characters a terminal shows for `s`: ANSI color sequences take no room.
fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut in_esc = false;
    for c in s.chars() {
        match (in_esc, c) {
            (false, '\x1b') => in_esc = true,
            (false, _) => n += 1,
            (true, 'm') => in_esc = false,
            (true, _) => {}
        }
    }
    n
}

/// Pads `s` with spaces up to `width` visible characters.
fn pad_visible<const N: usize>(s: &mut heapless::String<N>, width: usize) {
    for _ in visible_len(s)..width {
        let _ = s.push(' ');
    }
}

/// One frame line, for both directions, in fixed columns:
///
/// ```text
///    12.513 TX ▶  DAT  #00004          btn ○ released  event     3  Δ   357.206 ms ← RX
///    14.656 RX ◀  DAT  #00004 node-B   btn ● pressed   event     4  Δ  2142.821 ms ← TX
///    14.810 RX ◀  ECH  #00005 node-B   echo of #00004            rtt 182 µs
///    15.002 TX ▶  EVT  #00005          ▼ press    key 0123456789ABCDEF  3 edges
/// ```
fn frame_line(now_ms: u64, tx: bool, frame: &Frame<'_>, extra: &str, stats: &Stats) {
    use core::fmt::Write;
    let (s, ms) = stamp(now_ms);
    let dir = if tx { "\x1b[36mTX ▶\x1b[0m" } else { "\x1b[32mRX ◀\x1b[0m" };
    let from = if tx { "" } else { frame.node };
    let badge = match frame.payload {
        Payload::Data { .. } => DAT_BADGE,
        Payload::Event { .. } => EVT_BADGE,
        Payload::Baud { .. } | Payload::Echo { .. } => LNK_BADGE,
        Payload::Burst { .. } => "",
    };
    let mut body: heapless::String<96> = heapless::String::new();
    let kind = match frame.payload {
        Payload::Data { pressed, events, .. } => {
            let btn = if pressed { "\x1b[33m● pressed\x1b[0m " } else { "\x1b[90m○ released\x1b[0m" };
            let _ = write!(body, "{GRAY}btn{RST} {btn}  {GRAY}event{RST} {events:>5}");
            "DAT"
        }
        Payload::Event { edge, key } => {
            let edge = match edge {
                Edge::Falling => "\x1b[1;33m▼ press  \x1b[0m",
                Edge::Rising => "\x1b[1;36m▲ release\x1b[0m",
            };
            let _ = write!(body, "{edge}  {GRAY}key{RST} {BOLD}{key:016X}{RST}");
            "EVT"
        }
        Payload::Baud { baud, ack } => {
            // A UART-link frame: an Ethernet node only shows it if the peer sends one by mistake.
            let _ = write!(body, "{BOLD}{}{RST} {baud} baud", if ack { "ACK" } else { "REQ" });
            "BAU"
        }
        Payload::Echo { echo_seq } => {
            let _ = write!(body, "{GRAY}echo of{RST} #{echo_seq:05}");
            "ECH"
        }
        Payload::Burst { index, count } => {
            let _ = write!(body, "{}/{count}", index + 1);
            "BST"
        }
    };
    let live = live_mode();
    if !extra.is_empty() && !live {
        pad_visible(&mut body, BODY_W);
    }
    let mut line: heapless::String<512> = heapless::String::new();
    let _ = write!(
        line,
        "{GRAY}{s:>6}.{ms:03}{RST} {dir} {badge} {kind} {RST} {GRAY}#{:05}{RST} {YELLOW}{from:<NODE_W$}{RST} {body}{extra}",
        frame.seq
    );
    if live {
        // The counters stay on the live line, so nothing needs the periodic stats line.
        let _ = write!(line, "  {GRAY}│ tx {} rx {}{RST} ", stats.tx, stats.rx);
        if stats.lost > 0 || stats.bad > 0 {
            let _ = write!(line, "{RED}lost {} bad {}{RST}", stats.lost, stats.bad);
        } else {
            let _ = write!(line, "{GRAY}lost 0{RST}");
        }
        live_set(&line);
    } else {
        out!("{line}");
    }
}

/// `  Δ  1249.873 ms ← RX`: time since the previous `DAT`, sent or received,
/// measured from when each frame ended on the wire (µs resolution). The number is
/// right-aligned, so the decimal points line up.
fn push_delta(extra: &mut heapless::String<96>, d: DatDelta) {
    let prev = match d.prev {
        Dir::Tx => "\x1b[36m← TX\x1b[0m",
        Dir::Rx => "\x1b[32m← RX\x1b[0m",
    };
    let us = d.delta_us.unsigned_abs();
    let mut num: heapless::String<16> = heapless::String::new();
    let sign = if d.delta_us < 0 { "-" } else { "" };
    let _ = core::fmt::Write::write_fmt(&mut num, format_args!("{sign}{}.{:03}", us / 1000, us % 1000));
    let _ = core::fmt::Write::write_fmt(extra, format_args!("  {GRAY}Δ{RST} {BOLD}{num:>9}{RST} {GRAY}ms{RST} {prev}"));
}

/// A sent frame; `button`: for an `EVT`, which button and the interrupts behind
/// it; `delta`: for a `DAT`.
pub fn sent(now_ms: u64, frame: &Frame<'_>, button: Option<(&str, u32)>, delta: Option<DatDelta>, stats: &Stats) {
    let mut extra: heapless::String<96> = heapless::String::new();
    if let Some(d) = delta {
        push_delta(&mut extra, d);
    }
    if let Some((name, n)) = button {
        let _ = core::fmt::Write::write_fmt(&mut extra, format_args!("  {CYAN}{name}{RST}"));
        if n > 1 {
            let _ = core::fmt::Write::write_fmt(&mut extra, format_args!("  {DIM}{n} edges{RST}"));
        }
    }
    frame_line(now_ms, true, frame, &extra, stats);
}

/// A received frame; `rtt_us`: round-trip time measured from it (`ECH`);
/// `delta`: for a `DAT`.
pub fn received(now_ms: u64, frame: &Frame<'_>, rtt_us: Option<u64>, delta: Option<DatDelta>, stats: &Stats) {
    let mut extra: heapless::String<96> = heapless::String::new();
    if let Some(d) = delta {
        push_delta(&mut extra, d);
    }
    if let Some(rtt) = rtt_us {
        let _ = core::fmt::Write::write_fmt(&mut extra, format_args!("  {GRAY}rtt{RST} {BOLD}{}{RST}", Us(rtt)));
    }
    frame_line(now_ms, false, frame, &extra, stats);
}

pub fn bounce_ignored(now_ms: u64, button: &str, raw_edges: u32) {
    let (s, ms) = stamp(now_ms);
    out!("{GRAY}{s:>6}.{ms:03}   ·  {button}: bounce ignored ({raw_edges} edge(s), level unchanged){RST}");
}

pub fn lost(now_ms: u64, count: u32) {
    let (s, ms) = stamp(now_ms);
    out!("{GRAY}{s:>6}.{ms:03}{RST} {RED}RX ⚠  {count} frame(s) lost{RST}");
}

pub fn peer_restarted(now_ms: u64, node: &str) {
    let (s, ms) = stamp(now_ms);
    out!("{GRAY}{s:>6}.{ms:03}{RST} {YELLOW}RX ↻  {node} restarted (sequence reset){RST}");
}

pub fn rx_error(now_ms: u64, err: ParseError, raw: &[u8]) {
    let (s, ms) = stamp(now_ms);
    match err {
        ParseError::Checksum { expected, received } => out!(
            "{GRAY}{s:>6}.{ms:03}{RST} {RED}RX ✗  bad checksum{RST} {GRAY}(got 0x{received:02X}, expected 0x{expected:02X}){RST} {DIM}{}{RST}",
            Raw(raw)
        ),
        ParseError::Malformed | ParseError::BadField => {
            out!("{GRAY}{s:>6}.{ms:03}{RST} {RED}RX ✗  invalid frame{RST} {DIM}{}{RST}", Raw(raw))
        }
    }
}

/// Received bytes: as text if they are printable ASCII, else as a hex dump
/// (`0x24 0x00 0xFF …`, first 16 bytes) so garbage on the line is readable.
struct Raw<'a>(&'a [u8]);

impl core::fmt::Display for Raw<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let printable = self.0.iter().all(|b| (0x20..0x7f).contains(b));
        if printable && let Ok(text) = core::str::from_utf8(self.0) {
            return f.write_str(text);
        }
        for (i, b) in self.0.iter().take(16).enumerate() {
            write!(f, "{}0x{b:02X}", if i == 0 { "" } else { " " })?;
        }
        if self.0.len() > 16 {
            write!(f, " … ({} bytes)", self.0.len())?;
        }
        Ok(())
    }
}

pub fn net_error(now_ms: u64, what: &str) {
    let (s, ms) = stamp(now_ms);
    out!("{GRAY}{s:>6}.{ms:03}{RST} {RED}NET ✗  {what}{RST}");
}

pub fn status(now_ms: u64, stats: &Stats, link_up: bool, phy: PhyLink, rtt: Option<RttStats>) {
    let (s, _) = stamp(now_ms);
    let link = if link_up { "\x1b[1;32m● link UP  \x1b[0m" } else { "\x1b[1;31m● link DOWN\x1b[0m" };
    let mut extra: heapless::String<64> = heapless::String::new();
    let _ = core::fmt::Write::write_fmt(&mut extra, format_args!(" {MAGENTA}{}{RST}", Speed(phy.mbps)));
    if let Some(r) = rtt {
        let _ = core::fmt::Write::write_fmt(
            &mut extra,
            format_args!(" {GRAY}rtt {}/{}/{}{RST}", Us(r.min_us), Us(r.avg_us), Us(r.max_us)),
        );
    }
    out!(
        "{GRAY}──── {s:>4}s ── tx {} · rx {} · lost {} · bad {} ──{RST} {link}{extra} {GRAY}────{RST}",
        stats.tx,
        stats.rx,
        stats.lost,
        stats.bad
    );
}

// --- Link speed, round-trip time, bursts ----------------------------------------

/// `1234` µs -> `1.234 ms`, `850` -> `850 µs`.
struct Us(u64);

impl core::fmt::Display for Us {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 < 1000 { write!(f, "{} µs", self.0) } else { write!(f, "{}.{:03} ms", self.0 / 1000, self.0 % 1000) }
    }
}

/// Bytes per second -> `512.3 kB/s`.
struct Bps(u64);

impl core::fmt::Display for Bps {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            b if b >= 1_000_000 => write!(f, "{}.{:02} MB/s", b / 1_000_000, b % 1_000_000 / 10_000),
            b if b >= 1000 => write!(f, "{}.{} kB/s", b / 1000, b % 1000 / 100),
            b => write!(f, "{b} B/s"),
        }
    }
}

/// A link event (baud handshake, revert...), in magenta.
pub fn link_event(now_ms: u64, args: core::fmt::Arguments<'_>) {
    let (s, ms) = stamp(now_ms);
    out!("{GRAY}{s:>6}.{ms:03}{RST} {MAGENTA}LINK ⇄  {args}{RST}");
}

/// `link`: the cable, what the PHY negotiated, the addresses, the transport and the counters.
pub fn link_info(cfg: &NodeConfig, phy: PhyLink, connected: bool, c: &crate::link::Counters, peer_alive: bool) {
    let n = cfg.net();
    let cable = if phy.up {
        let duplex = if phy.full_duplex { "full" } else { "half" };
        let mut s: heapless::String<48> = heapless::String::new();
        let _ = core::fmt::Write::write_fmt(
            &mut s,
            format_args!("\x1b[1;32m● up\x1b[0m · {} · {duplex} duplex", Speed(phy.mbps)),
        );
        s
    } else {
        let mut s: heapless::String<48> = heapless::String::new();
        let _ = core::fmt::Write::write_str(&mut s, "\x1b[1;31m● down\x1b[0m · no cable or no partner");
        s
    };
    out!("  {BOLD}cable{RST}     {cable}");
    out!(
        "  {BOLD}capacity{RST}  {GRAY}{} max allowed · {} frame rate ceiling is the CPU, not the wire{RST}",
        Speed(n.max_speed),
        cfg.net().transport
    );
    out!("  {BOLD}address{RST}   {}/{} {GRAY}→ peer {}{RST}", Addr(n.address), n.prefix_len, Addr(n.peer));
    let state = match (n.transport, connected) {
        (config::Transport::Udp, _) => "no connection (datagrams)",
        (config::Transport::Tcp, true) => "connected",
        (config::Transport::Tcp, false) => "not connected",
    };
    out!("  {BOLD}transport{RST} {} port {} {GRAY}· {state}{RST}", n.transport, n.port);
    out!(
        "  {BOLD}peer{RST}      {}",
        if peer_alive { "\x1b[1;32m● frames arriving\x1b[0m" } else { "\x1b[1;31m● silent\x1b[0m" }
    );
    out!(
        "  {BOLD}counters{RST}  {GRAY}tx {} frames / {} · rx {} frames / {} · rx queue overflow {} · tcp connects {} lost {}{RST}",
        c.tx_frames,
        Bytes(u64::from(c.tx_bytes)),
        c.rx_frames,
        Bytes(u64::from(c.rx_bytes)),
        c.rx_dropped,
        c.connects,
        c.disconnects
    );
}

pub fn burst_start(now_ms: u64, id: u32, count: u32, rate_hz: f32) {
    let (s, ms) = stamp(now_ms);
    if rate_hz == 0.0 {
        out!("{GRAY}{s:>6}.{ms:03}{RST} {CYAN}TX ▶{RST} {BOLD}burst #{id}{RST}: {count} frames back to back…");
    } else {
        out!(
            "{GRAY}{s:>6}.{ms:03}{RST} {CYAN}TX ▶{RST} {BOLD}burst #{id}{RST}: {count} frames at {} (one every {})…",
            Freq(rate_hz),
            Period(1000.0 / rate_hz)
        );
    }
}

fn throughput(t: &Throughput, capacity_bytes_per_s: u64) -> heapless::String<64> {
    let mut out = heapless::String::new();
    let pm = t.capacity_permille(capacity_bytes_per_s);
    let _ = core::fmt::Write::write_fmt(
        &mut out,
        format_args!(
            "{} in {} → {BOLD}{}{RST} ({}.{}% of the link)",
            Bytes(t.bytes),
            Us(t.duration_us),
            Bps(t.bytes_per_s()),
            pm / 10,
            pm % 10
        ),
    );
    out
}

/// `61234` -> `61.2 kB`.
struct Bytes(u64);

impl core::fmt::Display for Bytes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 >= 1000 {
            write!(f, "{}.{} kB", self.0 / 1000, self.0 % 1000 / 100)
        } else {
            write!(f, "{} B", self.0)
        }
    }
}

pub fn burst_sent(now_ms: u64, id: u32, frames: u32, t: &Throughput, capacity: u64) {
    let (s, ms) = stamp(now_ms);
    out!(
        "{GRAY}{s:>6}.{ms:03}{RST} {CYAN}TX ▶{RST} {BOLD}burst #{id}{RST} sent: {frames} frames, {}",
        throughput(t, capacity)
    );
}

pub fn burst_received(now_ms: u64, node: &str, id: u32, r: &BurstSummary, capacity: u64) {
    let (s, ms) = stamp(now_ms);
    let verdict = if r.lost == 0 && r.bad == 0 { "\x1b[1;32m✓\x1b[0m" } else { "\x1b[1;31m✗\x1b[0m" };
    out!(
        "{GRAY}{s:>6}.{ms:03}{RST} {GREEN}RX ◀{RST} {verdict} {BOLD}burst #{id}{RST} from {YELLOW}{node}{RST}: {}/{} frames · {} lost · {} bad",
        r.received,
        r.count,
        r.lost,
        r.bad
    );
    out!("{GRAY}                    {RST}{}", throughput(&r.throughput, capacity));
}

/// Prints a `width`-character color swatch of `pattern` and returns its width.
fn swatch(pattern: &fx::Pattern, width: usize) -> usize {
    for i in 0..width {
        // Samples along the loop, so the swatch shows the actual fade.
        let c = fx::preview_at(pattern.keys, i as f32 / width as f32);
        crate::con_print!("\x1b[38;2;{};{};{}m█", c.r, c.g, c.b);
    }
    crate::con_print!("{RST}");
    width
}

/// Card row whose value starts with a color swatch of the current pattern.
fn card_swatch_row(label: &str, pattern: &fx::Pattern, args: core::fmt::Arguments<'_>) {
    let mut value: heapless::String<128> = heapless::String::new();
    let _ = core::fmt::Write::write_fmt(&mut value, args);
    prompt_clear();
    crate::con_print!("{CYAN}│{RST}  {GRAY}{label:<LABEL$}{RST}");
    let used = swatch(pattern, 16);
    crate::con_println!(" {value:<w$}{CYAN}│{RST}", w = BOX - 2 - LABEL - used - 1);
    prompt_draw();
}

/// Echo of an answer to a `burst` question (empty = the default was taken).
pub fn cli_answer(question: &str, answer: &str) {
    let answer = if answer.is_empty() { "(default)" } else { answer };
    out!("{BOLD}{YELLOW}{question} ›{RST} {answer}");
}

pub fn cli_echo(line: &str) {
    out!("{BOLD}\x1b[35m›{RST} {line}");
}

pub fn cli_error(args: core::fmt::Arguments<'_>) {
    out!("  {RED}✗ {args}{RST}");
}

pub fn cli_ok(args: core::fmt::Arguments<'_>) {
    out!("  {GREEN}✓{RST} {args}");
}

/// `help` (no topic): every command, grouped, from the table in `cli`.
/// `help <topic>`: details on a command, or on a setting key.
pub fn cli_help(topic: Option<&str>, cfg: &NodeConfig, defaults: &NodeConfig) {
    match topic {
        None => help_overview(),
        Some(t) => {
            if let Some(c) = cli::find_help(t) {
                help_command(c);
            } else if let Some(k) = config::Key::from_name(t) {
                help_key(t, k, cfg, defaults);
            } else if cfg.read_only().contains(&t) {
                out!("  {BOLD}{t}{RST} {GRAY}—{RST} read-only: set in firmware/ethernet/presets/*.toml, then rebuild");
            } else {
                cli_error(format_args!("no help for '{t}' (try `help`)"));
            }
        }
    }
}

const HELP_GROUPS: [(cli::Group, &str); 5] = [
    (cli::Group::Settings, "settings"),
    (cli::Group::Shortcuts, "shortcuts"),
    (cli::Group::Link, "link tests"),
    (cli::Group::Info, "info"),
    (cli::Group::System, "system"),
];

fn help_overview() {
    // Usage column as wide as the longest usage, so summaries always line up.
    let usage_w = cli::COMMANDS
        .iter()
        .filter(|c| c.on(config::LinkKind::Net))
        .map(|c| c.usage.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    for (group, title) in HELP_GROUPS {
        out!("  {BOLD}{title}{RST}");
        for c in cli::COMMANDS.iter().filter(|c| c.group == group && c.on(config::LinkKind::Net)) {
            out!("    {CYAN}{:<usage_w$}{RST}{GRAY}{}{RST}", c.usage, c.summary);
        }
    }
    out!("  {GRAY}`help <command>` or `help <key>` for details{RST}");
    out!("  {GRAY}← → edit · ↑ ↓ history · Home/End · Ctrl+U clears · Ctrl+C quits espflash{RST}");
}

/// Width of the label column in `help <command>` / `help <key>`.
const HELP_LABEL: usize = 10;

fn help_command(c: &cli::CmdHelp) {
    out!("  {BOLD}{}{RST} {GRAY}—{RST} {}", c.name, c.summary);
    out!("    {GRAY}{:<HELP_LABEL$}{RST}{CYAN}{}{RST}", "usage", c.usage);
    if !c.aliases.is_empty() {
        let mut aliases: heapless::String<32> = heapless::String::new();
        for (i, a) in c.aliases.iter().enumerate() {
            let _ = aliases.push_str(if i == 0 { "" } else { ", " });
            let _ = aliases.push_str(a);
        }
        out!("    {GRAY}{:<HELP_LABEL$}{RST}{aliases}", "aliases");
    }
    for (i, line) in c.details.iter().enumerate() {
        out!("    {GRAY}{:<HELP_LABEL$}{RST}{line}", if i == 0 { "details" } else { "" });
    }
    for (i, ex) in c.examples.iter().enumerate() {
        out!("    {GRAY}{:<HELP_LABEL$}{RST}{CYAN}{ex}{RST}", if i == 0 { "examples" } else { "" });
    }
    if c.name == "set" {
        for (row, chunk) in config::KEYS.chunks(4).enumerate() {
            let mut keys: heapless::String<96> = heapless::String::new();
            for (i, (name, _)) in chunk.iter().enumerate() {
                let _ = keys.push_str(if i == 0 { "" } else { " · " });
                let _ = keys.push_str(name);
            }
            out!("    {GRAY}{:<HELP_LABEL$}{RST}{keys}", if row == 0 { "keys" } else { "" });
        }
    }
}

fn help_key(name: &str, k: config::Key, cfg: &NodeConfig, defaults: &NodeConfig) {
    let (what, values) = k.describe();
    let (now, build) = (cfg.value(k), defaults.value(k));
    out!("  {BOLD}{name}{RST} {GRAY}—{RST} {what}");
    out!("    {GRAY}{:<HELP_LABEL$}{RST}{values}", "values");
    if now == build {
        out!("    {GRAY}{:<HELP_LABEL$}{RST}{now}", "current");
    } else {
        out!("    {GRAY}{:<HELP_LABEL$}{RST}{YELLOW}{now}{RST}   {GRAY}(build {build}){RST}", "current");
    }
    out!("    {GRAY}{:<HELP_LABEL$}{RST}{CYAN}set {name} <value>{RST}", "change");
}

// --- Verbose output ----------------------------------------------------------

/// The exact line on the wire, its size, and (RX) time since the peer's previous frame.
/// `wait_us`: for a received frame, how long it sat between the network task and the main loop.
pub fn wire(line: &[u8], delta_ms: Option<u64>, wait_us: Option<u64>) {
    let text = core::str::from_utf8(line).unwrap_or("<non-UTF-8>").trim_end();
    // Size on the wire, including the `\n` terminator (stripped from received lines).
    let size = text.len() + 1;
    // The XOR checksum is the 2 hex digits after `*`.
    let cs = text.rsplit_once('*').map_or("??", |(_, cs)| cs);
    let mut tail: heapless::String<48> = heapless::String::new();
    if let Some(d) = delta_ms {
        let _ = core::fmt::Write::write_fmt(&mut tail, format_args!("  +{d} ms"));
    }
    if let Some(w) = wait_us {
        let _ = core::fmt::Write::write_fmt(&mut tail, format_args!("  queued {}", Us(w)));
    }
    out!("{GRAY}             wire {DIM}{text}{RST}{GRAY}  cs 0x{cs}  {size} B{tail}{RST}")
}

pub fn link_counters(c: &crate::link::Counters, phy: PhyLink, peer_period_us: Option<u32>, loops_per_s: u64) {
    let mut peer: heapless::String<24> = heapless::String::new();
    let _ = match peer_period_us {
        Some(us) => core::fmt::Write::write_fmt(&mut peer, format_args!("{}", Period(us as f32 / 1000.0))),
        None => core::fmt::Write::write_str(&mut peer, "?"),
    };
    out!(
        "{GRAY}            bytes tx {} · rx {} · rx overflow {} · phy {} · main loop {loops_per_s}/s · last tx waited {} µs · peer DAT period {peer}{RST}",
        c.tx_bytes,
        c.rx_bytes,
        c.rx_dropped,
        Speed(phy.mbps),
        c.tx_wait_us
    );
}

/// `config`: every setting, read-only ones first, changed ones marked.
pub fn cli_config(cfg: &NodeConfig, defaults: &NodeConfig) {
    let p = cfg.pins;
    let Link::Net(n) = cfg.link else { return };
    out!("  {BOLD}{:<22}{:<16}{RST}{GRAY}build value{RST}", "key", "value");
    let mut fixed: heapless::Vec<(&str, heapless::String<24>), 12> = heapless::Vec::new();
    let mut add = |key: &'static str, value: heapless::String<24>| {
        let _ = fixed.push((key, value));
    };
    let num = |n: u8| {
        let mut s = heapless::String::new();
        let _ = core::fmt::Write::write_fmt(&mut s, format_args!("{n}"));
        s
    };
    add("pins.button", num(p.button));
    add("pins.led1", num(p.led1));
    add("pins.led2", num(p.led2));
    for (key, _, _) in workshop_core::net::NET_KEYS {
        let mut value = heapless::String::new();
        let _ = n.write_value(key, &mut value);
        add(key, value);
    }
    out!("  {GRAY}{:<22}{:<16}read-only (rebuild){RST}", "name", cfg.name);
    for (key, value) in &fixed {
        out!("  {GRAY}{key:<22}{value:<16}read-only (rebuild){RST}");
    }
    for (key, k) in config::KEYS {
        let now = cfg.value(k);
        let build = defaults.value(k);
        if now == build {
            out!("  {CYAN}{key:<22}{RST}{now:<16}");
        } else {
            out!("  {CYAN}{key:<22}{RST}{BOLD}{YELLOW}{now:<16}{RST}{GRAY}{build}  (changed){RST}");
        }
    }
}

pub fn cli_list(current: usize) {
    out!("  {BOLD}patterns{RST}");
    for (i, p) in fx::PATTERNS.iter().enumerate() {
        let mark = if i == current { "\x1b[1;35m▶\x1b[0m" } else { " " };
        prompt_clear();
        crate::con_print!("  {mark} {GRAY}{}{RST} {BOLD}{:<7}{RST} ", i + 1, p.name);
        swatch(p, 16);
        crate::con_println!("  {GRAY}{}{RST}", p.description);
        prompt_draw();
    }
}

pub fn cli_status(led: &fx::Settings, output: crate::led::RGB8, leds: &ExtLeds) {
    prompt_clear();
    crate::con_print!("  {BOLD}LED{RST}  ");
    swatch(led.pattern(), 16);
    crate::con_println!(
        "  {BOLD}{}{RST} {GRAY}·{RST} {} {GRAY}·{RST} cycle {} {GRAY}·{RST} {}%",
        led.pattern().name,
        Freq(led.freq_hz),
        Period(led.cycle_ms),
        led.brightness
    );
    prompt_draw();
    let mut keys: heapless::String<96> = heapless::String::new();
    for (i, c) in led.pattern().keys.iter().enumerate() {
        let _ = core::fmt::Write::write_fmt(
            &mut keys,
            format_args!("{}#{:02X}{:02X}{:02X}", if i == 0 { "" } else { " " }, c.r, c.g, c.b),
        );
    }
    out!("       {GRAY}keys{RST}  {keys}");
    out!(
        "       {GRAY}out{RST}   #{:02X}{:02X}{:02X}  {GRAY}(sent to the LED now, after brightness + gamma){RST}",
        output.r,
        output.g,
        output.b
    );
    for i in 0..2 {
        out!("  {BOLD}LED{}{RST} {GRAY}GPIO{}{RST}  {}", i + 1, leds.pins[i], leds.modes[i]);
    }
}
