//! Tasks, the scheduler and time.

/// Time, in ticks. It wraps around: compare instants with [`deadline_reached`], never with `<`.
pub type Tick = u32;

/// Identifies what a task waits on (a [`Semaphore`](crate::Semaphore), a [`Queue`](crate::Queue)...).
/// Every object has its own, chosen by whoever creates it.
pub type ObjId = u16;

/// Whether `deadline` has come at `now`, across the wrap-around of the tick counter (valid while the
/// two are less than half the counter's range, 2^31 ticks, apart).
pub const fn deadline_reached(now: Tick, deadline: Tick) -> bool {
    now.wrapping_sub(deadline) as i32 >= 0
}

/// How long a blocking call may wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timeout {
    /// Don't wait: fail at once.
    NoWait,
    /// Wait at most this many ticks.
    Ticks(Tick),
    /// Wait for as long as it takes.
    Forever,
}

impl Timeout {
    /// The instant at which the wait ends, `None` when it never does.
    pub const fn deadline(self, now: Tick) -> Option<Tick> {
        match self {
            Timeout::NoWait => Some(now),
            Timeout::Ticks(n) => Some(now.wrapping_add(n)),
            Timeout::Forever => None,
        }
    }
}

/// Task priority: 0 (the idle task) to 31, a higher number is more urgent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority(u8);

impl Priority {
    /// The idle task's priority: nothing runs below it.
    pub const IDLE: Priority = Priority(0);
    /// The most urgent priority.
    pub const MAX: Priority = Priority(31);

    /// A priority, if `level` is 0 to 31.
    pub const fn new(level: u8) -> Option<Priority> {
        if level <= Self::MAX.0 { Some(Priority(level)) } else { None }
    }

    /// The number, 0 to 31.
    pub const fn level(self) -> u8 {
        self.0
    }
}

/// A task: its index in the kernel's table, which is also its index in the port's tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskId(u8);

impl TaskId {
    /// The index.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// What a task is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The table slot is unused.
    Free,
    /// Can run, waits for the processor.
    Ready,
    /// Has the processor.
    Running,
    /// Sleeps until a given tick.
    Delayed,
    /// Waits on an object, with or without a time limit.
    Blocked,
    /// Stopped until [`Kernel::resume`].
    Suspended,
}

/// Why a blocked task became ready again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeReason {
    /// Nothing happened yet (or the task never blocked).
    None,
    /// The object it waited on was signaled: retry the operation.
    Signaled,
    /// Its time limit ran out.
    TimedOut,
}

/// What a kernel call refused to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// All `N` slots are used.
    NoFreeTask,
    /// That task doesn't exist (or no longer does).
    BadTask,
    /// The call is for the running task and nothing is running (before [`Kernel::start`]).
    NoCurrentTask,
    /// The task isn't in a state where that makes sense.
    BadState,
}

/// What [`Kernel::schedule`] decided: save `from`, run `to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switch {
    /// The task that stops running, `None` the very first time.
    pub from: Option<TaskId>,
    /// The task that runs next.
    pub to: TaskId,
    /// `from` ended ([`Kernel::exit_current`]): its context is not worth saving.
    pub from_exited: bool,
}

#[derive(Clone, Copy, Debug)]
struct Tcb {
    state: State,
    name: &'static str,
    /// Priority the task was created with (or last given by `set_priority`).
    base: Priority,
    /// Priority in use: `base`, or more while a mutex lent it a higher one.
    effective: Priority,
    /// When a delayed or blocked task times out, `None` for no limit.
    deadline: Option<Tick>,
    waiting_on: Option<ObjId>,
    reason: WakeReason,
    /// Order among ready tasks of the same priority: smaller runs first.
    order: u64,
    /// Order among tasks blocked on the same object.
    blocked_order: u64,
    slice_left: u16,
}

const FREE: Tcb = Tcb {
    state: State::Free,
    name: "",
    base: Priority::IDLE,
    effective: Priority::IDLE,
    deadline: None,
    waiting_on: None,
    reason: WakeReason::None,
    order: 0,
    blocked_order: 0,
    slice_left: 0,
};

