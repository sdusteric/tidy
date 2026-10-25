//! The actor behind [`crate::start`]: one task that owns the [`Env`], receives
//! [`Command`]s and sends [`Event`]s (design section 9, core boundary).
//!
//! - Scans run on `spawn_blocking`, at most one of each kind at a time; a second request
//!   while one runs is ignored. [`Command::Cancel`] stops them at their next folder and each
//!   answers with [`Event::ScanStopped`].
//! - Clean, Move to Trash and Empty Trash run on `spawn_blocking` one at a time, in the order
//!   they were asked for. They never overlap, so two actions never move the same item.
//! - The process list is read only when the UI asks ([`Command::CheckRunning`]).
//! - A memory snapshot is read only when asked ([`Command::SnapshotMemory`]); a request while
//!   one is being read is ignored, so a slow read never piles up behind the 2 s refresh.
//! - A Security run and a check up run on `spawn_blocking`, at most one of each at a time; a
//!   second request while one runs is ignored, and so is a Security run while a check up
//!   (which includes one) runs. Signature checks are reported one at a time with
//!   [`Event::SecurityProgress`].
//! - [`Command::Shutdown`] stops the scans, asks a running action to stop after its current
//!   item, waits a short grace for both, then replies. A running check up is told to stop
//!   before its next part and then writes nothing; a Security run is left to finish on its
//!   own thread (it only reads, then saves its run's file atomically).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::api::{Command, CoreHandle, Event, ScanKind};
use crate::checkup::{self, CheckupOutcome, CheckupStop};
use crate::clean::{self, NOT_IN_PLAN};
use crate::cleanup::{CleanupProgress, load_last_cleanup, scan_cleanup};
use crate::env::Env;
use crate::history;
use crate::large::{self, LargeOpts};
use crate::memory;
use crate::model::{
    HistoryEntry, Item, LARGE_RECORD_MIN, LargeScan, MemorySnapshot, Plan, RunningSet,
    SecurityReport,
};
use crate::reminder;
use crate::security::{self, Sources};
use crate::walk::{ProgressFn, WalkProgress};

/// How long [`Command::Shutdown`] waits for scans and a running action to stop.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Starts the core on `rt` and returns the command handle and the event stream.
///
/// Creates Tidy's folders (a failure is logged, not fatal). It does not take the instance
/// lock: the app and the destructive tidyctl commands take it themselves, so read-only
/// commands can run while the app is open. Safe to call from inside or outside the runtime;
/// it never blocks on it.
pub fn start(env: Env, rt: tokio::runtime::Handle) -> (CoreHandle, mpsc::UnboundedReceiver<Event>) {
    let sources = Sources::for_env(&env);
    start_with_sources(env, sources, rt)
}

/// [`start`] with the folders and signature checker the Security checks read, so tests can
/// point them at fixtures. [`start`] uses [`Sources::for_env`].
pub fn start_with_sources(
    env: Env,
    sources: Sources,
    rt: tokio::runtime::Handle,
) -> (CoreHandle, mpsc::UnboundedReceiver<Event>) {
    if let Err(e) = env.paths.create_all() {
        tracing::warn!("cannot create Tidy's folders: {e}");
    }
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let (done_tx, done_rx) = mpsc::unbounded_channel();
    let actor = Actor {
        env,
        sources,
        events: ev_tx,
        done: done_tx,
        last_running: None,
        plan: None,
        cleanup_scan: None,
        large_scan: None,
        job: None,
        queue: VecDeque::new(),
        memory_busy: false,
        memory_failing: false,
        security_busy: false,
        checkup: None,
        checkup_scanning: false,
    };
    rt.spawn(actor.run(cmd_rx, done_rx));
    (CoreHandle::new(cmd_tx), ev_rx)
}

/// A destructive action waiting for its turn.
#[derive(Debug)]
enum Job {
    Clean(Vec<String>),
    Trash(Vec<PathBuf>),
    EmptyTrash,
}

impl Job {
    fn what(&self) -> &'static str {
        match self {
            Job::Clean(_) => "clean",
            Job::Trash(_) => "trash",
            Job::EmptyTrash => "empty_trash",
        }
    }
}

/// What a blocking task hands back to the actor when it ends.
enum Done {
    Cleanup {
        plan: Option<Plan>,
        cancelled: bool,
    },
    Large {
        /// None when the scan was cancelled.
        scan: Option<LargeScan>,
        /// The scan panicked.
        failed: bool,
    },
    Job {
        what: &'static str,
        empty_trash: bool,
        entry: Option<HistoryEntry>,
    },
    /// A memory snapshot, or why the figures could not be read.
    Memory(Result<MemorySnapshot, String>),
    /// None when the run panicked.
    Security(Option<SecurityReport>),
    /// An event of a running check up. The check up sends its events through the actor,
    /// in order, so the actor takes its Cleanup plan before the app sees it.
    CheckupEvent(Event),
    /// None when the check up was aborted or panicked.
    Checkup(Option<CheckupOutcome>),
}

struct Actor {
    env: Env,
    /// Where the Security checks read from.
    sources: Sources,
    events: mpsc::UnboundedSender<Event>,
    done: mpsc::UnboundedSender<Done>,
    /// The process list last sent with `RunningChanged`.
    last_running: Option<RunningSet>,
    /// The last Cleanup plan, which `Clean` takes its items from. Loaded from
    /// `last-cleanup.json` on first use when no scan has run yet.
    plan: Option<Plan>,
    /// The stop flag of the running scan of each kind.
    cleanup_scan: Option<Arc<AtomicBool>>,
    large_scan: Option<Arc<AtomicBool>>,
    /// The stop flag of the running action.
    job: Option<Arc<AtomicBool>>,
    queue: VecDeque<Job>,
    /// A memory snapshot is being read.
    memory_busy: bool,
    /// The last snapshot failed, so a failing probe reports once, not every 2 s.
    memory_failing: bool,
    /// A Security run (not the check up's) is going.
    security_busy: bool,
    /// The stop flags of the running check up.
    checkup: Option<Arc<CheckupStop>>,
    /// The running check up is still in its Cleanup scan. A `ScanCleanup` then waits for
    /// that scan's plan instead of starting a second scan.
    checkup_scanning: bool,
}

