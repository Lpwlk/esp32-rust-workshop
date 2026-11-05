//! UART1 link at high speed (5 Mbaud), interrupt-driven on the receive side.
//!
//! At 5 Mbaud the 128-byte hardware RX FIFO fills in ~256 µs, faster than the
//! main loop can be relied on to poll it (printing to the console takes longer).
//! So an interrupt drains the FIFO into a 2 KB ring buffer, and the main loop
//! reads from that buffer whenever it has time.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU32, Ordering};

use critical_section::Mutex;
use esp_hal::{
    Blocking,
    gpio::AnyPin,
    handler,
    peripherals::UART1,
    uart::{Config, RxConfig, RxError, TxError, Uart, UartInterrupt},
};
use heapless::Deque;

const RX_BUFFER: usize = 2048;
/// RX interrupt when the FIFO holds this many bytes (half of its 128 bytes)...
const RX_FIFO_THRESHOLD: u16 = 64;
/// ...or when the line has been idle for this many symbol times (end of a frame).
const RX_IDLE_SYMBOLS: u8 = 10;

static UART: Mutex<RefCell<Option<Uart<'static, Blocking>>>> = Mutex::new(RefCell::new(None));
static RX: Mutex<RefCell<Deque<u8, RX_BUFFER>>> = Mutex::new(RefCell::new(Deque::new()));
static RX_ERROR: Mutex<Cell<Option<RxError>>> = Mutex::new(Cell::new(None));
/// Bytes lost because the ring buffer was full (main loop too slow).
static RX_DROPPED: AtomicU32 = AtomicU32::new(0);
/// Bytes received since boot, and the highest ring buffer fill seen (verbose stats).
static RX_BYTES: AtomicU32 = AtomicU32::new(0);
static RX_PEAK: AtomicU32 = AtomicU32::new(0);

#[handler]
fn on_uart1() {
    critical_section::with(|cs| {
        let mut uart = UART.borrow_ref_mut(cs);
        let Some(uart) = uart.as_mut() else { return };
        let mut rx = RX.borrow_ref_mut(cs);
        let mut chunk = [0u8; 64];
        // Bounded loop: an error is reported once, then reading continues.
        for _ in 0..8 {
            match uart.read_buffered(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    RX_BYTES.fetch_add(n as u32, Ordering::Relaxed);
                    for &byte in &chunk[..n] {
                        if rx.push_back(byte).is_err() {
                            RX_DROPPED.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Err(e) => RX_ERROR.borrow(cs).set(Some(e)),
            }
        }
        RX_PEAK.fetch_max(rx.len() as u32, Ordering::Relaxed);
        uart.clear_interrupts(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
    });
}

/// Byte counters, shown in verbose mode.
pub struct Counters {
    pub rx_bytes: u32,
    pub tx_bytes: u32,
    /// Highest fill of the RX ring buffer since boot, and its capacity.
    pub rx_peak: u32,
    pub rx_capacity: u32,
}

pub struct Link {
    tx_bytes: u32,
}

impl Link {
    pub fn new(uart: UART1<'static>, tx: AnyPin<'static>, rx: AnyPin<'static>, baud: u32) -> Self {
        let config = Config::default().with_baudrate(baud).with_rx(
            RxConfig::default()
                .with_fifo_full_threshold(RX_FIFO_THRESHOLD)
                .with_timeout(RX_IDLE_SYMBOLS),
        );
        let mut uart = Uart::new(uart, config).expect("invalid UART1 configuration").with_tx(tx).with_rx(rx);
        uart.set_interrupt_handler(on_uart1);
        critical_section::with(|cs| {
            uart.listen(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
            UART.borrow_ref_mut(cs).replace(uart);
        });
        Link { tx_bytes: 0 }
    }

    /// Queues `data` for transmission. A frame (< 100 bytes) fits in the 128-byte
    /// TX FIFO, so this returns almost immediately.
    pub fn send(&mut self, mut data: &[u8]) -> Result<(), TxError> {
        self.tx_bytes = self.tx_bytes.wrapping_add(data.len() as u32);
        critical_section::with(|cs| {
            let mut uart = UART.borrow_ref_mut(cs);
            let uart = uart.as_mut().expect("link not initialized");
            while !data.is_empty() {
                let n = uart.write(data)?;
                data = &data[n..];
            }
            Ok(())
        })
    }

    /// Moves received bytes into `buf`; returns how many. Never blocks.
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        critical_section::with(|cs| {
            let mut rx = RX.borrow_ref_mut(cs);
            let mut n = 0;
            while n < buf.len() {
                let Some(byte) = rx.pop_front() else { break };
                buf[n] = byte;
                n += 1;
            }
            n
        })
    }

    /// Last UART receive error (framing, parity, FIFO overflow...), if any since the last call.
    pub fn take_error(&mut self) -> Option<RxError> {
        critical_section::with(|cs| RX_ERROR.borrow(cs).take())
    }

    pub fn counters(&self) -> Counters {
        Counters {
            rx_bytes: RX_BYTES.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes,
            rx_peak: RX_PEAK.load(Ordering::Relaxed),
            rx_capacity: RX_BUFFER as u32,
        }
    }

    /// Bytes dropped because the ring buffer was full, since the last call.
    pub fn take_dropped(&mut self) -> u32 {
        RX_DROPPED.swap(0, Ordering::Relaxed)
    }
}
