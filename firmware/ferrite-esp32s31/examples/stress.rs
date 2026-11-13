//! ferrite on a board, second demo: checks what `demo` doesn't. It tests itself and prints PASS or FAIL.
//!
//! ```sh
//! cd firmware/ferrite-esp32s31
//! cargo run --release --example stress
//! ```
//!
//! | what | how | passes when |
//! |---|---|---|
//! | **float registers** | `fp-a` and `fp-b` (priority 2) sum 1000 floats in a loop, forever, while the kernel switches between them (and everything else) 100 times a second | every sum is exactly right: a switch that lost `f0-f31` would give a wrong one |
//! | **priority inheritance** | every second: `low` (3) takes a mutex and keeps it 300 ms; `mid` (4) hogs the processor from 150 to 750 ms; `high` (5) asks for the mutex at 100 ms | `high` waits about 200 ms (until `low` is done), never more than 350: without inheritance `mid` would starve `low` and `high` would wait about 650 ms |
//! | **semaphore and timeouts** | `giver` (7) gives a token every second; `taker` (6) waits for one at most 400 ms | a timed-out wait lasts 400 to 403 ticks, and a token is taken within 2 ticks of being given |
//!
//! `reporter` (priority 2) prints a line every 5 s.
#![no_std]
#![no_main]

use core::{
    hint::black_box,
    sync::atomic::{AtomicU32, Ordering::Relaxed},
};

use esp_backtrace as _;
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;
use ferrite_esp32s31::{
    Priority, Stack, Tick, Timeout, delay, delay_ms, ms, now, spawn, start,
    sync::{Mutex, Semaphore},
};

esp_bootloader_esp_idf::esp_app_desc!();

/// Every periodic task starts its first cycle here, so that their phases line up.
const EPOCH: Tick = 1_000;
const PI_LIMIT_MS: u32 = 350;
const TAKE_TIMEOUT_MS: u32 = 400;

static PI_MUTEX: Mutex = Mutex::new(2);
static SEM: Semaphore = Semaphore::new(3, 0, 1);

static FP_OK: AtomicU32 = AtomicU32::new(0);
static FP_BAD: AtomicU32 = AtomicU32::new(0);
static PI_CYCLES: AtomicU32 = AtomicU32::new(0);
static PI_BAD: AtomicU32 = AtomicU32::new(0);
static PI_WAIT_MIN: AtomicU32 = AtomicU32::new(u32::MAX);
static PI_WAIT_MAX: AtomicU32 = AtomicU32::new(0);
static SEM_OK: AtomicU32 = AtomicU32::new(0);
static SEM_TIMEOUTS: AtomicU32 = AtomicU32::new(0);
static SEM_LATE: AtomicU32 = AtomicU32::new(0);
static SEM_BAD_TIMEOUT: AtomicU32 = AtomicU32::new(0);
static GIVEN_AT: AtomicU32 = AtomicU32::new(0);

static STACKS: [Stack<4096>; 8] = [const { Stack::new() }; 8];

fn sleep_until(t: Tick) {
    let left = t.wrapping_sub(now()) as i32;
    if left > 0 {
        delay(left as Tick);
    }
}

/// Keeps the processor busy until tick `t`.
fn busy_until(t: Tick) {
    while (t.wrapping_sub(now()) as i32) > 0 {
        core::hint::spin_loop();
    }
}

// --- Floating-point registers across switches -------------------------------------------------

fn sums(factor: f32, expected: f32) -> ! {
    loop {
        let k = black_box(factor);
        let mut s = 0.0f32;
        for i in 1..=1000u32 {
            s += black_box(i as f32) * k;
        }
        let counter = if s == expected { &FP_OK } else { &FP_BAD };
        counter.fetch_add(1, Relaxed);
    }
}

// All the partial sums are multiples of the factor and far below 2^23: every addition is exact.
extern "C" fn fp_a(_: *mut ()) -> ! {
    sums(0.5, 250_250.0)
}

extern "C" fn fp_b(_: *mut ()) -> ! {
    sums(0.25, 125_125.0)
}

// --- Priority inheritance ----------------------------------------------------------------------

extern "C" fn low(_: *mut ()) -> ! {
    let mut cycle = EPOCH;
    loop {
        sleep_until(cycle);
        let guard = PI_MUTEX.lock(Timeout::Forever).unwrap();
        busy_until(cycle + ms(300));
        drop(guard);
        cycle += ms(1000);
    }
}

