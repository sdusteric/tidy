//! The check up of design section 5.5: the Cleanup scan, the Security checks (without
//! `softwareupdate --list` unless asked) and a memory snapshot, in that order, summed up in
//! one line of `checkups.jsonl`.
//!
//! Each part sends its usual event ([`Event::CleanupPlan`], [`Event::SecurityResult`],
//! [`Event::MemorySnapshot`]) so the other screens update too. A part that fails or is
//! stopped is None in the [`Checkup`] and the run goes on with the next part; the card for
//! that part says so. The weekly reminder date is counted from the finished check up.
//!
//! The core actor runs [`run`] on a blocking thread and relays its events, in order, so it
//! holds the Cleanup plan before the app shows it; tidyctl gets here through the actor too. Nothing here moves a file: the Cleanup part only scans.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::api::{Event, ScanKind};
use crate::cleanup::{CleanupProgress, scan_cleanup};
use crate::env::Env;
use crate::memory;
use crate::model::{Checkup, CheckupCleanup, CheckupMemory, CheckupSecurity, Plan};
use crate::reminder::{self, ReminderDate};
use crate::security::{self, Sources};

/// What stops a running check up.
#[derive(Debug, Default)]
pub struct CheckupStop {
    /// Stops the Cleanup scan at its next folder (the user's Cancel). Its part is then None
    /// and the run goes on with Security and Memory.
    pub cancel: Arc<AtomicBool>,
    /// Ends the run before the next part and writes nothing (shutdown).
    pub abort: AtomicBool,
}

impl CheckupStop {
    pub fn new() -> CheckupStop {
        CheckupStop::default()
    }

    fn aborted(&self) -> bool {
        self.abort.load(Ordering::Relaxed)
    }
}

/// A finished check up.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckupOutcome {
    /// Already appended to `checkups.jsonl` (unless saving failed, which was reported).
    pub checkup: Checkup,
    /// The next weekly reminder, counted from `checkup.at`.
    pub reminder: ReminderDate,
    /// The Cleanup plan, the same one sent with [`Event::CleanupPlan`]. The actor takes its
    /// plan from that event, which it relays, not from here.
    pub plan: Option<Plan>,
}

/// Runs a check up for `env`. `emit` gets every event of the parts as they happen, but not
/// [`Event::CheckupDone`]: the caller sends that with the outcome. Returns None when it was
/// aborted (nothing is written then).
pub fn run(
    env: &Env,
    sources: &Sources,
    include_updates: bool,
    stop: &CheckupStop,
    emit: &dyn Fn(Event),
) -> Option<CheckupOutcome> {
    // 1. Cleanup.
    let progress = |p: CleanupProgress| {
        emit(Event::ScanProgress {
            kind: ScanKind::Cleanup,
            files: p.files,
            bytes: p.bytes,
            current: p.current,
        });
    };
    let scanned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scan_cleanup(env, &progress, &stop.cancel)
    }));
    if stop.aborted() {
        return None;
    }
    let plan = match scanned {
        Ok(_) if stop.cancel.load(Ordering::Relaxed) => {
            emit(Event::ScanStopped {
                kind: ScanKind::Cleanup,
            });
            None
        }
        Ok(plan) => {
            emit(Event::CleanupPlan(plan.clone()));
            Some(plan)
        }
        Err(_) => {
            emit(failed("scan", "The scan stopped unexpectedly."));
            None
        }
    };

    // 2. Security.
    let report = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut progress = |done: usize, total: usize| {
            emit(Event::SecurityProgress { done, total });
        };
        security::run_and_save_with(env, sources, include_updates, &mut progress)
    }));
    if stop.aborted() {
        return None;
    }
    let report = match report {
        Ok(report) => {
            emit(Event::SecurityResult(report.clone()));
            Some(report)
        }
        Err(_) => {
            emit(failed(
                "security",
                "The security check stopped unexpectedly.",
            ));
            None
        }
    };

    // 3. Memory.
    let snapshot = match memory::take(env) {
        Ok(snap) => {
            emit(Event::MemorySnapshot(snap.clone()));
            Some(snap)
        }
        Err(e) => {
            tracing::warn!("check up: cannot read the memory figures: {e}");
            emit(failed("memory", "Tidy couldn't read the memory figures."));
            None
        }
    };
    if stop.aborted() {
        return None;
    }

    let at = env.now();
    let checkup = Checkup {
        at,
        include_updates,
        cleanup: plan.as_ref().map(CheckupCleanup::from_plan),
        security: report.as_ref().map(CheckupSecurity::from_report),
        memory: snapshot.as_ref().map(CheckupMemory::from_snapshot),
    };
    if let Err(e) = reminder::append_checkup(&env.paths, &checkup) {
        tracing::warn!("cannot save the check up: {e}");
        emit(failed(
            "checkup",
            &format!("Tidy couldn't save the check up: {e}"),
        ));
    }
    let reminder = reminder::next_reminder(Some(at), env.now(), &env.tz);
    Some(CheckupOutcome {
        checkup,
        reminder,
        plan,
    })
}

