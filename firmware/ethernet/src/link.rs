//! The link to the peer: frames over UDP or TCP. The counterpart of the UART firmware's `link`.
//!
//! The network runs in its own task ([`udp_task`] or [`tcp_task`]); the node's main loop talks to
//! it through two queues, so the loop keeps the shape it has in the UART firmware (it sends a
//! frame and goes on, and receives what has arrived):
//!
//! ```text
//!  main loop ── Link::send ──▶ TX queue ──▶ task ──▶ UDP datagram / TCP stream ──▶ peer
//!  main loop ◀─ Link::recv ─── RX queue ◀── task ◀── (stamped when it arrived)  ◀── peer
//! ```
//!
//! A received frame is stamped by the task at the moment it left the socket, so the timing shown
//! by the console (`Δ`, round-trip time) doesn't include the time the main loop took to get to it.
//!
//! With UDP, one frame is one datagram: lost or reordered datagrams show in the `seq` check. With
//! TCP, frames are lines in a stream; the client retries until the server is there.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_futures::select::{Either, select};
use embassy_net::{
    IpAddress, IpEndpoint, Ipv4Address, Stack,
    tcp::TcpSocket,
    udp::{PacketMetadata, UdpSocket},
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer, with_timeout};
use workshop_core::{
    config::{NetLink, TcpRole, Transport},
    line::{LineBuffer, LineEvent},
    protocol::MAX_FRAME,
};

/// A frame line, without its `\n`, and when it arrived (µs since boot).
pub struct Packet {
    pub data: heapless::Vec<u8, MAX_FRAME>,
    pub at_us: u64,
}

/// Frames waiting to be sent. Deep enough for a burst slice (see `BURST_SLICE_US`).
const QUEUE: usize = 64;
/// Frames waiting for the main loop. TCP hands over a whole segment (about 15 frames) at once, and the
/// loop can be busy for a few milliseconds (RGB LED, console formatting), so this is deeper.
const RX_QUEUE: usize = 256;
/// A frame to send, and when the main loop queued it.
type Outgoing = (heapless::Vec<u8, MAX_FRAME>, u64);
static TX: Channel<CriticalSectionRawMutex, Outgoing, QUEUE> = Channel::new();
/// How long the last frame waited between the main loop and the stack's socket (µs).
static TX_WAIT_US: AtomicU32 = AtomicU32::new(0);
static RX: Channel<CriticalSectionRawMutex, Packet, RX_QUEUE> = Channel::new();

/// TCP: the connection is established (always true for UDP, which has none).
static CONNECTED: AtomicBool = AtomicBool::new(false);

static TX_FRAMES: AtomicU32 = AtomicU32::new(0);
static RX_FRAMES: AtomicU32 = AtomicU32::new(0);
static TX_BYTES: AtomicU32 = AtomicU32::new(0);
static RX_BYTES: AtomicU32 = AtomicU32::new(0);
/// Frames dropped because the main loop didn't take them in time (RX queue full).
static RX_DROPPED: AtomicU32 = AtomicU32::new(0);
/// TCP connections established / lost.
static CONNECTS: AtomicU32 = AtomicU32::new(0);
static DISCONNECTS: AtomicU32 = AtomicU32::new(0);

/// Same clock as the main loop's `now_us`, so frame stamps and loop times compare.
fn now_us() -> u64 {
    esp_hal::time::Instant::now().duration_since_epoch().as_micros()
}

/// Traffic counters, for `link` and the verbose console.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counters {
    pub tx_frames: u32,
    pub rx_frames: u32,
    pub tx_bytes: u32,
    pub rx_bytes: u32,
    pub rx_dropped: u32,
    /// Last frame: time from the main loop queuing it to the network task having handed it to the stack, µs.
    pub tx_wait_us: u32,
    pub connects: u32,
    pub disconnects: u32,
}

/// Why a frame wasn't queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// TCP: no connection to the peer (yet).
    NotConnected,
    /// The queue to the network task is full.
    Full,
}

/// The main loop's end of the link.
#[derive(Clone, Copy)]
pub struct Link {
    transport: Transport,
}

impl Link {
    pub fn new(net: &NetLink) -> Self {
        // UDP has no connection: it is always "connected".
        CONNECTED.store(net.transport == Transport::Udp, Ordering::Relaxed);
        Self { transport: net.transport }
    }

    /// Queues one frame line (`protocol::encode`, `\n` included).
    pub fn send(&self, line: &[u8]) -> Result<(), SendError> {
        if self.transport == Transport::Tcp && !self.connected() {
            return Err(SendError::NotConnected);
        }
        let frame = heapless::Vec::from_slice(line).map_err(|_| SendError::Full)?;
        TX.try_send((frame, now_us())).map_err(|_| SendError::Full)
    }

    /// Room for another frame (the burst sender checks before it takes the next frame's turn).
    pub fn can_send(&self) -> bool {
        !TX.is_full() && (self.transport == Transport::Udp || self.connected())
    }

    /// The oldest received frame, if any.
    pub fn recv(&self) -> Option<Packet> {
        RX.try_receive().ok()
    }

