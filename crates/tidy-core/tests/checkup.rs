//! The check up end to end through the core, the way tidyctl and the app drive it: the
//! standard fixture home, the fake Security command outputs and the fake process table that
//! `tidy-fixture-home` writes (`commands.json`, `memory.json`), and Security sources that read
//! only folders inside the temp dir. Nothing reads the real home, runs a real command, or
//! moves a file: a check up only scans.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tidy_core::api::{Command, CoreHandle, Event};
use tidy_core::env::{Env, FixedClock, FixedMemory, FixedProbe, ReplayRunner, parse_tz};
use tidy_core::model::{
    CheckId, Checkup, CheckupCleanup, Class, Pressure, RunningSet, Status, iso_week_id,
};
use tidy_core::reminder::{self, ReminderDate};
use tidy_core::security::codesign::CodeSign;
use tidy_core::security::launchd::LaunchdRoots;
use tidy_core::security::{self, Sources};
use tidy_fixtures::fakes::{FAKE_COMPRESSOR_PAGES, FAKE_PAGE_SIZE, FAKE_SWAP_USED};
use tidy_fixtures::{Expected, FIXTURE_NOW, FakeHome, write_fakes};
use tokio::sync::mpsc::UnboundedReceiver;

const DAY: u64 = 86_400;

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    expected: Expected,
    env: Env,
    clock: Arc<FixedClock>,
    runner: Arc<ReplayRunner>,
}

/// The standard fixture with the fakes, in Los Angeles time.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let dir = base.join("fx");
    let expected = FakeHome::standard(&dir, FIXTURE_NOW).unwrap();
    let fakes = write_fakes(&dir).unwrap();
    let clock = Arc::new(FixedClock::new(expected.plan.now));
    let runner = Arc::new(ReplayRunner::from_file(&fakes.commands).unwrap());
    let env = Env::for_tests(&dir.join("home"), &dir.join("tidy"))
        .with_clock(clock.clone())
        .with_procs(Arc::new(FixedProbe::new(RunningSet::new(
            &expected.plan.running,
        ))))
        .with_runner(runner.clone())
        .with_memory(Arc::new(FixedMemory::from_file(&fakes.memory).unwrap()))
        .with_tz(parse_tz("PST8PDT,M3.2.0,M11.1.0").unwrap());
    Fixture {
        _tmp: tmp,
        base,
        expected,
        env,
        clock,
        runner,
    }
}

/// Launchd folders, XProtect and Chrome inside the temp dir (all missing), so the run reads
/// nothing of the real Mac.
fn sources(base: &Path) -> Sources {
    let none = base.join("not-there");
    Sources {
        launchd: LaunchdRoots {
            user_agents: none.join("LaunchAgents"),
            system_agents: none.join("SystemAgents"),
            system_daemons: none.join("SystemDaemons"),
        },
        xprotect: vec![none.join("XProtect/Info.plist")],
        chrome: none.join("Chrome"),
        uid: 501,
        signatures: Arc::new(CodeSign),
    }
}

struct Core {
    rt: tokio::runtime::Runtime,
    core: CoreHandle,
    events: UnboundedReceiver<Event>,
}

impl Core {
    fn start(f: &Fixture) -> Core {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (core, events) =
            tidy_core::start_with_sources(f.env.clone(), sources(&f.base), rt.handle().clone());
        Core { rt, core, events }
    }

    /// Every event up to and including `CheckupDone`.
    fn checkup(&mut self, include_updates: bool) -> Vec<Event> {
        assert!(self.core.send(Command::RunCheckup { include_updates }));
        let mut out = Vec::new();
        loop {
            let events = &mut self.events;
            let ev = self.rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(30), events.recv())
                    .await
                    .expect("an event in time")
                    .expect("the core is alive")
            });
            let end = matches!(ev, Event::CheckupDone { .. });
            out.push(ev);
            if end {
                return out;
            }
        }
    }
}

