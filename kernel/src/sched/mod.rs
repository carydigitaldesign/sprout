//! Kernel threads and a round-robin scheduler.
//!
//! Every thread owns a 16 KiB kernel stack and a saved [`Context`]. A
//! global scheduler holds raw pointers to leaked `Box<Thread>`s in a run
//! queue, a sleeping list and a blocked list; threads are freed by
//! whichever thread runs next after they exit. All of it lives behind one
//! spinlock, which masks interrupts, so the timer tick and threads never
//! race for it.
//!
//! The boot context becomes thread 1, `main`. Thread 2, `idle`, only runs
//! when nothing else can. Preemption: every timer tick, if another thread
//! is ready, the running one goes to the back of the queue.
//!
//! A thread may belong to a process (M4); switching to it also switches
//! the address space. Kernel-only threads run in whatever space is active,
//! since the kernel is mapped in all of them.

pub mod wait;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::arch::{self, Context, PAGE_SIZE, phys_to_virt};
use crate::mm::frame::{self, Frame};
use crate::obj::Process;
use crate::sync::SpinLock;

pub type ThreadId = u64;
/// A thread body. Returning ends the thread.
pub type Entry = fn(usize);

/// Frames per thread stack: 16 KiB.
pub const STACK_FRAMES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    Sleeping { until: u64 },
    Blocked,
    Exited,
}

struct Thread {
    id: ThreadId,
    name: String,
    state: State,
    context: Context,
    /// First frame of the kernel stack, or `None` for the boot stack.
    stack: Option<Frame>,
    stack_bounds: (usize, usize),
    /// What to run first; taken by the trampoline.
    entry: Option<(Entry, usize)>,
    /// The process this thread runs user code for, if any.
    process: Option<Arc<Process>>,
    /// Root of the address space to run in.
    root_pa: usize,
}

impl Drop for Thread {
    fn drop(&mut self) {
        if let Some(first) = self.stack.take() {
            frame::free_contiguous(first, STACK_FRAMES);
        }
    }
}

/// A thread the scheduler owns. Only dereferenced under the scheduler lock.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Ptr(*mut Thread);

// SAFETY: the pointer is only dereferenced while holding the scheduler
// lock, which also serialises the switches that move a thread between
// harts, so the `Thread` is never accessed from two places at once.
unsafe impl Send for Ptr {}

fn thread<'a>(p: Ptr) -> &'a mut Thread {
    // SAFETY: `p` came from `Box::leak` and has not been reaped (reaping
    // removes a pointer from every list first), and the caller holds the
    // scheduler lock, so this is the only live reference.
    unsafe { &mut *p.0 }
}

struct Scheduler {
    current: Ptr,
    idle: Ptr,
    run_queue: VecDeque<Ptr>,
    sleeping: Vec<Ptr>,
    blocked: Vec<Ptr>,
    zombies: Vec<Ptr>,
    /// Every thread that has not exited, for `is_alive` and counts.
    live: Vec<Ptr>,
    next_id: ThreadId,
}

static SCHED: SpinLock<Option<Scheduler>> = SpinLock::new(None);

fn with<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    let mut slot = SCHED.lock();
    f(slot.as_mut().expect("scheduler used before init"))
}

/// Turns the boot context into thread `main` and creates `idle`.
pub fn init() {
    let (bottom, top) = arch::boot_stack();
    let kernel_root = crate::mm::kernel_root_pa();
    let main = Box::leak(Box::new(Thread {
        id: 1,
        name: String::from("main"),
        state: State::Running,
        context: Context::empty(),
        stack: None,
        stack_bounds: (bottom, top),
        entry: None,
        process: None,
        root_pa: kernel_root,
    }));
    let idle = new_thread(2, "idle", idle_entry, 0, None, kernel_root);
    let mut slot = SCHED.lock();
    assert!(slot.is_none(), "scheduler initialised twice");
    *slot = Some(Scheduler {
        current: Ptr(main),
        idle: Ptr(idle),
        run_queue: VecDeque::new(),
        sleeping: Vec::new(),
        blocked: Vec::new(),
        zombies: Vec::new(),
        live: alloc::vec![Ptr(main), Ptr(idle)],
        next_id: 3,
    });
    arch::set_current_stack(bottom, top);
}

fn idle_entry(_: usize) {
    loop {
        arch::interrupts::wait_for_interrupt();
    }
}

