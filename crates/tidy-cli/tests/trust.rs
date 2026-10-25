//! The built `tidyctl` for the trust surfaces (design 17): `explain` gives the details of a
//! Cleanup item from the rule table, and `system-data` measures the fake home and lists local
//! snapshots only through captured `tmutil` output. Nothing here asks the real Mac.

mod common;

use common::Fixture;
use serde_json::Value;

#[test]
fn explain_prints_the_details_of_a_safe_item() {
    let f = Fixture::standard();
    let plan = f.tidyctl(&["scan"]);
    assert_eq!(plan.code, 0, "{}", plan.stderr);
    let safe = plan.last()["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["class"] == "safe" && i["id"].as_str().unwrap().starts_with("cache.app:"))
        .expect("a safe app cache in the fixture")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let run = f.tidyctl(&["explain", &safe]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let e = run.last();
    assert_eq!(e["event"], "explain");
    assert_eq!(e["id"], Value::String(safe.clone()));
    assert_eq!(e["rule_id"], "cache.app");
    assert!(e["verdict"].as_str().unwrap().starts_with("Safe to clean."));
    assert!(!e["rebuild"].as_str().unwrap().is_empty());
    assert!(
        e["patterns"]
            .as_array()
            .unwrap()
            .contains(&Value::String("Library/Caches/<app>".into()))
    );
    // Nothing moved.
    assert!(f.home.join(safe.split_once(':').unwrap().1).exists());
    let missing = f.tidyctl(&["explain", "cache.app:nope"]);
    assert_eq!(missing.code, 1);
    assert_eq!(missing.last()["what"], "explain");
}

#[test]
fn system_data_measures_the_fake_home_and_lists_snapshots_from_captured_output() {
    let f = Fixture::standard();
    std::fs::create_dir_all(f.home.join(".docker")).unwrap();
    let commands = f.dir.join("tmutil.json");
    std::fs::write(
        &commands,
        r#"{"tmutil listlocalsnapshots /": {"status": 0, "stdout": "Snapshots for disk /:\ncom.apple.TimeMachine.2026-10-25-101112.local\n"}}"#,
    )
    .unwrap();
    let mut vars = f.test_env();
    vars.push(("TIDY_TEST_COMMANDS".into(), commands.display().to_string()));
    let run = f.tidyctl_with(&["system-data"], &vars);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let r = run.last();
    assert_eq!(r["event"], "system_data");
    let rows = r["rows"].as_array().unwrap();
    let row = |kind: &str| rows.iter().find(|x| x["kind"] == kind).cloned();
    let caches = row("caches").expect("the fixture has caches");
    assert_eq!(caches["size"]["kind"], "measured");
    let docker = row("docker").expect(".docker exists");
    assert_eq!(docker["size"]["kind"], "not_measured");
    assert_eq!(docker["report_only"], true);
    assert_eq!(docker["commands"][1]["command"], "docker system prune");
    let snaps = row("snapshots").unwrap();
    assert_eq!(snaps["size"]["kind"], "not_reported");
    assert!(snaps["note"].as_str().unwrap().starts_with("1 snapshot,"));
    assert!(r["measured_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn system_data_under_tidy_home_never_runs_the_real_tmutil() {
    let f = Fixture::standard();
    // No TIDY_TEST_COMMANDS: the real runner refuses tmutil under TIDY_HOME.
    let run = f.tidyctl(&["system-data"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let rows = run.last()["rows"].as_array().unwrap().clone();
    let snaps = rows.iter().find(|x| x["kind"] == "snapshots").unwrap();
    assert_eq!(snaps["size"]["kind"], "not_listed");
}
