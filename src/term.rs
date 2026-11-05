//! Colored terminal output (USB console, shown by `espflash monitor`).

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, Ordering};

use critical_section::Mutex;
use esp_println::print;

use crate::{
    cli,
    config::{self, NodeConfig, shared},
    ext_leds::ExtLeds,
    fx,
    protocol::{Edge, Frame, ParseError, Payload},
};

/// Prints one line above the CLI prompt: erase the prompt line, print, redraw the prompt.
macro_rules! out {
    ($($arg:tt)*) => {{
        $crate::term::prompt_clear();
        esp_println::println!($($arg)*);
        $crate::term::prompt_draw();
    }};
}

static PROMPT_ON: AtomicBool = AtomicBool::new(false);
static PROMPT: Mutex<RefCell<heapless::String<{ crate::cli::MAX_INPUT }>>> =
    Mutex::new(RefCell::new(heapless::String::new()));

pub fn prompt_enable() {
    PROMPT_ON.store(true, Ordering::Relaxed);
    prompt_draw();
}

#[doc(hidden)]
pub fn prompt_clear() {
    if PROMPT_ON.load(Ordering::Relaxed) {
        print!("\r\x1b[2K");
    }
}

#[doc(hidden)]
pub fn prompt_draw() {
    if PROMPT_ON.load(Ordering::Relaxed) {
        // Copy, then print outside the critical section: printing can take a while,
        // and interrupts (UART RX at 5 Mbaud) must keep running meanwhile.
        let text = critical_section::with(|cs| PROMPT.borrow_ref(cs).clone());
        print!("\x1b[1;35mcli ›\x1b[0m {text}");
    }
}

/// Replaces the text being typed and redraws the prompt line.
pub fn prompt_set(text: &str) {
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

/// `5000000` -> `5 Mbaud`, `115200` -> `115.2 kbaud`.
struct Baud(u32);

impl core::fmt::Display for Baud {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let b = self.0;
        if b.is_multiple_of(1_000_000) {
            write!(f, "{} Mbaud", b / 1_000_000)
        } else if b >= 1000 {
            write!(f, "{}.{} kbaud", b / 1000, b % 1000 / 100)
        } else {
            write!(f, "{b} baud")
        }
    }
}

/// The boot card: shared link settings, this node's config, build, chip, LEDs and CLI.
pub fn boot_card(cfg: &NodeConfig, chip: &ChipInfo) {
    let p = cfg.pins;
    out!();
    card_top("link · shared, must match on both nodes");
    card_row("uart", format_args!("UART1 · {} · 8N1 · RX by interrupt", Baud(shared::BAUD_RATE)));
    card_row("format", format_args!("$TYPE,node,seq,t_ms,…*XOR  (NMEA-style)"));
    card_row("frames", format_args!("DAT (periodic) · EVT + 64-bit key per edge"));

    card_split("node · this board's config");
    card_row("name", format_args!("{}", cfg.name));
    card_row("board", format_args!("ESP32-S31-Function-CoreBoard-1"));
    card_row("uart io", format_args!("TX GPIO{} · RX GPIO{}", p.uart_tx, p.uart_rx));
    card_row("DAT", format_args!("every {} ms", cfg.data_period_ms));
    card_row("status", format_args!("stats line every {} ms", cfg.status_period_ms));
    card_row("timeout", format_args!("link DOWN after {} peer periods", cfg.link_timeout_periods));
    card_row("button", format_args!("GPIO{} ↔ GND · IRQ on both edges", p.button));
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
    card_row("TRNG", format_args!("{a:08X} {b:08X} {c:08X}"));

    card_split("rgb led");
    let rgb = cfg.rgb;
    let pattern = &fx::PATTERNS[fx::find(rgb.pattern).unwrap_or(0)];
    card_swatch_row("pattern", pattern, format_args!("{}", pattern.name));
    card_row("timing", format_args!("{} Hz refresh · {} ms cycle · {}%", rgb.freq_hz, rgb.cycle_ms, rgb.brightness));
    card_row("driver", format_args!("WS2812 on GPIO60 · RMT channel 0"));

    card_split("breadboard leds");
    card_row("LED1", format_args!("GPIO{} · {}", p.led1, cfg.led1));
    card_row("LED2", format_args!("GPIO{} · {}", p.led2, cfg.led2));

    card_split("cli");
    card_row("port", format_args!("USB Serial/JTAG (this console)"));
    card_row("usage", format_args!("type `help` then Enter"));
    out!("{CYAN}╰{:─<BOX$}╯{RST}", "");
    out!();
}

