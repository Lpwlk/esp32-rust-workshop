//! Blocking synchronization objects: the kernel's [`Semaphore`](ferrite::Semaphore), [`Mutex`](ferrite::Mutex) and
//! [`Queue`](ferrite::Queue) behind calls that wait.
//!
//! Each is a `static`-friendly value (`const fn new`). A call that has to wait blocks the running task on the
//! object, asks for a context switch, and tries again when it runs again: a task woken by `give`
//! that finds the token taken meanwhile waits for the time it has left.

use core::cell::RefCell;

use critical_section::Mutex as CsMutex;
use ferrite::{Tick, Timeout, deadline_reached};

use crate::{request_switch, with_kernel};

/// What one try of a blocking call found.
enum Attempt<T> {
    Done(T),
    TimedOut,
    Waiting,
}

/// Runs `try_once` until it gives a result or `timeout` ends. `try_once` returns `Some` when done, or
/// blocks the running task and returns `None` (it is given the deadline to block with).
fn blocking<T>(timeout: Timeout, mut try_once: impl FnMut(Option<Tick>) -> Attempt<T>) -> Option<T> {
    let deadline = with_kernel(|k| timeout.deadline(k.now()));
    loop {
        match try_once(deadline) {
            Attempt::Done(value) => return Some(value),
            Attempt::TimedOut => return None,
            // Blocked on the object: the switch interrupt that follows suspends this task right here.
            Attempt::Waiting => request_switch(),
        }
    }
}

fn expired(k: &ferrite::Kernel<{ crate::MAX_TASKS }>, deadline: Option<Tick>) -> bool {
    deadline.is_some_and(|d| deadline_reached(k.now(), d))
}

/// A counting semaphore.
pub struct Semaphore(CsMutex<RefCell<ferrite::Semaphore>>);

impl Semaphore {
    /// A semaphore with `initial` tokens, at most `max`. `id` must be unique among the objects of the system.
    pub const fn new(id: ferrite::ObjId, initial: u32, max: u32) -> Self {
        Self(CsMutex::new(RefCell::new(ferrite::Semaphore::new(id, initial, max))))
    }

    /// Adds a token (from a task or an interrupt). `false` if it was full.
    pub fn give(&self) -> bool {
        let given = critical_section::with(|cs| {
            let mut sem = self.0.borrow_ref_mut(cs);
            with_kernel(|k| sem.give(k))
        });
        if given {
            request_switch();
        }
        given
    }

    /// Takes a token, waiting up to `timeout`. `false` if none came.
    pub fn take(&self, timeout: Timeout) -> bool {
        blocking(timeout, |deadline| {
            critical_section::with(|cs| {
                let mut sem = self.0.borrow_ref_mut(cs);
                if sem.try_take() {
                    return Attempt::Done(());
                }
                with_kernel(|k| {
                    if expired(k, deadline) || timeout == Timeout::NoWait {
                        return Attempt::TimedOut;
                    }
                    match sem.wait(k, deadline) {
                        Ok(()) => Attempt::Waiting,
                        Err(_) => Attempt::TimedOut,
                    }
                })
            })
        })
        .is_some()
    }
}

/// A mutex with priority inheritance, locked through a guard.
pub struct Mutex(CsMutex<RefCell<ferrite::Mutex>>);

/// A locked [`Mutex`]: unlocks when dropped.
pub struct Guard<'a>(&'a Mutex);

impl Mutex {
    /// An unlocked mutex. `id` must be unique among the objects of the system.
    pub const fn new(id: ferrite::ObjId) -> Self {
        Self(CsMutex::new(RefCell::new(ferrite::Mutex::new(id))))
    }

    /// Locks it, waiting up to `timeout`.
    pub fn lock(&self, timeout: Timeout) -> Option<Guard<'_>> {
        blocking(timeout, |deadline| {
            critical_section::with(|cs| {
                let mut m = self.0.borrow_ref_mut(cs);
                with_kernel(|k| match m.try_lock(k) {
                    Ok(true) => Attempt::Done(()),
                    Ok(false) if expired(k, deadline) || timeout == Timeout::NoWait => Attempt::TimedOut,
                    Ok(false) => match m.wait(k, deadline) {
                        Ok(()) => Attempt::Waiting,
                        Err(_) => Attempt::TimedOut,
                    },
                    Err(_) => Attempt::TimedOut,
                })
            })
        })
        .map(|()| Guard(self))
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        critical_section::with(|cs| {
            let mut m = self.0.0.borrow_ref_mut(cs);
            with_kernel(|k| {
                let _ = m.unlock(k);
            });
        });
        request_switch();
    }
}

/// A bounded queue of `CAP` messages.
pub struct Queue<T: Copy, const CAP: usize>(CsMutex<RefCell<ferrite::Queue<T, CAP>>>);

impl<T: Copy, const CAP: usize> Queue<T, CAP> {
    /// An empty queue. `id` (below 0x8000) must be unique among the objects of the system.
    pub const fn new(id: ferrite::ObjId) -> Self {
        Self(CsMutex::new(RefCell::new(ferrite::Queue::new(id))))
    }

    /// Adds a message, waiting up to `timeout` for room. Gives it back if there was none.
    pub fn send(&self, item: T, timeout: Timeout) -> Result<(), T> {
        let sent = blocking(timeout, |deadline| {
            critical_section::with(|cs| {
                let mut q = self.0.borrow_ref_mut(cs);
                with_kernel(|k| match q.try_send(k, item) {
                    Ok(()) => Attempt::Done(()),
                    Err(_) if expired(k, deadline) || timeout == Timeout::NoWait => Attempt::TimedOut,
                    Err(_) => match q.wait_writable(k, deadline) {
                        Ok(()) => Attempt::Waiting,
                        Err(_) => Attempt::TimedOut,
                    },
                })
            })
        });
        match sent {
            Some(()) => {
                request_switch();
                Ok(())
            }
            None => Err(item),
        }
    }

    /// Takes the oldest message, waiting up to `timeout` for one.
    pub fn recv(&self, timeout: Timeout) -> Option<T> {
        let item = blocking(timeout, |deadline| {
            critical_section::with(|cs| {
                let mut q = self.0.borrow_ref_mut(cs);
                with_kernel(|k| match q.try_recv(k) {
                    Some(item) => Attempt::Done(item),
                    None if expired(k, deadline) || timeout == Timeout::NoWait => Attempt::TimedOut,
                    None => match q.wait_readable(k, deadline) {
                        Ok(()) => Attempt::Waiting,
                        Err(_) => Attempt::TimedOut,
                    },
                })
            })
        });
        if item.is_some() {
            request_switch();
        }
        item
    }
}