fn done(events: &[Event]) -> (Checkup, ReminderDate) {
    match events.last() {
        Some(Event::CheckupDone { checkup, reminder }) => (checkup.clone(), *reminder),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_check_up_of_the_fixture_home_logs_one_entry_with_the_three_parts() {
    let f = fixture();
    let mut c = Core::start(&f);
    let events = c.checkup(false);
    assert!(
        !events.iter().any(|e| matches!(e, Event::Failed { .. })),
        "{events:?}"
    );

    // Cleanup: the plan is exactly the fixture's, and the card sums its ticked items.
    let plan = events
        .iter()
        .find_map(|e| match e {
            Event::CleanupPlan(p) => Some(p.clone()),
            _ => None,
        })
        .expect("the plan");
    let ids: Vec<&str> = plan.items.iter().map(|i| i.id.as_str()).collect();
    let want: Vec<&str> = f
        .expected
        .plan
        .items
        .iter()
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(ids, want);
    let safe: u64 = f
        .expected
        .plan
        .items
        .iter()
        .filter(|i| i.ticked)
        .map(|i| i.bytes_alloc)
        .sum();
    let review: u64 = f
        .expected
        .plan
        .items
        .iter()
        .filter(|i| i.class == Class::Review)
        .map(|i| i.bytes_alloc)
        .sum();
    assert!(safe > 0);

    let (checkup, reminder) = done(&events);
    assert_eq!(checkup.at, FIXTURE_NOW);
    assert!(!checkup.include_updates);
    assert_eq!(checkup.cleanup, Some(CheckupCleanup::from_plan(&plan)));
    assert_eq!(checkup.cleanup.unwrap().safe_bytes, safe);
    assert_eq!(checkup.cleanup.unwrap().review_bytes, review);

    // Security: the captured outputs give one amber system check (the firewall); a first
    // run says so.
    let report = events
        .iter()
        .find_map(|e| match e {
            Event::SecurityResult(r) => Some(r.clone()),
            _ => None,
        })
        .expect("the report");
    assert!(report.first_run);
    assert_eq!(report.checks.len(), CheckId::ALL.len());
    assert_eq!(
        report.check(CheckId::Firewall).unwrap().status,
        Status::Amber
    );
    for id in [CheckId::FileVault, CheckId::Gatekeeper, CheckId::Sip] {
        assert_eq!(report.check(id).unwrap().status, Status::Green, "{id:?}");
    }
    assert_eq!(
        report.check(CheckId::MacosUpdates).unwrap().status,
        Status::Gray
    );
    let sec = checkup.security.clone().unwrap();
    assert_eq!(sec.week, iso_week_id(FIXTURE_NOW));
    assert_eq!(sec.amber, report.amber());
    assert_eq!(sec.headline(), report.headline());
    assert!(sec.first_run);
    assert!(!f.runner.ran("softwareupdate"));
    assert_eq!(
        security::saved_weeks(&f.env.paths),
        [iso_week_id(FIXTURE_NOW)]
    );

    // Memory: the fake table's figures and verdict.
    let mem = checkup.memory.unwrap();
    assert_eq!(mem.pressure, Pressure::Normal);
    assert_eq!(mem.swap_used, FAKE_SWAP_USED);
    assert_eq!(mem.compressed_bytes, FAKE_COMPRESSOR_PAGES * FAKE_PAGE_SIZE);
    assert_eq!(mem.verdict(), Pressure::Normal.verdict());
    let snap = events
        .iter()
        .find_map(|e| match e {
            Event::MemorySnapshot(s) => Some(s.clone()),
            _ => None,
        })
        .expect("the snapshot");
    assert_eq!(snap.apps[0].name, "Example Editor");
    assert_eq!(snap.apps[0].helpers, 2);

    // One line in checkups.jsonl, the same check up.
    let file = f.env.paths.checkups_file();
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert_eq!(
        reminder::read_checkups(&f.env.paths),
        std::slice::from_ref(&checkup)
    );

    // The reminder: 10:00 Los Angeles time, seven days after the check up's local date
    // (Friday 2026-10-23 07:02 PDT), which is 17:00 UTC.
    assert_eq!(
        (
            reminder.year,
            reminder.month,
            reminder.day,
            reminder.hour,
            reminder.minute
        ),
        (2026, 10, 30, 10, 0)
    );
    assert_eq!(reminder.at, 1_793_379_600, "2026-10-30T17:00:00Z");
    assert_eq!(c.rt.block_on(c.core.next_reminder()), Some(reminder));
    assert_eq!(c.rt.block_on(c.core.last_checkup()), Some(checkup));
    c.rt.block_on(c.core.shutdown());
}

#[test]
fn a_later_check_up_appends_compares_with_last_week_and_moves_the_reminder() {
    let f = fixture();
    let mut c = Core::start(&f);
    let (first, _) = done(&c.checkup(false));
    // Nine days later, across the end of daylight saving time (2026-11-01).
    f.clock.set(FIXTURE_NOW + 9 * DAY);
    let events = c.checkup(false);
    let (second, reminder) = done(&events);
    assert_eq!(second.at, FIXTURE_NOW + 9 * DAY);
    let sec = second.security.as_ref().unwrap();
    assert!(!sec.first_run);
    let report = events
        .iter()
        .find_map(|e| match e {
            Event::SecurityResult(r) => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(report.compared_to, Some(iso_week_id(FIXTURE_NOW)));
    assert_eq!(
        reminder::read_checkups(&f.env.paths),
        [first, second.clone()]
    );
    // 2026-11-01 06:02 PST plus seven days: 2026-11-08 10:00 PST, which is 18:00 UTC.
    assert_eq!(
        (reminder.year, reminder.month, reminder.day, reminder.hour),
        (2026, 11, 8, 10)
    );
    assert_eq!(
        reminder,
        reminder::next_reminder(Some(second.at), second.at, &f.env.tz)
    );
    assert_eq!(security::saved_weeks(&f.env.paths).len(), 2);
    c.rt.block_on(c.core.shutdown());
}
