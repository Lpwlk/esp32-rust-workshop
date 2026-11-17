//! Colored terminal output (USB console, shown by `espflash monitor`). Same look as the UART
//! firmware's; only what concerns the link differs (Ethernet instead of baud rate).

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use critical_section::Mutex;

use workshop_core::{
    burst::{BurstSummary, Throughput},
    config::{self, Link, NodeConfig, Trace, TraceView},
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
static PROMPT_TEXT: Mutex<RefCell<heapless::String<{ crate::cli::MAX_INPUT }>>> =
    Mutex::new(RefCell::new(heapless::String::new()));
/// Question shown instead of `cli` while the `burst` wizard runs.
static PROMPT_LABEL: Mutex<RefCell<Option<heapless::String<48>>>> = Mutex::new(RefCell::new(None));

/// The current `verbose` level (`workshop_core::config::Verbosity` as a number).
static VERBOSITY: AtomicU8 = AtomicU8::new(0);
/// The live line: the last frame event, redrawn in place just above the prompt (verbosity 1).
static LIVE: Mutex<RefCell<Option<heapless::String<512>>>> = Mutex::new(RefCell::new(None));
/// The traffic dashboard: up to four rows between the live line and the prompt, redrawn in place with them.
static DASH: Mutex<RefCell<heapless::Vec<heapless::String<400>, { config::DASH_MAX_ROWS }>>> =
    Mutex::new(RefCell::new(heapless::Vec::new()));

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

/// Frames go to the live line: verbosity 1, and the serial console is on (an RTT viewer has no
/// editable prompt to keep the line under, so it gets every line).
fn live_mode() -> bool {
    VERBOSITY.load(Ordering::Relaxed) == 1 && crate::rtt_console::serial_on()
}

/// Verbosity 0: only replies to what you typed, and faults, are printed.
fn silent() -> bool {
    VERBOSITY.load(Ordering::Relaxed) == 0
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

/// Rows the next [`prompt_draw`] is to scroll the screen down by, after the block above the prompt got shorter.
///
/// A shorter block pulls the prompt up, and what the taller block had pushed into the terminal's history doesn't come
/// back: the prompt would float above a few blank lines and the output above would look cut. Scrolling the screen
/// down by the difference (`ESC[nT`, then the cursor back down) puts the prompt where it was, at the bottom, with the
/// output in the same place relative to it. It is only done once enough has been printed for the screen to be full:
/// on a fresh terminal there is nothing to put back and the lines would only be pushed down.
static UNSCROLL: AtomicUsize = AtomicUsize::new(0);

/// Lines printed since boot above which the screen is taken to be full (no terminal has more rows than this).
const SCREEN_FULL_LINES: u32 = 100;

/// How many [`Block`]s are open.
static BLOCK_DEPTH: AtomicU8 = AtomicU8::new(0);

/// A reply of several lines (`config`, `help`, the boot card...): the prompt block is erased once when it opens
/// and drawn once when it drops, and the lines in between are printed bare.
///
/// Without it every line would erase and redraw the live line and the dashboard around it, which is a few
/// hundred bytes more than the line itself: a 33-line table then overflowed the 8 KB console pipe (now 16 KB) and lost its
/// end. [`prompt_clear`] and [`prompt_draw`] do nothing while a block is open.
pub struct Block;

impl Block {
    /// Opens a block.
    pub fn new() -> Block {
        prompt_clear();
        BLOCK_DEPTH.fetch_add(1, Ordering::Relaxed);
        Block
    }
}

impl Default for Block {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        BLOCK_DEPTH.fetch_sub(1, Ordering::Relaxed);
        prompt_draw();
    }
}

/// Whether the terminal is between `ESC[?2026h` and `ESC[?2026l`: an erase has been sent and its redraw not yet.
static SYNC_OPEN: AtomicBool = AtomicBool::new(false);

/// Erasing the prompt block and drawing it again takes a few milliseconds on the USB link, and a terminal that
/// paints in between shows the block empty: a flicker. "Synchronized output" (DEC mode 2026) tells the terminal to
/// hold what it receives and paint it all at once at the end; the cursor is hidden meanwhile so it doesn't jump
/// up and down the block. A terminal that doesn't know the mode ignores both.
fn sync_begin() {
    if !SYNC_OPEN.swap(true, Ordering::Relaxed) {
        raw(format_args!("\x1b[?2026h\x1b[?25l"));
    }
}

/// Closes what [`sync_begin`] opened. Always sent when it was opened, even if the prompt was switched off in
/// between (`set console rtt`): the terminal would otherwise wait for its own time-out before painting again.
fn sync_end() {
    if SYNC_OPEN.swap(false, Ordering::Relaxed) {
        raw(format_args!("\x1b[?25h\x1b[?2026l"));
    }
}

#[doc(hidden)]
pub fn prompt_clear() {
    if BLOCK_DEPTH.load(Ordering::Relaxed) > 0 {
        return;
    }
    if PROMPT_ON.load(Ordering::Relaxed) && crate::rtt_console::serial_on() {
        sync_begin();
        // The prompt line; then each row above it that is ours (the dashboard, the live line): up one row, erase.
        let rows = critical_section::with(|cs| usize::from(LIVE.borrow_ref(cs).is_some()) + DASH.borrow_ref(cs).len());
        raw(format_args!("\r\x1b[2K"));
        for _ in 0..rows {
            raw(format_args!("\x1b[1A\r\x1b[2K"));
        }
    }
}

#[doc(hidden)]
pub fn prompt_draw() {
    if BLOCK_DEPTH.load(Ordering::Relaxed) > 0 {
        return;
    }
    if PROMPT_ON.load(Ordering::Relaxed) && crate::rtt_console::serial_on() {
        // Copy, then print outside the critical section: printing can take a while,
        // and interrupts (buttons, the network) must keep running meanwhile.
        let (text, label) =
            critical_section::with(|cs| (PROMPT_TEXT.borrow_ref(cs).clone(), PROMPT_LABEL.borrow_ref(cs).clone()));
        // The live line first, above the prompt; no auto-wrap, so a long line is cut, not wrapped
        // onto a second row (which the erase above wouldn't know about).
        if let Some(live) = critical_section::with(|cs| LIVE.borrow_ref(cs).clone()) {
            raw(format_args!("\x1b[?7l{live}\x1b[?7h\n"));
        }
        // The dashboard right above the prompt.
        // One row at a time: the whole block can be 5 KB, too much to copy onto the stack.
        let mut i = 0;
        while let Some(row) = critical_section::with(|cs| DASH.borrow_ref(cs).get(i).cloned()) {
            raw(format_args!("\x1b[?7l{row}\x1b[?7h\n"));
            i += 1;
        }
        match label {
            Some(q) => crate::console::print(format_args!("{BOLD}{WARN}{q} ›{RST} {text}")),
            None => crate::console::print(format_args!("{BOLD}{PROMPT}cli ›{RST} {text}")),
        }
        // Put the terminal cursor where the editor's cursor is (ESC [ n D = n left).
        let back = text.len().saturating_sub(PROMPT_CURSOR.load(Ordering::Relaxed));
        if back > 0 {
            crate::console::print(format_args!("\x1b[{back}D"));
        }
        // The block got shorter: put the prompt back at the bottom (see [`UNSCROLL`]).
        let k = UNSCROLL.swap(0, Ordering::Relaxed);
        if k > 0 && crate::console::lines_printed() >= SCREEN_FULL_LINES {
            raw(format_args!("\x1b[{k}T\x1b[{k}B"));
        }
    }
    sync_end();
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
        let mut p = PROMPT_TEXT.borrow_ref_mut(cs);
        p.clear();
        let _ = p.push_str(text);
    });
    prompt_clear();
    prompt_draw();
}

use workshop_core::theme::{BOLD, FAINT, RST, roles::*};

// --- Pieces that carry their own color ------------------------------------------------------------------

/// `TX ▶` or `RX ◀`, in the direction's color.
struct Arrow(bool);

impl core::fmt::Display for Arrow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 { write!(f, "{TX}TX ▶{RST}") } else { write!(f, "{RX}RX ◀{RST}") }
    }
}

/// `← TX` or `← RX`: which direction the previous `DAT` went.
struct Previous(Dir);

impl core::fmt::Display for Previous {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Dir::Tx => write!(f, "{TX}← TX{RST}"),
            Dir::Rx => write!(f, "{RX}← RX{RST}"),
        }
    }
}

/// A bold marker, in the `ok` color when `.0` is true (the first text), else in the `error` color (the second):
/// `● UP` / `● DOWN`, `✓` / `✗`.
struct Mark(bool, &'static str, &'static str);

impl core::fmt::Display for Mark {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 { write!(f, "{BOLD}{OK}{}{RST}", self.1) } else { write!(f, "{BOLD}{ERROR}{}{RST}", self.2) }
    }
}

/// The button state in a `DAT` line.
struct Pressed(bool);

impl core::fmt::Display for Pressed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 { write!(f, "{WARN}● pressed {RST}") } else { write!(f, "{DIM}○ released{RST}") }
    }
}

/// The edge in an `EVT` line.
struct EdgeMark(Edge);

impl core::fmt::Display for EdgeMark {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Edge::Falling => write!(f, "{BOLD}{WARN}▼ press  {RST}"),
            Edge::Rising => write!(f, "{BOLD}{ACCENT}▲ release{RST}"),
        }
    }
}

/// The `▶` before the pattern in use (`list`).
struct Cursor(bool);

impl core::fmt::Display for Cursor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 { write!(f, "{BOLD}{PROMPT}▶{RST}") } else { f.write_str(" ") }
    }
}

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
    out!("{ACCENT}╭─ {BOLD}{title}{RST}{ACCENT} {:─<w$}╮{RST}", "", w = BOX - 3 - title.chars().count());
}

