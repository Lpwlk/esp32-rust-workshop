//! # mros on the ESP32-S31
//!
//! The hardware half of [mros](mros): it keeps the saved registers and the stack of each task,
//! switches between them, drives the kernel's clock from a timer interrupt, and offers the kernel's
//! synchronization objects as blocking calls ([`sync`]).
//!
//! ```ignore
//! #[esp_hal::main]
//! fn main() -> ! {
//!     let p = esp_hal::init(esp_hal::Config::default());
//!     mros_esp32s31::spawn("blink", Priority::new(5).unwrap(), BLINK_STACK.take(), blink, core::ptr::null_mut()).unwrap();
//!     mros_esp32s31::start(TimerGroup::new(p.TIMG1).timer0) // never returns
//! }
//! ```
//!
//! ## Status
//!
//! The kernel is tested on the host; this port **builds** for the ESP32-S31 and has not been run
//! on a board yet. Single core, one tick timer (a TIMG timer; the S31's other timers are left to the
//! application), a 1 kHz tick.
//!
//! ## How a switch happens
//!
//! Anything that makes another task preferable (a tick, a `give`, a `delay`) updates the kernel
//! inside a critical section, and asks `esp-hal` for a context switch. That is an interrupt at the
//! lowest priority, so it runs as soon as the critical section ends: [`context::switch_trampoline`]
//! saves the running task's registers, [`switch`] lets the kernel choose, and the chosen task's
//! registers are restored. A task that called `delay` is therefore suspended *inside* that call, and
//! comes back from it once it runs again.
#![no_std]
#![warn(missing_docs)]

pub mod context;
pub mod sync;

use core::cell::RefCell;

use critical_section::Mutex;
use esp_hal::{
    Blocking,
    interrupt::{__rtos_implementation as hal, InterruptHandler, Priority as IrqPriority},
    system::Cpu,
    time::Duration,
    timer::{AnyTimer, PeriodicTimer, Timer},
};
pub use mros::{Error, Priority, TaskId, Tick, Timeout, WakeReason};
use mros::{Kernel, State};

use crate::context::Context;

/// Tasks the system can hold, the idle task included.
pub const MAX_TASKS: usize = 12;
/// Ticks per second.
pub const TICK_HZ: u32 = 1_000;
/// Ticks a task runs before an equal-priority task gets its turn.
pub const TIME_SLICE_TICKS: u16 = 10;
/// Stack of the idle task, bytes.
const IDLE_STACK_BYTES: usize = 2048;

/// What a task is: a function that never returns (a task that ends calls [`exit`]).
pub type TaskFn = extern "C" fn(*mut ()) -> !;

struct Os {
    kernel: Kernel<MAX_TASKS>,
    contexts: [Context; MAX_TASKS],
    timer: Option<PeriodicTimer<'static, Blocking>>,
}

static OS: Mutex<RefCell<Os>> = Mutex::new(RefCell::new(Os {
    kernel: Kernel::new(TIME_SLICE_TICKS),
    contexts: [const { Context::EMPTY }; MAX_TASKS],
    timer: None,
}));

#[repr(align(16))]
struct IdleStack([u8; IDLE_STACK_BYTES]);
static mut IDLE_STACK: IdleStack = IdleStack([0; IDLE_STACK_BYTES]);

/// A stack for a task: a `static` of this type, taken once by [`spawn`].
#[repr(C, align(16))]
pub struct Stack<const BYTES: usize> {
    memory: core::cell::UnsafeCell<[u8; BYTES]>,
    taken: core::sync::atomic::AtomicBool,
}

// SAFETY: the memory is handed out once (`take`), as an exclusive slice.
unsafe impl<const BYTES: usize> Sync for Stack<BYTES> {}

impl<const BYTES: usize> Stack<BYTES> {
    /// An unused stack.
    pub const fn new() -> Self {
        Self { memory: core::cell::UnsafeCell::new([0; BYTES]), taken: core::sync::atomic::AtomicBool::new(false) }
    }

