//! `tidy-fixture-home <dir> [--now SECS]`
//!
//! Builds the standard fake home in a new, empty folder and prints one JSON line with the
//! values to use:
//!
//! ```text
//! TIDY_HOME=<dir>/tidy TIDY_SCAN_ROOT=<dir>/home TIDY_TEST_NOW=<test_now> \
//!     TIDY_TEST_RUNNING=com.google.Chrome tidyctl scan
//! ```
//!
//! It also writes `<dir>/expected-plan.json` and `<dir>/expected-large.json`, and creates the
//! empty `<dir>/tidy`. It refuses a folder that is not empty or that is under
//! `~/Documents`, `~/Desktop` or `~/Downloads`. Exit codes: 0 done, 1 failed or refused,
//! 2 bad usage.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde_json::json;
use tidy_fixtures::{FIXTURE_NOW, FakeHome, check_target_dir, write_expected};

const USAGE: &str = "usage: tidy-fixture-home <dir> [--now SECS]";

fn main() -> ExitCode {
    let mut dir: Option<PathBuf> = None;
    let mut now = FIXTURE_NOW;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--now" => match args.next().and_then(|v| v.parse::<u64>().ok()) {
                Some(v) => now = v,
                None => return usage(),
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => return usage(),
            other => {
                if dir.replace(PathBuf::from(other)).is_some() {
                    return usage();
                }
            }
        }
    }
    let Some(dir) = dir else {
        return usage();
    };
    match run(dir, now) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tidy-fixture-home: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn run(dir: PathBuf, now: u64) -> Result<()> {
    let dir = check_target_dir(&dir)?;
    let expected = FakeHome::standard(&dir, now)?;
    write_expected(&dir, &expected)?;
    let tidy = dir.join("tidy");
    fs::create_dir_all(&tidy).with_context(|| format!("creating {}", tidy.display()))?;

    let home = dir.join("home");
    let running = expected.plan.running.join(",");
    let line = json!({
        "event": "fixture",
        "dir": dir,
        "home": home,
        "tidy_home": tidy,
        "test_now": now,
        "test_running": running,
        "expected_plan": dir.join("expected-plan.json"),
        "expected_large": dir.join("expected-large.json"),
        "env": {
            "TIDY_HOME": tidy,
            "TIDY_SCAN_ROOT": home,
            "TIDY_TEST_NOW": now.to_string(),
            "TIDY_TEST_RUNNING": running,
        },
    });
    println!("{line}");
    Ok(())
}