/// The kernel: a table of `N` tasks and the rules that pick which one runs.
///
/// Calls that act on "the running task" (`delay`, `wait_on`, `yield_now`, `exit_current`) are for
/// that task's own use, made through the port inside a critical section. The others (`create`,
/// `resume`, `tick`, `wake_one`...) may come from any task or from an interrupt.
#[derive(Clone, Debug)]
pub struct Kernel<const N: usize> {
    tasks: [Tcb; N],
    now: Tick,
    current: Option<TaskId>,
    slice: u16,
    /// Next `order` at the back of a priority's line, and at its front (a preempted task keeps its place).
    back: u64,
    front: u64,
    switches: u32,
}

impl<const N: usize> Kernel<N> {
    /// An empty kernel. `slice_ticks` is how long a task keeps the processor before an equal-priority
    /// task gets a turn.
    pub const fn new(slice_ticks: u16) -> Self {
        assert!(N > 0 && N <= 255);
        Self {
            tasks: [FREE; N],
            now: 0,
            current: None,
            slice: slice_ticks,
            back: 1 << 32,
            front: (1 << 32) - 1,
            switches: 0,
        }
    }

    // --- Looking ---------------------------------------------------------------------------

    /// The tick count.
    pub const fn now(&self) -> Tick {
        self.now
    }

    /// The running task.
    pub const fn current(&self) -> Option<TaskId> {
        self.current
    }

    /// How many task switches were made.
    pub const fn switches(&self) -> u32 {
        self.switches
    }

    /// What a task is doing (`Free` for an unknown one).
    pub fn state(&self, id: TaskId) -> State {
        self.tasks.get(id.index()).map_or(State::Free, |t| t.state)
    }