fn card_split(title: &str) {
    out!("{ACCENT}├─ {BOLD}{title}{RST}{ACCENT} {:─<w$}┤{RST}", "", w = BOX - 3 - title.chars().count());
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
    out!("{ACCENT}│{RST}  {DIM}{label:<LABEL$}{RST}{value:<w$}{ACCENT}│{RST}");
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
    let _block = Block::new();
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
    out!("{ACCENT}╰{:─<BOX$}╯{RST}", "");
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
    if silent() {
        return;
    }
    let extra = extra.trim_start();
    use core::fmt::Write;
    let (s, ms) = stamp(now_ms);
    let dir = Arrow(tx);
    let from = if tx { "" } else { frame.node };
    let badge: &dyn core::fmt::Display = match frame.payload {
        Payload::Data { .. } => &DAT,
        Payload::Event { .. } => &EVT,
        Payload::Baud { .. } | Payload::Echo { .. } => &LNK,
        Payload::Burst { .. } => &"",
    };
    let mut body: heapless::String<96> = heapless::String::new();
    let kind = match frame.payload {
        Payload::Data { pressed, events, .. } => {
            let btn = Pressed(pressed);
            let _ = write!(body, "{DIM}btn{RST} {btn}  {DIM}event{RST} {events:>5}");
            "DAT"
        }
        Payload::Event { edge, key } => {
            let edge = EdgeMark(edge);
            let _ = write!(body, "{edge}  {DIM}key{RST} {BOLD}{key:016X}{RST}");
            "EVT"
        }
        Payload::Baud { baud, ack } => {
            // A UART-link frame: an Ethernet node only shows it if the peer sends one by mistake.
            let _ = write!(body, "{BOLD}{}{RST} {baud} baud", if ack { "ACK" } else { "REQ" });
            "BAU"
        }
        Payload::Echo { echo_seq } => {
            let _ = write!(body, "{DIM}echo of{RST} #{echo_seq:05}");
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
        "{TIME}{s:>6}.{ms:03}{RST} {dir} {badge} {kind} {RST} {DIM}#{:05}{RST} {NODE}{from:<NODE_W$}{RST} {SEP}│{RST} {body}",
        frame.seq
    );
    if !extra.is_empty() {
        let _ = write!(line, " {SEP}│{RST} {extra}");
    }
    if live {
        // The counters stay on the live line, so nothing needs the periodic stats line.
        let _ = write!(line, "  {DIM}│ tx {} rx {}{RST} ", stats.tx, stats.rx);
        if stats.lost > 0 || stats.bad > 0 {
            let _ = write!(line, "{ERROR}lost {} bad {}{RST}", stats.lost, stats.bad);
        } else {
            let _ = write!(line, "{DIM}lost 0{RST}");
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
    let prev = Previous(d.prev);
    let us = d.delta_us.unsigned_abs();
    let mut num: heapless::String<16> = heapless::String::new();
    let sign = if d.delta_us < 0 { "-" } else { "" };
    let _ = core::fmt::Write::write_fmt(&mut num, format_args!("{sign}{}.{:03}", us / 1000, us % 1000));
    let _ = core::fmt::Write::write_fmt(extra, format_args!("  {DIM}Δ{RST} {BOLD}{num:>9}{RST} {DIM}ms{RST} {prev}"));
}

/// A sent frame; `button`: for an `EVT`, which button and the interrupts behind
/// it; `delta`: for a `DAT`.
pub fn sent(now_ms: u64, frame: &Frame<'_>, button: Option<(&str, u32)>, delta: Option<DatDelta>, stats: &Stats) {
    let mut extra: heapless::String<96> = heapless::String::new();
    if let Some(d) = delta {
        push_delta(&mut extra, d);
    }
    if let Some((name, n)) = button {
        let _ = core::fmt::Write::write_fmt(&mut extra, format_args!("  {ACCENT}{name}{RST}"));
        if n > 1 {
            let _ = core::fmt::Write::write_fmt(&mut extra, format_args!("  {FAINT}{n} edges{RST}"));
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
        let _ = core::fmt::Write::write_fmt(&mut extra, format_args!("  {DIM}rtt{RST} {BOLD}{}{RST}", Us(rtt)));
    }
    frame_line(now_ms, false, frame, &extra, stats);
}

pub fn bounce_ignored(now_ms: u64, button: &str, raw_edges: u32) {
    let (s, ms) = stamp(now_ms);
    out!("{TIME}{s:>6}.{ms:03}   ·  {button}: bounce ignored ({raw_edges} edge(s), level unchanged){RST}");
}

pub fn lost(now_ms: u64, count: u32) {
    if silent() {
        return;
    }
    let (s, ms) = stamp(now_ms);
    out!("{TIME}{s:>6}.{ms:03}{RST} {ERROR}RX ⚠  {count} frame(s) lost{RST}");
}

pub fn peer_restarted(now_ms: u64, node: &str) {
    if silent() {
        return;
    }
    let (s, ms) = stamp(now_ms);
    out!("{TIME}{s:>6}.{ms:03}{RST} {WARN}RX ↻  {node} restarted (sequence reset){RST}");
}

pub fn rx_error(now_ms: u64, err: ParseError, raw: &[u8]) {
    let (s, ms) = stamp(now_ms);
    match err {
        ParseError::Checksum { expected, received } => out!(
            "{TIME}{s:>6}.{ms:03}{RST} {ERROR}RX ✗  bad checksum{RST} {DIM}(got 0x{received:02X}, expected 0x{expected:02X}){RST} {FAINT}{}{RST}",
            Raw(raw)
        ),
        ParseError::Malformed | ParseError::BadField => {
            out!("{TIME}{s:>6}.{ms:03}{RST} {ERROR}RX ✗  invalid frame{RST} {FAINT}{}{RST}", Raw(raw))
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
    out!("{TIME}{s:>6}.{ms:03}{RST} {ERROR}NET ✗  {what}{RST}");
}

/// Microseconds as milliseconds with two decimals: `4082` -> `4.08`.
struct Ms(u64);

impl core::fmt::Display for Ms {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{:02}", self.0 / 1000, self.0 % 1000 / 10)
    }
}

/// `text` right-aligned in `width` columns (a `Display` value formatted first, so padding applies
/// to types that ignore it).
fn cell<const N: usize>(value: impl core::fmt::Display) -> heapless::String<N> {
    let mut s = heapless::String::new();
    let _ = core::fmt::Write::write_fmt(&mut s, format_args!("{value}"));
    s
}

pub fn status(now_ms: u64, stats: &Stats, link_up: bool, phy: PhyLink, rtt: Option<RttStats>) {
    use core::fmt::Write;
    let (s, _) = stamp(now_ms);
    // `─  485s ─ tx   242 · rx   242 · lost  0 · bad  0 ─ ● UP   1G   ─ rtt  4.08 /  8.55 / 1005.62 ms`
    // Fixed-width fields: successive lines line up, and nothing shifts when a counter grows a digit.
    let link = Mark(link_up, "● UP  ", "● DOWN");
    // `1G`, `100M`, `-` (no link): the long form is in `link` and the boot card.
    let speed = match phy.mbps {
        0 => "-",
        10 => "10M",
        100 => "100M",
        _ => "1G",
    };
    let mut line: heapless::String<256> = heapless::String::new();
    let _ = write!(
        line,
        "{DIM}─ {s:>4}s ─ tx {:>5} · rx {:>5} · lost {:>2} · bad {:>2} ─{RST} {link} {LINK}{:<4}{RST}",
        stats.tx, stats.rx, stats.lost, stats.bad, speed
    );
    if let Some(r) = rtt {
        // Three numbers with the unit once, right-aligned so the columns line up from line to line.
        let (min, avg, max): (heapless::String<12>, heapless::String<12>, heapless::String<12>) =
            (cell(Ms(r.min_us)), cell(Ms(r.avg_us)), cell(Ms(r.max_us)));
        let _ = write!(
            line,
            " {DIM}─ rtt{RST} {BOLD}{:>5}{RST} {DIM}/{RST} {:>5} {DIM}/{RST} {:>7} {DIM}ms{RST}",
            &*min, &*avg, &*max
        );
    }
    out!("{line}");
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
    if silent() {
        return;
    }
    let (s, ms) = stamp(now_ms);
    out!("{TIME}{s:>6}.{ms:03}{RST} {LINK}LINK ⇄  {args}{RST}");
}

/// `link`: the cable, what the PHY negotiated, the addresses, the transport and the counters.
pub fn link_info(cfg: &NodeConfig, phy: PhyLink, connected: bool, c: &crate::link::Counters, peer_alive: bool) {
    let _block = Block::new();
    let n = cfg.net();
    let cable = if phy.up {
        let duplex = if phy.full_duplex { "full" } else { "half" };
        let mut s: heapless::String<48> = heapless::String::new();
        let _ = core::fmt::Write::write_fmt(
            &mut s,
            format_args!("{} · {} · {duplex} duplex", Mark(true, "● up", ""), Speed(phy.mbps)),
        );
        s
    } else {
        let mut s: heapless::String<48> = heapless::String::new();
        let _ =
            core::fmt::Write::write_fmt(&mut s, format_args!("{} · no cable or no partner", Mark(false, "", "● down")));
        s
    };
    out!("  {BOLD}cable{RST}     {SEP}│{RST} {cable}");
    out!(
        "  {BOLD}capacity{RST}  {SEP}│{RST} {DIM}{} max allowed · {} frame rate ceiling is the CPU, not the wire{RST}",
        Speed(n.max_speed),
        cfg.net().transport
    );
    out!("  {BOLD}address{RST}   {SEP}│{RST} {}/{} {DIM}→ peer {}{RST}", Addr(n.address), n.prefix_len, Addr(n.peer));
    let state = match (n.transport, connected) {
        (config::Transport::Udp, _) => "no connection (datagrams)",
        (config::Transport::Tcp, true) => "connected",
        (config::Transport::Tcp, false) => "not connected",
    };
    out!("  {BOLD}transport{RST} {SEP}│{RST} {} port {} {DIM}· {state}{RST}", n.transport, n.port);
    out!("  {BOLD}peer{RST}      {SEP}│{RST} {}", Mark(peer_alive, "● frames arriving", "● silent"));
    out!(
        "  {BOLD}counters{RST}  {SEP}│{RST} {DIM}tx {} frames / {} · rx {} frames / {} · rx queue overflow {} · tcp connects {} lost {}{RST}",
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
        out!("{TIME}{s:>6}.{ms:03}{RST} {TX}TX ▶{RST} {BOLD}burst #{id}{RST}: {count} frames back to back…");
    } else {
        out!(
            "{TIME}{s:>6}.{ms:03}{RST} {TX}TX ▶{RST} {BOLD}burst #{id}{RST}: {count} frames at {} (one every {})…",
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
        "{TIME}{s:>6}.{ms:03}{RST} {TX}TX ▶{RST} {BOLD}burst #{id}{RST} sent: {frames} frames, {}",
        throughput(t, capacity)
    );
}

pub fn burst_received(now_ms: u64, node: &str, id: u32, r: &BurstSummary, capacity: u64) {
    let (s, ms) = stamp(now_ms);
    let verdict = Mark(r.lost == 0 && r.bad == 0, "✓", "✗");
    out!(
        "{TIME}{s:>6}.{ms:03}{RST} {RX}RX ◀{RST} {verdict} {BOLD}burst #{id}{RST} from {NODE}{node}{RST}: {}/{} frames · {} lost · {} bad",
        r.received,
        r.count,
        r.lost,
        r.bad
    );
    out!("{DIM}                    {RST}{}", throughput(&r.throughput, capacity));
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
    crate::con_print!("{ACCENT}│{RST}  {DIM}{label:<LABEL$}{RST}");
    let used = swatch(pattern, 16);
    crate::con_println!(" {value:<w$}{ACCENT}│{RST}", w = BOX - 2 - LABEL - used - 1);
    prompt_draw();
}

/// Echo of an answer to a `burst` question (empty = the default was taken).
pub fn cli_answer(question: &str, answer: &str) {
    let answer = if answer.is_empty() { "(default)" } else { answer };
    out!("{BOLD}{WARN}{question} ›{RST} {answer}");
}

pub fn cli_echo(line: &str) {
    out!("{BOLD}{PROMPT}›{RST} {line}");
}

pub fn cli_error(args: core::fmt::Arguments<'_>) {
    out!("  {ERROR}✗ {args}{RST}");
}

pub fn cli_ok(args: core::fmt::Arguments<'_>) {
    out!("  {OK}✓{RST} {args}");
}

/// `help` (no topic): every command, grouped, from the table in `cli`.
/// `help <topic>`: details on a command, or on a setting key.
pub fn cli_help(t: &str, cfg: &NodeConfig, defaults: &NodeConfig) {
    if let Some(c) = cli::find_help(t) {
        help_command(c);
    } else if let Some(k) = config::Key::from_name(t) {
        help_key(t, k, cfg, defaults);
    } else if cfg.read_only().contains(&t) {
        out!("  {BOLD}{t}{RST} {DIM}—{RST} read-only: set in firmware/ethernet/presets/*.toml, then rebuild");
    } else {
        cli_error(format_args!("no help for '{t}' (try `help`)"));
    }
}

pub const HELP_GROUPS: [(cli::Group, &str); 6] = [
    (cli::Group::Settings, "settings"),
    (cli::Group::Shortcuts, "shortcuts"),
    (cli::Group::Link, "link tests"),
    (cli::Group::Space, "address space"),
    (cli::Group::Info, "info"),
    (cli::Group::System, "system"),
];

/// One piece of the `help` overview: a section of commands, or the closing hints (the main loop
/// prints them a pause apart, see `cli::HelpPager`).
pub fn help_step(step: cli::HelpStep) {
    match step {
        cli::HelpStep::Section(n) => help_section(n),
        cli::HelpStep::Footer => {
            out!("  {DIM}`help <command>` or `help <key>` for details{RST}");
            out!("  {DIM}← → edit · ↑ ↓ history · Home/End · Ctrl+U clears · Ctrl+C quits espflash{RST}");
        }
    }
}

fn help_section(n: usize) {
    let _block = Block::new();
    let Some(&(group, title)) = HELP_GROUPS.get(n) else { return };
    // The name column as wide as the longest `name (aliases)` of any section, so summaries line up across them. Not the
    // usage: some are 60 characters long and would push every line past the width of the terminal (`help <command>`
    // has the usage).
    let name_w =
        cli::COMMANDS.iter().filter(|c| c.on(config::LinkKind::Net)).map(cli::CmdHelp::label_width).max().unwrap_or(0)
            + 2;
    out!("  {BOLD}{title}{RST}");
    for c in cli::COMMANDS.iter().filter(|c| c.group == group && c.on(config::LinkKind::Net)) {
        out!("    {}{SEP}│{RST} {DIM}{}{RST}", CommandLabel(c, name_w), c.summary);
    }
}

/// `name (alias, alias)` padded to `.1` columns: the name in the accent color, the aliases dim.
struct CommandLabel<'a>(&'a cli::CmdHelp, usize);

impl core::fmt::Display for CommandLabel<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let c = self.0;
        write!(f, "{ACCENT}{}{RST}", c.name)?;
        if !c.aliases.is_empty() {
            write!(f, "{DIM} (")?;
            for (i, a) in c.aliases.iter().enumerate() {
                write!(f, "{}{a}", if i == 0 { "" } else { ", " })?;
            }
            write!(f, "){RST}")?;
        }
        for _ in c.label_width()..self.1 {
            f.write_str(" ")?;
        }
        Ok(())
    }
}

/// Width of the label column in `help <command>` / `help <key>`.
const HELP_LABEL: usize = 10;

fn help_command(c: &cli::CmdHelp) {
    let _block = Block::new();
    out!("  {BOLD}{}{RST} {DIM}—{RST} {}", c.name, c.summary);
    out!("    {DIM}{:<HELP_LABEL$}{RST}{SEP}│{RST} {ACCENT}{}{RST}", "usage", c.usage);
    if !c.aliases.is_empty() {
        let mut aliases: heapless::String<32> = heapless::String::new();
        for (i, a) in c.aliases.iter().enumerate() {
            let _ = aliases.push_str(if i == 0 { "" } else { ", " });
            let _ = aliases.push_str(a);
        }
        out!("    {DIM}{:<HELP_LABEL$}{RST}{SEP}│{RST} {aliases}", "aliases");
    }
    for (i, line) in c.details.iter().enumerate() {
        out!("    {DIM}{:<HELP_LABEL$}{RST}{SEP}│{RST} {line}", if i == 0 { "details" } else { "" });
    }
    for (i, ex) in c.examples.iter().enumerate() {
        out!("    {DIM}{:<HELP_LABEL$}{RST}{SEP}│{RST} {ACCENT}{ex}{RST}", if i == 0 { "examples" } else { "" });
    }
    if c.name == "set" {
        for (row, chunk) in config::KEYS.chunks(4).enumerate() {
            let mut keys: heapless::String<96> = heapless::String::new();
            for (i, (name, _)) in chunk.iter().enumerate() {
                let _ = keys.push_str(if i == 0 { "" } else { " · " });
                let _ = keys.push_str(name);
            }
            out!("    {DIM}{:<HELP_LABEL$}{RST}{SEP}│{RST} {keys}", if row == 0 { "keys" } else { "" });
        }
    }
}

fn help_key(name: &str, k: config::Key, cfg: &NodeConfig, defaults: &NodeConfig) {
    let _block = Block::new();
    let (what, values) = k.describe();
    let (now, build) = (cfg.value(k), defaults.value(k));
    out!("  {BOLD}{name}{RST} {DIM}—{RST} {what}");
    out!("    {DIM}{:<HELP_LABEL$}{RST}{SEP}│{RST} {values}", "values");
    if now == build {
        out!("    {DIM}{:<HELP_LABEL$}{RST}{SEP}│{RST} {now}", "current");
    } else {
        out!("    {DIM}{:<HELP_LABEL$}{RST}{SEP}│{RST} {WARN}{now}{RST}   {DIM}(build {build}){RST}", "current");
    }
    out!("    {DIM}{:<HELP_LABEL$}{RST}{SEP}│{RST} {ACCENT}set {name} <value>{RST}", "change");
}

// --- Verbose output ----------------------------------------------------------

pub fn link_counters(
    c: &crate::link::Counters,
    phy: PhyLink,
    peer_period_us: Option<u32>,
    loops_per_s: u64,
    loop_max_us: u32,
) {
    let peer: heapless::String<24> = match peer_period_us {
        Some(us) => cell(Period(us as f32 / 1000.0)),
        // The peer in `tx on-rx` mode sends no DAT, so it never announces a period.
        None => cell("—"),
    };
    // Two short lines under the status line, a label and its values, instead of one very long one:
    //   link   1 Gbit/s · tx 9.0 kB · rx 7.1 kB · rx queue overflow 0 · peer DAT period 2.5 s
    //   loop   22153/s · slowest 1067 µs · last tx waited 998 µs (task pickup 894 µs)
    out!(
        "{DIM}  link{RST}  {} · tx {} · rx {} · rx queue overflow {} · peer DAT period {}",
        Speed(phy.mbps),
        Bytes(u64::from(c.tx_bytes)),
        Bytes(u64::from(c.rx_bytes)),
        c.rx_dropped,
        &*peer
    );
    out!(
        "{DIM}  loop{RST}  {loops_per_s}/s · slowest {loop_max_us} µs · last tx waited {} µs (task pickup {} µs)",
        c.tx_wait_us,
        c.tx_pickup_us
    );
}

/// `config`: every setting, read-only ones first, changed ones marked.
pub fn cli_config(cfg: &NodeConfig, defaults: &NodeConfig) {
    let _block = Block::new();
    let p = cfg.pins;
    let Link::Net(n) = cfg.link else { return };
    out!("  {BOLD}{:<22}{RST}{SEP}│{RST} {BOLD}{:<16}{RST}{SEP}│{RST} {DIM}build value{RST}", "key", "value");
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
    out!("  {DIM}{:<22}{RST}{SEP}│{RST} {DIM}{:<16}{RST}{SEP}│{RST} {DIM}read-only (rebuild){RST}", "name", cfg.name);
    for (key, value) in &fixed {
        out!("  {DIM}{key:<22}{RST}{SEP}│{RST} {DIM}{value:<16}{RST}{SEP}│{RST} {DIM}read-only (rebuild){RST}");
    }
    for (key, k) in config::KEYS {
        let now = cfg.value(k);
        let build = defaults.value(k);
        if now == build {
            out!("  {ACCENT}{key:<22}{RST}{SEP}│{RST} {now:<16}{SEP}│{RST}");
        } else {
            out!(
                "  {ACCENT}{key:<22}{RST}{SEP}│{RST} {BOLD}{WARN}{now:<16}{RST}{SEP}│{RST} {DIM}{build}  (changed){RST}"
            );
        }
    }
}

pub fn cli_list(current: usize) {
    let _block = Block::new();
    out!("  {BOLD}patterns{RST}");
    for (i, p) in fx::PATTERNS.iter().enumerate() {
        let mark = Cursor(i == current);
        prompt_clear();
        crate::con_print!("  {mark} {DIM}{}{RST} {BOLD}{:<7}{RST} ", i + 1, p.name);
        swatch(p, 16);
        crate::con_println!("  {DIM}{}{RST}", p.description);
        prompt_draw();
    }
}

pub fn cli_status(led: &fx::Settings, output: crate::led::RGB8, leds: &ExtLeds) {
    let _block = Block::new();
    prompt_clear();
    crate::con_print!("  {BOLD}LED{RST}  ");
    swatch(led.pattern(), 16);
    crate::con_println!(
        "  {BOLD}{}{RST} {DIM}·{RST} {} {DIM}·{RST} cycle {} {DIM}·{RST} {}%",
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
    out!("       {DIM}keys{RST}  {keys}");
    out!(
        "       {DIM}out{RST}   #{:02X}{:02X}{:02X}  {DIM}(sent to the LED now, after brightness + gamma){RST}",
        output.r,
        output.g,
        output.b
    );
    for i in 0..2 {
        out!("  {BOLD}LED{}{RST} {DIM}GPIO{}{RST}  {}", i + 1, leds.pins[i], leds.modes[i]);
    }
}

// --- Address space (UA-lite) ----------------------------------------------------------------------

/// A line about the address space protocol: `rx` when it is the peer asking (or answering), else us.
pub fn space_event(now_ms: u64, rx: bool, args: core::fmt::Arguments<'_>) {
    if silent() {
        return;
    }
    let (s, ms) = stamp(now_ms);
    let dir = Arrow(!rx);
    out!("{TIME}{s:>6}.{ms:03}{RST} {dir} {LNK} UA {RST} {args}");
}

/// What a request from the peer says, for the console.
pub struct SpaceRequest<'a>(pub &'a str, pub &'a workshop_core::space::Body);

impl core::fmt::Display for SpaceRequest<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        use workshop_core::space::{Body, path_string};
        let who = self.0;
        match self.1 {
            Body::Browse { id, index } => write!(f, "{NODE}{who}{RST} browses {} #{index}", path_string(*id)),
            Body::Read { id } => write!(f, "{NODE}{who}{RST} reads {}", path_string(*id)),
            Body::Write { id, value } => {
                write!(f, "{NODE}{who}{RST} writes {} = {BOLD}{value}{RST}", path_string(*id))
            }
            Body::Subscribe { id, interval_ms: 0 } if *id == workshop_core::space::NONE => {
                write!(f, "{NODE}{who}{RST} stops watching everything")
            }
            Body::Subscribe { id, interval_ms: 0 } => {
                write!(f, "{NODE}{who}{RST} stops watching {}", path_string(*id))
            }
            Body::Subscribe { id, interval_ms } => {
                write!(f, "{NODE}{who}{RST} watches {} every {}", path_string(*id), Period(*interval_ms as f32))
            }
            _ => Ok(()),
        }
    }
}

/// One node of a browse, as a list line.
pub fn space_tree_head(path: &str, depth: u8) {
    if depth > 1 {
        out!("  {BOLD}{path}{RST} {DIM}· {depth} levels{RST}");
    } else {
        out!("  {BOLD}{path}{RST}");
    }
}

/// Width of the name column of a browsed tree, branches included.
const TREE_NAME_COL: usize = 30;

/// One node of a browsed tree: `├── name/` for an object, `└── name   u32  rw` for a variable.
pub fn space_tree_entry(e: &workshop_core::space::TreeEntry) {
    use core::fmt::Write as _;
    let mut line: heapless::String<96> = heapless::String::new();
    let mut used = 0;
    let _ = line.push_str("  ");
    for j in 0..e.level {
        if e.guides >> j & 1 == 1 {
            let _ = write!(line, "{SEP}│   {RST}");
        } else {
            let _ = line.push_str("    ");
        }
        used += 4;
    }
    let _ = write!(line, "{DIM}{}{RST}", if e.last { "└── " } else { "├── " });
    used += 4 + e.name.chars().count();
    if e.is_object() {
        let _ = write!(line, "{BOLD}{INFO}{}/{RST}", e.name);
        used += 1;
    } else {
        let _ = write!(line, "{ACCENT}{}{RST}", e.name);
    }
    let pad = TREE_NAME_COL.saturating_sub(used);
    if e.is_object() {
        out!("{line}");
    } else {
        let ty = match e.kind[1] {
            b'B' => "bool",
            b'U' => "u32",
            _ => "text",
        };
        let (access, color) = if e.kind[2] == b'W' { ("rw", WARN) } else { ("ro", DIM) };
        out!("{line}{:pad$} {DIM}{ty:<5}{RST}{color}{access}{RST}", "");
    }
}

/// The end of a browse: how many nodes it listed.
pub fn space_tree_foot(objects: u16, variables: u16) {
    if objects + variables > 0 {
        let s = |n: u16| if n == 1 { "" } else { "s" };
        out!("  {DIM}{objects} object{}, {variables} variable{}{RST}", s(objects), s(variables));
    }
}

/// A browsed node that has nothing under it.
pub fn space_tree_empty(path: &str) {
    out!("  {DIM}{path} has no children{RST}");
}

/// Width of the path column of a value line.
const VALUE_NAME_COL: usize = 26;

/// A path in a column: the branch dimmed, the last segment lit, padded to `width` characters.
struct PathCell<'a>(&'a str, usize);

impl core::fmt::Display for PathCell<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (branch, leaf) = self.0.rsplit_once('/').map_or(("", self.0), |(b, l)| (&self.0[..=b.len()], l));
        write!(f, "{DIM}{branch}{RST}{ACCENT}{leaf}{RST}")?;
        for _ in self.0.chars().count()..self.1 {
            f.write_str(" ")?;
        }
        Ok(())
    }
}

