//! Colored terminal output (USB console, shown by `espflash monitor`).

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, Ordering};

use critical_section::Mutex;
use esp_println::print;

use crate::{
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
        critical_section::with(|cs| print!("\x1b[1;35mcli ›\x1b[0m {}", PROMPT.borrow_ref(cs)));
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

fn box_row(color: &str, args: core::fmt::Arguments<'_>) {
    let mut text: heapless::String<128> = heapless::String::new();
    let _ = core::fmt::Write::write_fmt(&mut text, args);
    // `{:<w$}` pads by character count, so the right border stays aligned.
    out!("{CYAN}│{RST}  {color}{text:<w$}{RST}{CYAN}│{RST}", w = BOX - 2);
}

pub fn banner(node: &str, baud: u32, data_period_ms: u64) {
    out!();
    out!("{CYAN}╭{:─<BOX$}╮{RST}", "");
    box_row(BOLD, format_args!("ESP32-S31 UART link  ·  {node}"));
    box_row(GRAY, format_args!("UART1 {baud} 8N1  ·  TX GPIO2  ·  RX GPIO3"));
    box_row(GRAY, format_args!("button GPIO4 (jumper to GND)  ·  IRQ on both edges"));
    box_row(GRAY, format_args!("DAT every {data_period_ms} ms  ·  EVT + random key on each edge"));
    out!("{CYAN}╰{:─<BOX$}╯{RST}", "");
}

/// Hardware facts read at boot, shown in the "chip" card.
pub struct ChipInfo {
    pub rev_major: u8,
    pub rev_minor: u8,
    pub cpu_mhz: u32,
    pub mac: [u8; 6],
    pub reset: heapless::String<32>,
    pub rng_samples: [u32; 3],
    pub led: fx::Settings,
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

fn card_row(label: &str, args: core::fmt::Arguments<'_>) {
    let mut value: heapless::String<128> = heapless::String::new();
    let _ = core::fmt::Write::write_fmt(&mut value, args);
    out!(
        "{CYAN}│{RST}  {GRAY}{label:<LABEL$}{RST}{value:<w$}{CYAN}│{RST}",
        w = BOX - 2 - LABEL
    );
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

/// Build and hardware description, printed right after the banner.
pub fn build_card(chip: &ChipInfo) {
    card_top("build");
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
    let led = chip.led;
    card_swatch_row("LED", &led, format_args!("{} · {} Hz · cycle {} ms · {}%", led.pattern().name, led.freq_hz, led.cycle_ms, led.brightness));
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
        Payload::Data { pressed, events } => out!(
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

/// Prints a 16-character color swatch of `pattern` and returns its width.
fn swatch(pattern: &fx::Pattern) -> usize {
    for i in 0..16 {
        // 16 samples along the loop, so the swatch shows the actual fade.
        let c = fx::color_at(pattern.keys, i as f32 / 16.0, 1.0);
        print!("\x1b[38;2;{};{};{}m█", c.r, c.g, c.b);
    }
    print!("{RST}");
    16
}

/// Card row whose value starts with a color swatch of the current pattern.
fn card_swatch_row(label: &str, led: &fx::Settings, args: core::fmt::Arguments<'_>) {
    let mut value: heapless::String<128> = heapless::String::new();
    let _ = core::fmt::Write::write_fmt(&mut value, args);
    prompt_clear();
    print!("{CYAN}│{RST}  {GRAY}{label:<LABEL$}{RST}");
    let used = swatch(led.pattern());
    esp_println::println!(" {value:<w$}{CYAN}│{RST}", w = BOX - 2 - LABEL - used - 1);
    prompt_draw();
}

pub fn cli_hint() {
    out!("{GRAY}CLI on this USB port: type {RST}{BOLD}help{RST}{GRAY} then Enter.{RST}");
    out!();
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

pub fn cli_help() {
    out!("  {BOLD}commands{RST}");
    for (cmd, what) in [
        ("list", "show the LED patterns"),
        ("pattern <name|1-5>", "select a pattern  (alias p)"),
        ("next", "next pattern  (alias n)"),
        ("freq <1-200>", "LED refresh rate in Hz  (alias f)"),
        ("cycle <200-60000>", "duration of one fade loop in ms  (alias c)"),
        ("bright <0-100>", "brightness in %  (alias b)"),
        ("status", "current LED settings  (alias st)"),
        ("help", "this list"),
    ] {
        out!("    {CYAN}{cmd:<20}{RST}{GRAY}{what}{RST}");
    }
    out!("  {GRAY}Backspace edits, Ctrl+U clears the line. Ctrl+C quits espflash.{RST}");
}

pub fn cli_list(current: usize) {
    out!("  {BOLD}patterns{RST}");
    for (i, p) in fx::PATTERNS.iter().enumerate() {
        let mark = if i == current { "\x1b[1;35m▶\x1b[0m" } else { " " };
        prompt_clear();
        print!("  {mark} {GRAY}{}{RST} {BOLD}{:<7}{RST} ", i + 1, p.name);
        swatch(p);
        esp_println::println!("  {GRAY}{}{RST}", p.description);
        prompt_draw();
    }
}

pub fn cli_status(led: &fx::Settings) {
    prompt_clear();
    print!("  {BOLD}LED{RST}  ");
    swatch(led.pattern());
    esp_println::println!(
        "  {BOLD}{}{RST} {GRAY}·{RST} {} Hz {GRAY}·{RST} cycle {} ms {GRAY}·{RST} {}%",
        led.pattern().name,
        led.freq_hz,
        led.cycle_ms,
        led.brightness
    );
    prompt_draw();
}
