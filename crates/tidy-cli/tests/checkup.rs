//! `tidyctl memory`, `security` and `checkup` on the standard fixture with the fake command
//! outputs and process table (`TIDY_TEST_COMMANDS`, `TIDY_TEST_MEMORY`).
//!
//! Every run has `TIDY_HOME` and `TIDY_SCAN_ROOT` in a temp dir and `HOME` at an empty temp
//! folder. The status commands and the process table are fakes; the Security run still reads
//! the system launch agent and daemon folders and checks the signatures of the programs they
//! name, which is read-only. `softwareupdate` has no fake answer and the real runner refuses
//! it under `TIDY_HOME`, so it never runs.

mod common;

use common::Fixture;
use serde_json::Value;
use tidy_core::model::{Class, Pressure};
use tidy_fixtures::write_fakes;

/// The test environment plus the fakes and a fixed time zone.
fn fake_env(fx: &Fixture) -> Vec<(String, String)> {
    let fakes = write_fakes(&fx.dir).unwrap();
    let mut vars = fx.test_env();
    vars.push((
        "TIDY_TEST_COMMANDS".into(),
        fakes.commands.display().to_string(),
    ));
    vars.push((
        "TIDY_TEST_MEMORY".into(),
        fakes.memory.display().to_string(),
    ));
    vars.push(("TIDY_TEST_TZ".into(), "PST8PDT,M3.2.0,M11.1.0".into()));
    vars
}

fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no check {id}"))
}

#[test]
fn checkup_appends_one_entry_and_prints_the_three_parts() {
    let fx = Fixture::standard();
    let run = fx.tidyctl_with(&["checkup"], &fake_env(&fx));
    assert_eq!(run.code, 0, "{}\n{}", run.stdout, run.stderr);
    assert!(run.events("error").is_empty(), "{}", run.stdout);
    // Each part's own line, then the result.
    assert_eq!(run.events("plan").len(), 1);
    assert_eq!(run.events("security").len(), 1);
    assert_eq!(run.events("memory").len(), 1);
    let last = run.last();
    assert_eq!(last["event"], "checkup");

    let safe: u64 = fx
        .expected
        .plan
        .items
        .iter()
        .filter(|i| i.ticked)
        .map(|i| i.bytes_alloc)
        .sum();
    let review: u64 = fx
        .expected
        .plan
        .items
        .iter()
        .filter(|i| i.class == Class::Review)
        .map(|i| i.bytes_alloc)
        .sum();
    assert_eq!(last["cleanup_total"], safe);
    assert_eq!(last["checkup"]["cleanup"]["review_bytes"], review);
    let summary = last["security_summary"].as_str().unwrap();
    assert!(
        summary == "All good" || summary.ends_with("to look at"),
        "{summary}"
    );
    assert!(last["security_first_check"].is_string());
    assert_eq!(last["memory_verdict"], Pressure::Normal.verdict());
    assert_eq!(last["checkup"]["at"], fx.expected.plan.now);
    assert_eq!(last["checkup"]["include_updates"], false);
    // Seven days after 2026-10-23, 10:00 in the test zone.
    assert_eq!(last["reminder"]["month"], 10);
    assert_eq!(last["reminder"]["day"], 30);
    assert_eq!(last["reminder"]["hour"], 10);

    // The firewall is off in the captured outputs.
    let report = run.events("security")[0];
    assert_eq!(check(report, "firewall")["status"], "amber");
    assert_eq!(check(report, "macos_updates")["status"], "gray");

    let log = std::fs::read_to_string(fx.tidy.join("data/checkups.jsonl")).unwrap();
    let lines: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert_eq!(lines[0], last["checkup"]);
    for part in ["cleanup", "security", "memory"] {
        assert!(lines[0][part].is_object(), "{part} missing: {log}");
    }
    // Nothing moved.
    assert!(common::names(&fx.dir_trash()).is_empty());
    assert!(!fx.history_file().exists());
}

#[test]
fn checkup_include_updates_records_it_without_running_softwareupdate() {
    let fx = Fixture::standard();
    let run = fx.tidyctl_with(&["checkup", "--include-updates"], &fake_env(&fx));
    assert_eq!(run.code, 0, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.last()["checkup"]["include_updates"], true);
    let report = run.events("security")[0];
    assert_eq!(report["updates_checked"], true);
    // The fake runner has no answer for softwareupdate: gray, with the reason.
    assert_eq!(check(report, "macos_updates")["status"], "gray");
}

#[test]
fn memory_prints_one_snapshot_of_the_fake_table() {
    let fx = Fixture::standard();
    let run = fx.tidyctl_with(&["memory"], &fake_env(&fx));
    assert_eq!(run.code, 0, "{}\n{}", run.stdout, run.stderr);
    let snap = run.last();
    assert_eq!(snap["event"], "memory");
    assert_eq!(snap["pressure"], "normal");
    assert_eq!(snap["pressure_level"], 1);
    assert_eq!(snap["swap"]["used"], tidy_fixtures::fakes::FAKE_SWAP_USED);
    let apps = snap["apps"].as_array().unwrap();
    assert_eq!(apps[0]["name"], "Example Editor");
    assert_eq!(apps[0]["helpers"], 2);
    let system = apps.last().unwrap();
    assert_eq!(system["kind"], "system");
    assert_eq!(system["quittable"], false);
    // Read-only: no check up, no Security report.
    assert!(!fx.tidy.join("data/checkups.jsonl").exists());
}

#[test]
fn security_prints_progress_then_every_check_and_saves_each_run() {
    let fx = Fixture::standard();
    let run = fx.tidyctl_with(&["security"], &fake_env(&fx));
    assert_eq!(run.code, 0, "{}\n{}", run.stdout, run.stderr);
    let progress = run.events("security_progress");
    assert!(!progress.is_empty());
    let total = progress[0]["total"].as_u64().unwrap();
    assert!(progress.iter().all(|p| p["total"] == total));
    assert_eq!(progress.last().unwrap()["done"], total);
    let report = run.last();
    assert_eq!(report["event"], "security");
    assert_eq!(report["first_run"], true);
    let ids: Vec<&str> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "filevault",
            "firewall",
            "gatekeeper",
            "sip",
            "xprotect",
            "macos_updates",
            "login_items",
            "chrome_extensions"
        ]
    );
    assert_eq!(check(report, "filevault")["status"], "green");
    let id = report["id"].as_str().unwrap();
    assert!(fx.tidy.join(format!("data/security/{id}.json")).is_file());

    // The next run, a minute later, is saved next to it and compares with it.
    let mut env = fake_env(&fx);
    for (k, v) in &mut env {
        if k == "TIDY_TEST_NOW" {
            *v = (fx.expected.plan.now + 60).to_string();
        }
    }
    let again = fx.tidyctl_with(&["security"], &env);
    assert_eq!(again.code, 0);
    assert_eq!(again.last()["first_run"], false);
    assert_eq!(again.last()["compared_to"], id);
    assert_eq!(common::names(&fx.tidy.join("data/security")).len(), 2);
}

#[test]
fn m2_commands_reject_bad_arguments() {
    let fx = Fixture::standard();
    for args in [
        &["memory", "--all"][..],
        &["security", "--updates"][..],
        &["checkup", "now"][..],
    ] {
        let run = fx.tidyctl(args);
        assert_eq!(run.code, 2, "{args:?}");
    }
}