/// A value from the peer: the answer to a read, a write or a subscribe, or a notification.
pub fn space_value(path: &str, status: workshop_core::space::Status, value: &str) {
    let cell = PathCell(path, VALUE_NAME_COL);
    if status == workshop_core::space::Status::Good {
        out!("  {cell} {DIM}={RST} {BOLD}{value}{RST}");
    } else if value.is_empty() {
        out!("  {cell} {ERROR}✗ {status}{RST}");
    } else {
        // A refused write: the status, and the value that stayed.
        out!("  {cell} {ERROR}✗ {status}{RST} {DIM}(still {value}){RST}");
    }
}

// --- Short addresses, for the compact view ------------------------------------------------------------

/// This node's address and subnet mask, as numbers (0 until [`set_local`]).
static LOCAL_ADDR: AtomicU32 = AtomicU32::new(0);
static LOCAL_MASK: AtomicU32 = AtomicU32::new(0);

/// Whether [`ShortIp`] shortens (the `trace_short_ip` setting).
static SHORT_IP: AtomicBool = AtomicBool::new(false);

/// Applies `trace_short_ip`.
pub fn set_short_ip(on: bool) {
    SHORT_IP.store(on, Ordering::Relaxed);
}

/// Tells the console which subnet it is on, so the compact trace can drop what every address has in common.
pub fn set_local(net: &config::NetLink) {
    LOCAL_ADDR.store(u32::from_be_bytes(net.address), Ordering::Relaxed);
    LOCAL_MASK.store(net.netmask(), Ordering::Relaxed);
}

