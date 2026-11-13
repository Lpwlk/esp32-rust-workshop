//! Board-to-board traffic over the Ethernet cable, with static addresses (no switch, no DHCP).
//!
//! * the **initiator** (host `.11`) sends 20 UDP pings to the peer, prints the round-trip times
//!   and the loss, then pushes TCP data for 2 s and prints the throughput; and repeats;
//! * the **responder** (every other board) echoes the UDP packets back and swallows the TCP data.
//!
//! Ports: UDP echo 7777 (responder), UDP client 7778 (initiator), TCP sink 7779 (responder).
use core::sync::atomic::{AtomicU32, Ordering};

use embassy_net::{
    IpAddress, IpEndpoint, Ipv4Address, Stack,
    tcp::TcpSocket,
    udp::{PacketMetadata, UdpSocket},
};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_println::println;

/// Host number of the board that starts the tests, and of the one it talks to.
pub const INITIATOR_HOST: u8 = 11;
pub const PEER_HOST: u8 = 12;

const UDP_ECHO_PORT: u16 = 7777;
const UDP_CLIENT_PORT: u16 = 7778;
const TCP_SINK_PORT: u16 = 7779;

const PINGS_PER_ROUND: u32 = 20;
const PING_INTERVAL: Duration = Duration::from_millis(50);
const PING_TIMEOUT: Duration = Duration::from_millis(200);
const TCP_BURST: Duration = Duration::from_secs(2);

// Results, for the console and the status page. Plain 32-bit atomics (this core has no 64-bit ones).
/// Packets exchanged so far (drives the activity LED).
pub static ACTIVITY: AtomicU32 = AtomicU32::new(0);
/// Last ping round: sent, received, and round-trip time in microseconds (min / average / max).
pub static PING_SENT: AtomicU32 = AtomicU32::new(0);
pub static PING_RECV: AtomicU32 = AtomicU32::new(0);
pub static RTT_MIN_US: AtomicU32 = AtomicU32::new(0);
pub static RTT_AVG_US: AtomicU32 = AtomicU32::new(0);
pub static RTT_MAX_US: AtomicU32 = AtomicU32::new(0);
/// Last TCP burst, in kbit/s (0 = none yet or failed).
pub static TCP_KBPS: AtomicU32 = AtomicU32::new(0);
/// Responder side: UDP packets echoed, TCP bytes swallowed.
pub static ECHOED: AtomicU32 = AtomicU32::new(0);
pub static SUNK_BYTES: AtomicU32 = AtomicU32::new(0);

fn endpoint(host: Ipv4Address, port: u16) -> IpEndpoint {
    IpEndpoint::new(IpAddress::Ipv4(host), port)
}

/// Sends pings and runs TCP bursts to `peer`, forever.
#[embassy_executor::task]
pub async fn initiator_task(stack: Stack<'static>, peer: Ipv4Address) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 256];
    let mut udp = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    udp.bind(UDP_CLIENT_PORT).expect("UDP bind");

    let mut seq = 0u32;
    loop {
        stack.wait_link_up().await;
        ping_round(&udp, peer, &mut seq).await;
        tcp_burst(stack, peer).await;
        Timer::after(Duration::from_millis(500)).await;
    }
}

