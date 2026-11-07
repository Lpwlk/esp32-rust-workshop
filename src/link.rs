//! UART1 link at high speed (5 Mbaud), interrupt-driven on the receive side.
//!
//! At 5 Mbaud the 128-byte hardware RX FIFO fills in ~256 µs, faster than the
//! main loop can be relied on to poll it (printing to the console takes longer).
//! So an interrupt drains the FIFO into a 2 KB ring buffer, and the main loop
//! reads from that buffer whenever it has time.
//!
//! The interrupt also notes **when each `\n` finished on the wire**
//! ([`workshop_core::wire::rx_line_end_us`]), and [`Link::send`] returns when a sent
//! frame will: the console shows the time between `DAT` frames from these, not
//! from when the main loop got around to them.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU32, Ordering};

use critical_section::Mutex;
use esp_hal::{
    Blocking,
    gpio::AnyPin,
    handler,
    peripherals::UART1,
    time::Instant,
    uart::{Config, ConfigError, RxConfig, RxError, TxError, Uart, UartInterrupt},
};
use heapless::Deque;
use workshop_core::wire::{rx_line_end_us, tx_line_end_us};

// --8<-- [start:statics]
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
/// When each received `\n` finished on the wire (µs since boot), in order: one per
/// `\n` that made it into `RX`.
static RX_LINE_ENDS: Mutex<RefCell<Deque<u64, 32>>> = Mutex::new(RefCell::new(Deque::new()));
/// Current baud rate, for the timing corrections in the interrupt.
static BAUD: AtomicU32 = AtomicU32::new(0);
// --8<-- [end:statics]

fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

// --8<-- [start:isr]
#[handler]
fn on_uart1() {
    critical_section::with(|cs| {
        let mut uart = UART.borrow_ref_mut(cs);
        let Some(uart) = uart.as_mut() else { return };
        // Why we're here: line idle (timeout) or FIFO half full. Read before clearing.
        let fired = uart.interrupts();
        let idle = fired.contains(UartInterrupt::RxTimeout) && !fired.contains(UartInterrupt::RxFifoFull);

        // Read the whole batch first: the timing of each `\n` depends on how many
        // bytes came after it.
        let mut batch = [0u8; 256];
        let mut n = 0;
        // Bounded loop: an error is reported once, then reading continues.
        for _ in 0..8 {
            if n == batch.len() {
                break;
            }
            match uart.read_buffered(&mut batch[n..]) {
                Ok(0) => break,
                Ok(read) => n += read,
                Err(e) => RX_ERROR.borrow(cs).set(Some(e)),
            }
        }
        let read_at = now_us();
        let baud = BAUD.load(Ordering::Relaxed);
        RX_BYTES.fetch_add(n as u32, Ordering::Relaxed);

        let mut rx = RX.borrow_ref_mut(cs);
        let mut ends = RX_LINE_ENDS.borrow_ref_mut(cs);
        for (i, &byte) in batch[..n].iter().enumerate() {
            if rx.push_back(byte).is_err() {
                RX_DROPPED.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if byte == b'\n' {
                let end = rx_line_end_us(read_at, n - 1 - i, idle.then_some(RX_IDLE_SYMBOLS), baud);
                if ends.is_full() {
                    ends.pop_front();
                }
                let _ = ends.push_back(end);
            }
        }
        RX_PEAK.fetch_max(rx.len() as u32, Ordering::Relaxed);
        uart.clear_interrupts(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
    });
}
// --8<-- [end:isr]

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

// --8<-- [start:init]
fn uart_config(baud: u32) -> Config {
    Config::default()
        .with_baudrate(baud)
        .with_rx(RxConfig::default().with_fifo_full_threshold(RX_FIFO_THRESHOLD).with_timeout(RX_IDLE_SYMBOLS))
}

impl Link {
    pub fn new(uart: UART1<'static>, tx: AnyPin<'static>, rx: AnyPin<'static>, baud: u32) -> Self {
        let mut uart = Uart::new(uart, uart_config(baud)).expect("invalid UART1 configuration").with_tx(tx).with_rx(rx);
        uart.set_interrupt_handler(on_uart1);
        BAUD.store(baud, Ordering::Relaxed);
        critical_section::with(|cs| {
            uart.listen(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
            UART.borrow_ref_mut(cs).replace(uart);
        });
        Link { tx_bytes: 0 }
    }
    // --8<-- [end:init]

    // --8<-- [start:send]
    /// Queues `data` for transmission. A frame (< 100 bytes) fits in the 128-byte
    /// TX FIFO, so this returns almost immediately, with the time (µs since boot)
    /// at which its last byte will have left on the wire.
    pub fn send(&mut self, mut data: &[u8]) -> Result<u64, TxError> {
        self.tx_bytes = self.tx_bytes.wrapping_add(data.len() as u32);
        let len = data.len();
        critical_section::with(|cs| {
            let mut uart = UART.borrow_ref_mut(cs);
            let uart = uart.as_mut().expect("link not initialized");
            let queued_at = now_us();
            while !data.is_empty() {
                let n = uart.write(data)?;
                data = &data[n..];
            }
            Ok(tx_line_end_us(queued_at, len, BAUD.load(Ordering::Relaxed)))
        })
    }
    // --8<-- [end:send]

    /// Changes the baud rate. Waits until everything queued has left at the old
    /// speed (so a `BAU ACK` is fully sent before switching), then reconfigures the
    /// UART and drops the bytes received at the old speed.
    pub fn set_baud(&mut self, baud: u32) -> Result<(), ConfigError> {
        critical_section::with(|cs| {
            let mut uart = UART.borrow_ref_mut(cs);
            let uart = uart.as_mut().expect("link not initialized");
            let _ = uart.flush();
            uart.apply_config(&uart_config(baud))?;
            uart.listen(UartInterrupt::RxFifoFull | UartInterrupt::RxTimeout);
            BAUD.store(baud, Ordering::Relaxed);
            RX.borrow_ref_mut(cs).clear();
            RX_LINE_ENDS.borrow_ref_mut(cs).clear();
            RX_ERROR.borrow(cs).set(None);
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

    /// When the oldest received line not yet claimed finished on the wire (µs since
    /// boot). Call once per `\n` taken out of [`Self::read`], in order.
    pub fn take_line_end(&mut self) -> Option<u64> {
        critical_section::with(|cs| RX_LINE_ENDS.borrow_ref_mut(cs).pop_front())
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