    /// The name a task was created with.
    pub fn name(&self, id: TaskId) -> &'static str {
        self.tasks.get(id.index()).map_or("", |t| t.name)
    }

    /// The priority in use (higher than the base one while a mutex lends it).
    pub fn priority(&self, id: TaskId) -> Priority {
        self.tasks.get(id.index()).map_or(Priority::IDLE, |t| t.effective)
    }

    /// The priority the task was created with.
    pub fn base_priority(&self, id: TaskId) -> Priority {
        self.tasks.get(id.index()).map_or(Priority::IDLE, |t| t.base)
    }

    /// Why a task last stopped waiting.
    pub fn wake_reason(&self, id: TaskId) -> WakeReason {
        self.tasks.get(id.index()).map_or(WakeReason::None, |t| t.reason)
    }

    /// Ticks until the next delay or time limit ends, `None` if none is pending: how long a
    /// tickless port may sleep.
    pub fn ticks_to_next_event(&self) -> Option<Tick> {
        self.tasks
            .iter()
            .filter(|t| matches!(t.state, State::Delayed | State::Blocked))
            .filter_map(|t| t.deadline)
            .map(|d| d.wrapping_sub(self.now) as i32)
            .map(|left| left.max(0) as Tick)
            .min()
    }

    // --- Creating and ending -----------------------------------------------------------------

    /// Adds a task, ready to run.
    pub fn create(&mut self, name: &'static str, priority: Priority) -> Result<TaskId, Error> {
        let index = self.tasks.iter().position(|t| t.state == State::Free).ok_or(Error::NoFreeTask)?;
        let order = self.next_back();
        self.tasks[index] = Tcb {
            state: State::Ready,
            name,
            base: priority,
            effective: priority,
            order,
            slice_left: self.slice,
            ..FREE
        };
        Ok(TaskId(index as u8))
    }

    /// Ends the running task and frees its slot. The port then calls [`schedule`](Self::schedule).
    pub fn exit_current(&mut self) -> Result<(), Error> {
        let id = self.current.ok_or(Error::NoCurrentTask)?;
        self.tasks[id.index()] = FREE;
        Ok(())
    }

    // --- Scheduling --------------------------------------------------------------------------

    /// Picks the first task to run (call it once, after creating the tasks).
    pub fn start(&mut self) -> Option<TaskId> {
        self.schedule().map(|s| s.to)
    }

    /// Whether a task should take the processor now: a ready task is more urgent than the running one,
    /// or the running one can't go on. Cheap, for an interrupt to decide whether to trigger the switch.
    pub fn needs_switch(&self) -> bool {
        match (self.running_task(), self.best_ready()) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(c), Some(b)) => self.tasks[b.index()].effective > self.tasks[c.index()].effective,
        }
    }

    /// Decides who runs: the most urgent ready task (the oldest one among equals), if it isn't the
    /// running one already. Returns what to switch, `None` if the running task goes on.
    pub fn schedule(&mut self) -> Option<Switch> {
        let best = self.best_ready();
        let from = self.current;
        match self.running_task() {
            Some(running) => {
                let best = best?;
                if self.tasks[best.index()].effective <= self.tasks[running.index()].effective {
                    return None;
                }
                // Preempted, not finished: it keeps its place at the front of its priority.
                let order = self.next_front();
                let t = &mut self.tasks[running.index()];
                t.state = State::Ready;
                t.order = order;
                self.dispatch(from, best, false)
            }
            None => {
                let best = best?;
                // The previous task may be gone, or just have yielded and be the best one again.
                let exited = from.is_some_and(|f| self.tasks[f.index()].state == State::Free);
                if from == Some(best) {
                    self.tasks[best.index()].state = State::Running;
                    return None;
                }
                self.dispatch(from, best, exited)
            }
        }
    }

    /// Gives the processor to the next ready task of the same priority (or keeps it if there is none).
    pub fn yield_now(&mut self) -> Result<(), Error> {
        let id = self.current.ok_or(Error::NoCurrentTask)?;
        let order = self.next_back();
        let t = &mut self.tasks[id.index()];
        t.state = State::Ready;
        t.order = order;
        Ok(())
    }

    /// Changes a task's base priority (the one it falls back to once no mutex lends it more).
    pub fn set_priority(&mut self, id: TaskId, priority: Priority) -> Result<(), Error> {
        let t = self.live(id)?;
        let lent = t.effective > t.base;
        t.base = priority;
        t.effective = if lent { t.effective.max(priority) } else { priority };
        Ok(())
    }

    /// Lends a task a priority at least as high as `priority` (priority inheritance).
    pub fn boost(&mut self, id: TaskId, priority: Priority) {
        if let Ok(t) = self.live(id) {
            t.effective = t.effective.max(priority);
        }
    }

    /// Takes back what was lent: the task goes back to its base priority.
    pub fn restore(&mut self, id: TaskId) {
        if let Ok(t) = self.live(id) {
            t.effective = t.base;
        }
    }

    // --- Time ------------------------------------------------------------------------------------

    /// One tick of the clock: wakes the delayed and timed-out tasks, and uses up the running task's time
    /// slice. Returns whether [`schedule`](Self::schedule) now has something to do.
    pub fn tick(&mut self) -> bool {
        self.now = self.now.wrapping_add(1);
        let now = self.now;
        for i in 0..N {
            let t = self.tasks[i];
            let due = t.deadline.is_some_and(|d| deadline_reached(now, d));
            if due && matches!(t.state, State::Delayed | State::Blocked) {
                let order = self.next_back();
                let t = &mut self.tasks[i];
                if t.state == State::Blocked {
                    t.reason = WakeReason::TimedOut;
                }
                t.state = State::Ready;
                t.deadline = None;
                t.waiting_on = None;
                t.order = order;
                t.slice_left = self.slice;
            }
        }
        // Time slice: after `slice` ticks, the running task goes to the back of its line if another task
        // of at least its priority is waiting.
        if let Some(c) = self.running_task() {
            let left = self.tasks[c.index()].slice_left.saturating_sub(1);
            self.tasks[c.index()].slice_left = left;
            if left == 0 {
                self.tasks[c.index()].slice_left = self.slice;
                let waiting = self
                    .best_ready()
                    .is_some_and(|b| self.tasks[b.index()].effective >= self.tasks[c.index()].effective);
                if waiting {
                    let order = self.next_back();
                    let t = &mut self.tasks[c.index()];
                    t.state = State::Ready;
                    t.order = order;
                }
            }
        }
        self.needs_switch()
    }

    /// Puts the running task to sleep for `ticks` (0 is a yield).
    pub fn delay(&mut self, ticks: Tick) -> Result<(), Error> {
        if ticks == 0 {
            return self.yield_now();
        }
        let id = self.current.ok_or(Error::NoCurrentTask)?;
        let deadline = self.now.wrapping_add(ticks);
        let t = &mut self.tasks[id.index()];
        t.state = State::Delayed;
        t.deadline = Some(deadline);
        t.waiting_on = None;
        t.reason = WakeReason::None;
        Ok(())
    }

    // --- Waiting on objects --------------------------------------------------------------------------

    /// Blocks the running task on `object`, until [`wake_one`](Self::wake_one) or
    /// [`wake_all`](Self::wake_all) on it, or until `deadline` (`None`: no limit).
    pub fn wait_on(&mut self, object: ObjId, deadline: Option<Tick>) -> Result<(), Error> {
        let id = self.current.ok_or(Error::NoCurrentTask)?;
        let order = self.next_back();
        let t = &mut self.tasks[id.index()];
        t.state = State::Blocked;
        t.waiting_on = Some(object);
        t.deadline = deadline;
        t.reason = WakeReason::None;
        t.blocked_order = order;
        Ok(())
    }

    /// Wakes the most urgent task blocked on `object` (the one that waited longest among equals).
    pub fn wake_one(&mut self, object: ObjId) -> Option<TaskId> {
        let id = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.state == State::Blocked && t.waiting_on == Some(object))
            .max_by(|(_, a), (_, b)| a.effective.cmp(&b.effective).then(b.blocked_order.cmp(&a.blocked_order)))
            .map(|(i, _)| TaskId(i as u8))?;
        self.make_ready(id, WakeReason::Signaled);
        Some(id)
    }

    /// Wakes every task blocked on `object`.
    pub fn wake_all(&mut self, object: ObjId) -> usize {
        let mut woken = 0;
        while self.wake_one(object).is_some() {
            woken += 1;
        }
        woken
    }

    /// Stops a task (the running one, or any other) until [`resume`](Self::resume).
    pub fn suspend(&mut self, id: TaskId) -> Result<(), Error> {
        let t = self.live(id)?;
        t.state = State::Suspended;
        t.deadline = None;
        t.waiting_on = None;
        Ok(())
    }

    /// Lets a suspended task run again.
    pub fn resume(&mut self, id: TaskId) -> Result<(), Error> {
        if self.live(id)?.state != State::Suspended {
            return Err(Error::BadState);
        }
        self.make_ready(id, WakeReason::None);
        Ok(())
    }

    // --- Inside --------------------------------------------------------------------------------------

    fn live(&mut self, id: TaskId) -> Result<&mut Tcb, Error> {
        match self.tasks.get_mut(id.index()) {
            Some(t) if t.state != State::Free => Ok(t),
            _ => Err(Error::BadTask),
        }
    }

    /// The running task, if it still is `Running` (not blocked, delayed, suspended or gone since).
    fn running_task(&self) -> Option<TaskId> {
        self.current.filter(|c| self.tasks[c.index()].state == State::Running)
    }

    fn next_back(&mut self) -> u64 {
        self.back += 1;
        self.back
    }

    fn next_front(&mut self) -> u64 {
        self.front -= 1;
        self.front
    }

    fn make_ready(&mut self, id: TaskId, reason: WakeReason) {
        let order = self.next_back();
        let slice = self.slice;
        let t = &mut self.tasks[id.index()];
        t.state = State::Ready;
        t.reason = reason;
        t.deadline = None;
        t.waiting_on = None;
        t.order = order;
        t.slice_left = slice;
    }

    /// The most urgent ready task, the oldest among equals.
    fn best_ready(&self) -> Option<TaskId> {
        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.state == State::Ready)
            .max_by(|(_, a), (_, b)| a.effective.cmp(&b.effective).then(b.order.cmp(&a.order)))
            .map(|(i, _)| TaskId(i as u8))
    }

    fn dispatch(&mut self, from: Option<TaskId>, to: TaskId, from_exited: bool) -> Option<Switch> {
        let t = &mut self.tasks[to.index()];
        t.state = State::Running;
        t.slice_left = self.slice;
        self.current = Some(to);
        self.switches = self.switches.wrapping_add(1);
        Some(Switch { from, to, from_exited })
    }
}