fn new_thread(
    id: ThreadId,
    name: &str,
    entry: Entry,
    arg: usize,
    process: Option<Arc<Process>>,
    root_pa: usize,
) -> *mut Thread {
    let first = frame::alloc_contiguous(STACK_FRAMES).expect("no memory for a thread stack");
    let bottom = phys_to_virt(first.pa());
    let top = bottom + STACK_FRAMES * PAGE_SIZE;
    Box::leak(Box::new(Thread {
        id,
        name: String::from(name),
        state: State::Ready,
        context: Context::new(trampoline, top),
        stack: Some(first),
        stack_bounds: (bottom, top),
        entry: Some((entry, arg)),
        process,
        root_pa,
    }))
}

fn enqueue_new(t: *mut Thread) -> ThreadId {
    let p = Ptr(t);
    with(|s| {
        s.live.push(p);
        s.run_queue.push_back(p);
        thread(p).id
    })
}

fn next_id() -> ThreadId {
    with(|s| {
        let id = s.next_id;
        s.next_id += 1;
        id
    })
}

/// Creates a kernel thread that runs `entry(arg)` and queues it.
pub fn spawn(name: &str, entry: Entry, arg: usize) -> ThreadId {
    let id = next_id();
    let kernel_root = crate::mm::kernel_root_pa();
    enqueue_new(new_thread(id, name, entry, arg, None, kernel_root))
}

/// Creates the kernel thread behind a user process's thread. `entry` runs
/// on the kernel stack in the process's address space and is expected to
/// drop into user mode.
pub fn spawn_user(
    name: &str,
    process: Arc<Process>,
    root_pa: usize,
    entry: Entry,
    arg: usize,
) -> ThreadId {
    let id = next_id();
    enqueue_new(new_thread(id, name, entry, arg, Some(process), root_pa))
}

/// First code on a fresh stack, reached through `switch` with interrupts
/// off and no locks held.
extern "C" fn trampoline() -> ! {
    let (entry, arg) = with(|s| {
        thread(s.current)
            .entry
            .take()
            .expect("new thread has no entry")
    });
    arch::interrupts::enable();
    entry(arg);
    exit()
}

enum Next {
    Yield,
    Sleep { until: u64 },
    Block,
    Exit,
}

/// Puts the current thread where `why` says and runs the next ready one
/// (or idle). Returns when the current thread is scheduled again.
fn schedule(why: Next) {
    let interrupts_were_enabled = arch::interrupts::disable();
    let switch_to = with(|s| {
        let cur = s.current;
        match why {
            Next::Yield => {
                thread(cur).state = State::Ready;
                if cur != s.idle {
                    s.run_queue.push_back(cur);
                }
            }
            Next::Sleep { until } => {
                thread(cur).state = State::Sleeping { until };
                s.sleeping.push(cur);
            }
            Next::Block => {
                thread(cur).state = State::Blocked;
                s.blocked.push(cur);
            }
            Next::Exit => {
                thread(cur).state = State::Exited;
                s.live.retain(|p| *p != cur);
                s.zombies.push(cur);
            }
        }
        let next = s.run_queue.pop_front().unwrap_or(s.idle);
        thread(next).state = State::Running;
        if next == cur {
            return None;
        }
        s.current = next;
        let (bottom, top) = thread(next).stack_bounds;
        arch::set_current_stack(bottom, top);
        if thread(next).root_pa != thread(cur).root_pa {
            // SAFETY: every table carries the kernel half, so the code and
            // stack running this switch stay mapped across it.
            unsafe { arch::activate_root(thread(next).root_pa) };
        }
        Some((cur, next))
    });

    if let Some((prev, next)) = switch_to {
        // SAFETY: `prev` is the thread running this code, so its slot is
        // the right place to save into. `next` is a live thread whose
        // context was saved by an earlier switch or built by
        // `Context::new` on a stack that is still allocated. Interrupts
        // are off and the scheduler lock was released above.
        unsafe { arch::switch(&mut thread(prev).context, &thread(next).context) };
    }
    reap();
    arch::interrupts::restore(interrupts_were_enabled);
}

/// Frees threads that exited. Runs on a different stack than theirs.
fn reap() {
    let zombies = with(|s| core::mem::take(&mut s.zombies));
    for z in zombies {
        // SAFETY: the pointer came from `Box::leak`, the thread has exited
        // and no list refers to it any more, so this is the last use.
        drop(unsafe { Box::from_raw(z.0) });
    }
}