fn button_state(pressed: bool) -> &'static str {
    if pressed { "\x1b[33m● pressed \x1b[0m" } else { "\x1b[90m○ released\x1b[0m" }
}

fn edge_label(edge: Edge) -> &'static str {
    match edge {
        Edge::Falling => "\x1b[1;33m▼ FALL\x1b[0m \x1b[90m(pressed) \x1b[0m",
        Edge::Rising => "\x1b[1;36m▲ RISE\x1b[0m \x1b[90m(released)\x1b[0m",
    }
}

/// One frame line, for both directions.
fn frame_line(now_ms: u64, tx: bool, frame: &Frame<'_>, extra: &str) {
    let (s, ms) = stamp(now_ms);
    let dir = if tx { "\x1b[36mTX ▶\x1b[0m" } else { "\x1b[32mRX ◀\x1b[0m" };
    let from = if tx { "" } else { frame.node };
    let (ps, pms) = stamp(u64::from(frame.t_ms));
    match frame.payload {
        Payload::Data { pressed, events, .. } => out!(
            "{GRAY}{s:>6}.{ms:03}{RST} {dir} {DAT_BADGE} DAT {RST} {GRAY}#{:05}{RST} {YELLOW}{from:<7}{RST} {GRAY}t={ps}.{pms:03}s{RST}  btn {}  {GRAY}events{RST} {events}{extra}",
            frame.seq,
            button_state(pressed),
        ),
        Payload::Event { edge, key } => out!(
            "{GRAY}{s:>6}.{ms:03}{RST} {dir} {EVT_BADGE} EVT {RST} {GRAY}#{:05}{RST} {YELLOW}{from:<7}{RST} {GRAY}t={ps}.{pms:03}s{RST}  {}  {GRAY}key{RST} {BOLD}{key:016X}{RST}{extra}",
            frame.seq,
            edge_label(edge),
        ),
    }
}

pub fn sent(now_ms: u64, frame: &Frame<'_>, raw_edges: Option<u32>) {
    match raw_edges {
        Some(n) if n > 1 => {
            let mut extra: heapless::String<32> = heapless::String::new();
            let _ = core::fmt::Write::write_fmt(&mut extra, format_args!("  {DIM}{n} edges, bounces filtered{RST}"));
            frame_line(now_ms, true, frame, &extra);
        }
        _ => frame_line(now_ms, true, frame, ""),
    }
}

pub fn received(now_ms: u64, frame: &Frame<'_>) {
    frame_line(now_ms, false, frame, "");
}

pub fn bounce_ignored(now_ms: u64, raw_edges: u32) {
    let (s, ms) = stamp(now_ms);
    out!("{GRAY}{s:>6}.{ms:03}   ·  bounce ignored ({raw_edges} edge(s), level unchanged){RST}");
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
    let raw = core::str::from_utf8(raw).unwrap_or("<non-UTF-8>");
    match err {
        ParseError::Checksum { expected, received } => out!(
            "{GRAY}{s:>6}.{ms:03}{RST} {RED}RX ✗  bad checksum{RST} {GRAY}(got {received:02X}, expected {expected:02X}){RST} {DIM}{raw}{RST}"
        ),
        ParseError::Malformed | ParseError::BadField => {
            out!("{GRAY}{s:>6}.{ms:03}{RST} {RED}RX ✗  invalid frame{RST} {DIM}{raw}{RST}")
        }
    }
}

