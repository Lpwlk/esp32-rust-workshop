//! The kernel as a port drives it: the tests play the part of the tasks and of the tick interrupt.

use mros::{Error, Kernel, Mutex, Priority, Queue, Semaphore, State, TaskId, Timeout, WakeReason, deadline_reached};

fn p(level: u8) -> Priority {
    Priority::new(level).unwrap()
}

/// A kernel with an idle task, and the ids of the tasks made after it.
fn system<const N: usize>(slice: u16, priorities: &[u8]) -> (Kernel<N>, TaskId, Vec<TaskId>) {
    let mut k = Kernel::new(slice);
    let idle = k.create("idle", Priority::IDLE).unwrap();
    let tasks = priorities.iter().map(|&l| k.create("task", p(l)).unwrap()).collect();
    (k, idle, tasks)
}

/// The tick interrupt followed by the context switch it asks for: who runs afterwards.
fn tick<const N: usize>(k: &mut Kernel<N>) -> TaskId {
    if k.tick() {
        k.schedule();
    }
    k.current().unwrap()
}

#[test]
fn the_most_urgent_task_runs_first_and_the_oldest_among_equals() {
    let (mut k, _idle, t) = system::<5>(10, &[3, 7, 7, 1]);
    assert_eq!(k.start(), Some(t[1]));
    assert_eq!(k.state(t[1]), State::Running);
    assert_eq!(k.state(t[2]), State::Ready);
    assert_eq!(k.start(), None, "the running task is the most urgent: nothing to switch");
}

#[test]
fn a_delay_ends_on_time_across_the_wrap_of_the_clock() {
    let (mut k, idle, t) = system::<3>(10, &[5]);
    // Start close to the wrap of the 32-bit tick counter.
    for _ in 0..3 {
        k.tick();
    }
    assert!(deadline_reached(5, 5) && !deadline_reached(4, 5));
    assert!(deadline_reached(1, u32::MAX - 1), "1 is after u32::MAX - 1, across the wrap");
    assert_eq!(k.start(), Some(t[0]));
    k.delay(4).unwrap();
    assert_eq!(k.schedule().unwrap().to, idle);
    assert_eq!(k.ticks_to_next_event(), Some(4));
    for _ in 0..3 {
        assert_eq!(tick(&mut k), idle);
    }
    assert_eq!(tick(&mut k), t[0], "ready again at the 4th tick, and it preempts idle");
    assert_eq!(k.ticks_to_next_event(), None);
}

#[test]
fn a_task_made_ready_preempts_a_less_urgent_one_and_the_preempted_keeps_its_place() {
    let (mut k, idle, t) = system::<5>(100, &[2, 2, 9]);
    k.suspend(t[2]).unwrap();
    assert_eq!(k.start(), Some(t[0]));
    // t0 and t1 are equals; t2 wakes up and preempts t0.
    k.resume(t[2]).unwrap();
    assert!(k.needs_switch());
    let switch = k.schedule().unwrap();
    assert_eq!((switch.from, switch.to), (Some(t[0]), t[2]));
    // t2 sleeps: t0 (preempted, so first in line) comes back before t1.
    k.delay(5).unwrap();
    assert_eq!(k.schedule().unwrap().to, t[0]);
    let _ = idle;
}

#[test]
fn equal_priorities_share_the_processor_by_time_slices() {
    let (mut k, _idle, t) = system::<4>(3, &[4, 4]);
    assert_eq!(k.start(), Some(t[0]));
    let mut order = Vec::new();
    for _ in 0..12 {
        order.push(tick(&mut k));
    }
    // A turn every 3 ticks: t0 t0 t1 | t1 t1 t0 | ... (a change on the 3rd tick of each slice).
    assert_eq!(order, [t[0], t[0], t[1], t[1], t[1], t[0], t[0], t[0], t[1], t[1], t[1], t[0]]);
}

