//! ADC streaming mode: samples one analog pin at a fixed rate and sends the samples to a computer
//! over UDP, for a live spectrum (`tools/adc-fft/adc_fft.py`).
//!
//! ```text
//!   GPIO43 ──▶ SAR ADC1 ──▶ DMA ring ([`crate::dma`]) ──▶ main loop ──▶ UDP datagrams ──▶ computer
//!              (its own timer              (no CPU)             ▲                │
//!               triggers each                             `start` / `stop`  ◀────┘ (ASCII datagrams)
//!               conversion)
//! ```
//!
//! **Sampling** is the ESP32-S31's continuous mode: the ADC's digital controller triggers a conversion
//! every `timer_target` cycles of the 40 MHz crystal and a DMA channel stores the results, so the
//! sampling instants are exactly regular and the CPU only copies finished buffers into a ring (this
//! loop, between two network events). The chip allows 611 Hz to **83 333 Hz**. (The one-shot mode of
//! esp-hal takes about 57 µs per conversion and was capped at 10 kS/s.)
//!
//! **The protocol** (UDP, port [`PORT`]) is driven by the computer, so the board needs to know
//! nothing about it:
//!
//! - the computer sends `start rate=20000 n=256` (every second, as a keep-alive) to the board;
//! - the board answers `ok ...` and streams datagrams to whoever sent `start`, until it has heard
//!   nothing for [`IDLE_STOP_MS`] or receives `stop`.
//!
//! A sample datagram is [`HEADER`] bytes then `count` samples, all little-endian:
//!
//! | bytes | field | |
//! |---|---|---|
//! | 0..4 | magic | `ADC1` |
//! | 4..8 | `seq` | `u32`, +1 per datagram: a jump is a lost datagram |
//! | 8..12 | `rate_mhz` | `u32`, the exact sample rate in millihertz (the timer divides 40 MHz by integers) |
//! | 12..16 | `dropped` | `u32`, times samples were lost on the board since `start` (ring or DMA full) |
//! | 16..18 | `count` | `u16`, number of samples |
//! | 18..20 | `pin` | `u16`, GPIO number |
//! | 20.. | samples | `count` × `u16`, raw conversion codes |
//!
//! The board also sends its log to the computer that started the stream, as `LOG1` datagrams (see
//! [`board_log`]): the page shows them in a terminal, the history of the boot included. That terminal
//! also types commands, sent to the same port as ASCII datagrams; the answer comes back as `RPL1`:
//!
//! | command | does |
//! |---|---|
//! | `info` | six lines: chip, build, ADC, network, stream, uptime (the node firmware's `info`, compact) |
//! | `verbose [on\|off]` | one detail line per second (timings, ring level, DMA), without slowing the stream |
//! | `pwm [freq=HZ] [duty=%] [on\|off]` | the PWM generator on GPIO20 ([`crate::pwm`]): `pwm 1000 25` is 1 kHz at 25 %; no argument tells its state |
//! | `pot [off\|freq\|duty] [min=N] [max=N]` | the potentiometer on GPIO44 ([`crate::pot`]) sets the generator's frequency (Hz) or duty cycle (%) between `min` and `max`; no argument tells its state |
//! | `reboot` | software reset of the chip |
//! | `help` | the list above |
//!
//! While the potentiometer is on, the board also sends a `POT1` datagram ten times a second ([`pot::encode`]: the
//! mode, the knob's position, the value it gave the generator), and the samples come at half the conversion rate.
//!
//! The S31's SAR is differential: 0 V reads about [`ZERO_CODE`] and the full scale is
//! [`FULL_SCALE`], so the computer removes the mean before the FFT anyway.

use core::sync::atomic::{AtomicU16, AtomicU32, AtomicUsize, Ordering};

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{
    IpEndpoint,
    udp::{PacketMetadata, UdpSocket},
};
use embassy_time::{Duration as Wait, Instant as Clock, Timer};
use esp_hal::{
    clock::{ClockConfig, CpuClock, ll::MpllClkConfig},
    rng::Rng,
    timer::timg::TimerGroup,
};
use workshop_core::config::NetLink;

use esp32_rust_workshop_eth::net;

use crate::{
    blog,
    board_log::{self, Level},
    dma::{self, AdcDma},
    pot::{self, Mode},
    pwm::{self, Pwm},
    pwm_plan,
};

/// UDP port of the board.
pub const PORT: u16 = 7100;
/// The board's address (static, no DHCP). Not the nodes' (`.11`, `.12`): both can be cabled to the
/// same switch.
pub const ADDRESS: [u8; 4] = [192, 168, 77, 20];
/// The analog input. GPIO43 is `ADC1_CH1` on the ESP32-S31 and is free on the CoreBoard.
pub const PIN: u16 = 43;