/// Lets other ready threads run; returns once this one is picked again.
pub fn yield_now() {
    schedule(Next::Yield);
}

/// Ends the current thread.
pub fn exit() -> ! {
    schedule(Next::Exit);
    unreachable!("an exited thread was scheduled")
}

/// Blocks until the tick count reaches `until`.
pub fn sleep_until(until: u64) {
    schedule(Next::Sleep { until });
}

/// Timer tick: readies sleepers that are due. Returns whether the current
/// thread should give way.
pub fn on_tick(now: u64) -> bool {
    with(|s| {
        let mut i = 0;
        while i < s.sleeping.len() {
            let p = s.sleeping[i];
            if matches!(thread(p).state, State::Sleeping { until } if until <= now) {
                s.sleeping.swap_remove(i);
                thread(p).state = State::Ready;
                s.run_queue.push_back(p);
            } else {
                i += 1;
            }
        }
        !s.run_queue.is_empty()
    })
}

/// Readies a blocked thread. Returns false if it was not blocked.
pub fn wake(id: ThreadId) -> bool {
    with(|s| {
        let Some(pos) = s.blocked.iter().position(|p| thread(*p).id == id) else {
            return false;
        };
        let p = s.blocked.swap_remove(pos);
        thread(p).state = State::Ready;
        s.run_queue.push_back(p);
        true
    })
}

/// The running thread's id.
pub fn current_id() -> ThreadId {
    with(|s| thread(s.current).id)
}

/// The running thread's name.
pub fn current_name() -> String {
    with(|s| thread(s.current).name.clone())
}

/// The process the running thread belongs to, if any.
pub fn current_process() -> Option<Arc<Process>> {
    with(|s| thread(s.current).process.clone())
}

/// Top of the running thread's kernel stack.
pub fn current_stack_top() -> usize {
    with(|s| thread(s.current).stack_bounds.1)
}

/// Whether a thread exists and has not exited.
pub fn is_alive(id: ThreadId) -> bool {
    with(|s| s.live.iter().any(|p| thread(*p).id == id))
}

/// Threads that have not exited, idle included.
pub fn live_threads() -> usize {
    with(|s| s.live.len())
}

/// Waits for a thread to exit.
pub fn join(id: ThreadId) {
    while is_alive(id) {
        yield_now();
    }
}

#[cfg(feature = "ktest")]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::time;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn count_with_yields(n: usize) {
        for _ in 0..n {
            COUNTER.fetch_add(1, Ordering::Relaxed);
            yield_now();
        }
    }

    crate::ktest! {
        fn threads_run_round_robin_and_are_reaped() {
            let frames_before = frame::stats().free;
            let threads_before = live_threads();
            COUNTER.store(0, Ordering::Relaxed);
            let ids = [
                spawn("worker-a", count_with_yields, 300),
                spawn("worker-b", count_with_yields, 300),
                spawn("worker-c", count_with_yields, 300),
            ];
            for id in ids {
                join(id);
            }
            assert_eq!(COUNTER.load(Ordering::Relaxed), 900);
            yield_now();
            assert_eq!(live_threads(), threads_before);
            assert_eq!(frame::stats().free, frames_before, "thread stacks were not freed");
        }
    }

    static SPINNER: AtomicUsize = AtomicUsize::new(0);
    static STOP: AtomicUsize = AtomicUsize::new(0);

    fn spin_without_yielding(_: usize) {
        while STOP.load(Ordering::Relaxed) == 0 {
            SPINNER.fetch_add(1, Ordering::Relaxed);
        }
    }

    crate::ktest! {
        fn timer_preempts_a_thread_that_never_yields() {
            STOP.store(0, Ordering::Relaxed);
            SPINNER.store(0, Ordering::Relaxed);
            let id = spawn("spinner", spin_without_yielding, 0);
            // Sleeping hands the CPU to the spinner; only preemption can
            // bring us back while it is still spinning.
            time::sleep_ms(30);
            let seen = SPINNER.load(Ordering::Relaxed);
            assert!(seen > 0, "the spinner never ran");
            assert!(is_alive(id), "the spinner should still be running");
            STOP.store(1, Ordering::Relaxed);
            join(id);
        }
    }

    crate::ktest! {
        fn current_thread_is_main_during_tests() {
            assert_eq!(current_id(), 1);
            assert_eq!(current_name(), "main");
            assert!(current_process().is_none());
        }
    }
}