#[test]
fn a_task_alone_at_its_priority_keeps_its_slice() {
    let (mut k, _idle, t) = system::<3>(2, &[6]);
    assert_eq!(k.start(), Some(t[0]));
    for _ in 0..10 {
        assert_eq!(tick(&mut k), t[0]);
    }
    assert_eq!(k.switches(), 1);
}

#[test]
fn yield_passes_the_turn_to_an_equal_and_not_to_a_lesser() {
    let (mut k, _idle, t) = system::<4>(50, &[4, 4, 2]);
    assert_eq!(k.start(), Some(t[0]));
    k.yield_now().unwrap();
    assert_eq!(k.schedule().unwrap().to, t[1]);
    k.yield_now().unwrap();
    assert_eq!(k.schedule().unwrap().to, t[0]);
    k.suspend(t[1]).unwrap();
    k.yield_now().unwrap();
    assert_eq!(k.schedule(), None, "alone at its priority: the yield changes nothing");
    assert_eq!(k.state(t[0]), State::Running);
}

#[test]
fn exit_frees_the_slot_and_a_new_task_can_take_it() {
    let (mut k, idle, t) = system::<2>(10, &[5]);
    k.start();
    k.exit_current().unwrap();
    assert_eq!(k.state(t[0]), State::Free);
    let switch = k.schedule().unwrap();
    assert_eq!((switch.to, switch.from_exited), (idle, true));
    let again = k.create("again", p(5)).unwrap();
    assert_eq!(again, t[0], "same slot");
    assert_eq!(k.create("one too many", p(1)).unwrap_err(), Error::NoFreeTask);
}

#[test]
fn suspend_and_resume() {
    let (mut k, idle, t) = system::<3>(10, &[5]);
    k.start();
    k.suspend(t[0]).unwrap();
    assert_eq!(k.schedule().unwrap().to, idle);
    assert_eq!(k.resume(idle), Err(Error::BadState), "only a suspended task can be resumed");
    k.resume(t[0]).unwrap();
    assert_eq!(k.schedule().unwrap().to, t[0]);
}

#[test]
fn a_semaphore_wakes_the_most_urgent_waiter_and_a_timeout_wakes_by_itself() {
    let (mut k, idle, t) = system::<4>(100, &[3, 8, 5]);
    let mut sem = Semaphore::new(1, 0, 10);

    // Each of the three blocks on the semaphore, most urgent first as the scheduler runs them.
    let mut blocked = Vec::new();
    assert_eq!(k.start(), Some(t[1]));
    for _ in 0..3 {
        let me = k.current().unwrap();
        assert!(!sem.try_take());
        let deadline = if me == t[2] { Timeout::Ticks(4).deadline(k.now()) } else { None };
        sem.wait(&mut k, deadline).unwrap();
        blocked.push(me);
        k.schedule();
    }
    assert_eq!(blocked, [t[1], t[2], t[0]]);
    assert_eq!(k.current(), Some(idle));

    // A token: the most urgent waiter (t1, priority 8) is woken, and takes it.
    assert!(sem.give(&mut k));
    assert_eq!(k.wake_reason(t[1]), WakeReason::Signaled);
    assert_eq!(k.schedule().unwrap().to, t[1]);
    assert!(sem.try_take());
    assert_eq!(sem.count(), 0);

    // t2 timed out after 4 ticks; t0 still waits.
    k.delay(100).unwrap();
    k.schedule();
    let mut running = idle;
    for _ in 0..4 {
        running = tick(&mut k);
    }
    assert_eq!(running, t[2]);
    assert_eq!(k.wake_reason(t[2]), WakeReason::TimedOut);
    assert_eq!(k.state(t[0]), State::Blocked);
}

#[test]
fn a_semaphore_does_not_exceed_its_maximum() {
    let (mut k, _idle, _t) = system::<2>(10, &[]);
    let mut sem = Semaphore::new(1, 1, 2);
    assert!(sem.give(&mut k));
    assert!(!sem.give(&mut k));
    assert_eq!(sem.count(), 2);
}