/// An address without the part of the subnet it shares with this node: `.12` for `192.168.77.12` on a /24.
/// An address from elsewhere is written whole, and so is every address while `trace_short_ip` is off.
struct ShortIp([u8; 4]);

impl core::fmt::Display for ShortIp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (local, mask) = (LOCAL_ADDR.load(Ordering::Relaxed), LOCAL_MASK.load(Ordering::Relaxed));
        let this = u32::from_be_bytes(self.0);
        if !SHORT_IP.load(Ordering::Relaxed) || mask == 0 || (this & mask) != (local & mask) {
            return write!(f, "{}", Addr(self.0));
        }
        // The host bytes, from the first one the mask doesn't cover.
        let keep = (32 - mask.count_ones()).div_ceil(8).clamp(1, 3) as usize;
        for b in &self.0[4 - keep..] {
            write!(f, ".{b}")?;
        }
        Ok(())
    }
}

// --- Network trace (`trace`) ----------------------------------------------------------------------------

/// How many lines of node frames one traced segment shows (a TCP segment can hold about 15).
const TRACE_APP_LINES: usize = 4;
/// Under the time stamp and the direction: the layers start here, so they line up below the header.
const TRACE_INDENT: &str = "              ";
/// Visible width of the `from → to` column of a header line. The longest thing it holds is
/// `who-has 255.255.255.255 tell 255.255.255.255` (44) or two `255.255.255.255:65535`; the usual
/// `192.168.77.11:7000 → 192.168.77.12:7000` is 39. A line that is longer would push the bar after it out of
/// line, so the column is as wide as the widest.
const TRACE_WHO_W: usize = 44;

/// The same column in the compact view: with short addresses `.11:49152→.12:7000` (and `.11 is-at <MAC>`, 27)
/// it is narrower.
fn compact_who_w() -> usize {
    if SHORT_IP.load(Ordering::Relaxed) { 30 } else { TRACE_WHO_W }
}

/// The "what it is" column of the compact view: `[FIN,PSH,ACK] 1514 B` is the longest that follows a bar.
const COMPACT_WHAT_W: usize = 22;

