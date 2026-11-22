//! Translocator's effect runtime: the side of the Crux app that touches
//! disks, servers, clocks and other processes.
//!
//! [`Runtime::start`] creates the [`tl_app::Translocator`] core and a
//! runtime thread that owns every call that changes it (`process_event`,
//! `resolve`) and every request still waiting for an answer, so events and
//! answers are applied in one deterministic order. Blocking work (provider
//! calls, jobs, local folder scans) runs on a cached worker pool; workers
//! never touch the core, they send their result back to the runtime thread
//! as a task. After each batch of messages the shell's [`Ui::render`] is
//! called once if the core asked for a render.
//!
//! Threads, all named `tl-*`:
//! * `tl-runtime`: the core and the request tables.
//! * `tl-worker`: a pool that grows when every worker is busy and shrinks
//!   when they idle (a job waiting for an answer never holds up a listing).
//! * `tl-store`: reads and writes the settings and bookmarks files in order.
//! * `tl-timer`: sleeps, watcher debouncing, the job progress ticker.
//! * `tl-health`: polls the server connections once a second.

mod ask;
mod exec;
mod store;
mod terminal;
mod work;

use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crux_core::capability::Operation;
use crux_core::{Core, Request, RequestHandle, Resolvable, ResolveError};
use parking_lot::{Mutex, RwLock};
use tl_app::conn::ConnInfo;
use tl_app::{
    ConnId, Effect, Event, FsOp, FsOutput, Health, HealthOp, JobId, JobOp, JobUpdate, LOCAL, PlatformOp,
    PlatformOutput, PluginOp, PluginOutput, PromptAnswer, PromptId, PromptOp, PromptOutput, PromptRequest, StoreOp,
    TabId, Translocator, ViewModel, WatchOp, WatchOutput, Work,
};
use tl_core::askpass::AskpassServer;
use tl_core::transfer::{Conflict, ConflictAnswer, ConflictChoice};
use tl_core::{CancelToken, PluginEnv, PluginRegistry, Progress, ProgressState, VfsError, VfsResult, Vfs};

pub use tl_app::ToastKind;

use crate::ask::RuntimePrompter;
use crate::exec::{Activity, Inbox, Msg, Pool, Timer};
use crate::store::Store;
use crate::work::{JobCtx, JobDone, crashed, not_connected};

/// What the runtime asks of the window system. Called from the runtime
/// thread (and, for [`Ui::open`] of a downloaded copy, from a worker): the
/// shell marshals to its UI thread.
pub trait Ui: Send + Sync + 'static {
    /// The view changed: read [`Runtime::view`] and redraw.
    fn render(&self);
    fn toast(&self, kind: ToastKind, message: String);
    fn write_clipboard(&self, text: String);
    /// Opens a local file or folder with its application.
    fn open(&self, path: &Path);
    /// Shows a local file in the system file manager.
    fn reveal(&self, path: &Path);
    fn open_url(&self, url: &str);
    fn quit(&self);
}

/// Where the runtime keeps things, and what it can connect to.
pub struct Config {
    /// Holds `settings.json` and `bookmarks.json`.
    pub data_dir: PathBuf,
    /// The app's own cache folder: local copies of remote files opened with
    /// their application (`{cache_dir}/{label}/{remote dir}/{name}`), and the
    /// macOS terminal launcher scripts.
    pub cache_dir: PathBuf,
    /// Private folder for sockets. `None` creates `$TMPDIR/tl-<pid>` (mode
    /// 0700, removed at shutdown), clearing those of dead processes first.
    pub runtime_dir: Option<PathBuf>,
    /// The executable `ssh` runs as `SSH_ASKPASS`. `None` starts no askpass
    /// server.
    pub askpass_exe: Option<PathBuf>,
    /// `None` registers the plugins compiled in ([`default_plugins`]).
    pub plugins: Option<PluginRegistry>,
}

impl Config {
    /// The places a normal run uses: the app data and cache folders of this
    /// user, a fresh runtime folder, askpass through this executable, and
    /// every plugin.
    pub fn standard() -> Config {
        Config {
            data_dir: tl_core::bookmark::app_data_dir(),
            cache_dir: dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("Translocator"),
            runtime_dir: None,
            askpass_exe: std::env::current_exe().ok(),
            plugins: None,
        }
    }
}

