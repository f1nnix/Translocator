//! The plumbing under the runtime: the channel into the runtime thread, the
//! worker pool that runs blocking provider calls, the timer thread, and the
//! activity count `Runtime::settle` waits on.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::Rt;

/// Work for the runtime thread: applied to its state, in arrival order.
pub(crate) type Task = Box<dyn FnOnce(&mut Rt) + Send>;

pub(crate) enum Msg {
    Event(tl_app::Event),
    Task(Task),
    Shutdown(mpsc::Sender<()>),
}

/// How much is still going on: messages queued for the runtime thread,
/// worker tasks, timers that will deliver something. Long-lived things
/// (subscriptions, idle watchers, the progress ticker) are not counted, and
/// a worker blocked on a question to the user steps out while it waits.
#[derive(Clone, Default)]
pub(crate) struct Activity(Arc<AtomicIsize>);

impl Activity {
    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    pub fn dec(&self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn is_idle(&self) -> bool {
        self.0.load(Ordering::SeqCst) <= 0
    }

    /// The calling worker waits for the user: it is not activity until the
    /// guard drops.
    pub fn waiting(&self) -> WaitGuard {
        self.dec();
        WaitGuard(self.clone())
    }
}

pub(crate) struct WaitGuard(Activity);

impl Drop for WaitGuard {
    fn drop(&mut self) {
        self.0.inc();
    }
}

/// The way into the runtime thread. Sending never fails loudly: once the
/// runtime has stopped, messages are dropped (and with them any reply
/// channel inside, which unblocks whoever waits on it).
#[derive(Clone)]
pub(crate) struct Inbox {
    tx: mpsc::Sender<Msg>,
    activity: Activity,
}

impl Inbox {
    pub fn new(activity: Activity) -> (Inbox, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel();
        (Inbox { tx, activity }, rx)
    }

    pub fn send(&self, msg: Msg) -> bool {
        self.activity.inc();
        if self.tx.send(msg).is_err() {
            self.activity.dec();
            return false;
        }
        true
    }

    pub fn task(&self, task: impl FnOnce(&mut Rt) + Send + 'static) -> bool {
        self.send(Msg::Task(Box::new(task)))
    }

    pub fn activity(&self) -> &Activity {
        &self.activity
    }
}

// ----- worker pool -------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;

/// How long an idle worker waits for more work before its thread ends.
const IDLE: Duration = Duration::from_secs(30);

/// A cached thread pool: a task goes to an idle worker if there is one, and
/// to a new thread otherwise. Threads are reused (no spawn per listing), but
/// the pool is never full: a job blocked on a password prompt or a conflict
/// question cannot hold up a listing behind it, which a fixed-size pool
/// would allow.
#[derive(Clone)]
pub(crate) struct Pool {
    shared: Arc<PoolShared>,
    activity: Activity,
}

struct PoolShared {
    state: Mutex<PoolState>,
    wake: Condvar,
}

#[derive(Default)]
struct PoolState {
    queue: VecDeque<Job>,
    idle: usize,
    closed: bool,
}

impl Pool {
    pub fn new(activity: Activity) -> Pool {
        Pool {
            shared: Arc::new(PoolShared {
                state: Mutex::new(PoolState::default()),
                wake: Condvar::new(),
            }),
            activity,
        }
    }

    /// Runs `job` on a worker thread. It counts as activity until it returns.
    pub fn execute(&self, job: impl FnOnce() + Send + 'static) {
        self.activity.inc();
        let activity = self.activity.clone();
        let job: Job = Box::new(move || {
            if catch_unwind(AssertUnwindSafe(job)).is_err() {
                log::error!("a worker task panicked");
            }
            activity.dec();
        });
        let mut state = self.shared.state.lock();
        state.queue.push_back(job);
        if state.queue.len() > state.idle {
            drop(state);
            let shared = self.shared.clone();
            if let Err(err) = std::thread::Builder::new()
                .name("tl-worker".into())
                .spawn(move || worker(&shared))
            {
                // The task stays queued for the next worker that frees up.
                log::error!("cannot start a worker thread: {err}");
            }
        } else {
            self.shared.wake.notify_one();
        }
    }

    /// Lets idle workers end; tasks already queued still run.
    pub fn close(&self) {
        self.shared.state.lock().closed = true;
        self.shared.wake.notify_all();
    }
}

fn worker(shared: &PoolShared) {
    let mut state = shared.state.lock();
    loop {
        if let Some(job) = state.queue.pop_front() {
            drop(state);
            job();
            state = shared.state.lock();
            continue;
        }
        if state.closed {
            return;
        }
        state.idle += 1;
        let timed_out = shared.wake.wait_for(&mut state, IDLE).timed_out();
        state.idle -= 1;
        if timed_out && state.queue.is_empty() {
            return;
        }
    }
}

// ----- timer -----------------------------------------------------------------------

enum TimerMsg {
    At { at: Instant, task: Task, tracked: bool },
    Stop,
}

/// One thread for every delay the runtime needs (sleeps, watcher
/// debouncing, the progress ticker): a due task is sent to the runtime
/// thread.
#[derive(Clone)]
pub(crate) struct Timer {
    tx: mpsc::Sender<TimerMsg>,
    activity: Activity,
}

impl Timer {
    pub fn start(inbox: Inbox) -> Timer {
        let (tx, rx) = mpsc::channel();
        let activity = inbox.activity().clone();
        let thread_activity = activity.clone();
        let spawned = std::thread::Builder::new()
            .name("tl-timer".into())
            .spawn(move || timer_thread(rx, inbox, thread_activity));
        if let Err(err) = spawned {
            log::error!("cannot start the timer thread: {err}");
        }
        Timer { tx, activity }
    }

    /// Runs `task` on the runtime thread after `delay`. A `tracked` timer
    /// counts as activity until it fires.
    pub fn after(&self, delay: Duration, tracked: bool, task: impl FnOnce(&mut Rt) + Send + 'static) {
        if tracked {
            self.activity.inc();
        }
        let msg = TimerMsg::At {
            at: Instant::now() + delay,
            task: Box::new(task),
            tracked,
        };
        if self.tx.send(msg).is_err() && tracked {
            self.activity.dec();
        }
    }

    pub fn stop(&self) {
        let _ = self.tx.send(TimerMsg::Stop);
    }
}

fn timer_thread(rx: mpsc::Receiver<TimerMsg>, inbox: Inbox, activity: Activity) {
    let mut heap: BinaryHeap<Reverse<(Instant, u64)>> = BinaryHeap::new();
    let mut tasks: HashMap<u64, (Task, bool)> = HashMap::new();
    let mut seq = 0u64;
    loop {
        let now = Instant::now();
        while let Some(Reverse((at, id))) = heap.peek().copied() {
            if at > now {
                break;
            }
            heap.pop();
            if let Some((task, tracked)) = tasks.remove(&id) {
                inbox.send(Msg::Task(task));
                if tracked {
                    activity.dec();
                }
            }
        }
        let msg = match heap.peek() {
            Some(Reverse((at, _))) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(msg) => msg,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => TimerMsg::Stop,
            },
            None => rx.recv().unwrap_or(TimerMsg::Stop),
        };
        match msg {
            TimerMsg::At { at, task, tracked } => {
                seq += 1;
                heap.push(Reverse((at, seq)));
                tasks.insert(seq, (task, tracked));
            }
            TimerMsg::Stop => {
                for (_, (_, tracked)) in tasks.drain() {
                    if tracked {
                        activity.dec();
                    }
                }
                return;
            }
        }
    }
}
