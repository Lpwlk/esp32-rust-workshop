//! The saved CPU state of a task, and the context switch.
//!
//! A switch is an interrupt: `esp-hal` calls a "context switch handler" (ours, [`switch_trampoline`]) at its
//! lowest interrupt priority, with the interrupted `t0` parked in `mscratch`. The handler saves **every**
//! register of the interrupted task into its [`Context`], asks the kernel who runs next
//! ([`crate::switch`]), restores that task's registers and returns with `mret`, which resumes it
//! where it was interrupted. Because a switch is always an interrupt return, a task is never
//! "called back": a task that gave up the processor (a delay, a wait) was interrupted by a
//! software request made right there.
//!
//! The layout is fixed, because the assembly addresses it by word number:
//!
//! | word | content |
//! |---|---|
//! | 0 to 30 | `x1` to `x31` (`ra`, `sp`, `gp`, `tp`, `t0`..., `a0`..., `s0`...) |
//! | 31 | `pc` (`mepc`) |
//! | 32 | `fcsr` (feature `fpu`, on by default) |
//! | 33 to 64 | `f0` to `f31` |
//!
//! The approach (a handler in the HAL's lowest-priority interrupt, `mscratch` holding `t0`) follows
//! what `esp-rtos` does, which is the documented way to plug an RTOS into `esp-hal`; the code is
//! written for Ferrite (one context per task, the idle task has a stack like any other).

use core::ptr::addr_of_mut;

/// Registers of a task (see the module docs for the layout).
#[repr(C)]
pub struct Context {
    pub(crate) x: [usize; 31],
    pub(crate) pc: usize,
    #[cfg(feature = "fpu")]
    pub(crate) fcsr: usize,
    #[cfg(feature = "fpu")]
    pub(crate) f: [u32; 32],
}

impl Context {
    /// A context of zeros.
    pub const EMPTY: Context = Context {
        x: [0; 31],
        pc: 0,
        #[cfg(feature = "fpu")]
        fcsr: 0,
        #[cfg(feature = "fpu")]
        f: [0; 32],
    };

    /// The context a new task starts from: `entry(arg)` at the top of `stack`.
    pub(crate) fn new(entry: extern "C" fn(*mut ()) -> !, arg: *mut (), stack_top: usize) -> Context {
        let mut c = Context::EMPTY;
        c.pc = task_start as *const () as usize;
        c.x[9] = entry as usize; // a0 (x10)
        c.x[10] = arg as usize; // a1 (x11)
        c.x[1] = stack_top & !0xf; // sp (x2)
        c.x[2] = read_gp(); // gp (x3): the same for every task
        c
    }
}

fn read_gp() -> usize {
    let gp: usize;
    // SAFETY: reads a register.
    unsafe { core::arch::asm!("mv {}, gp", out(reg) gp, options(nomem, nostack)) };
    gp
}

/// Where a new task begins: calls its entry point, and ends the task if that returns.
extern "C" fn task_start(entry: extern "C" fn(*mut ()) -> !, arg: *mut ()) -> ! {
    entry(arg)
}

/// The context of the task that is running (the one the next switch saves into).
#[unsafe(no_mangle)]
static mut FERRITE_CURRENT_CONTEXT: *mut Context = core::ptr::null_mut();

/// Where the registers of the code that calls `start` go (it never runs again).
static mut BOOT_CONTEXT: Context = Context::EMPTY;

/// Points the next switch at the boot context: it saves into it, and the kernel's first choice runs.
pub(crate) fn point_at_boot_context() {
    // SAFETY: called once, before the first switch, with interrupts masked.
    unsafe { FERRITE_CURRENT_CONTEXT = addr_of_mut!(BOOT_CONTEXT) };
}

/// Makes `context` the one the next switch restores.
///
/// # Safety
///
/// `context` must stay valid and unmoved for as long as its task exists, and the call must be made
/// inside the switch (the kernel's decision and this pointer must change together).
pub(crate) unsafe fn set_current(context: *mut Context) {
    unsafe { FERRITE_CURRENT_CONTEXT = context };
}

#[cfg(feature = "fpu")]
macro_rules! fpu_save {
    () => {
        "
        .option push
        .option arch, +f
        frcsr t0
        sw t0, 32*4(t1)
        fsw f0,  33*4(t1)
        fsw f1,  34*4(t1)
        fsw f2,  35*4(t1)
        fsw f3,  36*4(t1)
        fsw f4,  37*4(t1)
        fsw f5,  38*4(t1)
        fsw f6,  39*4(t1)
        fsw f7,  40*4(t1)
        fsw f8,  41*4(t1)
        fsw f9,  42*4(t1)
        fsw f10, 43*4(t1)
        fsw f11, 44*4(t1)
        fsw f12, 45*4(t1)
        fsw f13, 46*4(t1)
        fsw f14, 47*4(t1)
        fsw f15, 48*4(t1)
        fsw f16, 49*4(t1)
        fsw f17, 50*4(t1)
        fsw f18, 51*4(t1)
        fsw f19, 52*4(t1)
        fsw f20, 53*4(t1)
        fsw f21, 54*4(t1)
        fsw f22, 55*4(t1)
        fsw f23, 56*4(t1)
        fsw f24, 57*4(t1)
        fsw f25, 58*4(t1)
        fsw f26, 59*4(t1)
        fsw f27, 60*4(t1)
        fsw f28, 61*4(t1)
        fsw f29, 62*4(t1)
        fsw f30, 63*4(t1)
        fsw f31, 64*4(t1)
        .option pop
        "
    };
}