/// SSH, S3, WebDAV and the local folder plugin, in that order.
pub fn default_plugins() -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.register(Arc::new(tl_ssh::SshPlugin));
    registry.register(Arc::new(tl_s3::S3Plugin));
    registry.register(Arc::new(tl_webdav::WebDavPlugin));
    registry.register(Arc::new(tl_local::LocalPlugin));
    registry
}

/// A running app: the core and the threads that answer it. Cheap to clone.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

struct Inner {
    core: Arc<Core<Translocator>>,
    inbox: Inbox,
    thread: Mutex<Option<JoinHandle<()>>>,
    stopped: AtomicBool,
}

/// How long [`Runtime::shutdown`] waits for the runtime thread.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);
/// How long shutdown waits for servers to say goodbye.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Progress snapshots of running jobs go out at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(150);
/// A folder that changes is listed again this long after the first change.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(250);
/// Messages handled before the view is rendered, even if more are queued.
const BATCH: usize = 64;

impl Runtime {
    /// Creates the core, starts the runtime's threads and sends
    /// `Event::Start`.
    pub fn start(config: Config, ui: Arc<dyn Ui>) -> Runtime {
        let activity = Activity::default();
        let (inbox, rx) = Inbox::new(activity.clone());
        let core = Arc::new(Core::<Translocator>::new());
        let closed = Arc::new(AtomicBool::new(false));

        let (runtime_dir, owns_runtime_dir) = match config.runtime_dir {
            Some(dir) => {
                if let Err(err) = std::fs::create_dir_all(&dir) {
                    log::error!("cannot create {}: {err}", dir.display());
                }
                (dir, false)
            }
            None => (make_runtime_dir(), true),
        };
        let prompter = Arc::new(RuntimePrompter::new(inbox.clone(), closed.clone()));
        let askpass = config.askpass_exe.and_then(|exe| {
            match AskpassServer::start(runtime_dir.join("askpass.sock"), exe, prompter.clone()) {
                Ok(server) => Some(server),
                Err(err) => {
                    log::error!("askpass server unavailable: {err}");
                    None
                }
            }
        });
        let env = PluginEnv {
            prompter,
            askpass: askpass.as_ref().map(AskpassServer::handle),
            runtime_dir: runtime_dir.clone(),
        };

        let local = tl_local::local_vfs();
        let local_info = work::conn_info(LOCAL, &*local, None, None);
        let conns = ConnTable::default();
        conns.insert(LOCAL, local);

        let rt = Rt {
            core: core.clone(),
            ui,
            inbox: inbox.clone(),
            pool: Pool::new(activity.clone()),
            timer: Timer::start(inbox.clone()),
            store: Store::start(config.data_dir, inbox.clone()),
            health: start_health(conns.clone(), inbox.clone()),
            conns,
            registry: config.plugins.unwrap_or_else(default_plugins),
            env,
            cache_dir: config.cache_dir,
            local_info,
            jobs: HashMap::new(),
            ticking: false,
            watches: HashMap::new(),
            watch_gen: 0,
            prompt_sub: None,
            prompt_queue: VecDeque::new(),
            prompt_waiting: HashMap::new(),
            health_sub: None,
            effects: VecDeque::new(),
            render: false,
            closed,
            askpass,
            runtime_dir,
            owns_runtime_dir,
        };
        let thread = std::thread::Builder::new()
            .name("tl-runtime".into())
            .spawn(move || rt.run(rx))
            .expect("cannot start the runtime thread");

        let runtime = Runtime {
            inner: Arc::new(Inner {
                core,
                inbox,
                thread: Mutex::new(Some(thread)),
                stopped: AtomicBool::new(false),
            }),
        };
        runtime.dispatch(Event::Start { seed: seed() });
        runtime
    }

    /// Queues an event for the core; returns at once.
    pub fn dispatch(&self, event: Event) {
        self.inner.inbox.send(Msg::Event(event));
    }

    /// What the window shows now.
    pub fn view(&self) -> ViewModel {
        self.inner.core.view()
    }