pub fn uart_error(now_ms: u64, what: &str) {
    let (s, ms) = stamp(now_ms);
    out!("{GRAY}{s:>6}.{ms:03}{RST} {RED}UART ✗  {what}{RST}");
}

pub fn status(now_ms: u64, stats: &Stats, link_up: bool) {
    let (s, _) = stamp(now_ms);
    let link = if link_up { "\x1b[1;32m● link UP  \x1b[0m" } else { "\x1b[1;31m● link DOWN\x1b[0m" };
    out!(
        "{GRAY}──── {s:>4}s ── tx {} · rx {} · lost {} · bad {} ──{RST} {link} {GRAY}────{RST}",
        stats.tx, stats.rx, stats.lost, stats.bad
    );
}

/// Prints a `width`-character color swatch of `pattern` and returns its width.
fn swatch(pattern: &fx::Pattern, width: usize) -> usize {
    for i in 0..width {
        // Samples along the loop, so the swatch shows the actual fade.
        let c = fx::color_at(pattern.keys, i as f32 / width as f32, 1.0);
        print!("\x1b[38;2;{};{};{}m█", c.r, c.g, c.b);
    }
    print!("{RST}");
    width
}

/// Card row whose value starts with a color swatch of the current pattern.
fn card_swatch_row(label: &str, pattern: &fx::Pattern, args: core::fmt::Arguments<'_>) {
    let mut value: heapless::String<128> = heapless::String::new();
    let _ = core::fmt::Write::write_fmt(&mut value, args);
    prompt_clear();
    print!("{CYAN}│{RST}  {GRAY}{label:<LABEL$}{RST}");
    let used = swatch(pattern, 16);
    esp_println::println!(" {value:<w$}{CYAN}│{RST}", w = BOX - 2 - LABEL - used - 1);
    prompt_draw();
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
    let Some(topic) = topic else {
        for (group, title) in [
            (cli::Group::Settings, "settings"),
            (cli::Group::Shortcuts, "shortcuts"),
            (cli::Group::Info, "info"),
        ] {
            out!("  {BOLD}{title}{RST}");
            for c in cli::COMMANDS.iter().filter(|c| c.group == group) {
                out!("    {CYAN}{:<20}{RST}{GRAY}{}{RST}", c.usage, c.summary);
            }
        }
        out!("  {GRAY}`help <command>` or `help <key>` for details. Backspace edits, Ctrl+U clears,{RST}");
        out!("  {GRAY}Ctrl+C quits espflash.{RST}");
        return;
    };

    if let Some(c) = cli::find_help(topic) {
        out!("  {BOLD}{}{RST} {GRAY}—{RST} {}", c.name, c.summary);
        out!("    {GRAY}usage{RST}     {CYAN}{}{RST}", c.usage);
        if !c.aliases.is_empty() {
            let mut aliases: heapless::String<32> = heapless::String::new();
            for (i, a) in c.aliases.iter().enumerate() {
                let _ = aliases.push_str(if i == 0 { "" } else { ", " });
                let _ = aliases.push_str(a);
            }
            out!("    {GRAY}aliases{RST}   {aliases}");
        }
        for line in c.details {
            out!("    {line}");
        }
        for (i, ex) in c.examples.iter().enumerate() {
            let label = if i == 0 { "examples" } else { "" };
            out!("    {GRAY}{label:<10}{RST}{CYAN}{ex}{RST}");
        }
        if c.name == "set" {
            for (row, chunk) in config::KEYS.chunks(4).enumerate() {
                let mut keys: heapless::String<96> = heapless::String::new();
                for (i, (name, _)) in chunk.iter().enumerate() {
                    let _ = keys.push_str(if i == 0 { "" } else { " · " });
                    let _ = keys.push_str(name);
                }
                let label = if row == 0 { "keys" } else { "" };
                out!("    {GRAY}{label:<10}{RST}{keys}");
            }
        }
    } else if let Some(k) = config::Key::from_name(topic) {
        let (what, values) = k.describe();
        out!("  {BOLD}{topic}{RST} {GRAY}—{RST} {what}");
        out!("    {GRAY}values{RST}    {values}");
        out!("    {GRAY}current{RST}   {}   {GRAY}(build {}){RST}", cfg.value(k), defaults.value(k));
        out!("    {GRAY}change{RST}    {CYAN}set {topic} <value>{RST}");
    } else if config::READ_ONLY.contains(&topic) {
        out!("  {BOLD}{topic}{RST} {GRAY}—{RST} read-only: set in src/bin/uart_node_*.rs or config::shared, then rebuild");
    } else {
        cli_error(format_args!("no help for '{topic}' (try `help`)"));
    }
}

