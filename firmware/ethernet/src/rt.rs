//! The Ethernet app on Ferrite, the project's own RTOS (feature `ferrite`): no async code.
//!
//! The Embassy runtime runs everything as futures on one executor. Here the same work is three
//! ordinary tasks, switched by the kernel (`firmware/ferrite-esp32s31`):
//!
//! ```text
//!   node task     the main loop: node.step() forever (CLI, buttons, frames, LEDs)
//!   net task      owns the EMAC and smoltcp: polls the stack, and moves frames between the sockets and
//!                 the TX/RX queues that the node's `Link` reads and writes
//!   console task  moves the print pipe to the USB FIFO
//! ```
//!
//! All three have the same priority and give the processor up with `yield_now` on each pass, so they
//! take turns the way the futures did; the kernel's time slice is the backstop for a task that
//! forgets to. The queues, the counters and the node code are exactly the ones the Embassy runtime
//! uses: only what waited with `.await` is different.
//!
//! The EMAC driver is used in its blocking mode (`receive`, `transmit`, `poll_link`), and wrapped as a
//! smoltcp [`Device`]; smoltcp is polled by the net task, there is no interrupt-driven wake-up yet
//! (the task polls on every turn, which is also what keeps the round-trip time short).

use core::{cell::UnsafeCell, sync::atomic::Ordering};

use esp_hal::{
    Blocking,
    clock::{CpuClock, ll::MpllClkConfig},
    ethernet::{Ethernet, RgmiiPinBundle, mac::Duplex, mac::Speed, phy::generic::GenericPhy},
    gpio::{Level, Output, OutputConfig},
    rng::Rng,
    timer::timg::TimerGroup,
    usb::usb_serial_jtag::UsbSerialJtag,
};
use ferrite_esp32s31 as os;
use os::{Priority, Stack};
use smoltcp::{
    iface::{Config, Interface, PollResult, SocketHandle, SocketSet, SocketStorage},
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    socket::{tcp, udp},
    time::{Duration, Instant as SmolInstant},
    wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address},
};
use static_cell::StaticCell;
use workshop_core::{
    config::{NetLink, TcpRole, Transport},
    line::{LineBuffer, LineEvent},
    protocol::MAX_FRAME,
};

use super::*;
use crate::link::{self, Outgoing};

static NODE_STACK: Stack<{ 32 * 1024 }> = Stack::new();
static NET_STACK: Stack<{ 16 * 1024 }> = Stack::new();
static CONSOLE_STACK: Stack<{ 6 * 1024 }> = Stack::new();

/// What the net task needs that only `run` can hand over.
struct NetBoot {
    phy: net::PhyPeripherals,
    link: NetLink,
    seed: u64,
}

/// A static cell the tasks take their one-time argument from.
struct Handoff<T>(UnsafeCell<Option<T>>);

// SAFETY: written once by `run` before the kernel starts, taken once by the task that owns it.
unsafe impl<T> Sync for Handoff<T> {}

impl<T> Handoff<T> {
    const fn new() -> Self {
        Self(UnsafeCell::new(None))
    }
    fn put(&'static self, value: T) -> *mut () {
        // SAFETY: see `Sync` above: single writer, before the tasks run.
        unsafe { *self.0.get() = Some(value) };
        self as *const Self as *mut ()
    }
    /// # Safety
    /// `arg` must come from [`put`](Self::put) of this very cell, and be taken once.
    unsafe fn take(arg: *mut ()) -> T {
        let cell = unsafe { &*(arg as *const Self) };
        unsafe { (*cell.0.get()).take().expect("the task's argument was already taken") }
    }
}

static NODE_ARG: Handoff<Node> = Handoff::new();
static NET_ARG: Handoff<NetBoot> = Handoff::new();
static CONSOLE_ARG: Handoff<esp_hal::usb::usb_serial_jtag::UsbSerialJtagTx<'static, Blocking>> = Handoff::new();