#[test]
fn priority_inheritance_keeps_a_middle_task_from_starving_the_holder() {
    // low holds the mutex; high wants it; mid is runnable the whole time.
    let (mut k, _idle, t) = system::<5>(1000, &[2, 5, 8]);
    let (low, mid, high) = (t[0], t[1], t[2]);
    let mut m = Mutex::new(7);
    k.suspend(mid).unwrap();
    k.suspend(high).unwrap();
    assert_eq!(k.start(), Some(low));
    assert_eq!(m.try_lock(&k), Ok(true));

    // mid and high become ready: high preempts, and blocks on the mutex.
    k.resume(mid).unwrap();
    k.resume(high).unwrap();
    assert_eq!(k.schedule().unwrap().to, high);
    assert_eq!(m.try_lock(&k), Ok(false));
    m.wait(&mut k, None).unwrap();

    // Without inheritance mid (5) would run now, ahead of low (2). With it, low runs at 8.
    assert_eq!(k.priority(low), p(8));
    assert_eq!(k.base_priority(low), p(2));
    assert_eq!(k.schedule().unwrap().to, low);

    // low is done: it unlocks, drops back to 2, and high (woken) takes over.
    m.unlock(&mut k).unwrap();
    assert_eq!(k.priority(low), p(2));
    assert_eq!(k.schedule().unwrap().to, high);
    assert_eq!(m.try_lock(&k), Ok(true));
    assert_eq!(m.owner(), Some(high));
    // Only the holder can unlock.
    k.delay(10).unwrap();
    assert_eq!(k.schedule().unwrap().to, mid);
    assert_eq!(m.unlock(&mut k), Err(Error::BadState));
}

#[test]
fn a_queue_hands_messages_over_in_order_and_blocks_both_ways() {
    let (mut k, idle, t) = system::<3>(100, &[6, 4]);
    let (reader, writer) = (t[0], t[1]);
    let mut q: Queue<u32, 2> = Queue::new(3);

    // The reader runs first, finds nothing, and blocks.
    assert_eq!(k.start(), Some(reader));
    assert_eq!(q.try_recv(&mut k), None);
    q.wait_readable(&mut k, None).unwrap();
    assert_eq!(k.schedule().unwrap().to, writer);

    // The writer sends: the reader wakes and, being more urgent, preempts at once.
    assert_eq!(q.try_send(&mut k, 10), Ok(()));
    assert!(k.needs_switch());
    assert_eq!(k.schedule().unwrap().to, reader);
    assert_eq!(q.try_recv(&mut k), Some(10));

    // Fill it (2 messages): the third doesn't fit, and the writer would wait.
    assert_eq!(q.try_send(&mut k, 1), Ok(()));
    assert_eq!(q.try_send(&mut k, 2), Ok(()));
    assert_eq!(q.try_send(&mut k, 3), Err(3));
    assert!(q.is_full());
    k.suspend(reader).unwrap();
    assert_eq!(k.schedule().unwrap().to, writer);
    q.wait_writable(&mut k, None).unwrap();
    assert_eq!(k.schedule().unwrap().to, idle);

    // A read makes room and wakes the writer; messages come out in the order they went in.
    k.resume(reader).unwrap();
    assert_eq!(k.schedule().unwrap().to, reader);
    assert_eq!(q.try_recv(&mut k), Some(1));
    assert_eq!(q.try_recv(&mut k), Some(2));
    assert_eq!(q.try_recv(&mut k), None);
    assert_eq!(k.state(writer), State::Ready);
}

#[test]
fn calls_for_the_running_task_refuse_before_the_start() {
    let mut k: Kernel<2> = Kernel::new(5);
    assert_eq!(k.delay(3), Err(Error::NoCurrentTask));
    assert_eq!(k.exit_current(), Err(Error::NoCurrentTask));
    assert_eq!(k.wait_on(1, None), Err(Error::NoCurrentTask));
    assert_eq!(k.schedule(), None, "no task at all");
}
