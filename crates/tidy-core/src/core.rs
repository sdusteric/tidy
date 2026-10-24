//! The actor behind [`crate::start`]: one task that owns the [`Env`], receives
//! [`Command`]s and sends [`Event`]s (design section 9, core boundary).
//!
//! - Scans run on `spawn_blocking`, at most one of each kind at a time; a second request
//!   while one runs is ignored. [`Command::Cancel`] stops them at their next folder and each
//!   answers with [`Event::ScanStopped`].
//! - Clean, Move to Trash and Empty Trash run on `spawn_blocking` one at a time, in the order
//!   they were asked for. They never overlap, so two actions never move the same item.
//! - The process list is read only when the UI asks ([`Command::CheckRunning`]).
//! - [`Command::Shutdown`] stops the scans, asks a running action to stop after its current
//!   item, waits a short grace for both, then replies.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::api::{Command, CoreHandle, Event, ScanKind};
use crate::clean::{self, NOT_IN_PLAN};
use crate::cleanup::{CleanupProgress, load_last_cleanup, scan_cleanup};
use crate::env::Env;
use crate::history;
use crate::large::{self, LargeOpts};
use crate::model::{HistoryEntry, Item, LARGE_RECORD_MIN, LargeScan, Plan, RunningSet};
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
    if let Err(e) = env.paths.create_all() {
        tracing::warn!("cannot create Tidy's folders: {e}");
    }
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let (done_tx, done_rx) = mpsc::unbounded_channel();
    let actor = Actor {
        env,
        events: ev_tx,
        done: done_tx,
        last_running: None,
        plan: None,
        cleanup_scan: None,
        large_scan: None,
        job: None,
        queue: VecDeque::new(),
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
}

struct Actor {
    env: Env,
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
    use crate::model::{Action, HistoryItem, ItemResult, Source, TrasherKind, from_unix_secs};
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
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (core, events) = start(env.clone(), rt.handle().clone());
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
    }

    impl ProcessProbe for Gate {
        fn running(&self) -> RunningSet {
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.cv.wait(open).unwrap();
            }
            RunningSet::default()
        }
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
}
