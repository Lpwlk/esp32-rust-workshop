//! Synchronization objects: a counting semaphore, a mutex with priority inheritance, a message queue.
//!
//! They hold their own state and act on the kernel to block or wake tasks. Each exposes the
//! non-blocking operation (`try_take`, `try_lock`, `try_send`...) and the call that blocks the
//! running task on it (`wait`); the port glues them in a loop, which is what makes a *waking* a
//! "retry" rather than a hand-over: a task woken by a `give` tries again, and if a more urgent task took
//! the token in between, it simply waits again, for the time it has left.

use crate::kernel::{Error, Kernel, ObjId, Priority, TaskId, Tick};

/// A counting semaphore.
#[derive(Clone, Debug)]
pub struct Semaphore {
    id: ObjId,
    count: u32,
    max: u32,
}

impl Semaphore {
    /// A semaphore holding `initial` tokens, at most `max`. `id` identifies it among the objects tasks wait on.
    pub const fn new(id: ObjId, initial: u32, max: u32) -> Self {
        Self { id, count: if initial > max { max } else { initial }, max }
    }

    /// Tokens available.
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// Takes a token if there is one.
    pub fn try_take(&mut self) -> bool {
        if self.count == 0 {
            return false;
        }
        self.count -= 1;
        true
    }

    /// Adds a token, and wakes the most urgent waiter. `false` if the semaphore was full.
    pub fn give<const N: usize>(&mut self, kernel: &mut Kernel<N>) -> bool {
        if self.count == self.max {
            return false;
        }
        self.count += 1;
        kernel.wake_one(self.id);
        true
    }

    /// Blocks the running task until a `give` (or `deadline`).
    pub fn wait<const N: usize>(&self, kernel: &mut Kernel<N>, deadline: Option<Tick>) -> Result<(), Error> {
        kernel.wait_on(self.id, deadline)
    }
}

/// A mutex with priority inheritance: while a more urgent task waits for it, its holder runs at that
/// task's priority, so a task of middle priority can't keep the holder from finishing (priority inversion).
///
/// Inheritance follows one hop, and giving the mutex back returns the holder to its base priority.
#[derive(Clone, Debug)]
pub struct Mutex {
    id: ObjId,
    owner: Option<TaskId>,
}

impl Mutex {
    /// An unlocked mutex. `id` identifies it among the objects tasks wait on.
    pub const fn new(id: ObjId) -> Self {
        Self { id, owner: None }
    }

    /// The task holding it.
    pub const fn owner(&self) -> Option<TaskId> {
        self.owner
    }

    /// Locks it for the running task if it is free.
    pub fn try_lock<const N: usize>(&mut self, kernel: &Kernel<N>) -> Result<bool, Error> {
        let me = kernel.current().ok_or(Error::NoCurrentTask)?;
        if self.owner.is_some() {
            return Ok(false);
        }
        self.owner = Some(me);
        Ok(true)
    }

    /// Blocks the running task until the mutex is released (or `deadline`), lending the holder the
    /// running task's priority meanwhile.
    pub fn wait<const N: usize>(&self, kernel: &mut Kernel<N>, deadline: Option<Tick>) -> Result<(), Error> {
        let me = kernel.current().ok_or(Error::NoCurrentTask)?;
        if let Some(owner) = self.owner {
            let urgency: Priority = kernel.priority(me);
            kernel.boost(owner, urgency);
        }
        kernel.wait_on(self.id, deadline)
    }

    /// Releases it: the holder goes back to its base priority and the most urgent waiter wakes up to try.
    /// Only the holder may do it.
    pub fn unlock<const N: usize>(&mut self, kernel: &mut Kernel<N>) -> Result<(), Error> {
        let me = kernel.current().ok_or(Error::NoCurrentTask)?;
        if self.owner != Some(me) {
            return Err(Error::BadState);
        }
        self.owner = None;
        kernel.restore(me);
        kernel.wake_one(self.id);
        Ok(())
    }
}

/// A bounded first-in first-out queue of `CAP` messages. Readers and writers wait on two ids: `id` and `id | 0x8000`.
#[derive(Clone, Debug)]
pub struct Queue<T: Copy, const CAP: usize> {
    readers: ObjId,
    writers: ObjId,
    items: [Option<T>; CAP],
    head: usize,
    len: usize,
}

impl<T: Copy, const CAP: usize> Queue<T, CAP> {
    /// An empty queue. `id` (below 0x8000) identifies it among the objects tasks wait on.
    pub const fn new(id: ObjId) -> Self {
        assert!(CAP > 0 && id < 0x8000);
        Self { readers: id, writers: id | 0x8000, items: [None; CAP], head: 0, len: 0 }
    }

    /// Messages waiting.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether there are none.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether it is full.
    pub const fn is_full(&self) -> bool {
        self.len == CAP
    }

    /// Adds a message at the end and wakes a waiting reader; gives it back if the queue is full.
    pub fn try_send<const N: usize>(&mut self, kernel: &mut Kernel<N>, item: T) -> Result<(), T> {
        if self.is_full() {
            return Err(item);
        }
        self.items[(self.head + self.len) % CAP] = Some(item);
        self.len += 1;
        kernel.wake_one(self.readers);
        Ok(())
    }

    /// Takes the oldest message and wakes a waiting writer.
    pub fn try_recv<const N: usize>(&mut self, kernel: &mut Kernel<N>) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        let item = self.items[self.head].take();
        self.head = (self.head + 1) % CAP;
        self.len -= 1;
        kernel.wake_one(self.writers);
        item
    }

    /// Blocks the running task until a message arrives (or `deadline`).
    pub fn wait_readable<const N: usize>(&self, kernel: &mut Kernel<N>, deadline: Option<Tick>) -> Result<(), Error> {
        kernel.wait_on(self.readers, deadline)
    }

    /// Blocks the running task until there is room (or `deadline`).
    pub fn wait_writable<const N: usize>(&self, kernel: &mut Kernel<N>, deadline: Option<Tick>) -> Result<(), Error> {
        kernel.wait_on(self.writers, deadline)
    }
}