    /// TCP: whether the connection is up. UDP: always true.
    pub fn connected(&self) -> bool {
        CONNECTED.load(Ordering::Relaxed)
    }

    pub fn counters(&self) -> Counters {
        Counters {
            tx_frames: TX_FRAMES.load(Ordering::Relaxed),
            rx_frames: RX_FRAMES.load(Ordering::Relaxed),
            tx_bytes: TX_BYTES.load(Ordering::Relaxed),
            rx_bytes: RX_BYTES.load(Ordering::Relaxed),
            rx_dropped: RX_DROPPED.load(Ordering::Relaxed),
            tx_wait_us: TX_WAIT_US.load(Ordering::Relaxed),
            connects: CONNECTS.load(Ordering::Relaxed),
            disconnects: DISCONNECTS.load(Ordering::Relaxed),
        }
    }
}

fn peer_endpoint(net: &NetLink) -> IpEndpoint {
    IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::from(net.peer)), net.port)
}

/// Hands a received frame (no `\n`) to the main loop.
fn deliver(frame: &[u8], at_us: u64) {
    let Ok(data) = heapless::Vec::from_slice(frame) else { return };
    RX_FRAMES.fetch_add(1, Ordering::Relaxed);
    RX_BYTES.fetch_add(frame.len() as u32 + 1, Ordering::Relaxed);
    if RX.try_send(Packet { data, at_us }).is_err() {
        RX_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Counts a sent frame.
fn sent(frame: &[u8]) {
    TX_FRAMES.fetch_add(1, Ordering::Relaxed);
    TX_BYTES.fetch_add(frame.len() as u32, Ordering::Relaxed);
}

/// UDP: one frame per datagram, to the peer's port; only datagrams from the peer's address count.
#[embassy_executor::task]
pub async fn udp_task(stack: Stack<'static>, net: NetLink) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 128];
    let mut rx_buf = [0u8; 8192];
    let mut tx_meta = [PacketMetadata::EMPTY; 64];
    let mut tx_buf = [0u8; 8192];
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    socket.bind(net.port).expect("UDP port");
    let peer = peer_endpoint(&net);

    let mut datagram = [0u8; 256];
    loop {
        match select(socket.recv_from(&mut datagram), TX.receive()).await {
            Either::First(Ok((n, meta))) => {
                let at = now_us();
                if meta.endpoint.addr == peer.addr {
                    deliver(datagram[..n].trim_ascii_end(), at);
                }
            }
            Either::First(Err(_)) => {}
            Either::Second((frame, queued_us)) => {
                // Waits for room in the stack's queue: back-pressure on the main loop (`can_send`).
                if socket.send_to(&frame, peer).await.is_ok() {
                    TX_WAIT_US.store(now_us().saturating_sub(queued_us) as u32, Ordering::Relaxed);
                    sent(&frame);
                }
            }
        }
    }
}

/// TCP: one connection, frames are lines. The server accepts, the client connects and retries.
#[embassy_executor::task]
pub async fn tcp_task(stack: Stack<'static>, net: NetLink) -> ! {
    let mut rx = [0u8; 4096];
    let mut tx = [0u8; 4096];
    let peer = peer_endpoint(&net);
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        // A peer that vanished (cable pulled, reset) is noticed within seconds.
        socket.set_timeout(Some(Duration::from_secs(5)));
        socket.set_keep_alive(Some(Duration::from_secs(2)));
        let connected = match net.tcp_role {
            TcpRole::Server => socket.accept(net.port).await.is_ok(),
            TcpRole::Client => matches!(with_timeout(Duration::from_secs(3), socket.connect(peer)).await, Ok(Ok(()))),
        };
        if !connected {
            socket.abort();
            Timer::after(Duration::from_millis(500)).await;
            continue;
        }
        // Frames queued while there was no connection are stale: drop them.
        while TX.try_receive().is_ok() {}
        CONNECTED.store(true, Ordering::Relaxed);
        CONNECTS.fetch_add(1, Ordering::Relaxed);

        let mut lines = LineBuffer::<MAX_FRAME>::new();
        let mut chunk = [0u8; 512];
        'connection: loop {
            match select(socket.read(&mut chunk), TX.receive()).await {
                Either::First(Ok(0) | Err(_)) => break,
                Either::First(Ok(n)) => {
                    let at = now_us();
                    for &byte in &chunk[..n] {
                        if let Some(LineEvent::Line(line)) = lines.push(byte) {
                            deliver(line, at);
                        }
                    }
                }
                Either::Second((frame, queued_us)) => {
                    let mut written = 0;
                    while written < frame.len() {
                        match socket.write(&frame[written..]).await {
                            Ok(n) if n > 0 => written += n,
                            _ => break 'connection,
                        }
                    }
                    TX_WAIT_US.store(now_us().saturating_sub(queued_us) as u32, Ordering::Relaxed);
                    sent(&frame);
                }
            }
        }
        CONNECTED.store(false, Ordering::Relaxed);
        DISCONNECTS.fetch_add(1, Ordering::Relaxed);
        socket.abort();
        Timer::after(Duration::from_millis(200)).await;
    }
}