/// Sample rate limits, Hz: the digital controller's (`SOC_ADC_SAMPLE_FREQ_THRES_*`).
pub const MIN_RATE_HZ: u32 = dma::MIN_RATE_HZ;
pub const MAX_RATE_HZ: u32 = dma::MAX_RATE_HZ;
pub const DEFAULT_RATE_HZ: u32 = 20_000;
/// Samples per datagram: `HEADER + 2 n` has to fit in one Ethernet frame (1472 bytes of UDP payload).
pub const MAX_COUNT: usize = 700;
pub const DEFAULT_COUNT: usize = 256;
/// Bytes before the samples in a datagram.
pub const HEADER: usize = 20;
/// The board stops streaming when it hasn't heard from the computer for this long.
pub const IDLE_STOP_MS: u64 = 3_000;

/// A reading of an input tied to ground (the SAR is differential), and the largest code.
pub const ZERO_CODE: u16 = 2198;
pub const FULL_SCALE: u16 = 4393;

/// Ring buffer between the DMA drain and the datagrams: 16384 samples, 0.2 s at 83 kS/s. A power of two,
/// indexed by free-running counters.
const RING: usize = 16384;
static BUF: [AtomicU16; RING] = [const { AtomicU16::new(0) }; RING];
static HEAD: AtomicUsize = AtomicUsize::new(0);
static TAIL: AtomicUsize = AtomicUsize::new(0);
/// Times samples were lost since `start`: the ring was full, or the DMA ran out of buffers.
static DROPPED: AtomicU32 = AtomicU32::new(0);

/// Appends one sample to the ring.
fn push(value: u16) {
    let head = HEAD.load(Ordering::Relaxed);
    if head.wrapping_sub(TAIL.load(Ordering::Relaxed)) >= RING {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    } else {
        BUF[head % RING].store(value, Ordering::Relaxed);
        HEAD.store(head.wrapping_add(1), Ordering::Release);
    }
}

/// Samples waiting in the ring.
fn available() -> usize {
    HEAD.load(Ordering::Acquire).wrapping_sub(TAIL.load(Ordering::Relaxed))
}

/// What a `start` asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Request {
    rate_hz: u32,
    count: usize,
}

/// Parses `start [rate=HZ] [n=N]` (values out of range are clamped, unknown words ignored).
/// `None` for anything that isn't `start`.
fn parse_start(text: &str) -> Option<Request> {
    let mut words = text.split_ascii_whitespace();
    if words.next()? != "start" {
        return None;
    }
    let mut req = Request { rate_hz: DEFAULT_RATE_HZ, count: DEFAULT_COUNT };
    for word in words {
        let Some((key, value)) = word.split_once('=') else { continue };
        let Ok(value) = value.parse::<u32>() else { continue };
        match key {
            "rate" => req.rate_hz = value.clamp(MIN_RATE_HZ, MAX_RATE_HZ),
            "n" => req.count = (value as usize).clamp(16, MAX_COUNT),
            _ => {}
        }
    }
    Some(req)
}

/// What a datagram from the computer asks.
#[derive(Debug, PartialEq)]
enum Command {
    Start(Request),
    Stop,
    Info,
    /// `verbose`: toggle; `verbose on` / `verbose off`.
    Verbose(Option<bool>),
    /// `pwm ...`; `None` when the arguments make no sense.
    Pwm(Option<PwmArgs>),
    /// `pot ...`; `None` when the arguments make no sense.
    Pot(Option<pot::Change>),
    Reboot,
    Help,
    Unknown,
}

/// What a `pwm` command changes (nothing at all: it asks for the state).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PwmArgs {
    freq: Option<f32>,
    duty: Option<f32>,
    on: Option<bool>,
}

/// `pwm [freq=HZ] [duty=PCT] [on|off]`, or the numbers alone: `pwm 1000 25`.
fn parse_pwm<'a>(words: impl Iterator<Item = &'a str>) -> Option<PwmArgs> {
    let mut args = PwmArgs::default();
    let mut bare = 0;
    for word in words {
        match word {
            "on" => args.on = Some(true),
            "off" => args.on = Some(false),
            _ => {
                let number = |text: &str| text.parse::<f32>().ok().filter(|v| v.is_finite());
                match word.split_once('=') {
                    Some(("freq" | "f" | "hz", v)) => args.freq = Some(number(v)?),
                    Some(("duty" | "d", v)) => args.duty = Some(number(v)?),
                    Some(_) => return None,
                    None => {
                        match bare {
                            0 => args.freq = Some(number(word)?),
                            1 => args.duty = Some(number(word)?),
                            _ => return None,
                        }
                        bare += 1;
                    }
                }
            }
        }
    }
    Some(args)
}