#[cfg(feature = "fpu")]
macro_rules! fpu_restore {
    () => {
        "
        .option push
        .option arch, +f
        lw t0, 32*4(t1)
        fscsr t0
        flw f0,  33*4(t1)
        flw f1,  34*4(t1)
        flw f2,  35*4(t1)
        flw f3,  36*4(t1)
        flw f4,  37*4(t1)
        flw f5,  38*4(t1)
        flw f6,  39*4(t1)
        flw f7,  40*4(t1)
        flw f8,  41*4(t1)
        flw f9,  42*4(t1)
        flw f10, 43*4(t1)
        flw f11, 44*4(t1)
        flw f12, 45*4(t1)
        flw f13, 46*4(t1)
        flw f14, 47*4(t1)
        flw f15, 48*4(t1)
        flw f16, 49*4(t1)
        flw f17, 50*4(t1)
        flw f18, 51*4(t1)
        flw f19, 52*4(t1)
        flw f20, 53*4(t1)
        flw f21, 54*4(t1)
        flw f22, 55*4(t1)
        flw f23, 56*4(t1)
        flw f24, 57*4(t1)
        flw f25, 58*4(t1)
        flw f26, 59*4(t1)
        flw f27, 60*4(t1)
        flw f28, 61*4(t1)
        flw f29, 62*4(t1)
        flw f30, 63*4(t1)
        flw f31, 64*4(t1)
        .option pop
        "
    };
}

#[cfg(not(feature = "fpu"))]
macro_rules! fpu_save {
    () => {
        ""
    };
}

#[cfg(not(feature = "fpu"))]
macro_rules! fpu_restore {
    () => {
        ""
    };
}

/// The context switch handler, registered with `esp-hal` ([`crate::start`]).
///
/// Register `xN` is word `N - 1` of the [`Context`]. `t1` (`x6`) is the base register while saving
/// and restoring, so the interrupted `t1` is parked on the stack for a moment, and restored last.
///
/// # Safety
///
/// Only the interrupt may jump here: it expects `mscratch` to hold the interrupted `t0`, and ends
/// with `mret`.
#[unsafe(link_section = ".trap.rust")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
#[rustfmt::skip]
pub(crate) unsafe extern "C" fn switch_trampoline() {
    core::arch::naked_asm! {concat!("
        # t0 = the interrupted t0; park t1 on the stack, then t1 = &mut Context of the interrupted task
        csrr t0, mscratch
        addi sp, sp, -16
        sw t1, 0(sp)
        la t1, {current}
        lw t1, 0(t1)

        sw ra,   0*4(t1)
        sw gp,   2*4(t1)
        sw tp,   3*4(t1)
        sw t0,   4*4(t1)
        lw t0, 0(sp)
        sw t0,   5*4(t1)
        sw t2,   6*4(t1)
        sw s0,   7*4(t1)
        sw s1,   8*4(t1)
        sw a0,   9*4(t1)
        sw a1,  10*4(t1)
        sw a2,  11*4(t1)
        sw a3,  12*4(t1)
        sw a4,  13*4(t1)
        sw a5,  14*4(t1)
        sw a6,  15*4(t1)
        sw a7,  16*4(t1)
        sw s2,  17*4(t1)
        sw s3,  18*4(t1)
        sw s4,  19*4(t1)
        sw s5,  20*4(t1)
        sw s6,  21*4(t1)
        sw s7,  22*4(t1)
        sw s8,  23*4(t1)
        sw s9,  24*4(t1)
        sw s10, 25*4(t1)
        sw s11, 26*4(t1)
        sw t3,  27*4(t1)
        sw t4,  28*4(t1)
        sw t5,  29*4(t1)
        sw t6,  30*4(t1)
        # sp: what it was before this handler took 16 bytes
        addi t0, sp, 16
        sw t0, 1*4(t1)
        csrr t0, mepc
        sw t0, 31*4(t1)
",
        fpu_save!(),
"
        # The registers are saved: the stack is the task's again from here, with its 16 bytes given back.
        addi sp, sp, 16

        # Ask the kernel who runs next. It changes the current context.
        la t0, {switch}
        jalr ra, t0, 0

        # t1 = &mut Context of the task that runs now
        la t1, {current}
        lw t1, 0(t1)
",
        fpu_restore!(),
"
        lw t0, 31*4(t1)
        csrw mepc, t0
        lw ra,   0*4(t1)
        lw sp,   1*4(t1)
        lw gp,   2*4(t1)
        lw tp,   3*4(t1)
        lw t0,   4*4(t1)
        lw t2,   6*4(t1)
        lw s0,   7*4(t1)
        lw s1,   8*4(t1)
        lw a0,   9*4(t1)
        lw a1,  10*4(t1)
        lw a2,  11*4(t1)
        lw a3,  12*4(t1)
        lw a4,  13*4(t1)
        lw a5,  14*4(t1)
        lw a6,  15*4(t1)
        lw a7,  16*4(t1)
        lw s2,  17*4(t1)
        lw s3,  18*4(t1)
        lw s4,  19*4(t1)
        lw s5,  20*4(t1)
        lw s6,  21*4(t1)
        lw s7,  22*4(t1)
        lw s8,  23*4(t1)
        lw s9,  24*4(t1)
        lw s10, 25*4(t1)
        lw s11, 26*4(t1)
        lw t3,  27*4(t1)
        lw t4,  28*4(t1)
        lw t5,  29*4(t1)
        lw t6,  30*4(t1)
        lw t1,   5*4(t1)
        mret
        "),
        current = sym FERRITE_CURRENT_CONTEXT,
        switch = sym crate::switch,
    }
}
