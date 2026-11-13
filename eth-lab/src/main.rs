//! Gigabit Ethernet on the ESP32-S31-Function-CoreBoard-1.
//!
//! The board's RJ45 is wired to a Motorcomm YT8531 PHY over RGMII. This firmware brings the
//! link up, gets an address by DHCP (or falls back to a link-local one for a direct cable),
//! and serves a small status page on port 80:
//!
//! ```text
//! curl http://<ip>/
//! ```
//!
//! External parts (same GPIOs as the workshop firmware, see docs/book/wiring.md):
//!
//! * GPIO20, LED 1: lit while the Ethernet link is up;
//! * GPIO21, LED 2: blinks on every HTTP request and every button edge;
//! * GPIO4, jumper to GND: read by the status page.
//!
//! GPIO5-19 belong to the PHY (MDC 5, MDIO 6, reset 7, RGMII 8-19): nothing else may use them.
#![no_std]
#![no_main]

use core::{
    fmt::Write as _,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use embassy_executor::Spawner;
use embassy_net::{
    Config, ConfigV4, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4, tcp::TcpSocket,
};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_backtrace as _;
use esp_hal::{
    Async,
    clock::CpuClock,
    efuse,
    ethernet::{Ethernet, EthernetDmaStorage, RgmiiPinBundle, mac::Duplex, mac::Speed, phy::generic::GenericPhy},
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    rng::Rng,
    timer::timg::TimerGroup,
};
use esp_println::println;
use heapless::String;
use static_cell::{ConstStaticCell, StaticCell};

esp_bootloader_esp_idf::esp_app_desc!();

/// How long to wait for a DHCP lease before using a link-local address (direct cable).
const DHCP_TIMEOUT: Duration = Duration::from_secs(15);

type EthDriver = Ethernet<'static, Async, GenericPhy>;

static DMA: ConstStaticCell<EthernetDmaStorage<10, 10>> = ConstStaticCell::new(EthernetDmaStorage::new());
static RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();

// State shared between the tasks (all plain atomics: no locks needed).
static REQUESTS: AtomicU32 = AtomicU32::new(0);
static BUTTON_EDGES: AtomicU32 = AtomicU32::new(0);
static BUTTON_PRESSED: AtomicBool = AtomicBool::new(false);
/// Link speed in Mbit/s and duplex, as negotiated at boot (0 = no link then).
static LINK_MBPS: AtomicU32 = AtomicU32::new(0);
static LINK_FULL_DUPLEX: AtomicBool = AtomicBool::new(false);
static LINK_LOCAL: AtomicBool = AtomicBool::new(false);

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let timg0 = TimerGroup::new(p.TIMG0);
    esp_rtos::start(timg0.timer0);

    println!("\n== eth-lab: ESP32-S31 CoreBoard RJ45 (YT8531, RGMII) ==");

    // External LEDs and the jumper button (GPIO20/21/4, as in the workshop wiring).
    let link_led = Output::new(p.GPIO20, Level::Low, OutputConfig::default());
    let activity_led = Output::new(p.GPIO21, Level::Low, OutputConfig::default());
    let button = Input::new(p.GPIO4, InputConfig::default().with_pull(Pull::Up));

    // PHY reset (GPIO7): low for 100 ms, then 300 ms for it to come up.
    let mut phy_reset = Output::new(p.GPIO7, Level::Low, OutputConfig::default());
    Timer::after(Duration::from_millis(100)).await;
    phy_reset.set_high();
    Timer::after(Duration::from_millis(300)).await;

    // MAC address: the chip's unique base MAC, +3 for the Ethernet interface as ESP-IDF does.
    let mut mac = [0u8; 6];
    mac.copy_from_slice(efuse::base_mac_address().as_bytes());
    mac[5] = mac[5].wrapping_add(3);
    println!("MAC {}", Mac(mac));

    let mut eth = Ethernet::new(
        p.ETH,
        DMA.take(),
        mac,
        GenericPhy::new_auto(),
        RgmiiPinBundle {
            rx_clk: p.GPIO14,
            tx_clk: p.GPIO13,
            rx_ctl: p.GPIO15,
            tx_ctl: p.GPIO12,
            rxd0: p.GPIO19,
            rxd1: p.GPIO18,
            rxd2: p.GPIO17,
            rxd3: p.GPIO16,
            txd0: p.GPIO8,
            txd1: p.GPIO9,
            txd2: p.GPIO10,
            txd3: p.GPIO11,
            mdc: p.GPIO5,
            mdio: p.GPIO6,
        },
    )
    .expect("Ethernet init failed");

    // Read the negotiated speed once, before the driver goes to embassy-net (which hides it).
    println!("Waiting for the link (plug a cable into the RJ45)...");
    let t0 = Instant::now();
    let link = loop {
        let link = eth.poll_link(None);
        if link.up || t0.elapsed() > Duration::from_secs(10) {
            break link;
        }
        Timer::after(Duration::from_millis(100)).await;
    };
    if link.up {
        let mbps = match link.speed {
            Speed::_10M => 10,
            Speed::_100M => 100,
            Speed::_1000M => 1000,
            _ => 0,
        };
        LINK_MBPS.store(mbps, Ordering::Relaxed);
        LINK_FULL_DUPLEX.store(link.duplex == Duplex::Full, Ordering::Relaxed);
        println!("Link up: {} Mbit/s, {:?} duplex (after {} ms)", mbps, link.duplex, t0.elapsed().as_millis());
    } else {
        println!("No link yet: the stack keeps polling, plug a cable any time.");
    }

    let rng = Rng::new();
    let seed = u64::from(rng.random()) << 32 | u64::from(rng.random());
    let (stack, runner) = embassy_net::new(
        eth.into_async(),
        Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );

    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(leds_task(stack, link_led, activity_led).unwrap());
    spawner.spawn(button_task(button).unwrap());

    stack.wait_link_up().await;
    println!("Waiting for a DHCP lease ({} s max)...", DHCP_TIMEOUT.as_secs());
    if with_timeout(DHCP_TIMEOUT, stack.wait_config_up()).await.is_err() {
        // No DHCP server (cable straight to a computer or another board): link-local address.
        let addr = Ipv4Address::new(169, 254, mac[4].clamp(1, 254), mac[5].clamp(1, 254));
        println!("No DHCP answer: using the link-local address {addr}/16");
        LINK_LOCAL.store(true, Ordering::Relaxed);
        stack.set_config_v4(ConfigV4::Static(StaticConfigV4 {
            address: Ipv4Cidr::new(addr, 16),
            gateway: None,
            dns_servers: heapless::Vec::new(),
        }));
    }
    if let Some(cfg) = stack.config_v4() {
        println!("IP {}  gateway {:?}", cfg.address, cfg.gateway);
        println!("Status page: http://{}/", cfg.address.address());
    }

    spawner.spawn(http_task(0, stack).unwrap());
    spawner.spawn(http_task(1, stack).unwrap());
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, EthDriver>) -> ! {
    runner.run().await
}