fn parse(text: &str) -> Command {
    if let Some(req) = parse_start(text) {
        return Command::Start(req);
    }
    let mut words = text.split_ascii_whitespace();
    match words.next() {
        Some("stop") => Command::Stop,
        Some("info") => Command::Info,
        Some("reboot") => Command::Reboot,
        Some("pwm") => Command::Pwm(parse_pwm(words)),
        Some("pot") => Command::Pot(pot::parse(words)),
        Some("help") => Command::Help,
        Some("verbose") => match words.next() {
            None => Command::Verbose(None),
            Some("on" | "1") => Command::Verbose(Some(true)),
            Some("off" | "0") => Command::Verbose(Some(false)),
            Some(_) => Command::Unknown,
        },
        _ => Command::Unknown,
    }
}

/// Sends the answer to a command: on the console and, as an `RPL1` datagram, to whoever asked.
async fn say(socket: &UdpSocket<'_>, to: IpEndpoint, level: Level, args: core::fmt::Arguments<'_>) {
    let mut out = [0u8; 13 + board_log::TEXT];
    let len = board_log::reply(level, args, &mut out);
    let _ = socket.send_to(&out[..len], to).await;
}

/// What `pot` answers: the mode, its range, and the generator's other number (the one the knob leaves alone).
async fn say_pot(socket: &UdpSocket<'_>, to: IpEndpoint, config: &pot::Config, generator: Option<pwm::State>) {
    let (low, high) = config.range();
    let pin = pot::PIN;
    match (config.mode, generator) {
        (Mode::Off, _) => {
            say(
                socket,
                to,
                Level::Reply,
                format_args!("pot off · GPIO{pin} not read · `pot freq` or `pot duty` turns it on"),
            )
            .await
        }
        (Mode::Freq, Some(s)) => {
            say(
                socket,
                to,
                Level::Reply,
                format_args!("pot freq · {low} to {high} Hz · GPIO{pin} · the duty stays {:.1} %", s.duty),
            )
            .await
        }
        (Mode::Duty, Some(s)) => {
            say(
                socket,
                to,
                Level::Reply,
                format_args!("pot duty · {low} to {high} % · GPIO{pin} · the frequency stays {:.1} Hz", s.asked_hz),
            )
            .await
        }
        (mode, None) => {
            say(
                socket,
                to,
                Level::Warn,
                format_args!("pot {}: there is no generator to drive in this firmware", mode.name()),
            )
            .await
        }
    }
}

/// What the `info` command reports about the stream (the rest it reads from the chip).
struct StreamInfo {
    pot_mode: Mode,
    client: Option<(IpEndpoint, Request)>,
    rate_mhz: u32,
    packets: u32,
    lost: u32,
    stalls: u32,
}

/// `info`: the node firmware's boot card, in six lines that fit the page's terminal.
async fn info(socket: &UdpSocket<'_>, to: IpEndpoint, s: &StreamInfo) {
    use Level::Reply;
    let rev = esp_hal::efuse::chip_revision();
    let mac = net::mac_address();
    let [a, b, c, d] = ADDRESS;
    let phy = net::link_state();
    let up = Clock::now().as_secs();
    let pot_mode = s.pot_mode;

    say(
        socket,
        to,
        Reply,
        format_args!(
            "chip   ESP32-S31 v{}.{} · {} MHz · MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            rev.major,
            rev.minor,
            esp_hal::clock::cpu_clock().as_mhz(),
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5]
        ),
    )
    .await;
    match esp_hal::system::reset_reason() {
        Some(reason) => {
            say(
                socket,
                to,
                Reply,
                format_args!(
                    "build  v{} · git {} · {} · last reset {reason:?}",
                    env!("CARGO_PKG_VERSION"),
                    env!("BUILD_GIT"),
                    env!("BUILD_DATE")
                ),
            )
            .await
        }
        None => {
            say(
                socket,
                to,
                Reply,
                format_args!(
                    "build  v{} · git {} · {}",
                    env!("CARGO_PKG_VERSION"),
                    env!("BUILD_GIT"),
                    env!("BUILD_DATE")
                ),
            )
            .await
        }
    }
    say(
        socket,
        to,
        Reply,
        format_args!(
            "adc    GPIO{PIN} (ADC1 ch{}) · DMA continuous · {MIN_RATE_HZ}-{MAX_RATE_HZ} Hz · 0 V = {ZERO_CODE}, full {FULL_SCALE} · pot GPIO{} {}",
            dma::CHANNEL,
            pot::PIN,
            pot_mode.name()
        ),
    )
    .await;
    if phy.up {
        say(
            socket,
            to,
            Reply,
            format_args!(
                "net    {a}.{b}.{c}.{d}:{PORT} · link {} Mbit/s {}",
                phy.mbps,
                if phy.full_duplex { "full duplex" } else { "half duplex" }
            ),
        )
        .await;
    } else {
        say(socket, to, Reply, format_args!("net    {a}.{b}.{c}.{d}:{PORT} · link down")).await;
    }
    match s.client {
        Some((endpoint, req)) => {
            say(
                socket,
                to,
                Reply,
                format_args!(
                    "stream {:.1} Hz · {} per datagram · to {endpoint} · {} sent · lost {} · stalls {}",
                    s.rate_mhz as f32 / 1000.0,
                    req.count,
                    s.packets,
                    s.lost,
                    s.stalls
                ),
            )
            .await
        }
        None => {
            say(socket, to, Reply, format_args!("stream idle · waiting for a `start` · DMA stalls {}", s.stalls)).await
        }
    }
    say(
        socket,
        to,
        Reply,
        format_args!(
            "system up {}:{:02}:{:02} · verbose {} · log history {} lines",
            up / 3600,
            up / 60 % 60,
            up % 60,
            if board_log::verbose() { "on" } else { "off" },
            board_log::len()
        ),
    )
    .await;
}