// --- Verbose output ----------------------------------------------------------

/// The exact line on the wire, its size, and (RX) time since the peer's previous frame.
pub fn wire(line: &[u8], delta_ms: Option<u64>) {
    let text = core::str::from_utf8(line).unwrap_or("<non-UTF-8>").trim_end();
    // Size on the wire, including the `\n` terminator (stripped from received lines).
    let size = text.len() + 1;
    match delta_ms {
        Some(d) => out!("{GRAY}             wire {DIM}{text}{RST}{GRAY}  {size} B  +{d} ms{RST}"),
        None => out!("{GRAY}             wire {DIM}{text}{RST}{GRAY}  {size} B{RST}"),
    }
}

pub fn link_counters(c: &crate::link::Counters, peer_period_ms: Option<u32>) {
    out!(
        "{GRAY}            bytes tx {} · rx {} · RX buffer peak {}/{} · peer DAT period {}{RST}",
        c.tx_bytes,
        c.rx_bytes,
        c.rx_peak,
        c.rx_capacity,
        peer_period_ms.map_or(Ms(u32::MAX), Ms)
    );
}

/// `2500 ms`, or `?` when unknown (u32::MAX).
struct Ms(u32);

impl core::fmt::Display for Ms {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 == u32::MAX { f.write_str("?") } else { write!(f, "{} ms", self.0) }
    }
}

/// `config`: every setting, read-only ones first, changed ones marked.
pub fn cli_config(cfg: &NodeConfig, defaults: &NodeConfig) {
    let p = cfg.pins;
    out!("  {BOLD}{:<22}{:<16}{RST}{GRAY}build value{RST}", "key", "value");
    let fixed: [(&str, u32); 7] = [
        ("name", 0),
        ("pins.uart_tx", p.uart_tx.into()),
        ("pins.uart_rx", p.uart_rx.into()),
        ("pins.button", p.button.into()),
        ("pins.led1", p.led1.into()),
        ("pins.led2", p.led2.into()),
        ("shared.baud_rate", shared::BAUD_RATE),
    ];
    for (key, n) in fixed {
        if key == "name" {
            out!("  {GRAY}{key:<22}{:<16}read-only (rebuild){RST}", cfg.name);
        } else {
            out!("  {GRAY}{key:<22}{n:<16}read-only (rebuild){RST}");
        }
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
        print!("  {mark} {GRAY}{}{RST} {BOLD}{:<7}{RST} ", i + 1, p.name);
        swatch(p, 16);
        esp_println::println!("  {GRAY}{}{RST}", p.description);
        prompt_draw();
    }
}

pub fn cli_status(led: &fx::Settings, leds: &ExtLeds) {
    prompt_clear();
    print!("  {BOLD}LED{RST}  ");
    swatch(led.pattern(), 16);
    esp_println::println!(
        "  {BOLD}{}{RST} {GRAY}·{RST} {} Hz {GRAY}·{RST} cycle {} ms {GRAY}·{RST} {}%",
        led.pattern().name,
        led.freq_hz,
        led.cycle_ms,
        led.brightness
    );
    prompt_draw();
    for i in 0..2 {
        out!("  {BOLD}LED{}{RST} {GRAY}GPIO{}{RST}  {}", i + 1, leds.pins[i], leds.modes[i]);
    }
}