    /// Waits until nothing is going on: no message queued, no worker
    /// running, no timer about to deliver. A job waiting for the user's
    /// answer and long-lived subscriptions do not count. `false` if that
    /// did not happen within `timeout`.
    pub fn settle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let activity = self.inner.inbox.activity();
        loop {
            if activity.is_idle() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Tells the core the app quits, writes what is pending, stops every
    /// job and watcher, disconnects the servers and removes the runtime
    /// folder it created. Later calls do nothing; dropping the last handle
    /// does the same.
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }
}

impl Inner {
    fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        self.inbox.send(Msg::Event(Event::Quitting));
        let (tx, rx) = mpsc::channel();
        if !self.inbox.send(Msg::Shutdown(tx)) {
            return;
        }
        if rx.recv_timeout(SHUTDOWN_TIMEOUT).is_err() {
            log::error!("the runtime did not stop within {SHUTDOWN_TIMEOUT:?}");
            return;
        }
        if let Some(thread) = self.thread.lock().take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ (u64::from(std::process::id()) << 32)
}

/// Removes runtime folders of instances that are no longer running.
fn remove_stale_runtime_dirs() {
    let Ok(read) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for item in read.flatten() {
        let name = item.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.strip_prefix("tl-")).and_then(|p| p.parse::<i32>().ok()) else {
            continue;
        };
        let alive = unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            let _ = std::fs::remove_dir_all(item.path());
        }
    }
}