/// `12.351420`: seconds, milliseconds and microseconds, in one color (the time stamp of the other lines, with
/// three more digits).
struct Stamp(u64);

impl core::fmt::Display for Stamp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (s, ms) = stamp(self.0 / 1000);
        write!(f, "{TIME}{s:>6}.{ms:03}{:03}{RST}", self.0 % 1000)
    }
}

/// The color of a layer's label and its lines.
fn layer_paint(layer: workshop_core::dissect::Layer) -> workshop_core::theme::Paint {
    use workshop_core::dissect::Layer;
    match layer {
        Layer::Eth => ETH,
        Layer::Arp | Layer::Ipv4 => IP,
        Layer::Udp | Layer::Tcp | Layer::Icmp => TRANSPORT,
        Layer::Bad => ERROR,
    }
}

/// What a header line says about a packet: `who` (from → to) and `what` (flags, size). `short` drops the
/// subnet of the addresses and the payload size (the compact view); the full form is for the other views.
fn trace_who_what(
    p: &workshop_core::dissect::Packet<'_>,
    short: bool,
) -> (heapless::String<160>, heapless::String<96>) {
    use core::fmt::Write as _;
    use workshop_core::dissect::{ArpOp, L4, Net, TcpFlags};
    let wire_len = p.wire_len;
    let mut who: heapless::String<160> = heapless::String::new();
    let mut what: heapless::String<96> = heapless::String::new();
    // `a:p → b:q`, or `.11:p→.12:q`.
    let flow = |who: &mut heapless::String<160>, src: [u8; 4], sp: u16, dst: [u8; 4], dp: u16| {
        let _ = if short {
            write!(who, "{DIM}{}:{sp}→{}:{dp}{RST}", ShortIp(src), ShortIp(dst))
        } else {
            write!(who, "{}:{sp} {DIM}→{RST} {}:{dp}", Addr(src), Addr(dst))
        };
    };
    match p.net {
        Net::Arp(a) => {
            let _ = match a.op {
                ArpOp::Request => write!(
                    who,
                    "{DIM}who-has{RST} {} {DIM}tell{RST} {}",
                    ShortIp(a.target_ip.0),
                    ShortIp(a.sender_ip.0)
                ),
                ArpOp::Reply => write!(who, "{} {DIM}is-at{RST} {}", ShortIp(a.sender_ip.0), a.sender_mac),
                ArpOp::Other(n) => write!(who, "{DIM}operation {n}{RST} {} {DIM}→{RST} {}", a.sender_ip, a.target_ip),
            };
            let _ = write!(what, "{wire_len} B");
        }
        Net::Ipv4(ip, L4::Udp(u, pl)) => {
            flow(&mut who, ip.src.0, u.src_port, ip.dst.0, u.dst_port);
            if short {
                let _ = write!(what, "{wire_len} B");
            } else {
                let _ = write!(what, "{wire_len} B  {DIM}payload{RST} {} B", pl.len());
            }
        }
        Net::Ipv4(ip, L4::Tcp(t, pl)) => {
            let flags = if t.flags.has(TcpFlags::RST) {
                ERROR
            } else if t.flags.any(TcpFlags::SYN | TcpFlags::FIN) {
                WARN
            } else {
                DIM
            };
            flow(&mut who, ip.src.0, t.src_port, ip.dst.0, t.dst_port);
            if short {
                let _ = write!(what, "{flags}[{}]{RST} {wire_len} B", t.flags);
            } else {
                let _ = write!(what, "{flags}[{}]{RST}  {wire_len} B  {DIM}payload{RST} {} B", t.flags, pl.len());
            }
        }
        Net::Ipv4(ip, L4::Icmp(i)) => {
            let _ = write!(who, "{} {DIM}→{RST} {}", ShortIp(ip.src.0), ShortIp(ip.dst.0));
            let _ = write!(what, "{i}");
        }
        Net::Ipv4(ip, _) => {
            let _ = write!(who, "{} {DIM}→{RST} {}", ShortIp(ip.src.0), ShortIp(ip.dst.0));
            let _ = write!(what, "protocol {}  {wire_len} B", ip.protocol);
        }
        Net::Other(t) => {
            let _ = write!(who, "{} {DIM}→{RST} {}", p.eth.src, p.eth.dst);
            let _ = write!(what, "type 0x{t:04x}  {wire_len} B");
        }
        Net::Malformed(why) => {
            let _ = write!(who, "{ERROR}{why}{RST}");
            let _ = write!(what, "{wire_len} B");
        }
    }
    (who, what)
}

/// What makes two consecutive packets "the same thing again": direction, protocol, endpoints and TCP flags.
/// The compact view folds a run of them into one line.
#[derive(PartialEq, Eq, Clone, Copy)]
struct FlowKey {
    tx: bool,
    protocol: &'static str,
    ends: ([u8; 4], u16, [u8; 4], u16),
    flags: u8,
}

fn flow_key(c: &crate::trace::Captured, p: &workshop_core::dissect::Packet<'_>) -> Option<FlowKey> {
    use workshop_core::dissect::{L4, Net};
    // A damaged or control frame stands alone: it is the interesting one.
    if p.is_damaged() || p.is_control() {
        return None;
    }
    let (ip, sp, dp, flags) = match p.net {
        Net::Ipv4(ip, L4::Udp(u, _)) => (ip, u.src_port, u.dst_port, 0),
        Net::Ipv4(ip, L4::Tcp(t, _)) => (ip, t.src_port, t.dst_port, t.flags.0),
        _ => return None,
    };
    Some(FlowKey { tx: c.tx, protocol: p.protocol(), ends: (ip.src.0, sp, ip.dst.0, dp), flags })
}

/// The part of a frame's payload that was captured whole: the lines that end in `\n` (a segment cut by the
/// capture loses its last, partial one).
fn whole_lines(payload: &[u8], cut: bool) -> &[u8] {
    match (cut, payload.last()) {
        (true, Some(b'\n')) | (false, _) => payload,
        (true, _) => payload.iter().rposition(|&b| b == b'\n').map_or(&payload[..0], |i| &payload[..=i]),
    }
}

/// One line of the compact view: `time dir proto │ flow │ what │ the node's frame`, each column padded to its
/// own width so that the lines line up at any speed; `repeat` > 1 says the line stands for that many
/// identical frames (the last one is shown).
fn trace_compact_line(c: &crate::trace::Captured, repeat: usize) -> heapless::String<512> {
    use core::fmt::Write as _;
    use workshop_core::dissect::{self, App};
    let mut line: heapless::String<512> = heapless::String::new();
    let (ts, arrow) = (Stamp(c.at_us), Arrow(c.tx));
    let wire_len = usize::from(c.wire_len);
    let packet = match dissect::dissect(&c.data, wire_len) {
        Ok(p) => p,
        Err(why) => {
            let _ = write!(
                line,
                "{ts} {arrow} {ERROR}bad  {RST} {SEP}│{RST} {ERROR}{why}{RST} {SEP}│{RST} {DIM}{wire_len} B{RST}"
            );
            return line;
        }
    };
    let tag = if packet.is_damaged() {
        ERROR
    } else if packet.is_control() {
        WARN
    } else {
        INFO
    };
    let (mut who, mut what) = trace_who_what(&packet, true);
    pad_visible(&mut who, compact_who_w());
    pad_visible(&mut what, COMPACT_WHAT_W);
    let _ = write!(line, "{ts} {arrow} {tag}{:<4}{RST} {SEP}│{RST} {who} {SEP}│{RST} {what}", packet.protocol());
    let whole = whole_lines(packet.payload(), c.data.len() < wire_len);
    let count = dissect::app_lines(whole).count();
    if let Some(first) = dissect::app_lines(whole).next() {
        let _ = write!(line, " {SEP}│{RST} {APP}{}{RST}", App(first));
        if count > 1 {
            let _ = write!(line, " {DIM}+{} more{RST}", count - 1);
        }
    }
    if packet.is_damaged() {
        let _ = write!(line, " {ERROR}✗ damaged{RST}");
    }
    if repeat > 1 {
        let _ = write!(line, " {SEP}│{RST} {BOLD}×{repeat}{RST}");
    }
    line
}

/// A few frames at once. In the one-line form (`trace events`, or the `compact` view), a run of identical
/// frames (same direction, endpoints and flags: the periodic `DAT` of a busy link, a burst) is folded into one
/// line with its count, the last frame shown, so a fast link reads as a few lines instead of a blur. Anything
/// else is shown one by one.
pub fn trace_batch(frames: &[crate::trace::Captured], level: Trace, view: TraceView, dump_bytes: usize) {
    use workshop_core::dissect;
    let one_line = level != Trace::Frames || view == TraceView::Compact;
    if !one_line {
        for c in frames {
            trace_frame(c, level, view, dump_bytes);
        }
        return;
    }
    prompt_clear();
    let mut i = 0;
    while i < frames.len() {
        let key = dissect::dissect(&frames[i].data, usize::from(frames[i].wire_len))
            .ok()
            .and_then(|p| flow_key(&frames[i], &p));
        let mut run = 1;
        if let Some(k) = key {
            while let Some(next) = frames.get(i + run) {
                let same =
                    dissect::dissect(&next.data, usize::from(next.wire_len)).ok().and_then(|p| flow_key(next, &p))
                        == Some(k);
                if !same {
                    break;
                }
                run += 1;
            }
        }
        crate::con_println!("{}", trace_compact_line(&frames[i + run - 1], run));
        i += run;
    }
    prompt_draw();
}