/// Boots the node on Ferrite. Never returns.
pub fn run(defaults: &'static NodeConfig) -> ! {
    let mut cfg = *defaults;
    let net_cfg = cfg.net();
    rtt_console::init();
    rtt_console::set_route(cfg.console);

    // 1 Gbit/s needs the MPLL at 500 MHz (the HAL default of 400 MHz caps RGMII at 100 Mbit/s).
    let mut clocks = esp_hal::clock::ClockConfig::from(CpuClock::max());
    if net_cfg.max_speed >= 1000 {
        clocks.mpll_clk = Some(MpllClkConfig::_500);
    }
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(clocks));

    let (usb_rx, usb_tx) = UsbSerialJtag::new(p.USB_DEVICE).split();

    let mut store = settings::Store::new(p.FLASH);
    let boot_load = store.load(&mut cfg);
    rtt_console::set_route(cfg.console);
    let pins = cfg.pins;

    trng::enable();
    let rng = Rng::new();
    let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());

    let phy = net::PhyPeripherals {
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
    };
    let link = Link::new(&net_cfg);

    let gpio = button::Gpio::new(p.IO_MUX);
    let button = Button::new(&gpio, Source::Jumper, pin(pins.button), ms(cfg.debounce_ms), ms(cfg.lockout_ms));
    let boot = Button::new(&gpio, Source::Boot, p.GPIO61, ms(cfg.debounce_ms), ms(cfg.lockout_ms));
    let leds = ExtLeds::new([(pin(pins.led1), pins.led1), (pin(pins.led2), pins.led2)], [cfg.led1, cfg.led2]);
    let rgb_led = led::Led::new(p.RMT, p.GPIO60);
    let fx = fx::Animator::new(fx_settings(&cfg.rgb));

    term::set_verbosity(cfg.verbose);
    term::boot_card(&cfg, &chip_info(&rng), &system_info(&cfg, defaults, boot_load));
    let cli = cli::Cli::new(usb_rx);

    let node = Node {
        cfg,
        defaults,
        link,
        button,
        leds,
        rgb_led,
        fx,
        rng,
        cli,
        stats: Stats::default(),
        peer: Peer::default(),
        seq: 0,
        events_sent: 0,
        last_data: Instant::now(),
        dat_clock: DatClock::new(),
        last_status: Instant::now(),
        seen_peer_up: None,
        subs: Subscriptions::new(),
        browse: None,
        help: None,
        rtt: RttTracker::new(),
        burst_id: 0,
        burst_tx: None,
        burst_rx: None,
        store,
        boot_load,
        _gpio: gpio,
        boot,
        seen_phy: None,
        seen_connected: link.connected(),
        seen_dropped: 0,
        loops: 0,
        loop_max_us: 0,
        watchdog: Watchdog::start(p.TIMG0),
    };

    let prio = Priority::new(4).unwrap();
    os::spawn("console", prio, CONSOLE_STACK.take().unwrap(), console_task, CONSOLE_ARG.put(usb_tx)).unwrap();
    os::spawn("net", prio, NET_STACK.take().unwrap(), net_task, NET_ARG.put(NetBoot { phy, link: net_cfg, seed }))
        .unwrap();
    os::spawn("node", prio, NODE_STACK.take().unwrap(), node_task, NODE_ARG.put(node)).unwrap();
    os::start(TimerGroup::new(p.TIMG1).timer0)
}

// --- The tasks --------------------------------------------------------------------------------------

extern "C" fn node_task(arg: *mut ()) -> ! {
    // SAFETY: `arg` is `NODE_ARG`, taken here once.
    let mut node = unsafe { NODE_ARG_TAKE(arg) };
    loop {
        let started = Instant::now();
        node.step();
        node.loop_max_us = node.loop_max_us.max(started.elapsed().as_micros() as u32);
        os::yield_now();
    }
}

#[allow(non_snake_case)]
unsafe fn NODE_ARG_TAKE(arg: *mut ()) -> Node {
    unsafe { Handoff::<Node>::take(arg) }
}

