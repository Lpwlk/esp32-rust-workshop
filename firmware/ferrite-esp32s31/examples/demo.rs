//! ferrite on a board: five tasks that show what the kernel does, printed on the USB console.
//!
//! ```sh
//! cd firmware/ferrite-esp32s31
//! cargo run --release --example demo
//! ```
//!
//! | task | priority | what it shows |
//! |---|---|---|
//! | `ticker` | 6 | a periodic task: wakes every 500 ms, preempts everything else |
//! | `producer` | 4 | sends a number to the queue every second |
//! | `consumer` | 3 | blocks on the queue, prints what arrives |
//! | `spin-a`, `spin-b` | 2 | equal priorities that never block: they share the processor by time slices, and the counters grow at the same pace |
//!
//! Every 5 s the `ticker` prints how many loops each spinner made and the number of task switches.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use esp_backtrace as _;
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;
use ferrite_esp32s31::{Priority, Stack, Timeout, delay_ms, now, spawn, start, sync::Queue};

esp_bootloader_esp_idf::esp_app_desc!();

static NUMBERS: Queue<u32, 4> = Queue::new(1);
static SPIN_A: AtomicU32 = AtomicU32::new(0);
static SPIN_B: AtomicU32 = AtomicU32::new(0);

static TICKER_STACK: Stack<4096> = Stack::new();
static PRODUCER_STACK: Stack<4096> = Stack::new();
static CONSUMER_STACK: Stack<4096> = Stack::new();
static SPIN_A_STACK: Stack<2048> = Stack::new();
static SPIN_B_STACK: Stack<2048> = Stack::new();

extern "C" fn ticker(_: *mut ()) -> ! {
    let mut beats = 0u32;
    loop {
        delay_ms(500);
        beats += 1;
        if beats.is_multiple_of(10) {
            println!(
                "[{:>6} ms] ticker: spin-a {} loops, spin-b {} loops, {} task switches",
                now(),
                SPIN_A.load(Ordering::Relaxed),
                SPIN_B.load(Ordering::Relaxed),
                ferrite_esp32s31::switches()
            );
        }
    }
}

extern "C" fn producer(_: *mut ()) -> ! {
    let mut n = 0u32;
    loop {
        delay_ms(1000);
        n += 1;
        let _ = NUMBERS.send(n, Timeout::Ticks(100));
    }
}

extern "C" fn consumer(_: *mut ()) -> ! {
    loop {
        if let Some(n) = NUMBERS.recv(Timeout::Forever) {
            println!("[{:>6} ms] consumer: got {n}", now());
        }
    }
}

extern "C" fn spinner(counter: *mut ()) -> ! {
    // SAFETY: `counter` is one of the two statics above.
    let counter = unsafe { &*(counter as *const AtomicU32) };
    loop {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

#[esp_hal::main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default());
    println!("ferrite demo: starting the kernel");

    let prio = |level| Priority::new(level).unwrap();
    let null = core::ptr::null_mut();
    spawn("ticker", prio(6), TICKER_STACK.take().unwrap(), ticker, null).unwrap();
    spawn("producer", prio(4), PRODUCER_STACK.take().unwrap(), producer, null).unwrap();
    spawn("consumer", prio(3), CONSUMER_STACK.take().unwrap(), consumer, null).unwrap();
    spawn("spin-a", prio(2), SPIN_A_STACK.take().unwrap(), spinner, SPIN_A.as_ptr().cast()).unwrap();
    spawn("spin-b", prio(2), SPIN_B_STACK.take().unwrap(), spinner, SPIN_B.as_ptr().cast()).unwrap();

    start(TimerGroup::new(p.TIMG1).timer0)
}