/// One frame of the network trace. Its look depends on `level` and `view`:
///
/// - `trace events`, or the `compact` view: one line, `time dir proto │ from → to │ what │ node frame` (see
///   [`trace_batch`]).
/// - `layers` (default): a header line, then a line per layer from the cable inwards, the node's own
///   frames, and `dump_bytes` raw bytes as a hex dump.
/// - `inside-out`: the same lines, the node's own frames first and the layers back out to the cable.
///
/// ```text
///  12.351420 TX ▶ UDP   │ 192.168.77.11:7000 → 192.168.77.12:7000   │ 78 B  payload 36 B
///               app  │ DAT node-A #41 t=12351 ms  button released  events 3  period 2500.000 ms
///               udp  │ 7000 → 7000  len 44 (payload 36 B)  cs ok
///               ipv4 │ 192.168.77.11 → 192.168.77.12  ttl 64  proto 17 (UDP)  total 64  id 0x0000  DF  cs ok
///               eth  │ 02:00:00:00:00:11 → 02:00:00:00:00:12  type 0x0800 (IPv4)
///               raw  │ 0000 │ 02 00 00 00 00 12 02 00  00 00 00 11 08 00 45 00 │ ..............E.
///                    │ … 46 B more
/// ```
pub fn trace_frame(c: &crate::trace::Captured, level: Trace, view: TraceView, dump_bytes: usize) {
    use core::fmt::Write as _;
    use workshop_core::dissect::{self, App, Layer};

    let wire_len = usize::from(c.wire_len);
    let (ts, arrow) = (Stamp(c.at_us), Arrow(c.tx));
    let dump = &c.data[..c.data.len().min(dump_bytes)];
    let pad = TRACE_INDENT;

    if level != Trace::Frames || view == TraceView::Compact {
        prompt_clear();
        crate::con_println!("{}", trace_compact_line(c, 1));
        prompt_draw();
        return;
    }

    let raw = || {
        if dump.is_empty() {
            return;
        }
        let mut first = true;
        dissect::hexdump(dump, |row| {
            let label = if first { "raw" } else { "" };
            first = false;
            crate::con_println!(
                "{pad}{RAW}{label:<5}{RST}{SEP}│{RST} {DIM}{:04x}{RST} {SEP}│{RST} {RAW}{}{RST} {SEP}│{RST} {DIM}{}{RST}",
                row.offset(),
                row.hex(),
                row.ascii()
            );
        });
        if dump.len() < wire_len {
            crate::con_println!("{pad}{:<5}{SEP}│{RST} {DIM}… {} B more{RST}", "", wire_len - dump.len());
        }
    };

    prompt_clear();
    let packet = match dissect::dissect(&c.data, wire_len) {
        Ok(p) => p,
        Err(why) => {
            crate::con_println!(
                "{ts} {arrow} {ERROR}bad  {RST} {SEP}│{RST} {ERROR}{why}{RST} {SEP}│{RST} {DIM}{wire_len} B{RST}"
            );
            raw();
            prompt_draw();
            return;
        }
    };

    // The node's own frames: only the whole lines of what was captured of the payload.
    let payload = packet.payload();
    let cut = c.data.len() < wire_len;
    let whole = whole_lines(payload, cut);
    let app_count = dissect::app_lines(whole).count();

    // --- The header line: `time dir proto │ from → to │ what`
    let tag = if packet.is_damaged() {
        ERROR
    } else if packet.is_control() {
        WARN
    } else {
        INFO
    };
    let (mut who, what) = trace_who_what(&packet, false);
    pad_visible(&mut who, TRACE_WHO_W);
    let damaged = if packet.is_damaged() { " ✗ damaged" } else { "" };
    crate::con_println!(
        "{ts} {arrow} {tag}{:<5}{RST} {SEP}│{RST} {who} {SEP}│{RST} {what}{ERROR}{damaged}{RST}",
        packet.protocol()
    );

    // --- The lines below it, in the order of the view.
    let app = || {
        for (i, text) in dissect::app_lines(whole).enumerate() {
            if i < TRACE_APP_LINES {
                crate::con_println!("{pad}{APP}{:<5}{RST}{SEP}│{RST} {}", "app", App(text));
            }
        }
        if app_count > TRACE_APP_LINES {
            crate::con_println!(
                "{pad}{:<5}{SEP}│{RST} {DIM}… {} more frames in this segment{RST}",
                "",
                app_count - TRACE_APP_LINES
            );
        }
        if app_count == 0 && cut && !payload.is_empty() {
            crate::con_println!(
                "{pad}{APP}{:<5}{RST}{SEP}│{RST} {DIM}(frame cut by the capture: raise `dump`, or see the next segment){RST}",
                "app"
            );
        }
    };
    let mut layers: heapless::Vec<(Layer, heapless::String<200>), 6> = heapless::Vec::new();
    packet.layers(|layer, text| {
        let mut buf: heapless::String<200> = heapless::String::new();
        let _ = write!(buf, "{text}");
        let _ = layers.push((layer, buf));
    });
    let show = |(layer, text): &(Layer, heapless::String<200>)| {
        let color = layer_paint(*layer);
        let body = if *layer == Layer::Bad { ERROR } else { DIM };
        crate::con_println!("{pad}{color}{:<5}{RST}{SEP}│{RST} {body}{text}{RST}", layer.name());
    };
    match view {
        TraceView::InsideOut => {
            app();
            layers.iter().rev().for_each(show);
        }
        _ => {
            layers.iter().for_each(show);
            app();
        }
    }
    raw();
    prompt_draw();
}

// --- The traffic dashboard ------------------------------------------------------------------------------

/// Columns of the widest graph, for the buffers that hold one.
const MAX_BARS: usize = workshop_core::traffic::MAX_COLUMNS;

/// Draws a row of bars for `values`, scaled so `max` is a full bar. A bar is 3 bytes of UTF-8: `N` is 3 per column.
fn bars<const N: usize>(values: impl Iterator<Item = u32>, max: u32) -> heapless::String<N> {
    let mut s = heapless::String::new();
    let _ = workshop_core::traffic::spark(values, max, &mut s);
    s
}

/// A rate given in bytes per second, shown in bits per second: `1.20 Mbit/s`, `845.3 kbit/s`.
struct BitRate(u64);

impl core::fmt::Display for BitRate {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let bits = self.0.saturating_mul(8);
        match bits {
            b if b >= 1_000_000_000 => write!(f, "{}.{:02} Gbit/s", b / 1_000_000_000, b % 1_000_000_000 / 10_000_000),
            b if b >= 1_000_000 => write!(f, "{}.{:02} Mbit/s", b / 1_000_000, b % 1_000_000 / 10_000),
            b if b >= 1_000 => write!(f, "{}.{} kbit/s", b / 1_000, b % 1_000 / 100),
            b => write!(f, "{b} bit/s"),
        }
    }
}

/// A bit rate as a right-aligned cell of 12 columns.
fn rate_cell(bytes_per_s: u64) -> heapless::String<16> {
    cell(BitRate(bytes_per_s))
}

/// `0.90%`, or `-` when there is no link speed to compare with.
struct Use(u64, u64);

impl core::fmt::Display for Use {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (rate, capacity) = (self.0, self.1);
        if capacity == 0 {
            return f.write_str("-");
        }
        // Hundredths of a percent: a busy-looking 120 kB/s is 0.10% of a gigabit.
        let h = rate * 10_000 / capacity;
        write!(f, "{}.{:02}%", h / 100, h % 100)
    }
}

/// `30s` or `2m`: a window, short.
fn span_text(seconds: u64) -> heapless::String<8> {
    cell(format_args!("{}", SpanText(seconds)))
}

struct SpanText(u64);

impl core::fmt::Display for SpanText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 < 120 { write!(f, "{}s", self.0) } else { write!(f, "{}m", self.0 / 60) }
    }
}

/// `── network · chart · normal · 30s ─────────────`: a dim line that sets the dashboard apart from the output above
/// it. It is cut by the terminal if it is narrower (the block doesn't wrap).
struct Rule<'a> {
    title: &'a str,
    width: usize,
}

impl core::fmt::Display for Rule<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{SEP}── {RST}{DIM}{}{RST} {SEP}", self.title)?;
        for _ in 0..self.width.saturating_sub(self.title.chars().count() + 4) {
            f.write_str("─")?;
        }
        f.write_str(RST)
    }
}

/// One direction of what the dashboard measures, from the last second and the window.
struct Side {
    name: &'static str,
    paint: workshop_core::theme::Paint,
    /// Bytes per second now.
    rate: u64,
    /// Frames per second now.
    fps: u64,
    /// Average frame size now, bytes.
    avg: Option<u32>,
    /// Bytes per second in the busiest column.
    peak: u64,
    /// Share of the link it uses.
    used: Use,
    /// Bytes of the busiest column (the full height of its graph).
    max: u32,
    tx: bool,
}

fn side(snap: &crate::trace::Snapshot, tx: bool, capacity: u64) -> Side {
    let pick = |c: &workshop_core::traffic::Slice| if tx { c.tx } else { c.rx };
    let recent = pick(&snap.recent.0);
    let max = pick(&snap.peak).bytes;
    let rate = snap.per_second(recent.bytes);
    Side {
        name: if tx { "tx" } else { "rx" },
        paint: if tx { TX } else { RX },
        rate,
        fps: snap.per_second(recent.frames),
        avg: recent.bytes.checked_div(recent.frames),
        peak: snap.per_second_of_slice(max),
        used: Use(rate, capacity),
        max,
        tx,
    }
}