extern "C" fn console_task(arg: *mut ()) -> ! {
    // SAFETY: `arg` is `CONSOLE_ARG`, taken here once.
    let mut tx = unsafe { Handoff::<esp_hal::usb::usb_serial_jtag::UsbSerialJtagTx<'static, Blocking>>::take(arg) };
    loop {
        if !console::pump(&mut tx) {
            os::yield_now();
        }
    }
}

// --- The network ------------------------------------------------------------------------------------

type Eth = Ethernet<'static, Blocking, GenericPhy>;

/// The EMAC as a smoltcp device. A received frame is copied out of the DMA ring at once, so the ring slot is
/// free again while smoltcp reads it.
struct EthDevice {
    eth: Eth,
    rx: [u8; 1536],
    rx_len: usize,
}

struct EthRx<'a>(&'a [u8]);
struct EthTx<'a>(&'a mut Eth);

impl RxToken for EthRx<'_> {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(self.0)
    }
}

impl TxToken for EthTx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = [0u8; 1536];
        let len = len.min(frame.len());
        let result = f(&mut frame[..len]);
        // A full TX ring drops the frame: TCP resends it, UDP counts it as lost, like on a real wire.
        let _ = self.0.transmit(&frame[..len]);
        result
    }
}

impl Device for EthDevice {
    type RxToken<'a> = EthRx<'a>;
    type TxToken<'a> = EthTx<'a>;

    fn receive(&mut self, _: SmolInstant) -> Option<(EthRx<'_>, EthTx<'_>)> {
        let len = match self.eth.receive() {
            Ok(frame) if frame.len() <= self.rx.len() => {
                let len = frame.len();
                self.rx[..len].copy_from_slice(frame);
                len
            }
            Ok(_) => 0, // too big for the buffer: dropped
            Err(_) => return None,
        };
        self.eth.pop_rx();
        self.rx_len = len;
        (len > 0).then(|| (EthRx(&self.rx[..len]), EthTx(&mut self.eth)))
    }

    fn transmit(&mut self, _: SmolInstant) -> Option<EthTx<'_>> {
        Some(EthTx(&mut self.eth))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(16);
        // Checksums are computed and verified by smoltcp (the default): correct whatever the MAC does.
        caps
    }
}

fn smol_now() -> SmolInstant {
    SmolInstant::from_micros(link::now_us() as i64)
}

fn peer_endpoint(net: &NetLink) -> IpEndpoint {
    IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::from(net.peer)), net.port)
}

/// How often the PHY is read (each read is several slow MDIO transactions), ms.
const LINK_EVERY_MS: u64 = 500;

/// Reads what the PHY negotiated, and sets the MAC to match.
fn check_link(eth: &mut Eth) {
    let state = eth.poll_link(None);
    if state.up {
        eth.set_speed(state.speed);
        eth.set_duplex(state.duplex);
        let mbps = match state.speed {
            Speed::_10M => 10,
            Speed::_100M => 100,
            Speed::_1000M => 1000,
            _ => 0,
        };
        net::store_link(mbps, state.duplex == Duplex::Full);
    } else {
        net::store_link(0, false);
    }
}

fn start_ethernet(p: net::PhyPeripherals) -> Eth {
    // PHY reset (GPIO7): low for 100 ms, then 300 ms for it to come up.
    let mut reset = Output::new(p.reset, Level::Low, OutputConfig::default());
    os::delay_ms(100);
    reset.set_high();
    os::delay_ms(300);
    net::keep_reset_high(reset);
    Ethernet::new(
        p.eth,
        net::dma_storage(),
        net::mac_address(),
        // The PHY's address on the MDIO bus is found by scanning.
        GenericPhy::new_auto(),
        RgmiiPinBundle {
            rx_clk: p.rx_clk,
            tx_clk: p.tx_clk,
            rx_ctl: p.rx_ctl,
            tx_ctl: p.tx_ctl,
            rxd0: p.rxd0,
            rxd1: p.rxd1,
            rxd2: p.rxd2,
            rxd3: p.rxd3,
            txd0: p.txd0,
            txd1: p.txd1,
            txd2: p.txd2,
            txd3: p.txd3,
            mdc: p.mdc,
            mdio: p.mdio,
        },
    )
    .expect("Ethernet init failed")
}