impl Actor {
    async fn run(
        mut self,
        mut rx: mpsc::UnboundedReceiver<Command>,
        mut done_rx: mpsc::UnboundedReceiver<Done>,
    ) {
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(Command::Shutdown(reply)) => {
                        tracing::info!("core: shutting down");
                        self.stop_all(&mut done_rx).await;
                        let _ = reply.send(());
                        return;
                    }
                    Some(other) => self.handle(other).await,
                    None => {
                        // Every handle is gone: stop the same way, with no one to tell.
                        self.stop_all(&mut done_rx).await;
                        return;
                    }
                },
                Some(done) = done_rx.recv() => self.finish(done),
            }
        }
    }

    fn emit(&self, ev: Event) {
        // The receiver is gone only when the UI is shutting down.
        let _ = self.events.send(ev);
    }

    async fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::ScanCleanup => self.start_cleanup_scan(),
            Command::ScanLarge { record_min } => self.start_large_scan(record_min),
            Command::Clean { item_ids } => self.enqueue(Job::Clean(item_ids)),
            Command::TrashPaths { paths } => self.enqueue(Job::Trash(paths)),
            Command::EmptyTidyTrash => self.enqueue(Job::EmptyTrash),
            Command::Cancel => {
                for flag in [&self.cleanup_scan, &self.large_scan].into_iter().flatten() {
                    flag.store(true, Ordering::Relaxed);
                }
                if let Some(stop) = &self.checkup {
                    stop.cancel.store(true, Ordering::Relaxed);
                }
            }
            Command::Ignore { path } => {
                let mut s = self.env.settings.clone();
                s.ignore(path);
                self.save_settings(s);
            }
            Command::UpdateSettings(s) => self.save_settings(s),
            Command::CheckRunning => {
                let procs = self.env.procs.clone();
                let running = match tokio::task::spawn_blocking(move || procs.running()).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("process check failed: {e}");
                        return;
                    }
                };
                if self.last_running.as_ref() != Some(&running) {
                    self.last_running = Some(running.clone());
                    self.emit(Event::RunningChanged(running));
                }
            }
            Command::LoadHistory { reply } => {
                let paths = self.env.paths.clone();
                let entries = tokio::task::spawn_blocking(move || history::read(&paths))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("cannot read history: {e}");
                        Vec::new()
                    });
                let _ = reply.send(entries);
            }
            // ----- M2 -----
            Command::SnapshotMemory => self.start_memory_snapshot(),
            Command::RunSecurity { include_updates } => self.start_security(include_updates),
            Command::LastSecurity { reply } => {
                let paths = self.env.paths.clone();
                let last = tokio::task::spawn_blocking(move || security::latest_report(&paths))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("cannot read the security reports: {e}");
                        None
                    });
                let _ = reply.send(last);
            }
            Command::RunCheckup { include_updates } => self.start_checkup(include_updates),
            Command::LastCheckup { reply } => {
                let paths = self.env.paths.clone();
                let last = tokio::task::spawn_blocking(move || reminder::last_checkup(&paths))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("cannot read the check ups: {e}");
                        None
                    });
                let _ = reply.send(last);
            }
            Command::NextReminder { reply } => {
                let paths = self.env.paths.clone();
                let last = tokio::task::spawn_blocking(move || reminder::last_checkup(&paths))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("cannot read the check ups: {e}");
                        None
                    });
                let date =
                    reminder::next_reminder(last.map(|c| c.at), self.env.now(), &self.env.tz);
                let _ = reply.send(date);
            }
            Command::MeasureSystemData { reply } => {
                // Read-only and possibly slow (a walk of a few folders): run it beside the
                // actor so scans and actions are not held up.
                let env = self.env.clone();
                tokio::spawn(async move {
                    let report =
                        tokio::task::spawn_blocking(move || crate::sysdata::measure(&env)).await;
                    match report {
                        Ok(r) => {
                            let _ = reply.send(r);
                        }
                        Err(e) => tracing::warn!("system data measure failed: {e}"),
                    }
                });
            }
            // Handled in `run`; kept so the match stays exhaustive.
            Command::Shutdown(done) => {
                let _ = done.send(());
            }
        }
    }

    // ----- Scans -----

    fn start_cleanup_scan(&mut self) {
        if self.cleanup_scan.is_some() {
            tracing::debug!("a cleanup scan is already running");
            return;
        }
        if self.checkup_scanning {
            // The check up's scan sends the plan this request asks for.
            tracing::debug!("the check up is scanning for Cleanup");
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cleanup_scan = Some(cancel.clone());
        let env = self.env.clone();
        let events = self.events.clone();
        let done = self.done.clone();
        tokio::task::spawn_blocking(move || {
            let progress = |p: CleanupProgress| {
                let _ = events.send(Event::ScanProgress {
                    kind: ScanKind::Cleanup,
                    files: p.files,
                    bytes: p.bytes,
                    current: p.current,
                });
            };
            let plan = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scan_cleanup(&env, &progress, &cancel)
            }))
            .ok();
            let _ = done.send(Done::Cleanup {
                plan,
                cancelled: cancel.load(Ordering::Relaxed),
            });
        });
    }

    fn start_large_scan(&mut self, record_min: Option<u64>) {
        if self.large_scan.is_some() {
            tracing::debug!("a large files scan is already running");
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.large_scan = Some(cancel.clone());
        let env = self.env.clone();
        let events = self.events.clone();
        let done = self.done.clone();
        let opts = LargeOpts {
            cancel: Some(cancel),
            progress: Some(ProgressFn::new(move |p: &WalkProgress| {
                let _ = events.send(Event::ScanProgress {
                    kind: ScanKind::Large,
                    files: p.files,
                    bytes: p.bytes,
                    current: p.current.clone(),
                });
            })),
            ..LargeOpts::with_record_min(record_min.unwrap_or(LARGE_RECORD_MIN))
        };
        tokio::task::spawn_blocking(move || {
            let scan = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let scan = large::scan(&env, &opts);
                if let Some(s) = &scan
                    && let Err(e) = large::save(&env.paths, s)
                {
                    tracing::warn!("cannot save the large files scan: {e}");
                }
                scan
            }));
            let _ = done.send(match scan {
                Ok(scan) => Done::Large {
                    scan,
                    failed: false,
                },
                Err(_) => Done::Large {
                    scan: None,
                    failed: true,
                },
            });
        });
    }

    // ----- Memory, Security, Check up -----

    fn start_memory_snapshot(&mut self) {
        if self.memory_busy {
            tracing::debug!("a memory snapshot is already being read");
            return;
        }
        self.memory_busy = true;
        let env = self.env.clone();
        let done = self.done.clone();
        tokio::task::spawn_blocking(move || {
            let snap =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| memory::take(&env)))
                    .unwrap_or_else(|_| Err(std::io::Error::other("the memory read panicked")))
                    .map_err(|e| e.to_string());
            let _ = done.send(Done::Memory(snap));
        });
    }

    fn start_security(&mut self, include_updates: bool) {
        if self.security_busy || self.checkup.is_some() {
            tracing::debug!("a security run is already going");
            return;
        }
        self.security_busy = true;
        let env = self.env.clone();
        let sources = self.sources.clone();
        let events = self.events.clone();
        let done = self.done.clone();
        tokio::task::spawn_blocking(move || {
            let report = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut progress = |done: usize, total: usize| {
                    let _ = events.send(Event::SecurityProgress { done, total });
                };
                security::run_and_save_with(&env, &sources, include_updates, &mut progress)
            }))
            .ok();
            let _ = done.send(Done::Security(report));
        });
    }

    fn start_checkup(&mut self, include_updates: bool) {
        if self.checkup.is_some() {
            tracing::debug!("a check up is already going");
            return;
        }
        let stop = Arc::new(CheckupStop::new());
        self.checkup = Some(stop.clone());
        self.checkup_scanning = true;
        let env = self.env.clone();
        let sources = self.sources.clone();
        let done = self.done.clone();
        tokio::task::spawn_blocking(move || {
            // Every event goes through the actor (see `Done::CheckupEvent`).
            let emit = |e: Event| {
                let _ = done.send(Done::CheckupEvent(e));
            };
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                checkup::run(&env, &sources, include_updates, &stop, &emit)
            }))
            .ok()
            .flatten();
            let _ = done.send(Done::Checkup(outcome));
        });
    }

    // ----- Actions -----

    fn enqueue(&mut self, job: Job) {
        self.queue.push_back(job);
        self.start_next_job();
    }

    fn start_next_job(&mut self) {
        if self.job.is_some() {
            return;
        }
        let Some(job) = self.queue.pop_front() else {
            return;
        };
        let what = job.what();
        let empty_trash = matches!(job, Job::EmptyTrash);
        // Ids the plan does not know are answered here; the rest go to the executor.
        let work = match job {
            Job::Clean(ids) => Work::Clean(self.items_for(&ids)),
            Job::Trash(paths) => Work::Trash(paths),
            Job::EmptyTrash => Work::EmptyTrash,
        };
        let stop = Arc::new(AtomicBool::new(false));
        self.job = Some(stop.clone());
        let env = self.env.clone();
        let events = self.events.clone();
        let done = self.done.clone();
        tokio::task::spawn_blocking(move || {
            let mut send = |e: Event| {
                let _ = events.send(e);
            };
            let entry = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match work {
                Work::Clean(items) => clean::clean_until(&env, &items, &mut send, Some(&stop)),
                Work::Trash(paths) => {
                    clean::trash_paths_until(&env, &paths, &mut send, Some(&stop))
                }
                Work::EmptyTrash => clean::empty_tidy_trash_until(&env, Some(&stop)),
            }))
            .ok();
            let _ = done.send(Done::Job {
                what,
                empty_trash,
                entry,
            });
        });
    }

    /// The plan items for `ids`, in the order given. Unknown ids are answered at once with
    /// [`Event::ItemSkipped`].
    fn items_for(&mut self, ids: &[String]) -> Vec<Item> {
        if self.plan.is_none() {
            self.plan = load_last_cleanup(&self.env);
        }
        let mut items = Vec::new();
        for id in ids {
            match self.plan.as_ref().and_then(|p| p.item(id)) {
                Some(item) => items.push(item.clone()),
                None => self.emit(Event::ItemSkipped {
                    id: id.clone(),
                    reason: NOT_IN_PLAN.to_string(),
                }),
            }
        }
        items
    }

    // ----- Results -----

    fn finish(&mut self, done: Done) {
        match done {
            Done::Cleanup { plan, cancelled } => {
                self.cleanup_scan = None;
                match plan {
                    Some(_) if cancelled => self.emit(Event::ScanStopped {
                        kind: ScanKind::Cleanup,
                    }),
                    Some(plan) => {
                        self.plan = Some(plan.clone());
                        self.emit(Event::CleanupPlan(plan));
                    }
                    None => self.failed("scan", "The scan stopped unexpectedly."),
                }
            }
            Done::Large { scan, failed } => {
                self.large_scan = None;
                match scan {
                    Some(scan) => self.emit(Event::LargeResult(scan)),
                    None if failed => self.failed("large", "The scan stopped unexpectedly."),
                    None => self.emit(Event::ScanStopped {
                        kind: ScanKind::Large,
                    }),
                }
            }
            Done::Job {
                what,
                empty_trash,
                entry,
            } => {
                self.job = None;
                match entry {
                    Some(entry) if empty_trash => self.emit(Event::EmptyTrashDone(entry)),
                    Some(entry) => self.emit(Event::CleanDone(entry)),
                    None => self.failed(
                        what,
                        "Tidy stopped partway. Check History to see what moved.",
                    ),
                }
                self.start_next_job();
            }
            Done::Memory(result) => {
                self.memory_busy = false;
                match result {
                    Ok(snap) => {
                        self.memory_failing = false;
                        self.emit(Event::MemorySnapshot(snap));
                    }
                    Err(e) => {
                        tracing::warn!("cannot read the memory figures: {e}");
                        if !self.memory_failing {
                            self.memory_failing = true;
                            self.failed("memory", "Tidy couldn't read the memory figures.");
                        }
                    }
                }
            }
            Done::Security(report) => {
                self.security_busy = false;
                match report {
                    Some(report) => self.emit(Event::SecurityResult(report)),
                    None => self.failed("security", "The security check stopped unexpectedly."),
                }
            }
            Done::CheckupEvent(event) => {
                match &event {
                    // The plan the app is about to show is the one Clean takes items from.
                    Event::CleanupPlan(plan) => {
                        self.checkup_scanning = false;
                        self.plan = Some(plan.clone());
                    }
                    Event::ScanStopped {
                        kind: ScanKind::Cleanup,
                    } => self.checkup_scanning = false,
                    Event::Failed { what, .. } if what == "scan" => {
                        self.checkup_scanning = false;
                    }
                    _ => {}
                }
                self.emit(event);
            }
            Done::Checkup(outcome) => {
                self.checkup_scanning = false;
                let aborted = self
                    .checkup
                    .take()
                    .is_some_and(|stop| stop.abort.load(Ordering::Relaxed));
                match outcome {
                    // The plan was taken when its `CleanupPlan` came through: taking it again
                    // here could replace a newer plan from a scan that ended since.
                    Some(out) => {
                        self.emit(Event::CheckupDone {
                            checkup: out.checkup,
                            reminder: out.reminder,
                        });
                    }
                    None if aborted => {}
                    None => self.failed("checkup", "The check up stopped unexpectedly."),
                }
            }
        }
    }

    fn failed(&self, what: &str, message: &str) {
        self.emit(Event::Failed {
            what: what.to_string(),
            message: message.to_string(),
        });
    }

    /// Stops the scans and the running action and waits up to [`SHUTDOWN_GRACE`] for them.
    /// Queued actions are dropped.
    async fn stop_all(&mut self, done_rx: &mut mpsc::UnboundedReceiver<Done>) {
        self.queue.clear();
        if let Some(stop) = &self.checkup {
            stop.abort.store(true, Ordering::Relaxed);
            stop.cancel.store(true, Ordering::Relaxed);
        }
        for flag in [&self.cleanup_scan, &self.large_scan, &self.job]
            .into_iter()
            .flatten()
        {
            flag.store(true, Ordering::Relaxed);
        }
        let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
        while self.cleanup_scan.is_some() || self.large_scan.is_some() || self.job.is_some() {
            match tokio::time::timeout_at(deadline, done_rx.recv()).await {
                Ok(Some(done)) => self.finish(done),
                Ok(None) => break,
                Err(_) => {
                    tracing::warn!("core: shutting down before every task stopped");
                    break;
                }
            }
        }
    }

    fn save_settings(&mut self, s: crate::settings::Settings) {
        let s = s.clamped();
        match s.save(&self.env.paths) {
            Ok(()) => {
                self.env.settings = s.clone();
                self.emit(Event::SettingsChanged(s));
            }
            Err(e) => {
                tracing::warn!("cannot save settings: {e}");
                self.emit(Event::Failed {
                    what: "settings".into(),
                    message: format!("Tidy couldn't save its settings: {e}"),
                });
            }
        }
    }
}