async fn ping_round(udp: &UdpSocket<'_>, peer: Ipv4Address, seq: &mut u32) {
    let dest = endpoint(peer, UDP_ECHO_PORT);
    let (mut received, mut min, mut max, mut sum) = (0u32, u32::MAX, 0u32, 0u32);

    for _ in 0..PINGS_PER_ROUND {
        *seq = seq.wrapping_add(1);
        let this = *seq;
        let started = Instant::now();

        let mut packet = [0u8; 12];
        packet[..4].copy_from_slice(&this.to_le_bytes());
        packet[4..].copy_from_slice(&started.as_micros().to_le_bytes());
        let _ = udp.send_to(&packet, dest).await;

        // Wait for the echo of *this* packet (an old, late echo is skipped).
        let answered = with_timeout(PING_TIMEOUT, async {
            let mut buf = [0u8; 32];
            loop {
                if let Ok((n, _)) = udp.recv_from(&mut buf).await
                    && n >= 4
                    && buf[..4] == this.to_le_bytes()
                {
                    return;
                }
            }
        })
        .await
        .is_ok();

        if answered {
            let rtt = started.elapsed().as_micros() as u32;
            received += 1;
            min = min.min(rtt);
            max = max.max(rtt);
            sum += rtt;
            ACTIVITY.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(rest) = PING_INTERVAL.checked_sub(started.elapsed()) {
            Timer::after(rest).await;
        }
    }

    let avg = sum.checked_div(received).unwrap_or(0);
    let min = if received > 0 { min } else { 0 };
    PING_SENT.store(PINGS_PER_ROUND, Ordering::Relaxed);
    PING_RECV.store(received, Ordering::Relaxed);
    RTT_MIN_US.store(min, Ordering::Relaxed);
    RTT_AVG_US.store(avg, Ordering::Relaxed);
    RTT_MAX_US.store(max, Ordering::Relaxed);
    println!("udp ping {peer}: {received}/{PINGS_PER_ROUND} answered, rtt min/avg/max = {min}/{avg}/{max} us");
}

async fn tcp_burst(stack: Stack<'static>, peer: Ipv4Address) {
    let mut rx = [0u8; 512];
    let mut tx = [0u8; 8192];
    let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
    socket.set_timeout(Some(Duration::from_secs(5)));

    match with_timeout(Duration::from_secs(3), socket.connect(endpoint(peer, TCP_SINK_PORT))).await {
        Ok(Ok(())) => {}
        _ => {
            TCP_KBPS.store(0, Ordering::Relaxed);
            println!("tcp {peer}:{TCP_SINK_PORT}: connection failed");
            return;
        }
    }

    let chunk = [0xA5u8; 1024];
    let mut bytes = 0u32;
    let started = Instant::now();
    while started.elapsed() < TCP_BURST {
        match socket.write(&chunk).await {
            Ok(n) => bytes += n as u32,
            Err(_) => break,
        }
    }
    let _ = socket.flush().await; // until the peer has acknowledged everything
    let micros = started.elapsed().as_micros().max(1);

    // bits per microsecond = Mbit/s; x1000 = kbit/s.
    let kbps = (u64::from(bytes) * 8 * 1000 / micros) as u32;
    TCP_KBPS.store(kbps, Ordering::Relaxed);
    ACTIVITY.fetch_add(1, Ordering::Relaxed);
    println!(
        "tcp  {peer}: {} kB in {} ms = {}.{:02} Mbit/s",
        bytes / 1000,
        micros / 1000,
        kbps / 1000,
        kbps % 1000 / 10
    );

    socket.close();
    Timer::after(Duration::from_millis(50)).await;
    socket.abort();
}

/// Echoes every UDP packet back to its sender.
#[embassy_executor::task]
pub async fn echo_task(stack: Stack<'static>) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buf = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buf = [0u8; 256];
    let mut udp = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);
    udp.bind(UDP_ECHO_PORT).expect("UDP bind");

    let mut buf = [0u8; 64];
    loop {
        if let Ok((n, from)) = udp.recv_from(&mut buf).await {
            let _ = udp.send_to(&buf[..n], from).await;
            ECHOED.fetch_add(1, Ordering::Relaxed);
            ACTIVITY.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Accepts TCP connections and discards what they send.
#[embassy_executor::task]
pub async fn sink_task(stack: Stack<'static>) -> ! {
    let mut rx = [0u8; 8192];
    let mut tx = [0u8; 256];
    let mut buf = [0u8; 2048];
    loop {
        let mut socket = TcpSocket::new(stack, &mut rx, &mut tx);
        socket.set_timeout(Some(Duration::from_secs(5)));
        if socket.accept(TCP_SINK_PORT).await.is_err() {
            continue;
        }
        loop {
            match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    SUNK_BYTES.fetch_add(n as u32, Ordering::Relaxed);
                }
            }
        }
        socket.close();
        Timer::after(Duration::from_millis(50)).await;
        socket.abort();
    }
}