/// Redraws the traffic dashboard above the prompt. Three views, each in three sizes:
///
/// - `bars`: a small bar graph per direction, with rate, frames per second, peak and link use;
/// - `chart`: the same data as taller plots with a bit-rate axis and a time axis, and beside them what a network
///   engineer reads: rate, packets per second, average frame size, peak, and the use of the link;
/// - `mix`: the chart, and the protocols that went by ranked as horizontal bars on colored backgrounds.
///
/// ```text
///      rx   5.72 Mbit/s ┤                ▃████▃          │ now   1.20 Mbit/s    1000 pkt/s
///                       ┤▁▁▁▁▁▁▁▁▁▁▃█████████▆▁▁▁▁▁▁▁▁▁   │ avg 150 B · peak 5.72 Mbit/s · 0.10%
///      tx  154.9 kbit/s ┤                          ▃████  │ ...
///                     0 ┤·········································
///                       └-30s ─────────── -15s ────────── now
///                   UDP │███████████████████████████ 94.2% 51230 frames
/// ```
pub fn dash_update(mode: config::TraceDash, view: config::DashView, snap: &crate::trace::Snapshot, phy: PhyLink) {
    use config::{DashView, TraceDash};
    use core::fmt::Write as _;
    if mode == TraceDash::Off {
        dash_clear();
        return;
    }
    let capacity = u64::from(phy.mbps) * 125_000;
    let (rx, tx) = (side(snap, false, capacity), side(snap, true, capacity));
    let window_s = snap.slice_us * snap.columns.len() as u64 / 1_000_000;

    let mut rows: heapless::Vec<heapless::String<400>, { config::DASH_MAX_ROWS }> = heapless::Vec::new();
    let mut push = |text: &dyn core::fmt::Display| {
        let mut r: heapless::String<400> = heapless::String::new();
        let _ = write!(r, "{text}");
        let _ = rows.push(r);
    };
    // A thin rule first: where what scrolls ends and the dashboard begins.
    let mut title: heapless::String<64> = heapless::String::new();
    let _ = write!(title, "network · {view} · {mode} · {}", span_text(window_s));
    push(&Rule { title: &title, width: snap.columns.len() + 56 });

    match view {
        DashView::Bars => bars_rows(mode, snap, &rx, &tx, window_s, phy, &mut push),
        DashView::Chart => chart_rows(mode, snap, [&rx, &tx], window_s, &mut push),
        DashView::Mix => {
            chart_rows(mode, snap, [&rx, &tx], window_s, &mut push);
            mix_rows(mode, snap, &mut push);
        }
    }

    // Nothing moved (a quiet link): nothing to send.
    if critical_section::with(|cs| *DASH.borrow_ref(cs) == rows) {
        return;
    }
    prompt_clear();
    let shrunk = critical_section::with(|cs| {
        let mut dash = DASH.borrow_ref_mut(cs);
        let shrunk = dash.len().saturating_sub(rows.len());
        *dash = rows;
        shrunk
    });
    UNSCROLL.store(shrunk, Ordering::Relaxed);
    prompt_draw();
}

/// The `bars` view.
fn bars_rows(
    mode: config::TraceDash,
    snap: &crate::trace::Snapshot,
    rx: &Side,
    tx: &Side,
    window_s: u64,
    phy: PhyLink,
    push: &mut dyn FnMut(&dyn core::fmt::Display),
) {
    use config::TraceDash;
    use core::fmt::Write as _;
    // Each graph is scaled to its own peak (printed beside it): a quiet direction still shows its shape.
    let graph = |s: &Side| {
        bars::<{ 3 * MAX_BARS }>(snap.columns.iter().map(|c| if s.tx { c.tx.bytes } else { c.rx.bytes }), s.max)
    };
    // One direction's row: label, graph, rate, frames per second, peak, link use.
    struct Row<'a> {
        s: &'a Side,
        label: heapless::String<12>,
        graph: heapless::String<{ 3 * MAX_BARS }>,
    }
    impl core::fmt::Display for Row<'_> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            let s = self.s;
            write!(
                f,
                "{p}{:<7}{RST}{SEP}│{RST} {p}{}{RST} {SEP}│{RST} {BOLD}{:>12}{RST} {SEP}│{RST} {:>6} {DIM}pkt/s{RST} {SEP}│{RST} {DIM}peak{RST} {:>12} {SEP}│{RST} {DIM}{:>6}{RST}",
                self.label,
                self.graph,
                &*rate_cell(s.rate),
                s.fps,
                &*rate_cell(s.peak),
                s.used,
                p = s.paint
            )
        }
    }
    let win = span_text(window_s);
    fn row<'a>(s: &'a Side, label: &str, graph: heapless::String<{ 3 * MAX_BARS }>) -> Row<'a> {
        Row { s, label: cell(label), graph }
    }
    match mode {
        TraceDash::Off => {}
        TraceDash::Mini => {
            let small = |s: &Side| graph(s);
            push(&format_args!(
                "{DIM}net {win:<4}{RST}{SEP}│{RST} {RX}rx{RST} {RX}{}{RST} {BOLD}{:>12}{RST} {SEP}│{RST} {TX}tx{RST} {TX}{}{RST} {BOLD}{:>12}{RST}",
                small(rx),
                &*rate_cell(rx.rate),
                small(tx),
                &*rate_cell(tx.rate)
            ));
        }
        TraceDash::Normal => {
            let mut l: heapless::String<12> = heapless::String::new();
            let _ = write!(l, "rx {win}");
            push(&row(rx, &l, graph(rx)));
            push(&row(tx, "tx", graph(tx)));
        }
        TraceDash::Full => {
            let link: heapless::String<24> = if phy.up {
                cell(format_args!(
                    "{} {}",
                    Speed(phy.mbps),
                    if phy.full_duplex { "full duplex" } else { "half duplex" }
                ))
            } else {
                cell("no link")
            };
            let ms = snap.slice_us / 1000;
            push(&format_args!(
                "{DIM}net{RST}    {SEP}│{RST} {DIM}window {window_s} s · {ms} ms a column · each graph scaled to its own peak · link {}{RST}",
                &*link
            ));
            push(&row(rx, "rx", graph(rx)));
            push(&row(tx, "tx", graph(tx)));
            let t = snap.totals;
            push(&format_args!(
                "{DIM}total{RST}  {SEP}│{RST} {RX}rx{RST} {} frames {} {SEP}│{RST} {TX}tx{RST} {} frames {} {SEP}│{RST} {DIM}trace dropped {}{RST}",
                t.rx_frames,
                Bytes(t.rx_bytes),
                t.tx_frames,
                Bytes(t.tx_bytes),
                snap.dropped
            ));
        }
    }
}

/// What goes beside a plot row: the numbers a network engineer reads, spread over the rows the size allows.
fn chart_note(s: &Side, h: usize, idx: usize) -> heapless::String<128> {
    use core::fmt::Write as _;
    let mut out: heapless::String<128> = heapless::String::new();
    let avg: heapless::String<12> = match s.avg {
        Some(n) => cell(format_args!("{n} B")),
        None => cell("-"),
    };
    let now = rate_cell(s.rate);
    let peak = rate_cell(s.peak);
    // (rate, packets per second) · (average size, peak) · (link use)
    let _ = match (h, idx) {
        (1, _) => write!(out, "{BOLD}{now:>12}{RST} {:>6} {DIM}pkt/s{RST} {DIM}{}{RST}", s.fps, s.used),
        (_, 0) => write!(out, "{DIM}now{RST} {BOLD}{now:>12}{RST} {:>6} {DIM}pkt/s{RST}", s.fps),
        (2, 1) => write!(out, "{DIM}avg{RST} {avg:>6} {DIM}peak{RST} {peak:>12} {DIM}{}{RST}", s.used),
        (_, 1) => write!(out, "{DIM}avg{RST} {avg:>6} {DIM}frame{RST}"),
        _ => write!(out, "{DIM}peak{RST} {peak:>12} {DIM}{} of link{RST}", s.used),
    };
    out
}

/// The `chart` view: both directions as plots `h` rows tall, a bit-rate scale on the left, notes on the right
/// and a time axis under them.
fn chart_rows(
    mode: config::TraceDash,
    snap: &crate::trace::Snapshot,
    sides: [&Side; 2],
    window_s: u64,
    push: &mut dyn FnMut(&dyn core::fmt::Display),
) {
    use config::TraceDash;
    use core::fmt::Write as _;
    use workshop_core::traffic::plot_cell;
    let h = match mode {
        TraceDash::Off => return,
        TraceDash::Mini => 1,
        TraceDash::Normal => 2,
        TraceDash::Full => 3,
    };
    let cols = snap.columns.len();
    for s in sides {
        for row in (0..h).rev() {
            let mut plot: heapless::String<{ 3 * MAX_BARS }> = heapless::String::new();
            for c in snap.columns.iter() {
                let value = if s.tx { c.tx.bytes } else { c.rx.bytes };
                let _ = plot.push(plot_cell(value, s.max, h, row));
            }
            // The scale: the full height at the top row, zero at the bottom one.
            let (name, scale, joint): (&str, heapless::String<16>, &str) = match (row == h - 1, row == 0) {
                (true, _) => (s.name, rate_cell(s.peak), "┤"),
                (_, true) => ("", cell("0"), "┤"),
                _ => ("", cell(""), "│"),
            };
            push(&format_args!(
                "{p}{name:<2}{RST} {DIM}{:>12}{RST} {SEP}{joint}{RST}{p}{plot}{RST} {SEP}│{RST} {}",
                &*scale,
                chart_note(s, h, h - 1 - row),
                p = s.paint
            ));
        }
    }
    if mode != TraceDash::Mini {
        // `-30s ───── -15s ───── now`, as wide as the plots.
        let mut axis: heapless::String<{ 3 * MAX_BARS + 24 }> = heapless::String::new();
        let (left, mid) = (span_text(window_s), span_text(window_s / 2));
        let _ = write!(axis, "-{left} ");
        let at_mid = cols.saturating_sub(mid.len() + 1) / 2;
        let mut used = left.len() + 2;
        while used < at_mid {
            let _ = axis.push('─');
            used += 1;
        }
        let _ = write!(axis, " -{mid} ");
        used += mid.len() + 2;
        while used + 4 < cols {
            let _ = axis.push('─');
            used += 1;
        }
        let _ = axis.push_str(" now");
        push(&format_args!("{:<15} {SEP}└{RST}{DIM}{axis}{RST}", ""));
    }
}

/// The background color of a protocol's bar.
fn class_paint(c: workshop_core::traffic::Class) -> workshop_core::theme::Paint {
    use workshop_core::traffic::Class;
    match c {
        Class::Udp => MIX_UDP,
        Class::Tcp => MIX_TCP,
        Class::Arp => MIX_ARP,
        Class::Icmp => MIX_ICMP,
        Class::OtherIp => MIX_IP,
        Class::Other => MIX_OTHER,
    }
}