    /// The stack's memory, once: `None` the second time.
    #[allow(clippy::mut_from_ref, reason = "the flag makes the exclusive borrow happen once")]
    pub fn take(&'static self) -> Option<&'static mut [u8]> {
        use core::sync::atomic::Ordering;
        if self.taken.swap(true, Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the flag above makes this the only reference ever made.
        Some(unsafe { &mut *self.memory.get() })
    }
}

impl<const BYTES: usize> Default for Stack<BYTES> {
    fn default() -> Self {
        Self::new()
    }
}

/// Adds a task, ready to run, with `entry(arg)` on `stack`. Callable before [`start`] and from tasks.
///
/// The stack must hold the task's own frames plus about 300 bytes for the context switch that runs on it.
pub fn spawn(
    name: &'static str,
    priority: Priority,
    stack: &'static mut [u8],
    entry: TaskFn,
    arg: *mut (),
) -> Result<TaskId, Error> {
    let top = stack.as_mut_ptr() as usize + stack.len();
    critical_section::with(|cs| {
        let mut os = OS.borrow_ref_mut(cs);
        let id = os.kernel.create(name, priority)?;
        os.contexts[id.index()] = Context::new(entry, arg, top);
        Ok(id)
    })
}

/// Starts the system: creates the idle task, starts the tick on `timer`, and runs the most urgent task.
/// Never returns.
pub fn start(timer: impl Timer + Into<AnyTimer<'static>>) -> ! {
    // The idle task: always ready, runs when nothing else does.
    // SAFETY: the idle stack is used by this task alone, and only here.
    let idle_stack = unsafe { &mut *core::ptr::addr_of_mut!(IDLE_STACK.0) };
    spawn("idle", Priority::IDLE, idle_stack, idle, core::ptr::null_mut()).expect("a slot for the idle task");

    let mut tick = PeriodicTimer::new(timer);
    tick.set_interrupt_handler(InterruptHandler::new(tick_handler, IrqPriority::Priority1));
    // SAFETY: the handler is ours, `context::switch_trampoline` follows the HAL's contract.
    unsafe { hal::set_context_switch_handler(Cpu::current(), context::switch_trampoline) };
    context::point_at_boot_context();
    tick.start(Duration::from_micros(1_000_000 / u64::from(TICK_HZ))).expect("the tick period fits the timer");
    tick.listen();
    critical_section::with(|cs| OS.borrow_ref_mut(cs).timer = Some(tick));

    // The first switch: it saves the registers of this call into the boot context, and the first task runs.
    hal::request_context_switch(Cpu::current());
    loop {
        // Not reached once the first switch has happened.
        core::hint::spin_loop();
    }
}

extern "C" fn idle(_: *mut ()) -> ! {
    loop {
        // SAFETY: waits for an interrupt (the tick), which is what wakes a sleeping system.
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

/// The kernel's decision at the end of the switch interrupt: sets which context is restored.
pub(crate) extern "C" fn switch() {
    critical_section::with(|cs| {
        let mut os = OS.borrow_ref_mut(cs);
        if let Some(switch) = os.kernel.schedule() {
            let next: *mut Context = &raw mut os.contexts[switch.to.index()];
            // SAFETY: `contexts` is a static: the pointer stays valid, and this runs inside the switch.
            unsafe { context::set_current(next) };
        }
    });
}

extern "C" fn tick_handler() {
    critical_section::with(|cs| {
        let mut os = OS.borrow_ref_mut(cs);
        if let Some(timer) = os.timer.as_mut() {
            timer.clear_interrupt();
        }
        if os.kernel.tick() {
            hal::request_context_switch(Cpu::current());
        }
    });
}

/// Asks for a context switch. It happens when the critical section ends, if the kernel wants one.
pub(crate) fn request_switch() {
    hal::request_context_switch(Cpu::current());
}

/// The time since [`start`], in ticks (it wraps after 49 days at 1 kHz).
pub fn now() -> Tick {
    critical_section::with(|cs| OS.borrow_ref(cs).kernel.now())
}

/// Milliseconds as ticks.
pub const fn ms(milliseconds: u32) -> Tick {
    ((milliseconds as u64 * TICK_HZ as u64) / 1000) as Tick
}

/// Sleeps the running task for `ticks`.
pub fn delay(ticks: Tick) {
    let ok = critical_section::with(|cs| OS.borrow_ref_mut(cs).kernel.delay(ticks).is_ok());
    if ok {
        request_switch();
    }
}

/// Sleeps the running task for `milliseconds`.
pub fn delay_ms(milliseconds: u32) {
    delay(ms(milliseconds).max(1));
}

/// Lets the next ready task of the same priority run.
pub fn yield_now() {
    let ok = critical_section::with(|cs| OS.borrow_ref_mut(cs).kernel.yield_now().is_ok());
    if ok {
        request_switch();
    }
}

/// Ends the running task. Never returns.
pub fn exit() -> ! {
    critical_section::with(|cs| {
        let _ = OS.borrow_ref_mut(cs).kernel.exit_current();
    });
    request_switch();
    loop {
        core::hint::spin_loop();
    }
}

/// The running task.
pub fn current() -> Option<TaskId> {
    critical_section::with(|cs| OS.borrow_ref(cs).kernel.current())
}

/// What a task is doing.
pub fn state(id: TaskId) -> State {
    critical_section::with(|cs| OS.borrow_ref(cs).kernel.state(id))
}

/// The number of task switches since [`start`].
pub fn switches() -> u32 {
    critical_section::with(|cs| OS.borrow_ref(cs).kernel.switches())
}

/// Runs `f` on the kernel (for the blocking objects of [`sync`]) inside a critical section.
pub(crate) fn with_kernel<R>(f: impl FnOnce(&mut Kernel<MAX_TASKS>) -> R) -> R {
    critical_section::with(|cs| f(&mut OS.borrow_ref_mut(cs).kernel))
}