/// LED 1 follows the link; LED 2 blinks when a request or a button edge happened.
#[embassy_executor::task]
async fn leds_task(stack: Stack<'static>, mut link_led: Output<'static>, mut activity_led: Output<'static>) -> ! {
    let mut seen = 0;
    loop {
        link_led.set_level(stack.is_link_up().into());
        let activity = REQUESTS.load(Ordering::Relaxed).wrapping_add(BUTTON_EDGES.load(Ordering::Relaxed));
        if activity != seen {
            seen = activity;
            activity_led.set_high();
            Timer::after(Duration::from_millis(80)).await;
            activity_led.set_low();
        }
        Timer::after(Duration::from_millis(40)).await;
    }
}

#[embassy_executor::task]
async fn button_task(mut button: Input<'static>) -> ! {
    loop {
        BUTTON_PRESSED.store(button.is_low(), Ordering::Relaxed);
        button.wait_for_any_edge().await;
        BUTTON_EDGES.fetch_add(1, Ordering::Relaxed);
        // Debounce, then loop to read the settled level.
        Timer::after(Duration::from_millis(20)).await;
    }
}

/// A minimal HTTP/1.0 server: every request gets the status page.
#[embassy_executor::task(pool_size = 2)]
async fn http_task(id: u8, stack: Stack<'static>) -> ! {
    let mut rx = [0u8; 1024];
    let mut tx = [0u8; 2048];
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        socket.set_timeout(Some(Duration::from_secs(5)));
        if socket.accept(80).await.is_err() {
            continue;
        }
        // We don't parse the request: read what arrived so the peer is not reset, then answer.
        let mut request = [0u8; 256];
        let _ = socket.read(&mut request).await;
        let n = REQUESTS.fetch_add(1, Ordering::Relaxed) + 1;

        let mut body: String<640> = String::new();
        status_page(&mut body, stack, id, n);
        let mut head: String<128> = String::new();
        let _ = write!(
            head,
            "HTTP/1.0 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        for part in [head.as_bytes(), body.as_bytes()] {
            let mut sent = 0;
            while sent < part.len() {
                match socket.write(&part[sent..]).await {
                    Ok(0) | Err(_) => break,
                    Ok(k) => sent += k,
                }
            }
        }
        let _ = socket.flush().await;
        socket.close();
        Timer::after(Duration::from_millis(50)).await;
        socket.abort();
    }
}

fn status_page(out: &mut String<640>, stack: Stack<'static>, task: u8, request: u32) {
    let up = Instant::now().as_secs();
    let _ = writeln!(out, "ESP32-S31 CoreBoard, on-board RJ45 (YT8531, RGMII)");
    let _ = writeln!(out, "uptime      {}h {:02}m {:02}s", up / 3600, up / 60 % 60, up % 60);
    let mbps = LINK_MBPS.load(Ordering::Relaxed);
    let duplex = if LINK_FULL_DUPLEX.load(Ordering::Relaxed) { "full" } else { "half" };
    let _ = if mbps == 0 {
        writeln!(out, "link        {} (speed unknown: no link at boot)", if stack.is_link_up() { "up" } else { "down" })
    } else {
        writeln!(
            out,
            "link        {} at boot: {mbps} Mbit/s, {duplex} duplex",
            if stack.is_link_up() { "up" } else { "down" }
        )
    };
    if let Some(cfg) = stack.config_v4() {
        let how = if LINK_LOCAL.load(Ordering::Relaxed) { "link-local" } else { "DHCP" };
        let _ = writeln!(out, "address     {} ({how})", cfg.address);
        if let Some(gw) = cfg.gateway {
            let _ = writeln!(out, "gateway     {gw}");
        }
    }
    if let Some(mac) = stack.hardware_address().as_bytes().get(..6) {
        let _ = writeln!(out, "mac         {}", Mac([mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]]));
    }
    let _ = writeln!(
        out,
        "button      {} ({} edges)",
        if BUTTON_PRESSED.load(Ordering::Relaxed) { "pressed" } else { "released" },
        BUTTON_EDGES.load(Ordering::Relaxed)
    );
    let _ = writeln!(out, "request     #{request} (served by task {task})");
}

struct Mac([u8; 6]);

impl core::fmt::Display for Mac {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let m = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}
