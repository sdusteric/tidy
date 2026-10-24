//! Fake home folders for tests: declarative trees with exact sizes and dates under a temp
//! dir, plus the results the scans must produce.
//!
//! - [`tree`] is the builder: folders, pattern-filled files, sparse files, hard links,
//!   symlinks, markers and fake `.app` bundles, with times set from a fixed clock.
//! - [`standard`] builds the standard fixture ([`FakeHome::standard`]) and returns the
//!   [`Expected`] results, written by hand from the design's rules.
//! - [`fakes`] writes the fake command outputs and process table a check up runs with
//!   (`TIDY_TEST_COMMANDS`, `TIDY_TEST_MEMORY`).
//! - The `tidy-fixture-home` binary builds the standard fixture into a new folder for
//!   `tidyctl` and the acceptance script.
//!
//! Nothing here reads or writes the real home folder. [`check_target_dir`] refuses a target
//! that is not empty or that is under `~/Documents`, `~/Desktop` or `~/Downloads`.

pub mod fakes;
pub mod standard;
pub mod tree;

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

pub use fakes::{FakeFiles, write_fakes};
pub use standard::{
    Expected, ExpectedFolder, ExpectedItem, ExpectedLarge, ExpectedLargeItem, ExpectedPlan,
    FIXTURE_NOW, FakeHome,
};
pub use tree::{Ledger, Node, NodeKind, Tree};

/// The folders under the home folder that macOS protects, which fixtures never go in.
const PROTECTED: [&str; 3] = ["Documents", "Desktop", "Downloads"];

/// Checks a folder before a fixture is built into it, using `$HOME`. See
/// [`check_target_dir_in`].
pub fn check_target_dir(dir: &Path) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    check_target_dir_in(dir, home.as_deref())
}

/// Returns `dir` as an absolute path if a fixture may be built there: it does not exist or is
/// an empty folder, and it is not under `home/Documents`, `home/Desktop` or `home/Downloads`
/// (compared after resolving symlinks, so a link into those folders is refused too).
pub fn check_target_dir_in(dir: &Path, home: Option<&Path>) -> Result<PathBuf> {
    let abs = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(dir)
    };
    let mut clean = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other.as_os_str()),
        }
    }

    match fs::metadata(&clean) {
        Ok(meta) if !meta.is_dir() => bail!("{} is not a folder", clean.display()),
        Ok(_) => {
            let mut entries =
                fs::read_dir(&clean).with_context(|| format!("reading {}", clean.display()))?;
            if entries.next().is_some() {
                bail!("{} is not empty", clean.display());
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("checking {}", clean.display())),
    }

    if let Some(home) = home {
        let resolved = resolve(&clean);
        for name in PROTECTED {
            let protected = home.join(name);
            if clean.starts_with(&protected) || resolved.starts_with(resolve(&protected)) {
                bail!(
                    "{} is under {}; fixtures never go in Documents, Desktop or Downloads",
                    clean.display(),
                    protected.display()
                );
            }
        }
    }
    Ok(clean)
}

/// Canonicalizes the longest existing prefix of `path` and appends the rest.
fn resolve(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(real) = fs::canonicalize(cur) {
            return rest.iter().rev().fold(real, |acc, c| acc.join(c));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Writes `expected-plan.json` and `expected-large.json` into `dir`.
pub fn write_expected(dir: &Path, expected: &Expected) -> Result<()> {
    for (name, json) in [
        (
            "expected-plan.json",
            serde_json::to_string_pretty(&expected.plan)?,
        ),
        (
            "expected-large.json",
            serde_json::to_string_pretty(&expected.large)?,
        ),
    ] {
        let path = dir.join(name);
        fs::write(&path, format!("{json}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}
