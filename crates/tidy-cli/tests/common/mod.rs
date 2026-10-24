//! Shared helpers for the tidyctl tests: a standard fixture in a fresh temp folder and a way
//! to run the built `tidyctl` against it.
//!
//! Every run starts from an empty environment: no inherited `TIDY_*` variable, and `HOME`
//! pointed at an empty temp folder, so even a broken guard could not reach the real home
//! folder or the real Trash.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tidy_fixtures::{Expected, FIXTURE_NOW, FakeHome, write_expected};

pub struct Fixture {
    _tmp: tempfile::TempDir,
    /// The fixture folder (`tidy-fixture-home`'s `<dir>`).
    pub dir: PathBuf,
    /// The fake home (`TIDY_SCAN_ROOT`).
    pub home: PathBuf,
    /// `TIDY_HOME`.
    pub tidy: PathBuf,
    /// An empty folder used as `HOME` for every run.
    pub fake_user_home: PathBuf,
    pub expected: Expected,
}

/// What one run printed.
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    /// stdout, one JSON value per line.
    pub lines: Vec<Value>,
}

impl Run {
    /// The last line: the result.
    pub fn last(&self) -> &Value {
        self.lines.last().expect("at least one line")
    }

    pub fn events(&self, name: &str) -> Vec<&Value> {
        self.lines.iter().filter(|l| l["event"] == name).collect()
    }
}

impl Fixture {
    /// Builds the standard fixture the way `tidy-fixture-home` does: the tree, the two
    /// expected files and an empty `tidy/`.
    pub fn standard() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let dir = base.join("fixture");
        let expected = FakeHome::standard(&dir, FIXTURE_NOW).unwrap();
        write_expected(&dir, &expected).unwrap();
        let tidy = dir.join("tidy");
        std::fs::create_dir_all(&tidy).unwrap();
        let fake_user_home = base.join("user-home");
        std::fs::create_dir_all(&fake_user_home).unwrap();
        Fixture {
            _tmp: tmp,
            home: dir.join("home"),
            dir,
            tidy,
            fake_user_home,
            expected,
        }
    }

    /// The environment of the acceptance criteria: `TIDY_HOME`, `TIDY_SCAN_ROOT`,
    /// `TIDY_TEST_NOW` and `TIDY_TEST_RUNNING`.
    pub fn test_env(&self) -> Vec<(String, String)> {
        vec![
            ("TIDY_HOME".into(), self.tidy.display().to_string()),
            ("TIDY_SCAN_ROOT".into(), self.home.display().to_string()),
            ("TIDY_TEST_NOW".into(), self.expected.plan.now.to_string()),
            (
                "TIDY_TEST_RUNNING".into(),
                self.expected.plan.running.join(","),
            ),
        ]
    }

    /// Runs tidyctl with the test environment.
    pub fn tidyctl(&self, args: &[&str]) -> Run {
        self.tidyctl_with(args, &self.test_env())
    }

    /// Runs tidyctl with only `vars` set (plus `PATH`, and `HOME` at the empty folder).
    pub fn tidyctl_with(&self, args: &[&str], vars: &[(String, String)]) -> Run {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tidyctl"));
        cmd.args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.fake_user_home);
        for (k, v) in vars {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("tidyctl runs");
        let stdout = String::from_utf8(out.stdout).unwrap();
        let lines = stdout
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l)
                    .unwrap_or_else(|e| panic!("not a JSON line: {l:?}: {e}"))
            })
            .collect::<Vec<_>>();
        for l in &lines {
            assert!(l["event"].is_string(), "line without an event key: {l}");
        }
        Run {
            code: out.status.code().unwrap_or(-1),
            stdout,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            lines,
        }
    }

    pub fn read_json(&self, name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.join(name)).unwrap()).unwrap()
    }

    pub fn dir_trash(&self) -> PathBuf {
        self.tidy.join("data/trash")
    }

    pub fn history_file(&self) -> PathBuf {
        self.tidy.join("data/history.jsonl")
    }
}

/// Every path under `root` (relative, symlinks not followed) with its kind and length.
pub fn listing(root: &Path) -> BTreeMap<String, (char, u64)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd {
            let path = entry.unwrap().path();
            let meta = path.symlink_metadata().unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let kind = if meta.file_type().is_symlink() {
                'l'
            } else if meta.is_dir() {
                stack.push(path.clone());
                'd'
            } else {
                'f'
            };
            out.insert(rel, (kind, if kind == 'f' { meta.len() } else { 0 }));
        }
    }
    out
}

/// The names directly in `dir`, sorted. Empty when it does not exist.
pub fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Whether `path` is `item` or inside it (both relative, `/` separators).
pub fn under(path: &str, item: &str) -> bool {
    path == item || path.starts_with(&format!("{item}/"))
}

/// Picks `keys` out of a JSON object.
pub fn pick(v: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .map(|k| (k.to_string(), v[*k].clone()))
            .collect(),
    )
}