/// The protocol ranking of the `mix` view: the classes of frame that went by (both directions, since the graphs
/// started), most frames first, each as a bar on its own background color (`mix_*` in `[colors]`): spaces, so the
/// background is the bar.
fn mix_rows(mode: config::TraceDash, snap: &crate::trace::Snapshot, push: &mut dyn FnMut(&dyn core::fmt::Display)) {
    use config::TraceDash;
    use workshop_core::traffic::{CLASSES, Class};
    let count = |c: Class| u64::from(snap.mix[0].classes[c.index()]) + u64::from(snap.mix[1].classes[c.index()]);
    let total: u64 = Class::ALL.iter().map(|c| count(*c)).sum();
    let mut ranked: heapless::Vec<Class, CLASSES> = Class::ALL.iter().copied().collect();
    ranked.sort_unstable_by_key(|c| core::cmp::Reverse(count(*c)));
    let share = |c: Class| (count(c) * 10_000).checked_div(total).unwrap_or(0);
    let hundredths = |v: u64| Pct(v);
    struct Pct(u64);
    impl core::fmt::Display for Pct {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(f, "{:>3}.{:02}%", self.0 / 100, self.0 % 100)
        }
    }
    // A row of `width` columns where the first `filled` are a colored background.
    struct Bar(workshop_core::theme::Paint, usize, usize);
    impl core::fmt::Display for Bar {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            let (filled, width) = (self.1.min(self.2), self.2);
            write!(f, "{}{:filled$}{RST}{:rest$}", self.0, "", "", rest = width - filled)
        }
    }
    let width = snap.columns.len();
    match mode {
        TraceDash::Off => {}
        TraceDash::Mini => {
            // One stacked bar, every class its share of the width, and the three biggest named.
            let mut stacked: heapless::String<200> = heapless::String::new();
            let mut used = 0;
            for c in &ranked {
                let cells = (share(*c) as usize * width).div_ceil(10_000).min(width - used);
                if cells > 0 {
                    let _ = core::fmt::Write::write_fmt(
                        &mut stacked,
                        format_args!("{}", Bar(class_paint(*c), cells, cells)),
                    );
                    used += cells;
                }
            }
            let mut legend: heapless::String<160> = heapless::String::new();
            for c in ranked.iter().take(3).filter(|c| count(**c) > 0) {
                let _ = core::fmt::Write::write_fmt(
                    &mut legend,
                    format_args!("{} {} {RST} {}  ", class_paint(*c), c.name(), hundredths(share(*c))),
                );
            }
            push(&format_args!(
                "{DIM}{:>15}{RST} {SEP}│{RST}{stacked}{:pad$} {SEP}│{RST} {legend}",
                "mix",
                "",
                pad = width - used
            ));
        }
        TraceDash::Normal | TraceDash::Full => {
            let shown = if mode == TraceDash::Normal { 3 } else { CLASSES };
            for (i, c) in ranked.iter().take(shown).enumerate() {
                let filled = (share(*c) as usize * width).div_ceil(10_000);
                let (rx, tx) = (snap.mix[0].classes[c.index()], snap.mix[1].classes[c.index()]);
                push(&format_args!(
                    "{DIM}{:>15}{RST} {SEP}│{RST}{}{SEP}│{RST} {BOLD}{}{RST} {DIM}{} frames · rx {rx} · tx {tx}{}{RST}",
                    c.name(),
                    Bar(class_paint(*c), if count(*c) == 0 { 0 } else { filled.max(1) }, width),
                    hundredths(share(*c)),
                    count(*c),
                    if i == 0 { "  (since the graphs started)" } else { "" }
                ));
            }
        }
    }
}

/// Removes the dashboard.
pub fn dash_clear() {
    let rows = critical_section::with(|cs| DASH.borrow_ref(cs).len());
    if rows == 0 {
        return;
    }
    prompt_clear();
    critical_section::with(|cs| DASH.borrow_ref_mut(cs).clear());
    UNSCROLL.store(rows, Ordering::Relaxed);
    prompt_draw();
}

/// Time behind a received frame (`verbose wire`): since the peer's previous one, and queued in this node.
pub fn timing(delta_ms: Option<u64>, wait_us: u64) {
    let since: heapless::String<40> = match delta_ms {
        Some(d) => cell(format_args!("+{d} ms since the previous frame")),
        None => cell("first frame"),
    };
    out!(
        "{TRACE_INDENT}{DIM}{:<5}{RST}{SEP}│{RST} {DIM}{}{RST} {SEP}│{RST} {DIM}queued {}{RST}",
        "time",
        &*since,
        Us(wait_us)
    );
}

/// Frames the trace had no room to show.
pub fn trace_dropped(count: u32) {
    out!(
        "{WARN}trace: {count} frame(s) not shown, the console can't keep up. `trace events` or `dump 0` print less{RST}"
    );
}

// --- The traffic generator (`gen`) ----------------------------------------------------------------------

/// A duration of a stream: `850 ms`, `2.00 s`, `1 min 5 s`.
struct Dur(u64);

impl core::fmt::Display for Dur {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let us = self.0;
        match us {
            u if u < 1_000_000 => write!(f, "{}.{:01} ms", u / 1000, u % 1000 / 100),
            u if u < 60_000_000 => write!(f, "{}.{:02} s", u / 1_000_000, u % 1_000_000 / 10_000),
            u => write!(f, "{} min {} s", u / 60_000_000, u % 60_000_000 / 1_000_000),
        }
    }
}

/// `gen`: what the generator is doing, and the settings it has.
pub fn gen_status(st: &workshop_core::pktgen::Status) {
    use workshop_core::pktgen::{Limit, Rate, State};
    let _block = Block::new();
    let (mark, paint, word) = match st.state {
        State::Idle => ("○", DIM, "idle"),
        State::Running => ("●", OK, "running"),
        State::Paused => ("‖", WARN, "paused"),
        State::Done => ("✓", ACCENT, "done"),
    };
    let spec = st.spec;
    // The time it will really run: for the default limit, the length of the shape.
    let ends: heapless::String<64> = match (spec.limit, spec.limit_ms()) {
        (Limit::Forever, _) => cell("runs until stopped"),
        (Limit::Auto, Some(ms)) => {
            cell(format_args!("stops after {} {DIM}(the shape's length){RST}", Dur(u64::from(ms) * 1000)))
        }
        (limit @ Limit::Packets(_), _) => cell(format_args!("stops after {limit}")),
        (_, Some(ms)) => cell(format_args!("stops after {}", Dur(u64::from(ms) * 1000))),
        (_, None) => cell("runs until stopped"),
    };
    out!(
        "  {BOLD}gen{RST}      {SEP}│{RST} {BOLD}{paint}{mark} {word}{RST}  {DIM}rate{RST} {} · {DIM}size{RST} {} · {DIM}shape{RST} {} · {}",
        spec.rate,
        spec.size,
        spec.shape,
        &*ends
    );
    // What it aims at: where it starts, and where its shape goes.
    let target: heapless::String<64> = match spec.shape_target() {
        Some(to) => cell(format_args!(
            " {DIM}→{RST} {} {DIM}({} pkt/s){RST}",
            Rate::Bps(spec.bps_of(to)),
            spec.nominal_pps_of(to)
        )),
        None => cell(""),
    };
    out!(
        "  {:<8} {SEP}│{RST} {DIM}aims at{RST} {} {DIM}({} pkt/s){RST}{}  {DIM}on the wire, to port net.port + 1{RST}",
        "",
        Rate::Bps(spec.nominal_bps()),
        spec.nominal_pps(),
        &*target
    );
    if let Some(warning) = spec.warning() {
        out!("  {:<8} {SEP}│{RST} {WARN}⚠ {warning}{RST}", "");
    }
    if st.stream > 0 {
        out!(
            "  {:<8} {SEP}│{RST} {DIM}stream #{}: sent{RST} {} {DIM}datagrams,{RST} {} {DIM}in{RST} {} {DIM}· socket full {} times{RST}",
            "",
            st.stream,
            st.sent,
            Bytes(st.wire_bytes),
            Dur(st.run_us),
            st.blocked
        );
    }
}

/// A stream this node sent has ended.
pub fn gen_sent(now_ms: u64, s: &workshop_core::pktgen::TxSummary, capacity_bytes_per_s: u64) {
    use workshop_core::pktgen::Rate;
    let (sec, ms) = stamp(now_ms);
    let used = (s.bps() / 8 * 1000).checked_div(capacity_bytes_per_s).unwrap_or(0);
    out!(
        "{TIME}{sec:>6}.{ms:03}{RST} {TX}TX ▶{RST} {BOLD}gen #{}{RST} {OK}done{RST} {SEP}│{RST} {} datagrams, {} in {} {SEP}│{RST} {BOLD}{}{RST} · {} pkt/s {DIM}· {}.{}% of the link{}{RST}",
        s.stream,
        s.sent,
        Bytes(s.wire_bytes),
        Dur(s.run_us),
        Rate::Bps(s.bps()),
        s.pps(),
        used / 10,
        used % 10,
        if s.blocked > 0 { " · the socket was full at times: the rate asked for wasn't reached" } else { "" }
    );
}

/// A stream from the peer has ended (its end marker came, or it went quiet).
pub fn gen_received(now_ms: u64, s: &workshop_core::pktgen::RxSummary, capacity_bytes_per_s: u64) {
    use workshop_core::pktgen::Rate;
    let (sec, ms) = stamp(now_ms);
    let used = (s.bps() / 8 * 1000).checked_div(capacity_bytes_per_s).unwrap_or(0);
    let loss = s.loss_per_10000();
    let lost = if s.lost == 0 { DIM } else { ERROR };
    let note = if s.ended { "" } else { " · no end marker: datagrams lost at the very end aren't counted" };
    out!(
        "{TIME}{sec:>6}.{ms:03}{RST} {RX}RX ◀{RST} {BOLD}gen #{}{RST} {SEP}│{RST} {} received · {lost}lost {} ({}.{:02}%){RST} · {DIM}out of order{RST} {} · {DIM}jitter{RST} {} µs {SEP}│{RST} {BOLD}{}{RST} · {} pkt/s {DIM}· {}.{}% of the link · {}{note}{RST}",
        s.stream,
        s.received,
        s.lost,
        loss / 100,
        loss % 100,
        s.reordered,
        s.jitter_us,
        Rate::Bps(s.bps()),
        s.pps(),
        used / 10,
        used % 10,
        Dur(s.duration_us)
    );
}
