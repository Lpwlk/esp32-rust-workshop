//! # Ferrite: a small preemptive RTOS kernel
//!
//! Ferrite is a fixed-priority, preemptive kernel with round-robin time slicing between tasks of the
//! same priority, written in plain `no_std` Rust. It is split in two on purpose:
//!
//! - **this crate** is the *kernel*: tasks, the scheduler, time, and the synchronization objects
//!   ([`Semaphore`], [`Mutex`] with priority inheritance, [`Queue`]). It is pure logic over indices
//!   and counters, with no `unsafe`, no allocation and no hardware, so it is tested on the host
//!   (`cargo test-host`);
//! - a **port** (for the ESP32-S31: `firmware/ferrite-esp32s31`) owns what is hardware: the saved CPU
//!   context of each task, the context switch, the tick timer and the critical section. It calls
//!   into the kernel and acts on what the kernel decides.
//!
//! ## The model
//!
//! The kernel owns a fixed table of `N` tasks, created once with [`Kernel::create`]. A task is a
//! [`TaskId`] (an index): the port keeps the stack and the saved registers of task `i` in its own
//! table at index `i`. Exactly one task is [`State::Running`]. The rest are ready, delayed,
//! blocked on an object, or suspended.
//!
//! The kernel never switches by itself: every call that can change who should run (a delay, a wake-up,
//! a tick) just updates the tables, and the port then calls [`Kernel::schedule`], which answers
//! with a [`Switch`] (from which task to which) or `None`.
//!
//! ```
//! use ferrite::{Kernel, Priority};
//!
//! let mut k: Kernel<4> = Kernel::new(10); // up to 4 tasks, 10 ticks of time slice
//! let idle = k.create("idle", Priority::IDLE).unwrap(); // every system needs an idle task
//! let worker = k.create("worker", Priority::new(5).unwrap()).unwrap();
//!
//! assert_eq!(k.start(), Some(worker)); // the highest priority runs first
//! k.delay(3).unwrap(); // the worker sleeps 3 ticks...
//! assert_eq!(k.schedule().map(|s| s.to), Some(idle)); // ...and the idle task runs
//!
//! for _ in 0..2 {
//!     assert!(!k.tick()); // nothing to do yet
//! }
//! assert!(k.tick()); // third tick: the worker is ready again, and more urgent
//! assert_eq!(k.schedule().map(|s| s.to), Some(worker));
//! ```
//!
//! ## What is not (yet) in it
//!
//! Single core only; priority inheritance follows one hop (a mutex holder), not a chain; ready tasks
//! are found by scanning the table (fine for a dozen tasks, to be replaced by per-priority queues
//! for more); there is no dynamic task memory: stacks are the port's, statically allocated.
#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]

mod kernel;
mod sync;

pub use kernel::{Error, Kernel, ObjId, Priority, State, Switch, TaskId, Tick, Timeout, WakeReason, deadline_reached};
pub use sync::{Mutex, Queue, Semaphore};