/// A [`Job`] resolved for the executor.
enum Work {
    Clean(Vec<Item>),
    Trash(Vec<PathBuf>),
    EmptyTrash,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{FixedProbe, ProcessProbe, TEST_NOW};
    use crate::model::{
        Action, Checkup, HistoryItem, ItemResult, Source, TrasherKind, from_unix_secs,
    };
    use crate::settings::Settings;
    use std::path::Path;
    use std::sync::{Condvar, Mutex};

    const MIB: usize = 1 << 20;

    struct Running {
        _tmp: tempfile::TempDir,
        rt: tokio::runtime::Runtime,
        env: Env,
        probe: Arc<FixedProbe>,
        core: CoreHandle,
        events: mpsc::UnboundedReceiver<Event>,
    }

    fn running() -> Running {
        let probe = Arc::new(FixedProbe::parse("com.google.Chrome"));
        running_with(probe.clone(), probe)
    }

    fn running_with(procs: Arc<dyn ProcessProbe>, probe: Arc<FixedProbe>) -> Running {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let env = Env::for_tests(&base.join("home"), &base.join("tidy")).with_procs(procs);
        running_env(tmp, env, probe)
    }

    /// Starts the core on `env`, with Security sources that read only folders inside `tmp`
    /// and a signature checker that answers from a table.
    fn running_env(tmp: tempfile::TempDir, env: Env, probe: Arc<FixedProbe>) -> Running {
        let base = tmp.path().canonicalize().unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (core, events) =
            start_with_sources(env.clone(), test_sources(&base), rt.handle().clone());
        Running {
            _tmp: tmp,
            rt,
            env,
            probe,
            core,
            events,
        }
    }