/// What one second of streaming looked like, for the verbose line. Updated with a few integer operations
/// per pass (no allocation, no formatting, no I/O); the line is written once a second.
struct Window {
    since: Clock,
    datagrams: u32,
    burst_max: u32,
    send_max_us: u32,
    pass_max_us: u32,
    ring_max: usize,
    passes: u32,
    log_lines: u32,
    buffers_before: u32,
    stalls_before: u32,
}

impl Window {
    fn new(buffers: u32, stalls: u32) -> Self {
        Self {
            since: Clock::now(),
            datagrams: 0,
            burst_max: 0,
            send_max_us: 0,
            pass_max_us: 0,
            ring_max: 0,
            passes: 0,
            log_lines: 0,
            buffers_before: buffers,
            stalls_before: stalls,
        }
    }
}

/// The firmware's `main`: network up, then serve the computer; the ADC is set up at the first `start`.
pub async fn run(spawner: Spawner) -> ! {
    // 1 Gbit/s needs the MPLL at 500 MHz (same as the node firmware).
    let mut clocks = ClockConfig::from(CpuClock::max());
    clocks.mpll_clk = Some(MpllClkConfig::_500);
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(clocks));
    esp_rtos::start(TimerGroup::new(p.TIMG1).timer0);

    // The ADC and its DMA are set up at the first `start`, once the network is known to work: that way
    // an ADC problem can't be confused with a network one.
    let mut adc_parts = Some((p.GPIO43, p.GPIO44, p.ADC1));
    let mut adc: Option<AdcDma> = None;
    // The potentiometer: what it does, its averaged reading, and when the generator last followed it. `adc_pot` is
    // whether the running conversions include its pin.
    let mut pot_config = pot::Config::DEFAULT;
    let mut knob = pot::Filter::new();
    let mut follower = pot::Follower::default();
    let mut adc_pot = false;
    let mut pot_followed_at = Clock::now();
    let mut pot_sent_at = Clock::now();
    // The PWM generator (RMT channel 0 on GPIO20): idle (low) until the page asks.
    let mut pwm = Pwm::new(p.RMT, p.GPIO20);

    // trng::enable() is the node's business: the seed only picks TCP/UDP port numbers here.
    let rng = Rng::new();
    let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());
    let link = NetLink { address: ADDRESS, ..NetLink::DEFAULT };
    let (stack, runner) = net::start(
        net::PhyPeripherals {
            eth: p.ETH,
            mdc: p.GPIO5,
            mdio: p.GPIO6,
            reset: p.GPIO7,
            txd0: p.GPIO8,
            txd1: p.GPIO9,
            txd2: p.GPIO10,
            txd3: p.GPIO11,
            tx_ctl: p.GPIO12,
            tx_clk: p.GPIO13,
            rx_clk: p.GPIO14,
            rx_ctl: p.GPIO15,
            rxd3: p.GPIO16,
            rxd2: p.GPIO17,
            rxd1: p.GPIO18,
            rxd0: p.GPIO19,
        },
        &link,
        seed,
    )
    .await;
    spawner.spawn(net::net_task(runner).unwrap());

    // The boot banner: on the console, and kept in the log history for the computer that connects later.
    let [a, b, c, d] = ADDRESS;
    let mac = net::mac_address();
    blog!(
        Level::Info,
        "adc-stream · ESP32-S31 · MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    blog!(Level::Info, "build {} · {}", env!("BUILD_GIT"), env!("BUILD_DATE"));
    blog!(
        Level::Info,
        "GPIO{PIN} (ADC1 channel {}) · continuous mode with DMA · {MIN_RATE_HZ} to {MAX_RATE_HZ} Hz",
        dma::CHANNEL
    );
    blog!(Level::Info, "UDP {a}.{b}.{c}.{d}:{PORT} · static address, no DHCP");
    blog!(
        Level::Info,
        "potentiometer on GPIO{} (ADC1 channel {}) · off until `pot freq` or `pot duty`",
        pot::PIN,
        pot::CHANNEL
    );
    if pwm.is_some() {
        blog!(
            Level::Info,
            "PWM generator on GPIO{} (RMT) · {} Hz to {} kHz · off until `pwm`",
            pwm::PIN,
            pwm_plan::MIN_HZ,
            pwm_plan::MAX_HZ / 1000.0
        );
    } else {
        blog!(Level::Warn, "PWM generator: the RMT could not be set up");
    }

    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 32];
    let mut tx_buf = [0u8; 16 * 1536];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    socket.bind(PORT).expect("UDP port");
    blog!(Level::Info, "waiting for the Ethernet link…");

    let mut command = [0u8; 128];
    let mut datagram = [0u8; HEADER + 2 * MAX_COUNT];
    let mut log_datagram = [0u8; 13 + board_log::TEXT];
    // Who gets the samples, what they asked for, and when we last heard from them.
    let mut client: Option<(IpEndpoint, Request)> = None;
    let mut rate_mhz = 0u32;
    let mut last_heard = Clock::now();
    let mut seq = 0u32;
    let mut packets = 0u32;
    // The next log line to send to the client.
    let mut log_sent = board_log::oldest_seq();
    let mut link = (false, 0u16);
    let mut stat_at = Clock::now();
    let mut stat_packets = 0u32;
    let mut loss_at = Clock::now();
    let mut loss_reported = 0u32;
    let mut idle_at = Clock::now();
    let mut window = Window::new(0, 0);

    loop {
        // Commands from the computer, between two bursts of samples.
        match select(socket.recv_from(&mut command), Timer::after(Wait::from_millis(2))).await {
            Either::First(Ok((n, meta))) => {
                let text = core::str::from_utf8(&command[..n]).unwrap_or("").trim();
                let cmd = parse(text);
                if !matches!(cmd, Command::Start(_)) {
                    // (`start` comes every second as the keep-alive: not worth a line.)
                    blog!(Level::Debug, "command {text:?} from {}", meta.endpoint);
                }
                match cmd {
                    Command::Start(req) => {
                        last_heard = Clock::now();
                        // A different request (or a different computer) restarts the stream; the same one is the keep-alive.
                        // So does the potentiometer being turned on or off: the ADC's conversions change.
                        let pot_on = pot_config.mode != Mode::Off;
                        if client != Some((meta.endpoint, req)) || adc_pot != pot_on {
                            let new_client = client.map(|(endpoint, _)| endpoint) != Some(meta.endpoint);
                            if let Some((gpio, pot_gpio, adc1)) = adc_parts.take() {
                                adc = Some(AdcDma::new(gpio, pot_gpio, adc1));
                                blog!(Level::Info, "ADC1 and the LP-GDMA channel set up");
                            }
                            if let Some(adc) = adc.as_mut() {
                                rate_mhz = adc.start(req.rate_hz, pot_on);
                            }
                            adc_pot = pot_on;
                            knob.reset();
                            follower.reset();
                            client = Some((meta.endpoint, req));
                            seq = 0;
                            TAIL.store(HEAD.load(Ordering::Relaxed), Ordering::Relaxed);
                            DROPPED.store(0, Ordering::Relaxed);
                            loss_reported = 0;
                            packets = 0;
                            stat_packets = 0;
                            stat_at = Clock::now();
                            let pps = rate_mhz as f32 / 1000.0 / req.count as f32;
                            let mbit = pps * (HEADER + 2 * req.count) as f32 * 8.0 / 1e6;
                            blog!(
                                Level::Ok,
                                "{} {} · {:.1} Hz (asked {}) · {} samples/datagram · {:.0} datagrams/s · {:.2} Mbit/s",
                                if new_client { "client" } else { "request from" },
                                meta.endpoint,
                                rate_mhz as f32 / 1000.0,
                                req.rate_hz,
                                req.count,
                                pps,
                                mbit
                            );
                            if pot_on {
                                blog!(
                                    Level::Info,
                                    "potentiometer {} on GPIO{}: the conversions alternate, the signal is sampled at {:.1} Hz",
                                    pot_config.mode.name(),
                                    pot::PIN,
                                    rate_mhz as f32 / 1000.0
                                );
                            }
                            if new_client {
                                // A new reader gets the history first (the boot banner included).
                                log_sent = board_log::oldest_seq();
                            }
                        }
                        let mut reply = heapless::String::<256>::new();
                        let _ = core::fmt::Write::write_fmt(
                            &mut reply,
                            format_args!(
                                "ok rate_mhz={rate_mhz} n={} pin={PIN} zero={ZERO_CODE} full={FULL_SCALE} verbose={}",
                                req.count,
                                u8::from(board_log::verbose())
                            ),
                        );
                        if let Some(pwm) = pwm.as_ref() {
                            let s = pwm.state();
                            let _ = core::fmt::Write::write_fmt(
                                &mut reply,
                                format_args!(
                                    " pwm={} pwm_hz={:.4} pwm_duty={:.3} pwm_pin={}",
                                    u8::from(s.on),
                                    s.asked_hz,
                                    s.duty,
                                    pwm::PIN
                                ),
                            );
                        }
                        let (pot_low, pot_high) = pot_config.range();
                        let _ = core::fmt::Write::write_fmt(
                            &mut reply,
                            format_args!(
                                " pot={} pot_pin={} pot_lo={pot_low} pot_hi={pot_high}",
                                pot_config.mode.name(),
                                pot::PIN
                            ),
                        );
                        let _ = socket.send_to(reply.as_bytes(), meta.endpoint).await;
                    }
                    Command::Stop => {
                        if client.is_some() {
                            client = None;
                            if let Some(adc) = adc.as_mut() {
                                adc.stop();
                            }
                            blog!(Level::Info, "stream stopped by the computer ({packets} datagrams sent)");
                        }
                    }
                    Command::Info => {
                        let s = StreamInfo {
                            pot_mode: pot_config.mode,
                            client,
                            rate_mhz,
                            packets,
                            lost: DROPPED.load(Ordering::Relaxed),
                            stalls: adc.as_ref().map_or(0, |a| a.stalls),
                        };
                        info(&socket, meta.endpoint, &s).await;
                    }
                    Command::Verbose(want) => {
                        let on = want.unwrap_or(!board_log::verbose());
                        board_log::set_verbose(on);
                        window =
                            Window::new(adc.as_ref().map_or(0, |a| a.buffers), adc.as_ref().map_or(0, |a| a.stalls));
                        let text = if on { "verbose on: one detail line per second" } else { "verbose off" };
                        say(&socket, meta.endpoint, Level::Reply, format_args!("{text}")).await;
                    }
                    Command::Pwm(args) => {
                        let Some(pwm) = pwm.as_mut() else {
                            say(
                                &socket,
                                meta.endpoint,
                                Level::Warn,
                                format_args!("pwm: the generator is not available"),
                            )
                            .await;
                            continue;
                        };
                        let Some(args) = args else {
                            say(
                                &socket,
                                meta.endpoint,
                                Level::Warn,
                                format_args!("pwm [freq=HZ] [duty=%] [on|off] (or `pwm 1000 25`)"),
                            )
                            .await;
                            continue;
                        };
                        let now = pwm.state();
                        let changes = args.freq.is_some() || args.duty.is_some();
                        let freq = args.freq.unwrap_or(now.asked_hz);
                        let duty = args.duty.unwrap_or(now.duty);
                        let on = args.on.unwrap_or(changes || now.on);
                        if changes || args.on.is_some() {
                            match pwm.apply(freq, duty, on) {
                                Ok(_) => {}
                                Err(why) => {
                                    say(
                                        &socket,
                                        meta.endpoint,
                                        Level::Error,
                                        format_args!(
                                            "pwm: {why} ({} Hz to {} kHz)",
                                            pwm_plan::MIN_HZ,
                                            pwm_plan::MAX_HZ / 1000.0
                                        ),
                                    )
                                    .await;
                                    continue;
                                }
                            }
                        }
                        let s = pwm.state();
                        if s.on {
                            say(
                                &socket,
                                meta.endpoint,
                                Level::Reply,
                                format_args!(
                                    "pwm on · {:.4} Hz · duty {:.3} % · GPIO{} · period {} ticks of {} ns",
                                    s.hz,
                                    s.duty,
                                    pwm::PIN,
                                    s.period,
                                    u32::from(s.divider) * 25 / 2
                                ),
                            )
                            .await;
                        } else {
                            say(
                                &socket,
                                meta.endpoint,
                                Level::Reply,
                                format_args!(
                                    "pwm off · GPIO{} low · set to {:.4} Hz, duty {:.3} %",
                                    pwm::PIN,
                                    s.asked_hz,
                                    s.duty
                                ),
                            )
                            .await;
                        }
                    }
                    Command::Pot(change) => {
                        let Some(change) = change else {
                            say(
                                &socket,
                                meta.endpoint,
                                Level::Warn,
                                format_args!("pot [off|freq|duty] [min=N] [max=N] (Hz for freq, % for duty)"),
                            )
                            .await;
                            continue;
                        };
                        if let Err(why) = pot_config.apply(&change, pwm_plan::MIN_HZ, pwm_plan::MAX_HZ) {
                            say(&socket, meta.endpoint, Level::Error, format_args!("pot: {why}")).await;
                            continue;
                        }
                        if change != pot::Change::default() {
                            // The next reading is followed whatever it is. Turning the knob on starts the wave:
                            // there is nothing to see on the input otherwise.
                            follower.reset();
                            if let (Some(pwm), true) =
                                (pwm.as_mut(), pot_config.mode != Mode::Off && change.mode.is_some())
                            {
                                let s = pwm.state();
                                let _ = pwm.apply(s.asked_hz, s.duty, true);
                            }
                        }
                        say_pot(&socket, meta.endpoint, &pot_config, pwm.as_ref().map(Pwm::state)).await;
                    }
                    Command::Reboot => {
                        say(&socket, meta.endpoint, Level::Reply, format_args!("rebooting…")).await;
                        if let Some(adc) = adc.as_mut() {
                            adc.stop();
                        }
                        // Let the stack send the answer (and the console print it) before the reset.
                        Timer::after(Wait::from_millis(150)).await;
                        esp_hal::system::software_reset();
                    }
                    Command::Help => {
                        say(
                            &socket,
                            meta.endpoint,
                            Level::Reply,
                            format_args!(
                                "commands: info · verbose [on|off] · pwm [freq=HZ] [duty=%] [on|off] · pot [off|freq|duty] [min=N] [max=N] · reboot · help"
                            ),
                        )
                        .await;
                    }
                    Command::Unknown => {
                        say(
                            &socket,
                            meta.endpoint,
                            Level::Warn,
                            format_args!("{n} bytes from {}: {text:?}: not a command (try `help`)", meta.endpoint),
                        )
                        .await;
                    }
                }
            }
            Either::First(Err(_)) | Either::Second(()) => {}
        }

        // How long this pass takes, without the wait for a command (the verbose line's `pass`).
        let pass_start = Clock::now();

        // Finished DMA buffers into the ring.
        if let Some(adc) = adc.as_mut()
            && adc.drain(push, |code| knob.push(code))
        {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }

        // The cable.
        let phy = net::link_state();
        if (phy.up, phy.mbps) != link {
            link = (phy.up, phy.mbps);
            if phy.up {
                blog!(
                    Level::Ok,
                    "link up · {} Mbit/s · {} duplex",
                    phy.mbps,
                    if phy.full_duplex { "full" } else { "half" }
                );
            } else {
                blog!(Level::Warn, "link down: is the cable plugged in?");
            }
        }

        let Some((endpoint, req)) = client else {
            if idle_at.elapsed() > Wait::from_secs(10) {
                idle_at = Clock::now();
                blog!(Level::Stat, "waiting for a computer · UDP {a}.{b}.{c}.{d}:{PORT} · link {} Mbit/s", phy.mbps);
            }
            continue;
        };
        if last_heard.elapsed() > Wait::from_millis(IDLE_STOP_MS) {
            client = None;
            if let Some(adc) = adc.as_mut() {
                adc.stop();
            }
            blog!(
                Level::Warn,
                "no keep-alive from {endpoint} for {} s: stream stopped ({packets} datagrams sent)",
                IDLE_STOP_MS / 1000
            );
            continue;
        }

        // The potentiometer: the generator follows the knob when it has moved, and the page is told where it is.
        if adc_pot && pot_followed_at.elapsed() > Wait::from_millis(40) {
            pot_followed_at = Clock::now();
            if let Some(position) = knob.position(ZERO_CODE, FULL_SCALE) {
                if follower.moved(position)
                    && let (Some(pwm), Some(value)) = (pwm.as_mut(), pot_config.value(position))
                {
                    let s = pwm.state();
                    if s.on {
                        let (freq, duty) =
                            if pot_config.mode == Mode::Duty { (s.asked_hz, value) } else { (value, s.duty) };
                        let _ = pwm.apply(freq, duty, true);
                    }
                }
                if pot_sent_at.elapsed() > Wait::from_millis(100) {
                    pot_sent_at = Clock::now();
                    let given = pwm.as_ref().map_or(0.0, |pwm| {
                        let s = pwm.state();
                        if pot_config.mode == Mode::Duty { s.duty } else { s.hz }
                    });
                    let datagram = pot::encode(pot_config.mode, position, knob.code().unwrap_or(0.0), given);
                    let _ = socket.send_to(&datagram, endpoint).await;
                }
            }
        }

        // Losses are said as they happen (at most once a second), not only in the statistics.
        let losses = DROPPED.load(Ordering::Relaxed);
        if losses != loss_reported && loss_at.elapsed() > Wait::from_secs(1) {
            loss_at = Clock::now();
            loss_reported = losses;
            blog!(
                Level::Warn,
                "samples lost: {losses} since the start ({} DMA stalls): the network or the computer is too slow",
                adc.as_ref().map_or(0, |a| a.stalls)
            );
        }

        // The log, to the computer: a few lines per pass, so the samples keep priority.
        for _ in 0..6 {
            let Some(line) = board_log::line_from(log_sent) else { break };
            let len = board_log::encode(&line, b"LOG1", &mut log_datagram);
            if socket.send_to(&log_datagram[..len], endpoint).await.is_err() {
                break;
            }
            log_sent = line.seq.wrapping_add(1);
            window.log_lines += 1;
        }

        // Whole datagrams of `count` samples as long as the ring has them.
        window.ring_max = window.ring_max.max(available());
        let mut burst = 0u32;
        while available() >= req.count {
            let tail = TAIL.load(Ordering::Relaxed);
            datagram[0..4].copy_from_slice(b"ADC1");
            datagram[4..8].copy_from_slice(&seq.to_le_bytes());
            datagram[8..12].copy_from_slice(&rate_mhz.to_le_bytes());
            datagram[12..16].copy_from_slice(&DROPPED.load(Ordering::Relaxed).to_le_bytes());
            datagram[16..18].copy_from_slice(&(req.count as u16).to_le_bytes());
            datagram[18..20].copy_from_slice(&PIN.to_le_bytes());
            for i in 0..req.count {
                let v = BUF[tail.wrapping_add(i) % RING].load(Ordering::Relaxed);
                datagram[HEADER + 2 * i..HEADER + 2 * i + 2].copy_from_slice(&v.to_le_bytes());
            }
            // The ring is only released once the stack has taken the datagram: if the stack is full, the
            // ring and the DMA buffers absorb the wait, and `dropped` counts what they couldn't.
            let sent_at = Clock::now();
            if socket.send_to(&datagram[..HEADER + 2 * req.count], endpoint).await.is_err() {
                break;
            }
            window.send_max_us = window.send_max_us.max(sent_at.elapsed().as_micros() as u32);
            burst += 1;
            TAIL.store(tail.wrapping_add(req.count), Ordering::Relaxed);
            seq = seq.wrapping_add(1);
            packets = packets.wrapping_add(1);
        }

        window.datagrams += burst;
        window.burst_max = window.burst_max.max(burst);
        window.passes += 1;
        window.pass_max_us = window.pass_max_us.max(pass_start.elapsed().as_micros() as u32);

        // Verbose: one line a second with what the counters above saw (nothing is formatted otherwise).
        if board_log::verbose() && window.since.elapsed() > Wait::from_secs(1) {
            let (buffers, stalls) = adc.as_ref().map_or((0, 0), |a| (a.buffers, a.stalls));
            blog!(
                Level::Debug,
                "{} dgrams · burst {} · send ≤{} µs · pass ≤{} µs · ring ≤{} % · {} passes · DMA {} bufs {} stalls · log {}",
                window.datagrams,
                window.burst_max,
                window.send_max_us,
                window.pass_max_us,
                window.ring_max * 100 / RING,
                window.passes,
                buffers.wrapping_sub(window.buffers_before),
                stalls.wrapping_sub(window.stalls_before),
                window.log_lines
            );
            window = Window::new(buffers, stalls);
        }

        // Statistics every 2 s (one dim line, replaced by the next in the page).
        let since = stat_at.elapsed();
        if since > Wait::from_secs(2) {
            let seconds = since.as_millis() as f32 / 1000.0;
            let pps = packets.wrapping_sub(stat_packets) as f32 / seconds;
            let mbit = pps * (HEADER + 2 * req.count) as f32 * 8.0 / 1e6;
            stat_at = Clock::now();
            stat_packets = packets;
            blog!(
                Level::Stat,
                "{:.1} kS/s · {:.0} datagrams/s · {:.2} Mbit/s · {packets} sent · lost {} · ring {} %",
                rate_mhz as f32 / 1e6,
                pps,
                mbit,
                DROPPED.load(Ordering::Relaxed),
                available() * 100 / RING
            );
        }
    }
}