static SOCKETS: StaticCell<[SocketStorage<'static>; 2]> = StaticCell::new();
static UDP_RX_META: StaticCell<[udp::PacketMetadata; 128]> = StaticCell::new();
static UDP_TX_META: StaticCell<[udp::PacketMetadata; 64]> = StaticCell::new();
static UDP_RX: StaticCell<[u8; 8192]> = StaticCell::new();
static UDP_TX: StaticCell<[u8; 8192]> = StaticCell::new();
static TCP_RX: StaticCell<[u8; 4096]> = StaticCell::new();
static TCP_TX: StaticCell<[u8; 4096]> = StaticCell::new();

extern "C" fn net_task(arg: *mut ()) -> ! {
    // SAFETY: `arg` is `NET_ARG`, taken here once.
    let NetBoot { phy, link: net, seed } = unsafe { Handoff::<NetBoot>::take(arg) };
    let eth = start_ethernet(phy);

    let mut device = EthDevice { eth, rx: [0; 1536], rx_len: 0 };
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(net::mac_address())));
    config.random_seed = seed;
    let mut iface = Interface::new(config, &mut device, smol_now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(Ipv4Address::from(net.address)), net.prefix_len));
    });
    let mut sockets = SocketSet::new(&mut SOCKETS.init([SocketStorage::EMPTY; 2])[..]);

    let mut service: Service = match net.transport {
        Transport::Udp => {
            let rx = udp::PacketBuffer::new(
                &mut UDP_RX_META.init([udp::PacketMetadata::EMPTY; 128])[..],
                &mut UDP_RX.init([0; 8192])[..],
            );
            let tx = udp::PacketBuffer::new(
                &mut UDP_TX_META.init([udp::PacketMetadata::EMPTY; 64])[..],
                &mut UDP_TX.init([0; 8192])[..],
            );
            let mut socket = udp::Socket::new(rx, tx);
            socket.bind(net.port).expect("UDP port");
            Service::Udp { handle: sockets.add(socket), pending: None }
        }
        Transport::Tcp => {
            let rx = tcp::SocketBuffer::new(&mut TCP_RX.init([0; 4096])[..]);
            let tx = tcp::SocketBuffer::new(&mut TCP_TX.init([0; 4096])[..]);
            Service::Tcp(TcpLink {
                handle: sockets.add(tcp::Socket::new(rx, tx)),
                pending: None,
                lines: LineBuffer::new(),
                up: false,
                last_attempt_ms: 0,
                local_port: 49152 + (seed % 16000) as u16,
            })
        }
    };

    let mut last_link_ms = 0;
    check_link(&mut device.eth);
    loop {
        let now_ms = link::now_us() / 1000;
        if now_ms.wrapping_sub(last_link_ms) >= LINK_EVERY_MS {
            last_link_ms = now_ms;
            check_link(&mut device.eth);
        }
        // Ingress, then the sockets' turn, then whatever they queued goes out.
        while iface.poll(smol_now(), &mut device, &mut sockets) == PollResult::SocketStateChanged {}
        service.run(&mut sockets, &mut iface, &net, now_ms);
        iface.poll(smol_now(), &mut device, &mut sockets);
        os::yield_now();
    }
}

/// What the net task does with its socket on each turn.
enum Service {
    Udp { handle: SocketHandle, pending: Option<Outgoing> },
    Tcp(TcpLink),
}

struct TcpLink {
    handle: SocketHandle,
    /// A frame taken from the queue that the socket had no room for yet.
    pending: Option<Outgoing>,
    lines: LineBuffer<MAX_FRAME>,
    up: bool,
    last_attempt_ms: u64,
    local_port: u16,
}

impl Service {
    fn run(&mut self, sockets: &mut SocketSet<'_>, iface: &mut Interface, net: &NetLink, now_ms: u64) {
        match self {
            Service::Udp { handle, pending } => udp_turn(sockets.get_mut::<udp::Socket>(*handle), pending, net),
            Service::Tcp(tcp_link) => tcp_turn(tcp_link, sockets, iface, net, now_ms),
        }
    }
}