    impl Running {
        fn next(&mut self) -> Event {
            let events = &mut self.events;
            self.rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(10), events.recv())
                    .await
                    .expect("an event in time")
                    .expect("the core is alive")
            })
        }

        /// The next event that is not scan progress.
        fn next_result(&mut self) -> Event {
            loop {
                match self.next() {
                    Event::ScanProgress { .. } => continue,
                    other => return other,
                }
            }
        }

        /// Events up to and including the first one `last` accepts, progress left out.
        fn until(&mut self, last: impl Fn(&Event) -> bool) -> Vec<Event> {
            let mut out = Vec::new();
            loop {
                let e = self.next_result();
                let end = last(&e);
                out.push(e);
                if end {
                    return out;
                }
            }
        }

        fn put(&self, rel: &str, len: usize) -> PathBuf {
            let path = self.env.at(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8 + 1).collect();
            std::fs::write(&path, data).unwrap();
            let t = from_unix_secs(TEST_NOW - 60 * 86_400);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t))
                .unwrap();
            path
        }
    }

    /// The user LaunchAgents folder of [`test_sources`].
    fn user_agents(base: &Path) -> PathBuf {
        base.join("launchd/user")
    }

    fn test_sources(base: &Path) -> Sources {
        let launchd = base.join("launchd");
        Sources {
            launchd: crate::security::launchd::LaunchdRoots {
                user_agents: user_agents(base),
                system_agents: launchd.join("agents"),
                system_daemons: launchd.join("daemons"),
            },
            xprotect: vec![base.join("xprotect/Info.plist")],
            chrome: base.join("chrome"),
            uid: 501,
            signatures: Arc::new(TableSignatures),
        }
    }

    /// Answers `/bin/ls` as Apple and everything else as not signed, without reading a file.
    struct TableSignatures;

    impl crate::security::codesign::Signatures for TableSignatures {
        fn check(&self, program: &Path) -> crate::security::codesign::Signature {
            use crate::model::Signer;
            crate::security::codesign::Signature {
                signer: if program == Path::new("/bin/ls") {
                    Signer::Apple
                } else {
                    Signer::Unsigned
                },
                notarized: None,
            }
        }
    }

    /// A process probe that blocks until opened, so a test can act while a scan is
    /// guaranteed to be running.
    #[derive(Default)]
    struct Gate {
        open: Mutex<bool>,
        cv: Condvar,
    }

    impl Gate {
        fn open(&self) {
            *self.open.lock().unwrap() = true;
            self.cv.notify_all();
        }

        fn wait(&self) {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.cv.wait(open).unwrap();
            }
        }
    }

    /// Opens the gate when dropped, so a failing test does not leave a blocking task waiting
    /// forever (the runtime waits for it when it is dropped).
    struct OpenOnDrop(Arc<Gate>);

    impl Drop for OpenOnDrop {
        fn drop(&mut self) {
            self.0.open();
        }
    }

    impl ProcessProbe for Gate {
        fn running(&self) -> RunningSet {
            self.wait();
            RunningSet::default()
        }
    }

    #[test]
    fn system_data_is_measured_off_the_actor_and_only_lists_snapshots() {
        let r = running();
        r.put("Library/Caches/com.example.app/blob", 2 * MIB);
        let core = r.core.clone();
        let report =
            r.rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(10), core.system_data()).await
            })
            .expect("a report in time")
            .expect("the core answered");
        let caches = report.row(crate::sysdata::SysKind::Caches).unwrap();
        assert!(caches.size.bytes() >= 2 * MIB as u64);
        // The test runner has no reply for tmutil, so the row says it could not list them.
        assert_eq!(
            report.row(crate::sysdata::SysKind::Snapshots).unwrap().size,
            crate::sysdata::SysSize::NotListed
        );
        // Nothing moved: the file is still there.
        assert!(r.env.at("Library/Caches/com.example.app/blob").is_file());
    }

    #[test]
    fn creates_folders_and_answers_scans() {
        let mut r = running();
        assert!(r.env.paths.data.is_dir());
        r.put("Movies/trip.mov", 2 * MIB);
        assert!(r.core.send(Command::ScanCleanup));
        match r.next_result() {
            Event::CleanupPlan(p) => {
                assert_eq!(p.root, r.env.root);
                assert_eq!(p.scanned_at, TEST_NOW);
                assert!(p.items.is_empty());
            }
            other => panic!("{other:?}"),
        }
        r.core.send(Command::ScanLarge {
            record_min: Some(1_000_000),
        });
        match r.next_result() {
            Event::LargeResult(s) => {
                assert_eq!(s.record_min, 1_000_000);
                assert_eq!(s.root, r.env.root);
                assert_eq!(s.items.len(), 1);
                assert_eq!(s.items[0].rel, "Movies/trip.mov");
            }
            other => panic!("{other:?}"),
        }
        // The scan was saved for the next visit.
        assert_eq!(large::load(&r.env.paths).unwrap().items.len(), 1);
        r.core.send(Command::ScanLarge { record_min: None });
        match r.next_result() {
            Event::LargeResult(s) => {
                assert_eq!(s.record_min, LARGE_RECORD_MIN);
                assert!(s.items.is_empty());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn clean_moves_plan_items_and_skips_unknown_ids() {
        let mut r = running();
        r.put(".npm/_cacache/a.bin", 2 * MIB);
        r.core.send(Command::ScanCleanup);
        let plan = match r.next_result() {
            Event::CleanupPlan(p) => p,
            other => panic!("{other:?}"),
        };
        let id = "dev.npm-cacache:.npm/_cacache".to_string();
        assert!(plan.item(&id).unwrap().ticked);
        r.core.send(Command::Clean {
            item_ids: vec!["bogus:x".into(), id.clone()],
        });
        let events = r.until(|e| matches!(e, Event::CleanDone(_)));
        let trash = r.env.paths.dir_trash().join("_cacache");
        assert_eq!(
            events[..3],
            [
                Event::ItemSkipped {
                    id: "bogus:x".into(),
                    reason: NOT_IN_PLAN.into()
                },
                Event::ItemTrashed {
                    id: id.clone(),
                    trash_path: Some(trash.clone())
                },
                Event::CleanProgress { done: 1, total: 1 },
            ]
        );
        let Event::CleanDone(entry) = &events[3] else {
            panic!("{events:?}")
        };
        assert_eq!(entry.action, Action::Clean);
        assert_eq!(entry.trasher, TrasherKind::Dir);
        assert_eq!(entry.trashed_bytes, 2 * MIB as u64);
        assert!(trash.join("a.bin").is_file());
        assert!(!r.env.at(".npm/_cacache").exists());
        assert_eq!(r.rt.block_on(r.core.history()), std::slice::from_ref(entry));
    }

    #[test]
    fn clean_without_a_scan_uses_the_saved_plan() {
        let mut r = running();
        r.put(".npm/_cacache/a.bin", 2 * MIB);
        // A scan by another process (tidyctl) saved the plan.
        crate::cleanup::scan_cleanup(&r.env, &|_| {}, &Arc::new(AtomicBool::new(false)));
        r.core.send(Command::Clean {
            item_ids: vec!["dev.npm-cacache:.npm/_cacache".into()],
        });
        let events = r.until(|e| matches!(e, Event::CleanDone(_)));
        assert!(matches!(events[0], Event::ItemTrashed { .. }), "{events:?}");
    }

    #[test]
    fn actions_run_one_at_a_time_in_order() {
        let mut r = running();
        let movie = r.put("Movies/trip.mov", 4096);
        let other = r.put("Movies/other.mov", 4096);
        r.core.send(Command::TrashPaths {
            paths: vec![movie.clone()],
        });
        r.core.send(Command::TrashPaths {
            paths: vec![other.clone(), r.env.root.clone()],
        });
        r.core.send(Command::EmptyTidyTrash);
        let first = r.until(|e| matches!(e, Event::CleanDone(_)));
        assert_eq!(
            first[0],
            Event::ItemTrashed {
                id: movie.to_string_lossy().into_owned(),
                trash_path: Some(r.env.paths.dir_trash().join("trip.mov")),
            }
        );
        let second = r.until(|e| matches!(e, Event::CleanDone(_)));
        let Event::CleanDone(entry) = second.last().unwrap() else {
            unreachable!()
        };
        assert_eq!(entry.source, Source::Large);
        assert_eq!(entry.skipped(), 1);
        assert!(second.iter().any(|e| matches!(
            e,
            Event::ItemSkipped { id, .. } if Path::new(id) == r.env.root
        )));
        match r.next_result() {
            Event::EmptyTrashDone(entry) => {
                assert_eq!(entry.action, Action::EmptyTrash);
                assert_eq!(entry.items.len(), 2);
                assert!(entry.items.iter().all(|i| i.result == ItemResult::Deleted));
                assert_eq!(entry.message, None);
            }
            other => panic!("{other:?}"),
        }
        assert!(!movie.exists() && !other.exists());
        assert!(!r.env.paths.dir_trash().join("trip.mov").exists());
        assert!(r.env.root.is_dir());
        assert_eq!(r.rt.block_on(r.core.history()).len(), 3);
    }

    #[test]
    fn cancel_stops_a_running_scan_and_a_second_request_is_ignored() {
        let gate = Arc::new(Gate::default());
        let mut r = running_with(gate.clone(), Arc::new(FixedProbe::default()));
        r.put(".npm/_cacache/a.bin", 2 * MIB);
        r.core.send(Command::ScanCleanup);
        // The scan is now waiting on the process list. A second request is ignored.
        r.core.send(Command::ScanCleanup);
        r.core.send(Command::Cancel);
        // Let the actor take both commands before the scan may go on.
        let (tx, rx) = tokio::sync::oneshot::channel();
        r.core.send(Command::LoadHistory { reply: tx });
        r.rt.block_on(rx).unwrap();
        gate.open();
        assert_eq!(
            r.next_result(),
            Event::ScanStopped {
                kind: ScanKind::Cleanup
            }
        );
        // Nothing else arrives for the ignored request; the next scan runs normally.
        r.core.send(Command::ScanCleanup);
        assert!(matches!(r.next_result(), Event::CleanupPlan(_)));
        // Cancel with nothing running does nothing.
        r.core.send(Command::Cancel);
        r.core.send(Command::ScanLarge { record_min: None });
        assert!(matches!(r.next_result(), Event::LargeResult(_)));
    }

    #[test]
    fn shutdown_waits_for_a_running_scan() {
        let gate = Arc::new(Gate::default());
        let mut r = running_with(gate.clone(), Arc::new(FixedProbe::default()));
        r.core.send(Command::ScanCleanup);
        let opener = {
            let gate = gate.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                gate.open();
            })
        };
        r.rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), r.core.shutdown())
                .await
                .unwrap()
        });
        opener.join().unwrap();
        // The scan was stopped, reported, and the stream ended.
        let mut rest = Vec::new();
        while let Ok(Some(e)) = r
            .rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(5), r.events.recv()).await })
        {
            rest.push(e);
        }
        assert!(rest.contains(&Event::ScanStopped {
            kind: ScanKind::Cleanup
        }));
    }

    #[test]
    fn ignore_and_update_settings_save() {
        let mut r = running();
        let p = r.env.root.join("Library/Caches/x");
        r.core.send(Command::Ignore { path: p.clone() });
        match r.next() {
            Event::SettingsChanged(s) => assert_eq!(s.ignored_paths, std::slice::from_ref(&p)),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            Settings::load(&r.env.paths).ignored_paths,
            std::slice::from_ref(&p)
        );
        let s = Settings {
            not_opened_days: 0,
            ..Settings::default()
        };
        r.core.send(Command::UpdateSettings(s));
        match r.next() {
            Event::SettingsChanged(s) => assert_eq!(s.not_opened_days, 1, "clamped"),
            other => panic!("{other:?}"),
        }
        assert!(Settings::load(&r.env.paths).ignored_paths.is_empty());
    }

    #[test]
    fn ignored_paths_are_skipped_by_the_next_clean() {
        let mut r = running();
        let cache = r.put(".npm/_cacache/a.bin", 2 * MIB);
        r.core.send(Command::ScanCleanup);
        assert!(matches!(r.next_result(), Event::CleanupPlan(_)));
        r.core.send(Command::Ignore {
            path: r.env.at(".npm"),
        });
        assert!(matches!(r.next(), Event::SettingsChanged(_)));
        r.core.send(Command::Clean {
            item_ids: vec!["dev.npm-cacache:.npm/_cacache".into()],
        });
        let events = r.until(|e| matches!(e, Event::CleanDone(_)));
        assert_eq!(
            events[0],
            Event::ItemSkipped {
                id: "dev.npm-cacache:.npm/_cacache".into(),
                reason: "On your ignore list".into()
            }
        );
        assert!(cache.is_file());
    }

    #[test]
    fn check_running_reports_only_changes() {
        let mut r = running();
        r.core.send(Command::CheckRunning);
        match r.next() {
            Event::RunningChanged(set) => assert!(set.contains("com.google.chrome")),
            other => panic!("{other:?}"),
        }
        // Unchanged: no event, so the next event is the scan's.
        r.core.send(Command::CheckRunning);
        r.core.send(Command::ScanCleanup);
        assert!(matches!(r.next_result(), Event::CleanupPlan(_)));
        r.probe.set(RunningSet::default());
        r.core.send(Command::CheckRunning);
        match r.next() {
            Event::RunningChanged(set) => assert!(set.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    fn entry(n: u32) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntry::make_id(TEST_NOW, n),
            at: TEST_NOW,
            action: Action::Clean,
            source: Source::Cleanup,
            trasher: TrasherKind::Dir,
            items: vec![HistoryItem {
                path: PathBuf::from("/h/a"),
                trash_path: Some(PathBuf::from("/t/a")),
                bytes: 4096,
                rule: "cache.app".into(),
                result: ItemResult::Trashed,
                reason: None,
                trash_id: None,
            }],
            trashed_bytes: 4096,
            message: None,
        }
    }

    #[test]
    fn load_history_skips_bad_lines() {
        let r = running();
        let file = r.env.paths.history_file();
        crate::store::append_json_line(&file, &entry(1)).unwrap();
        crate::store::append_line(&file, "{not json").unwrap();
        crate::store::append_json_line(&file, &entry(2)).unwrap();
        let got = r.rt.block_on(r.core.history());
        assert_eq!(got, [entry(1), entry(2)]);
    }

    #[test]
    fn load_history_without_a_file_is_empty() {
        let r = running();
        assert!(r.rt.block_on(r.core.history()).is_empty());
        assert!(!Path::new(&r.env.paths.history_file()).exists());
    }

    #[test]
    fn shutdown_replies_and_stops_the_actor() {
        let mut r = running();
        r.rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), r.core.shutdown())
                .await
                .unwrap()
        });
        // The actor dropped its sender: the stream ends.
        let ended = r.rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), r.events.recv()).await
        });
        assert!(matches!(ended, Ok(None)));
        // Commands are dropped once the receiver is gone.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while r.core.send(Command::Cancel) {
            assert!(
                std::time::Instant::now() < deadline,
                "the actor never stopped"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn reminder_and_last_checkup_come_from_the_checkup_log() {
        let r = running();
        let core = r.core.clone();
        // No check up yet: seven days from now, 10:00 in the test zone (UTC).
        let date = r.rt.block_on(core.next_reminder()).unwrap();
        assert_eq!((date.month, date.day, date.hour), (10, 30, 10));
        assert_eq!(r.rt.block_on(core.last_checkup()), None);
        let at = TEST_NOW + 3 * 86_400;
        reminder::append_checkup(
            &r.env.paths,
            &Checkup {
                at,
                ..Checkup::default()
            },
        )
        .unwrap();
        assert_eq!(r.rt.block_on(core.last_checkup()).map(|c| c.at), Some(at));
        let date = r.rt.block_on(core.next_reminder()).unwrap();
        assert_eq!(
            (date.year, date.month, date.day, date.hour),
            (2026, 11, 2, 10)
        );
        assert_eq!(date.at, 1_793_613_600, "2026-11-02T10:00:00Z");
    }

    // ----- M2 -----

    use crate::env::{CommandOutput, FixedClock, FixedMemory, MemoryTable, NoMemory, ReplayRunner};
    use crate::model::{CheckId, Pressure, ProcInfo, Status, VmStats};

    fn memory_table(level: u32) -> MemoryTable {
        let app = "/Applications/Big.app/Contents/MacOS/Big";
        MemoryTable {
            own_pid: 0,
            processes: vec![
                ProcInfo {
                    pid: 100,
                    ppid: 1,
                    name: "Big".into(),
                    path: Some(app.into()),
                    footprint: Some(3 << 30),
                    ..ProcInfo::default()
                },
                ProcInfo {
                    pid: 101,
                    ppid: 100,
                    name: "Big Helper".into(),
                    footprint: Some(1 << 30),
                    ..ProcInfo::default()
                },
                ProcInfo {
                    pid: 1,
                    ppid: 0,
                    uid: 0,
                    name: "launchd".into(),
                    ..ProcInfo::default()
                },
            ],
            vm: VmStats {
                pressure_level: level,
                total_bytes: 16 << 30,
                ..VmStats::default()
            },
        }
    }

    /// The captured outputs of this Mac's status commands (FileVault on, firewall off).
    fn replay() -> ReplayRunner {
        ReplayRunner::new()
            .with(
                "/usr/bin/fdesetup status",
                CommandOutput::ok("FileVault is On.\n"),
            )
            .with(
                "/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate",
                CommandOutput::ok("Firewall is disabled. (State = 0)\n"),
            )
            .with(
                "/usr/sbin/spctl --status",
                CommandOutput::ok("assessments enabled\n"),
            )
            .with(
                "/usr/bin/csrutil status",
                CommandOutput::ok("System Integrity Protection status: enabled.\n"),
            )
    }

    struct M2 {
        r: Running,
        memory: Arc<FixedMemory>,
        runner: Arc<ReplayRunner>,
        clock: Arc<FixedClock>,
    }

    fn running_m2(procs: Arc<dyn ProcessProbe>) -> M2 {
        let runner = Arc::new(replay());
        running_m2_with(procs, runner.clone(), runner)
    }

    /// A command runner that waits for a gate before every command, so a test can act while
    /// a check up is guaranteed to be in its Security part.
    struct GatedRunner {
        gate: Arc<Gate>,
        inner: Arc<ReplayRunner>,
    }

    impl crate::env::CommandRunner for GatedRunner {
        fn run(
            &self,
            program: &str,
            args: &[&str],
            timeout: Duration,
        ) -> std::io::Result<CommandOutput> {
            self.gate.wait();
            self.inner.run(program, args, timeout)
        }
    }

    /// [`running_m2`] whose Security commands go through `env_runner` (which may wrap
    /// `runner`, the replay the test inspects).
    fn running_m2_with(
        procs: Arc<dyn ProcessProbe>,
        env_runner: Arc<dyn crate::env::CommandRunner>,
        runner: Arc<ReplayRunner>,
    ) -> M2 {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let memory = Arc::new(FixedMemory::new(memory_table(1)));
        let clock = Arc::new(FixedClock::new(TEST_NOW));
        let env = Env::for_tests(&base.join("home"), &base.join("tidy"))
            .with_procs(procs)
            .with_memory(memory.clone())
            .with_runner(env_runner)
            .with_clock(clock.clone());
        // Two user agents: one Apple program, one unsigned.
        let agents = user_agents(&base);
        std::fs::create_dir_all(&agents).unwrap();
        for (label, program) in [
            ("com.example.ls", "/bin/ls"),
            ("com.example.odd", "/opt/odd"),
        ] {
            std::fs::write(
                agents.join(format!("{label}.plist")),
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                     <key>Label</key><string>{label}</string>\
                     <key>Program</key><string>{program}</string>\
                     <key>RunAtLoad</key><true/></dict></plist>\n"
                ),
            )
            .unwrap();
        }
        let r = running_env(tmp, env, Arc::new(FixedProbe::default()));
        M2 {
            r,
            memory,
            runner,
            clock,
        }
    }

    #[test]
    fn memory_snapshots_come_on_request_with_the_grouped_rows() {
        let mut m = running_m2(Arc::new(FixedProbe::default()));
        m.r.core.send(Command::SnapshotMemory);
        let snap = match m.r.next() {
            Event::MemorySnapshot(s) => s,
            other => panic!("{other:?}"),
        };
        assert_eq!(snap.at, TEST_NOW);
        assert_eq!(snap.pressure, Pressure::Normal);
        assert_eq!(snap.apps[0].name, "Big");
        assert_eq!(snap.apps[0].footprint, 4 << 30);
        assert_eq!(snap.apps[0].helpers, 1);
        assert!(
            snap.apps
                .iter()
                .any(|a| a.name == crate::model::SYSTEM_PROCESSES)
        );
        m.memory.set_vm(memory_table(4).vm);
        m.r.core.send(Command::SnapshotMemory);
        match m.r.next() {
            Event::MemorySnapshot(s) => assert_eq!(s.pressure, Pressure::Critical),
            other => panic!("{other:?}"),
        }
        assert!(!m.runner.ran("softwareupdate"));
    }

    #[test]
    fn unreadable_memory_figures_fail_once_until_they_read_again() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        let env =
            Env::for_tests(&base.join("home"), &base.join("tidy")).with_memory(Arc::new(NoMemory));
        let mut r = running_env(tmp, env, Arc::new(FixedProbe::default()));
        r.core.send(Command::SnapshotMemory);
        assert!(matches!(r.next(), Event::Failed { what, .. } if what == "memory"));
        // A second failure is quiet: the next event is the scan's.
        r.core.send(Command::SnapshotMemory);
        let (tx, rx) = tokio::sync::oneshot::channel();
        r.core.send(Command::LoadHistory { reply: tx });
        r.rt.block_on(rx).unwrap();
        r.core.send(Command::ScanCleanup);
        assert!(matches!(r.next_result(), Event::CleanupPlan(_)));
    }

    #[test]
    fn security_streams_each_signature_saves_the_run_and_compares_the_next() {
        let mut m = running_m2(Arc::new(FixedProbe::default()));
        assert_eq!(m.r.rt.block_on(m.r.core.last_security()), None);
        m.r.core.send(Command::RunSecurity {
            include_updates: false,
        });
        let events = m.r.until(|e| matches!(e, Event::SecurityResult(_)));
        let steps: Vec<(usize, usize)> = events
            .iter()
            .filter_map(|e| match e {
                Event::SecurityProgress { done, total } => Some((*done, *total)),
                _ => None,
            })
            .collect();
        // Every check, plus one step per program (two agents).
        let total = CheckId::ALL.len() + 2;
        assert_eq!(steps.len(), total + 1, "{steps:?}");
        assert!(steps.iter().enumerate().all(|(i, s)| *s == (i, total)));
        let Some(Event::SecurityResult(report)) = events.last() else {
            unreachable!()
        };
        assert!(report.first_run);
        assert_eq!(report.checks.len(), CheckId::ALL.len());
        assert_eq!(
            report.check(CheckId::Firewall).unwrap().status,
            Status::Amber
        );
        assert_eq!(
            report.check(CheckId::MacosUpdates).unwrap().status,
            Status::Gray
        );
        assert_eq!(report.login_items.len(), 2);
        assert!(!m.runner.ran("softwareupdate"));
        let saved = m.r.rt.block_on(m.r.core.last_security()).unwrap();
        assert_eq!(&saved, report);
        assert_eq!(
            security::saved_runs(&m.r.env.paths),
            [crate::model::run_id(TEST_NOW)]
        );

        // An hour later a new agent shows up: it is New, and the run compares with the last.
        m.clock.advance(3_600);
        std::fs::copy(
            user_agents(&m.r._tmp.path().canonicalize().unwrap()).join("com.example.ls.plist"),
            user_agents(&m.r._tmp.path().canonicalize().unwrap()).join("com.example.new.plist"),
        )
        .unwrap();
        m.r.core.send(Command::RunSecurity {
            include_updates: false,
        });
        let events = m.r.until(|e| matches!(e, Event::SecurityResult(_)));
        let Some(Event::SecurityResult(next)) = events.last() else {
            unreachable!()
        };
        assert!(!next.first_run);
        assert_eq!(next.compared_to.as_deref(), Some(report.id.as_str()));
        assert_eq!(next.compared_at, Some(report.at));
        assert_eq!(security::saved_runs(&m.r.env.paths).len(), 2);
    }

    #[test]
    fn a_check_up_runs_the_three_parts_logs_them_and_sends_the_reminder() {
        let mut m = running_m2(Arc::new(FixedProbe::default()));
        m.r.put(".npm/_cacache/a.bin", 2 * MIB);
        m.r.core.send(Command::RunCheckup {
            include_updates: false,
        });
        let events = m.r.until(|e| matches!(e, Event::CheckupDone { .. }));
        let kinds: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Event::CleanupPlan(_) => Some("plan"),
                Event::SecurityResult(_) => Some("security"),
                Event::MemorySnapshot(_) => Some("memory"),
                Event::CheckupDone { .. } => Some("done"),
                Event::Failed { .. } => Some("failed"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, ["plan", "security", "memory", "done"]);
        let Some(Event::CheckupDone { checkup, reminder }) = events.last() else {
            unreachable!()
        };
        assert_eq!(checkup.at, TEST_NOW);
        assert!(!checkup.include_updates);
        let cleanup = checkup.cleanup.unwrap();
        assert_eq!(cleanup.safe_bytes, 2 * MIB as u64);
        assert_eq!(cleanup.safe_items, 1);
        let sec = checkup.security.as_ref().unwrap();
        assert!(sec.first_run);
        assert!(sec.amber >= 1, "the firewall is off");
        assert_eq!(checkup.memory.unwrap().pressure, Pressure::Normal);
        // 2026-10-23 plus seven days, 10:00 UTC.
        assert_eq!(
            (reminder.year, reminder.month, reminder.day, reminder.hour),
            (2026, 10, 30, 10)
        );
        assert_eq!(
            reminder::read_checkups(&m.r.env.paths),
            std::slice::from_ref(checkup)
        );
        assert_eq!(
            m.r.rt.block_on(m.r.core.last_checkup()).as_ref(),
            Some(checkup)
        );
        assert_eq!(m.r.rt.block_on(m.r.core.next_reminder()), Some(*reminder));
        assert!(!m.runner.ran("softwareupdate"));

        // The Cleanup card's Clean uses the check up's plan.
        m.r.core.send(Command::Clean {
            item_ids: vec!["dev.npm-cacache:.npm/_cacache".into()],
        });
        let done = m.r.until(|e| matches!(e, Event::CleanDone(_)));
        assert!(matches!(done[0], Event::ItemTrashed { .. }), "{done:?}");
    }

    #[test]
    fn a_security_run_is_ignored_while_a_check_up_runs_and_cancel_stops_its_scan() {
        let gate = Arc::new(Gate::default());
        let mut m = running_m2(gate.clone());
        m.r.core.send(Command::RunCheckup {
            include_updates: false,
        });
        // The check up's scan waits on the process list. Both of these are ignored or
        // answered by the check up itself.
        m.r.core.send(Command::RunCheckup {
            include_updates: false,
        });
        m.r.core.send(Command::RunSecurity {
            include_updates: false,
        });
        // A Cleanup scan asked for now waits for the check up's scan instead of starting a
        // second one, so only one scan answers (here with ScanStopped, after the Cancel).
        m.r.core.send(Command::ScanCleanup);
        m.r.core.send(Command::Cancel);
        let (tx, rx) = tokio::sync::oneshot::channel();
        m.r.core.send(Command::LoadHistory { reply: tx });
        m.r.rt.block_on(rx).unwrap();
        gate.open();
        let events = m.r.until(|e| matches!(e, Event::CheckupDone { .. }));
        let stopped = events
            .iter()
            .filter(|e| {
                **e == Event::ScanStopped {
                    kind: ScanKind::Cleanup,
                }
            })
            .count();
        assert_eq!(stopped, 1, "{events:?}");
        assert!(
            !events.iter().any(|e| matches!(e, Event::CleanupPlan(_))),
            "{events:?}"
        );
        let results = events
            .iter()
            .filter(|e| matches!(e, Event::SecurityResult(_)))
            .count();
        assert_eq!(results, 1);
        let Some(Event::CheckupDone { checkup, .. }) = events.last() else {
            unreachable!()
        };
        assert_eq!(checkup.cleanup, None);
        assert!(checkup.security.is_some() && checkup.memory.is_some());
        // Nothing else was queued: the next event answers the next command.
        m.r.core.send(Command::SnapshotMemory);
        assert!(matches!(m.r.next_result(), Event::MemorySnapshot(_)));
        assert_eq!(reminder::read_checkups(&m.r.env.paths).len(), 1);
    }

    /// Regression: the check up's plan reaches the app while Security still runs. A Clean
    /// sent then must take its items from that plan, not from an older Cleanup scan, so what
    /// moves and what history records is what the user saw.
    #[test]
    fn clean_during_a_check_up_uses_the_plan_the_check_up_just_sent() {
        let gate = Arc::new(Gate::default());
        let runner = Arc::new(replay());
        let gated = Arc::new(GatedRunner {
            gate: gate.clone(),
            inner: runner.clone(),
        });
        let mut m = running_m2_with(Arc::new(FixedProbe::default()), gated, runner);
        let _open = OpenOnDrop(gate.clone());
        // P1: the npm cache at 2 MiB.
        m.r.put(".npm/_cacache/a.bin", 2 * MIB);
        m.r.core.send(Command::ScanCleanup);
        let p1 = match m.r.next_result() {
            Event::CleanupPlan(p) => p,
            other => panic!("{other:?}"),
        };
        assert_eq!(p1.items.len(), 1, "{p1:?}");

        // Later the npm cache grows and a pnpm cache appears.
        m.r.put(".npm/_cacache/b.bin", 3 * MIB);
        m.r.put("Library/Caches/pnpm/c.bin", 2 * MIB);
        m.r.core.send(Command::RunCheckup {
            include_updates: false,
        });
        // P2 arrives while Security waits on the gate.
        let p2 = match m.r.until(|e| matches!(e, Event::CleanupPlan(_))).pop() {
            Some(Event::CleanupPlan(p)) => p,
            other => panic!("{other:?}"),
        };
        let ids: Vec<String> = p2
            .items
            .iter()
            .filter(|i| i.ticked)
            .map(|i| i.id.clone())
            .collect();
        assert_eq!(ids.len(), 2, "{p2:?}");
        assert!(ids.iter().any(|id| p1.item(id).is_none()), "a new item");
        let want: u64 = p2
            .items
            .iter()
            .filter(|i| i.ticked)
            .map(|i| i.bytes_alloc)
            .sum();

        m.r.core.send(Command::Clean {
            item_ids: ids.clone(),
        });
        let events = m.r.until(|e| matches!(e, Event::CleanDone(_)));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::ItemSkipped { .. })),
            "{events:?}"
        );
        let trashed = events
            .iter()
            .filter(|e| matches!(e, Event::ItemTrashed { .. }))
            .count();
        assert_eq!(trashed, 2, "{events:?}");
        let Some(Event::CleanDone(entry)) = events.last() else {
            unreachable!()
        };
        assert_eq!(entry.trashed_bytes, want, "history records P2's sizes");
        assert!(
            entry.trashed_bytes > p1.items[0].bytes_alloc,
            "not P1's 2 MiB"
        );

        gate.open();
        let events = m.r.until(|e| matches!(e, Event::CheckupDone { .. }));
        assert!(
            !events.iter().any(|e| matches!(e, Event::Failed { .. })),
            "{events:?}"
        );
    }

    /// The Security screen's Move to Trash for a user login item sends `TrashPaths` with the
    /// plist under `~/Library/LaunchAgents` (the scan root's, see `Sources::for_env`). The
    /// guard lets it through: that folder is inside the home folder and on no denylist. Here
    /// the home is a temp folder and the Trash is a `DirTrash`.
    #[test]
    fn a_user_launch_agent_plist_can_go_to_the_trash() {
        let mut r = running();
        let plist = r.put("Library/LaunchAgents/com.example.helper.plist", 4096);
        assert_eq!(
            Sources::for_env(&r.env).launchd.user_agents,
            plist.parent().unwrap()
        );
        r.core.send(Command::TrashPaths {
            paths: vec![plist.clone()],
        });
        let events = r.until(|e| matches!(e, Event::CleanDone(_)));
        assert!(
            events.iter().any(
                |e| matches!(e, Event::ItemTrashed { id, .. } if *id == plist.to_string_lossy())
            ),
            "{events:?}"
        );
        assert!(!plist.exists());
        let Some(Event::CleanDone(entry)) = events.last() else {
            unreachable!()
        };
        assert_eq!(entry.trasher, TrasherKind::Dir);
        // The folder itself stays.
        assert!(r.env.at("Library/LaunchAgents").is_dir());
    }
}