fn failed(what: &str, message: &str) -> Event {
    Event::Failed {
        what: what.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{FixedMemory, MemoryTable, ReplayRunner, TEST_NOW};
    use crate::model::{Pressure, ProcInfo, VmStats};
    use crate::security::launchd::LaunchdRoots;
    use std::path::Path;
    use std::sync::Mutex;

    /// Sources that read nothing real: empty launchd folders, no XProtect, no Chrome.
    fn sources(base: &Path) -> Sources {
        let none = base.join("none");
        Sources {
            launchd: LaunchdRoots {
                user_agents: none.join("ua"),
                system_agents: none.join("sa"),
                system_daemons: none.join("sd"),
            },
            xprotect: vec![none.join("XProtect.plist")],
            chrome: none.join("Chrome"),
            uid: 501,
            signatures: Arc::new(crate::security::codesign::CodeSign),
        }
    }

    fn env(base: &Path) -> Env {
        std::fs::create_dir_all(base.join("home")).unwrap();
        let memory = FixedMemory::new(MemoryTable {
            own_pid: 0,
            processes: vec![ProcInfo {
                pid: 10,
                ppid: 1,
                name: "worker".into(),
                footprint: Some(1 << 20),
                ..ProcInfo::default()
            }],
            vm: VmStats {
                pressure_level: 2,
                total_bytes: 16 << 30,
                ..VmStats::default()
            },
        });
        Env::for_tests(&base.join("home"), &base.join("tidy"))
            .with_memory(Arc::new(memory))
            .with_runner(Arc::new(ReplayRunner::new()))
    }

    #[test]
    fn writes_one_line_with_the_three_parts() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let env = env(&base);
        let events = Mutex::new(Vec::new());
        let out = run(&env, &sources(&base), false, &CheckupStop::new(), &|e| {
            events.lock().unwrap().push(e)
        })
        .unwrap();
        assert_eq!(out.checkup.at, TEST_NOW);
        assert!(out.checkup.cleanup.is_some() && out.plan.is_some());
        assert!(out.checkup.security.as_ref().unwrap().first_run);
        assert_eq!(out.checkup.memory.unwrap().pressure, Pressure::Warning);
        assert_eq!(
            reminder::read_checkups(&env.paths),
            std::slice::from_ref(&out.checkup)
        );
        assert_eq!(
            out.reminder,
            reminder::next_reminder(Some(TEST_NOW), TEST_NOW, &env.tz)
        );
        let events = events.into_inner().unwrap();
        let pos = |f: &dyn Fn(&Event) -> bool| events.iter().position(f).unwrap();
        assert!(
            pos(&|e| matches!(e, Event::CleanupPlan(_)))
                < pos(&|e| matches!(e, Event::SecurityResult(_)))
        );
        assert!(
            pos(&|e| matches!(e, Event::SecurityResult(_)))
                < pos(&|e| matches!(e, Event::MemorySnapshot(_)))
        );
        assert!(!events.iter().any(|e| matches!(e, Event::Failed { .. })));
    }

    #[test]
    fn a_cancelled_scan_and_unreadable_memory_leave_their_parts_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let env = env(&base).with_memory(Arc::new(crate::env::NoMemory));
        let stop = CheckupStop::new();
        stop.cancel.store(true, Ordering::Relaxed);
        let events = Mutex::new(Vec::new());
        let out = run(&env, &sources(&base), false, &stop, &|e| {
            events.lock().unwrap().push(e)
        })
        .unwrap();
        assert_eq!(out.checkup.cleanup, None);
        assert_eq!(out.plan, None);
        assert!(out.checkup.security.is_some());
        assert_eq!(out.checkup.memory, None);
        let events = events.into_inner().unwrap();
        assert!(events.contains(&Event::ScanStopped {
            kind: ScanKind::Cleanup
        }));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Failed { what, .. } if what == "memory"))
        );
        assert_eq!(reminder::read_checkups(&env.paths).len(), 1);
    }

    #[test]
    fn an_aborted_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let env = env(&base);
        let stop = CheckupStop::new();
        stop.abort.store(true, Ordering::Relaxed);
        assert_eq!(run(&env, &sources(&base), false, &stop, &|_| {}), None);
        assert!(reminder::read_checkups(&env.paths).is_empty());
        assert!(security::saved_weeks(&env.paths).is_empty());
    }
}