/// UDP: one frame per datagram, to the peer's port; only datagrams from the peer's address count.
fn udp_turn(socket: &mut udp::Socket<'_>, pending: &mut Option<Outgoing>, net: &NetLink) {
    let peer = peer_endpoint(net);
    while let Ok((data, meta)) = socket.recv() {
        let at = link::now_us();
        if meta.endpoint.addr == peer.addr {
            link::deliver(data.trim_ascii_end(), at);
        }
    }
    while socket.can_send() {
        let Some((frame, queued_us)) = pending.take().or_else(|| link::TX.try_receive().ok()) else { break };
        link::TX_PICKUP_US.store(link::now_us().saturating_sub(queued_us) as u32, Ordering::Relaxed);
        match socket.send_slice(&frame, peer) {
            Ok(()) => {
                link::TX_WAIT_US.store(link::now_us().saturating_sub(queued_us) as u32, Ordering::Relaxed);
                link::sent(&frame);
            }
            // No room in the stack's queue yet: back-pressure on the main loop (`can_send`).
            Err(udp::SendError::BufferFull) => {
                *pending = Some((frame, queued_us));
                break;
            }
            Err(_) => {}
        }
    }
}

/// TCP: one connection, frames are lines. The server listens, the client connects and retries.
fn tcp_turn(l: &mut TcpLink, sockets: &mut SocketSet<'_>, iface: &mut Interface, net: &NetLink, now_ms: u64) {
    let socket = sockets.get_mut::<tcp::Socket>(l.handle);
    let established = socket.state() == tcp::State::Established;

    if l.up && !established {
        // The connection ended (peer gone, reset, timeout).
        l.up = false;
        link::CONNECTED.store(false, Ordering::Relaxed);
        link::DISCONNECTS.fetch_add(1, Ordering::Relaxed);
        socket.abort();
        l.last_attempt_ms = now_ms;
        return;
    }
    if !l.up && !established {
        let state = socket.state();
        // A try that didn't get anywhere in 3 s is dropped; between tries, 500 ms.
        if state == tcp::State::SynSent && now_ms.wrapping_sub(l.last_attempt_ms) > 3000 {
            socket.abort();
            l.last_attempt_ms = now_ms;
        } else if state == tcp::State::Closed && now_ms.wrapping_sub(l.last_attempt_ms) >= 500 {
            // A peer that vanished (cable pulled, reset) is noticed within seconds.
            socket.set_timeout(Some(Duration::from_secs(5)));
            socket.set_keep_alive(Some(Duration::from_secs(2)));
            l.last_attempt_ms = now_ms;
            match net.tcp_role {
                TcpRole::Server => {
                    let _ = socket.listen(net.port);
                }
                TcpRole::Client => {
                    let _ = socket.connect(iface.context(), peer_endpoint(net), l.local_port);
                }
            }
        }
        return;
    }
    if !l.up {
        // Just connected. Frames queued while there was no connection are stale: drop them.
        l.up = true;
        l.pending = None;
        l.lines = LineBuffer::new();
        while link::TX.try_receive().is_ok() {}
        link::CONNECTED.store(true, Ordering::Relaxed);
        link::CONNECTS.fetch_add(1, Ordering::Relaxed);
    }

    // Receive: bytes to lines, each stamped as it leaves the socket.
    let at = link::now_us();
    while socket.can_recv() {
        let lines = &mut l.lines;
        let got = socket.recv(|bytes| {
            for &byte in bytes.iter() {
                if let Some(LineEvent::Line(line)) = lines.push(byte) {
                    link::deliver(line, at);
                }
            }
            (bytes.len(), ())
        });
        if got.is_err() {
            break;
        }
    }
    // Send: whole frames only.
    while socket.may_send() {
        let Some((frame, queued_us)) = l.pending.take().or_else(|| link::TX.try_receive().ok()) else { break };
        link::TX_PICKUP_US.store(link::now_us().saturating_sub(queued_us) as u32, Ordering::Relaxed);
        if socket.send_capacity() - socket.send_queue() < frame.len() {
            l.pending = Some((frame, queued_us));
            break;
        }
        match socket.send_slice(&frame) {
            Ok(n) if n == frame.len() => {
                link::TX_WAIT_US.store(link::now_us().saturating_sub(queued_us) as u32, Ordering::Relaxed);
                link::sent(&frame);
            }
            _ => break,
        }
    }
}