extern "C" fn mid(_: *mut ()) -> ! {
    let mut cycle = EPOCH;
    loop {
        sleep_until(cycle + ms(150));
        busy_until(cycle + ms(750));
        cycle += ms(1000);
    }
}

extern "C" fn high(_: *mut ()) -> ! {
    let mut cycle = EPOCH;
    loop {
        sleep_until(cycle + ms(100));
        let asked = now();
        let guard = PI_MUTEX.lock(Timeout::Forever).unwrap();
        let waited = now().wrapping_sub(asked);
        drop(guard);
        PI_CYCLES.fetch_add(1, Relaxed);
        PI_WAIT_MIN.fetch_min(waited, Relaxed);
        PI_WAIT_MAX.fetch_max(waited, Relaxed);
        if waited > ms(PI_LIMIT_MS) {
            PI_BAD.fetch_add(1, Relaxed);
        }
        cycle += ms(1000);
    }
}

// --- Semaphore and timeouts --------------------------------------------------------------------

extern "C" fn giver(_: *mut ()) -> ! {
    let mut cycle = EPOCH;
    loop {
        sleep_until(cycle);
        GIVEN_AT.store(now(), Relaxed);
        SEM.give();
        cycle += ms(1000);
    }
}

extern "C" fn taker(_: *mut ()) -> ! {
    loop {
        let started = now();
        if SEM.take(Timeout::Ticks(ms(TAKE_TIMEOUT_MS))) {
            SEM_OK.fetch_add(1, Relaxed);
            if now().wrapping_sub(GIVEN_AT.load(Relaxed)) > 2 {
                SEM_LATE.fetch_add(1, Relaxed);
            }
        } else {
            SEM_TIMEOUTS.fetch_add(1, Relaxed);
            let lasted = now().wrapping_sub(started);
            if !(ms(TAKE_TIMEOUT_MS)..=ms(TAKE_TIMEOUT_MS) + 3).contains(&lasted) {
                SEM_BAD_TIMEOUT.fetch_add(1, Relaxed);
            }
        }
    }
}

// --- The report --------------------------------------------------------------------------------

extern "C" fn reporter(_: *mut ()) -> ! {
    loop {
        delay_ms(5000);
        let (fp_ok, fp_bad) = (FP_OK.load(Relaxed), FP_BAD.load(Relaxed));
        let (pi_n, pi_bad) = (PI_CYCLES.load(Relaxed), PI_BAD.load(Relaxed));
        let (wmin, wmax) = (PI_WAIT_MIN.load(Relaxed), PI_WAIT_MAX.load(Relaxed));
        let (s_ok, s_to) = (SEM_OK.load(Relaxed), SEM_TIMEOUTS.load(Relaxed));
        let (s_late, s_bad) = (SEM_LATE.load(Relaxed), SEM_BAD_TIMEOUT.load(Relaxed));
        let pass =
            fp_ok > 0 && fp_bad == 0 && pi_n > 0 && pi_bad == 0 && s_ok > 0 && s_to > 0 && s_late == 0 && s_bad == 0;
        println!(
            "[{:>7} ms] floats {fp_ok} ok {fp_bad} bad | inversion {pi_n} cycles, high waited {wmin}..{wmax} ms (limit {PI_LIMIT_MS}), {pi_bad} bad | semaphore {s_ok} taken {s_to} timed out, {s_late} late, {s_bad} wrong timeouts | {}",
            now(),
            if pass { "PASS" } else { "FAIL (or not enough time yet)" }
        );
    }
}

#[esp_hal::main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default());
    println!("ferrite stress: floats, priority inheritance, semaphore timeouts (first report in 5 s)");

    let prio = |level| Priority::new(level).unwrap();
    let null = core::ptr::null_mut();
    let tasks: [(&str, u8, extern "C" fn(*mut ()) -> !); 8] = [
        ("giver", 7, giver),
        ("taker", 6, taker),
        ("high", 5, high),
        ("mid", 4, mid),
        ("low", 3, low),
        ("fp-a", 2, fp_a),
        ("fp-b", 2, fp_b),
        ("reporter", 2, reporter),
    ];
    for (stack, (name, level, entry)) in STACKS.iter().zip(tasks) {
        spawn(name, prio(level), stack.take().unwrap(), entry, null).unwrap();
    }

    start(TimerGroup::new(p.TIMG1).timer0)
}