/// Private per-process folder for control sockets.
fn make_runtime_dir() -> PathBuf {
    remove_stale_runtime_dirs();
    let dir = std::env::temp_dir().join(format!("tl-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

// ----- the connection table ------------------------------------------------------------

struct Conn {
    vfs: Arc<dyn Vfs>,
    /// A server (not the local disk, not an archive): it is polled for
    /// health and disconnected when released.
    server: bool,
}

fn is_server(vfs: &dyn Vfs) -> bool {
    vfs.plugin_id() != tl_local::PLUGIN_ID && vfs.host().is_none()
}

/// The live file systems by id. Written only by the runtime thread; read by
/// it and by the health poller.
#[derive(Clone, Default)]
struct ConnTable(Arc<RwLock<HashMap<ConnId, Conn>>>);

impl ConnTable {
    fn get(&self, id: ConnId) -> Option<Arc<dyn Vfs>> {
        self.0.read().get(&id).map(|c| c.vfs.clone())
    }

    fn contains(&self, id: ConnId) -> bool {
        self.0.read().contains_key(&id)
    }

    fn insert(&self, id: ConnId, vfs: Arc<dyn Vfs>) {
        let server = is_server(&*vfs);
        let old = self.0.write().insert(id, Conn { vfs, server });
        if let Some(old) = old.filter(|o| o.server) {
            work::disconnect(old.vfs);
        }
    }

    fn remove(&self, id: ConnId) -> Option<Conn> {
        self.0.write().remove(&id)
    }

    fn servers(&self) -> Vec<(ConnId, Arc<dyn Vfs>)> {
        self.0
            .read()
            .iter()
            .filter(|(_, c)| c.server)
            .map(|(id, c)| (*id, c.vfs.clone()))
            .collect()
    }

    fn drain_servers(&self) -> Vec<Arc<dyn Vfs>> {
        let mut table = self.0.write();
        let ids: Vec<ConnId> = table.iter().filter(|(_, c)| c.server).map(|(id, _)| *id).collect();
        ids.into_iter().filter_map(|id| table.remove(&id)).map(|c| c.vfs).collect()
    }
}

/// Polls `is_connected` of every server about once a second and reports
/// changes. Ends when the returned sender is dropped.
fn start_health(conns: ConnTable, inbox: Inbox) -> mpsc::Sender<()> {
    let (stop, stopped) = mpsc::channel::<()>();
    let spawned = std::thread::Builder::new().name("tl-health".into()).spawn(move || {
        let mut last: HashMap<ConnId, bool> = HashMap::new();
        while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(Duration::from_secs(1)) {
            let servers = conns.servers();
            last.retain(|id, _| servers.iter().any(|(s, _)| s == id));
            for (id, vfs) in servers {
                let connected = vfs.is_connected();
                if last.insert(id, connected).unwrap_or(true) != connected {
                    inbox.task(move |rt| rt.health(id, connected));
                }
            }
        }
    });
    if let Err(err) = spawned {
        log::error!("cannot start the connection health thread: {err}");
    }
    stop
}

// ----- the runtime thread ------------------------------------------------------------------

struct JobEntry {
    request: Request<JobOp>,
    cancel: CancelToken,
    progress: Progress,
    started: Instant,
    /// The last snapshot sent.
    last: Option<ProgressState>,
    /// Conflicts the worker waits on, by ask id.
    asks: HashMap<u64, mpsc::Sender<ConflictAnswer>>,
}

impl JobEntry {
    /// Unblocks the worker's open questions with "cancel".
    fn cancel_asks(&mut self) {
        for (_, tx) in self.asks.drain() {
            let _ = tx.send(ConflictAnswer::new(ConflictChoice::Cancel));
        }
    }
}

struct WatchEntry {
    request: Request<WatchOp>,
    _watcher: notify::RecommendedWatcher,
    /// A change is waiting out the debounce.
    pending: Arc<AtomicBool>,
    /// Tells this watcher's timers from those of one it replaced.
    generation: u64,
}

/// The state the runtime thread owns.
pub(crate) struct Rt {
    core: Arc<Core<Translocator>>,
    ui: Arc<dyn Ui>,
    inbox: Inbox,
    pool: Pool,
    timer: Timer,
    store: Store,
    health: mpsc::Sender<()>,
    conns: ConnTable,
    registry: PluginRegistry,
    env: PluginEnv,
    cache_dir: PathBuf,
    local_info: ConnInfo,
    jobs: HashMap<JobId, JobEntry>,
    /// A progress tick is scheduled.
    ticking: bool,
    watches: HashMap<TabId, WatchEntry>,
    watch_gen: u64,
    prompt_sub: Option<Request<PromptOp>>,
    /// Questions asked before the core subscribed (or after it let go).
    prompt_queue: VecDeque<PromptRequest>,
    /// Workers waiting for an answer, by prompt.
    prompt_waiting: HashMap<PromptId, mpsc::Sender<PromptAnswer>>,
    health_sub: Option<Request<HealthOp>>,
    /// Effects still to be carried out, in the order the core issued them.
    effects: VecDeque<Effect>,
    /// The core asked for a render in this batch.
    render: bool,
    /// Shutdown has begun: questions are answered "no" at once.
    closed: Arc<AtomicBool>,
    askpass: Option<AskpassServer>,
    runtime_dir: PathBuf,
    owns_runtime_dir: bool,
}

/// Resolves through `Core::resolve` without tripping its debug assertion:
/// a request the core no longer listens to (a notification, a stream it
/// dropped) is reported back instead.
struct Guarded<'a, O> {
    handle: &'a mut RequestHandle<O>,
    open: bool,
}

impl<O> Resolvable<O> for Guarded<'_, O> {
    fn resolve(&mut self, output: O) -> Result<(), ResolveError> {
        if self.handle.resolve(output).is_err() {
            self.open = false;
        }
        Ok(())
    }
}

/// Answers `request`; the effects that follow are queued. Returns whether
/// the request still takes answers (a stream the core still reads).
fn resolve_into<Op: Operation>(
    core: &Core<Translocator>,
    effects: &mut VecDeque<Effect>,
    request: &mut Request<Op>,
    output: Op::Output,
) -> bool {
    if matches!(request.handle, RequestHandle::Never) {
        return false;
    }
    let mut guarded = Guarded {
        handle: &mut request.handle,
        open: true,
    };
    if let Ok(more) = core.resolve(&mut guarded, output) {
        effects.extend(more);
    }
    guarded.open && !matches!(request.handle, RequestHandle::Never)
}

impl Rt {
    fn run(mut self, rx: mpsc::Receiver<Msg>) {
        while let Ok(first) = rx.recv() {
            let mut handled = 0;
            let mut next = Some(first);
            let mut stop = None;
            while let Some(msg) = next.take() {
                handled += 1;
                if let Some(ack) = self.handle(msg) {
                    stop = Some(ack);
                    break;
                }
                if handled < BATCH {
                    next = rx.try_recv().ok();
                }
            }
            if std::mem::take(&mut self.render) && catch_unwind(AssertUnwindSafe(|| self.ui.render())).is_err() {
                log::error!("the shell panicked while rendering");
            }
            for _ in 0..handled {
                self.inbox.activity().dec();
            }
            if let Some(ack) = stop {
                self.shutdown();
                let _ = ack.send(());
                // Whatever is still queued is dropped, and with it every
                // reply channel a worker may be waiting on.
                while let Ok(msg) = rx.try_recv() {
                    drop(msg);
                    self.inbox.activity().dec();
                }
                return;
            }
        }
    }

    /// One message and the effects that follow from it.
    fn handle(&mut self, msg: Msg) -> Option<mpsc::Sender<()>> {
        let result = catch_unwind(AssertUnwindSafe(|| {
            match msg {
                Msg::Event(event) => {
                    let effects = self.core.process_event(event);
                    self.effects.extend(effects);
                }
                Msg::Task(task) => task(self),
                Msg::Shutdown(ack) => return Some(ack),
            }
            while let Some(effect) = self.effects.pop_front() {
                self.effect(effect);
            }
            None
        }));
        match result {
            Ok(stop) => stop,
            Err(_) => {
                log::error!("the runtime caught a panic while handling a message");
                self.effects.clear();
                None
            }
        }
    }

    pub(crate) fn resolve<Op: Operation>(&mut self, request: &mut Request<Op>, output: Op::Output) -> bool {
        resolve_into(&self.core, &mut self.effects, request, output)
    }

    /// Runs `work` on a worker, then `done` with its result (`None` if it
    /// panicked) back on the runtime thread.
    fn spawn<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut Rt, Option<T>) + Send + 'static,
    ) {
        let inbox = self.inbox.clone();
        self.pool.execute(move || {
            let result = catch_unwind(AssertUnwindSafe(work)).ok();
            inbox.task(move |rt| done(rt, result));
        });
    }

    fn effect(&mut self, effect: Effect) {
        match effect {
            Effect::Render(_) => self.render = true,
            Effect::Fs(request) => self.fs(request),
            Effect::Job(request) => self.job(request),
            Effect::Prompt(request) => self.prompt(request),
            Effect::Store(request) => self.store(request),
            Effect::Watch(request) => self.watch(request),
            Effect::Health(request) => match request.operation {
                HealthOp::Subscribe => self.health_sub = Some(request),
            },
            Effect::Platform(request) => self.platform(request),
            Effect::Plugin(request) => self.plugin(request),
            Effect::Sleep(mut request) => {
                let delay = Duration::from_millis(request.operation.0);
                self.timer.after(delay, true, move |rt| {
                    rt.resolve(&mut request, ());
                });
            }
        }
    }

    // ----- file systems -----

    fn fs(&mut self, mut request: Request<FsOp>) {
        let conn = work::fs_conn(&request.operation);
        if let FsOp::Release { conn } = request.operation {
            self.release(conn);
            self.resolve(&mut request, FsOutput::Done(Ok(())));
            return;
        }
        let Some(vfs) = self.conns.get(conn) else {
            let output = work::fs_failure(&request.operation, not_connected());
            self.resolve(&mut request, output);
            return;
        };
        let op = request.operation.clone();
        let registry = self.registry.clone();
        self.spawn(
            move || work::fs(&op, vfs, &registry),
            move |rt, done| {
                let done = done.unwrap_or_else(|| work::fs_failure(&request.operation, crashed()).into());
                if let Some((id, vfs)) = done.register {
                    rt.conns.insert(id, vfs);
                }
                rt.resolve(&mut request, done.output);
            },
        );
    }

    fn release(&mut self, conn: ConnId) {
        if conn == LOCAL {
            return;
        }
        if let Some(gone) = self.conns.remove(conn).filter(|c| c.server) {
            work::disconnect(gone.vfs);
        }
    }

    // ----- jobs -----

    fn job(&mut self, request: Request<JobOp>) {
        match request.operation.clone() {
            JobOp::Start { id, work } => self.job_start(id, work, request),
            JobOp::Cancel { id } => {
                if let Some(job) = self.jobs.get_mut(&id) {
                    job.cancel.cancel();
                    job.cancel_asks();
                }
            }
            JobOp::Answer { id, ask, answer } => {
                if let Some(tx) = self.jobs.get_mut(&id).and_then(|j| j.asks.remove(&ask)) {
                    let _ = tx.send(answer);
                }
            }
        }
    }

    fn job_start(&mut self, id: JobId, work: Work, mut request: Request<JobOp>) {
        let refuse = if self.closed.load(Ordering::SeqCst) {
            Some(VfsError::Cancelled)
        } else if self.jobs.contains_key(&id) {
            Some(VfsError::other(format!("job {} is already running", id.0)))
        } else {
            None
        };
        if let Some(err) = refuse {
            self.resolve(&mut request, JobUpdate::Finished(Err(err)));
            return;
        }
        let mut conns = HashMap::new();
        for conn in work::work_conns(&work) {
            match self.conns.get(conn) {
                Some(vfs) => {
                    conns.insert(conn, vfs);
                }
                None => {
                    self.resolve(&mut request, JobUpdate::Finished(Err(not_connected())));
                    return;
                }
            }
        }
        let cancel = CancelToken::new();
        let progress = Progress::new();
        let cx = JobCtx {
            job: id,
            cancel: cancel.clone(),
            progress: progress.clone(),
            conns,
            registry: self.registry.clone(),
            env: self.env.clone(),
            inbox: self.inbox.clone(),
            cache_dir: self.cache_dir.clone(),
            ui: self.ui.clone(),
        };
        self.jobs.insert(
            id,
            JobEntry {
                request,
                cancel,
                progress,
                started: Instant::now(),
                last: None,
                asks: HashMap::new(),
            },
        );
        self.schedule_tick();
        self.spawn(
            move || work::job(work, &cx),
            move |rt, done| rt.job_finished(id, done.unwrap_or_else(|| Err(crashed()))),
        );
    }

    fn job_finished(&mut self, id: JobId, result: VfsResult<JobDone>) {
        let Some(mut job) = self.jobs.remove(&id) else {
            if let Ok(JobDone {
                register: Some((_, vfs)), ..
            }) = result
                && is_server(&*vfs)
            {
                work::disconnect(vfs);
            }
            return;
        };
        job.cancel_asks();
        let result = result.and_then(|done| match done.register {
            Some((_, vfs)) if job.cancel.is_cancelled() || self.closed.load(Ordering::SeqCst) => {
                // Connected after all, but nobody wants it any more.
                if is_server(&*vfs) {
                    work::disconnect(vfs);
                }
                Err(VfsError::Cancelled)
            }
            Some((conn, vfs)) => {
                self.conns.insert(conn, vfs);
                Ok(done.output)
            }
            None => Ok(done.output),
        });
        self.resolve(&mut job.request, JobUpdate::Finished(result));
    }

    /// A worker of job `id` waits for the answer to `conflict`.
    pub(crate) fn job_conflict(&mut self, id: JobId, ask: u64, conflict: Conflict, tx: mpsc::Sender<ConflictAnswer>) {
        let closed = self.closed.load(Ordering::SeqCst);
        match self.jobs.get_mut(&id) {
            Some(job) if !closed && !job.cancel.is_cancelled() => {
                job.asks.insert(ask, tx);
                let update = JobUpdate::Conflict { ask, conflict };
                if !resolve_into(&self.core, &mut self.effects, &mut job.request, update) {
                    // Nobody will answer: stop the job.
                    job.cancel.cancel();
                    job.cancel_asks();
                }
            }
            _ => {
                let _ = tx.send(ConflictAnswer::new(ConflictChoice::Cancel));
            }
        }
    }

    fn schedule_tick(&mut self) {
        if !self.ticking && !self.jobs.is_empty() {
            self.ticking = true;
            self.timer.after(PROGRESS_EVERY, false, Rt::tick);
        }
    }

    /// Sends what changed in each running job's progress.
    fn tick(&mut self) {
        self.ticking = false;
        for job in self.jobs.values_mut() {
            let state = job.progress.snapshot();
            if job.last.as_ref() == Some(&state) {
                continue;
            }
            job.last = Some(state.clone());
            let update = JobUpdate::Progress {
                state,
                elapsed_ms: job.started.elapsed().as_millis() as u64,
            };
            resolve_into(&self.core, &mut self.effects, &mut job.request, update);
        }
        self.schedule_tick();
    }

    // ----- prompts -----

    fn prompt(&mut self, request: Request<PromptOp>) {
        match request.operation.clone() {
            PromptOp::Subscribe => {
                self.prompt_sub = Some(request);
                while let Some(question) = self.prompt_queue.pop_front() {
                    if !self.offer_prompt(question.clone()) {
                        self.prompt_queue.push_front(question);
                        break;
                    }
                }
            }
            PromptOp::Answer { id, answer } => {
                if let Some(tx) = self.prompt_waiting.remove(&id) {
                    let _ = tx.send(answer);
                }
            }
        }
    }

    /// Hands a question to the core, if it listens.
    fn offer_prompt(&mut self, question: PromptRequest) -> bool {
        let Some(sub) = self.prompt_sub.as_mut() else { return false };
        if resolve_into(&self.core, &mut self.effects, sub, PromptOutput::Request(question)) {
            true
        } else {
            self.prompt_sub = None;
            false
        }
    }

    /// A worker waits for the answer to `question`.
    pub(crate) fn prompt_ask(&mut self, question: PromptRequest, tx: mpsc::Sender<PromptAnswer>) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        self.prompt_waiting.insert(question.id, tx);
        if !self.offer_prompt(question.clone()) {
            self.prompt_queue.push_back(question);
        }
    }

    // ----- settings and bookmarks -----

    fn store(&mut self, mut request: Request<StoreOp>) {
        if let StoreOp::Flush = request.operation {
            self.store.flush();
            self.resolve(&mut request, tl_app::StoreOutput::Saved(Ok(())));
        } else {
            self.store.submit(request);
        }
    }

    // ----- watchers -----

    fn watch(&mut self, request: Request<WatchOp>) {
        match request.operation.clone() {
            WatchOp::Start { tab, path } => {
                self.stop_watch(tab);
                self.start_watch(tab, &path.to_local(), request);
            }
            WatchOp::Stop { tab } => self.stop_watch(tab),
        }
    }

    fn start_watch(&mut self, tab: TabId, path: &Path, mut request: Request<WatchOp>) {
        use notify::Watcher as _;
        self.watch_gen += 1;
        let generation = self.watch_gen;
        let pending = Arc::new(AtomicBool::new(false));
        let (timer, flag) = (self.timer.clone(), pending.clone());
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            if !flag.swap(true, Ordering::SeqCst) {
                timer.after(WATCH_DEBOUNCE, true, move |rt| rt.watch_fired(tab, generation));
            }
        });
        let watcher = watcher.and_then(|mut w| w.watch(path, notify::RecursiveMode::NonRecursive).map(|()| w));
        match watcher {
            Ok(watcher) => {
                self.watches.insert(
                    tab,
                    WatchEntry {
                        request,
                        _watcher: watcher,
                        pending,
                        generation,
                    },
                );
            }
            Err(err) => {
                log::warn!("cannot watch {}: {err}", path.display());
                self.resolve(&mut request, WatchOutput::Stopped);
            }
        }
    }

    fn watch_fired(&mut self, tab: TabId, generation: u64) {
        let Some(watch) = self.watches.get_mut(&tab).filter(|w| w.generation == generation) else { return };
        watch.pending.store(false, Ordering::SeqCst);
        if !resolve_into(&self.core, &mut self.effects, &mut watch.request, WatchOutput::Changed) {
            self.watches.remove(&tab);
        }
    }

    fn stop_watch(&mut self, tab: TabId) {
        if let Some(mut watch) = self.watches.remove(&tab) {
            self.resolve(&mut watch.request, WatchOutput::Stopped);
        }
    }

    // ----- connection health -----

    pub(crate) fn health(&mut self, conn: ConnId, connected: bool) {
        if !self.conns.contains(conn) {
            return;
        }
        if let Some(sub) = self.health_sub.as_mut()
            && !resolve_into(&self.core, &mut self.effects, sub, Health { conn, connected })
        {
            self.health_sub = None;
        }
    }

    // ----- the desktop -----

    fn platform(&mut self, mut request: Request<PlatformOp>) {
        let output = match &request.operation {
            PlatformOp::Open(path) => {
                self.ui.open(&path.to_local());
                PlatformOutput::Done
            }
            PlatformOp::Reveal(path) => {
                self.ui.reveal(&path.to_local());
                PlatformOutput::Done
            }
            PlatformOp::WriteClipboard(text) => {
                self.ui.write_clipboard(text.clone());
                PlatformOutput::Done
            }
            PlatformOp::OpenUrl(url) => {
                self.ui.open_url(url);
                PlatformOutput::Done
            }
            PlatformOp::Toast { kind, message } => {
                self.ui.toast(*kind, message.clone());
                PlatformOutput::Done
            }
            PlatformOp::Quit => {
                self.ui.quit();
                PlatformOutput::Done
            }
            PlatformOp::LocalTimes(times) => PlatformOutput::Times(
                times
                    .iter()
                    .map(|t| t.and_then(tl_core::multirename::local_time))
                    .collect(),
            ),
            PlatformOp::LocalFolders => {
                self.spawn(work::local_folders, move |rt, folders| {
                    rt.resolve(&mut request, PlatformOutput::Folders(folders.unwrap_or_default()));
                });
                return;
            }
            PlatformOp::OpenTerminal {
                conn,
                path,
                elevated,
                preferred,
            } => {
                let Some(vfs) = self.conns.get(*conn) else {
                    self.resolve(&mut request, PlatformOutput::Failed(not_connected().to_string()));
                    return;
                };
                let (path, elevated, preferred) = (path.clone(), *elevated, preferred.clone());
                let scripts = self.cache_dir.clone();
                self.spawn(
                    move || work::open_terminal(&*vfs, &path, elevated, &preferred, &scripts),
                    move |rt, result| {
                        let output = match result {
                            Some(Ok(())) => PlatformOutput::Done,
                            Some(Err(err)) => PlatformOutput::Failed(err.to_string()),
                            None => PlatformOutput::Failed(crashed().to_string()),
                        };
                        rt.resolve(&mut request, output);
                    },
                );
                return;
            }
        };
        self.resolve(&mut request, output);
    }

    // ----- plugins -----

    fn plugin(&mut self, mut request: Request<PluginOp>) {
        let output = match &request.operation {
            PluginOp::List => PluginOutput::Plugins {
                plugins: self
                    .registry
                    .all()
                    .iter()
                    .map(|p| tl_core::plugin::plugin_info(&**p))
                    .collect(),
                local: self.local_info.clone(),
            },
            PluginOp::Validate { plugin, config } => PluginOutput::Valid(match self.registry.get(plugin) {
                Some(p) => p.validate(config),
                None => Err(format!("unknown type “{plugin}”")),
            }),
            PluginOp::Summaries(bookmarks) => PluginOutput::Summaries(
                bookmarks
                    .iter()
                    .map(|b| match self.registry.get(&b.plugin) {
                        Some(p) => p.summary(b),
                        None => format!("unknown type “{}”", b.plugin),
                    })
                    .collect(),
            ),
        };
        self.resolve(&mut request, output);
    }

    // ----- shutdown -----

    fn shutdown(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        // What the core asked to write, including the last settings.
        while let Some(effect) = self.effects.pop_front() {
            self.effect(effect);
        }
        self.store.flush();
        // Unblock every worker that waits for the user.
        for job in self.jobs.values_mut() {
            job.cancel.cancel();
            job.cancel_asks();
        }
        self.prompt_waiting.clear();
        self.prompt_queue.clear();
        self.prompt_sub = None;
        self.health_sub = None;
        self.watches.clear();
        self.timer.stop();
        self.pool.close();
        let (health, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.health, health));

        // Say goodbye to the servers, all at once, for a few seconds at most.
        let servers = self.conns.drain_servers();
        let (done, finished) = mpsc::channel();
        let mut waiting = 0;
        for vfs in servers {
            let done = done.clone();
            if std::thread::Builder::new()
                .name("tl-disconnect".into())
                .spawn(move || {
                    vfs.disconnect();
                    let _ = done.send(());
                })
                .is_ok()
            {
                waiting += 1;
            }
        }
        let deadline = Instant::now() + DISCONNECT_TIMEOUT;
        while waiting > 0 && finished.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_ok() {
            waiting -= 1;
        }

        self.askpass = None;
        if self.owns_runtime_dir {
            let _ = std::fs::remove_dir_all(&self.runtime_dir);
        }
    }
}
