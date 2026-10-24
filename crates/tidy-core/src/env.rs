//! The environment every scan and clean runs in: the scan root, Tidy's folders, settings, and
//! four seams (traits) that tests replace with fakes.
//!
//! | Seam | Production | Fake |
//! |---|---|---|
//! | [`Trasher`] | `SystemTrash` (NSFileManager) | [`DirTrash`] (a folder) |
//! | [`ProcessProbe`] | the libc probe in `procs` | [`FixedProbe`] |
//! | [`Clock`] | [`SystemClock`] | [`FixedClock`] |
//! | [`LastUsed`] | Spotlight in `lastused` | [`FileTimesOnly`] |
//!
//! [`Env::from_env`] builds the production variant, or the `TIDY_HOME` variant when that
//! variable is set (design section 8.4). The test hooks (`TIDY_SCAN_ROOT`,
//! `TIDY_TEST_RUNNING`, `TIDY_TEST_NOW`, `TIDY_TEST_NO_SPOTLIGHT`) are honored only under
//! `TIDY_HOME`; without it they are ignored and logged. [`Env::for_tests`] builds the all-fake
//! variant and never touches the real home folder.

use std::ffi::OsString;
use std::fmt;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::model::{RunningSet, from_unix_secs, unix_secs};
use crate::paths::{AppPaths, HOME_ENV};
use crate::settings::Settings;
pub use crate::trash::{DirTrash, Trasher};

/// The fake home that rules and the large-file scan resolve against. Only under `TIDY_HOME`.
pub const SCAN_ROOT_ENV: &str = "TIDY_SCAN_ROOT";
/// Comma-separated bundle ids and process names treated as running, replacing the real
/// process list. Only under `TIDY_HOME`.
pub const TEST_RUNNING_ENV: &str = "TIDY_TEST_RUNNING";
/// A fixed clock (Unix seconds) for age rules and "not opened" dates. Only under `TIDY_HOME`.
pub const TEST_NOW_ENV: &str = "TIDY_TEST_NOW";
/// `1` (the default under `TIDY_HOME`, C22) uses file times only for last-use dates; `0`
/// allows Spotlight. Only under `TIDY_HOME`.
pub const TEST_NO_SPOTLIGHT_ENV: &str = "TIDY_TEST_NO_SPOTLIGHT";

/// The variables that only count under `TIDY_HOME`.
pub const TEST_HOOK_VARS: [&str; 4] = [
    SCAN_ROOT_ENV,
    TEST_RUNNING_ENV,
    TEST_NOW_ENV,
    TEST_NO_SPOTLIGHT_ENV,
];

/// The fixed time [`Env::for_tests`] starts at: 2026-10-23T14:02:11Z, the time in the
/// design's history example.
pub const TEST_NOW: u64 = 1_792_764_131;

// ---------- Seams ----------

/// Which bundle ids and process names are running.
pub trait ProcessProbe: Send + Sync {
    fn running(&self) -> RunningSet;
}

/// The current time.
pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;

    /// [`Clock::now`] as Unix seconds.
    fn unix_now(&self) -> u64 {
        unix_secs(self.now())
    }
}

/// The date a file or package was last opened, from a source other than its file times.
pub trait LastUsed: Send + Sync {
    /// Spotlight's `kMDItemLastUsedDate`, or None when there is none. The caller then falls
    /// back to [`file_time`] and labels the date "Modified".
    fn last_used(&self, path: &Path) -> Option<SystemTime>;
}

/// The later of mtime and atime: the fallback last-use date ("Modified 2 years ago").
pub fn file_time(meta: &Metadata) -> Option<SystemTime> {
    match (meta.modified().ok(), meta.accessed().ok()) {
        (Some(m), Some(a)) => Some(m.max(a)),
        (m, a) => m.or(a),
    }
}

// ---------- Production clock and fakes ----------

/// The system clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// A clock that says what it is told. Shared through an `Arc`, a test can move it between a
/// scan and a clean.
#[derive(Debug)]
pub struct FixedClock(Mutex<u64>);

impl FixedClock {
    /// A clock at `secs` (Unix seconds).
    pub fn new(secs: u64) -> FixedClock {
        FixedClock(Mutex::new(secs))
    }

    pub fn set(&self, secs: u64) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = secs;
    }

    pub fn advance(&self, secs: u64) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) += secs;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        from_unix_secs(*self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// A process list that says what it is told. Shared through an `Arc`, a test can flip it
/// between a scan and a clean to prove the clean re-checks.
#[derive(Debug, Default)]
pub struct FixedProbe(Mutex<RunningSet>);

impl FixedProbe {
    pub fn new(set: RunningSet) -> FixedProbe {
        FixedProbe(Mutex::new(set))
    }

    /// From a comma-separated list, as `TIDY_TEST_RUNNING` gives it.
    pub fn parse(list: &str) -> FixedProbe {
        FixedProbe::new(RunningSet::parse(list))
    }

    pub fn set(&self, set: RunningSet) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = set;
    }
}

impl ProcessProbe for FixedProbe {
    fn running(&self) -> RunningSet {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// A probe for when no real process list is available: every owner counts as running, so
/// nothing with an owner is offered on a guess. The default in [`SystemSeams`] until the real
/// probe is passed in.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnknownProbe;

impl ProcessProbe for UnknownProbe {
    fn running(&self) -> RunningSet {
        RunningSet::unknown()
    }
}

/// No Spotlight: every last-use date comes from the file times (C22, the default under
/// `TIDY_HOME`, so fixture dates decide).
#[derive(Clone, Copy, Debug, Default)]
pub struct FileTimesOnly;

impl LastUsed for FileTimesOnly {
    fn last_used(&self, _path: &Path) -> Option<SystemTime> {
        None
    }
}

/// The production process probe and last-use source. They live in their own modules (libc
/// and Spotlight FFI), so the caller passes them to [`Env::from_env_with`]. The default is the
/// conservative pair: [`UnknownProbe`] and [`FileTimesOnly`].
#[derive(Clone)]
pub struct SystemSeams {
    pub procs: Arc<dyn ProcessProbe>,
    pub last_used: Arc<dyn LastUsed>,
}

impl Default for SystemSeams {
    fn default() -> SystemSeams {
        SystemSeams {
            procs: Arc::new(UnknownProbe),
            last_used: Arc::new(FileTimesOnly),
        }
    }
}

// ---------- Env ----------

/// What the environment variables ask for, before anything is built. Pure, so tests can
/// check every rule without setting variables in the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvSpec {
    /// `TIDY_HOME`, when set and not empty.
    pub tidy_home: Option<PathBuf>,
    /// The scan root: `TIDY_SCAN_ROOT`, or `$TIDY_HOME/home`, or the real home folder.
    pub root: PathBuf,
    /// `TIDY_TEST_RUNNING`, replacing the real process list.
    pub running: Option<RunningSet>,
    /// `TIDY_TEST_NOW`.
    pub now: Option<u64>,
    /// Whether Spotlight may supply last-use dates.
    pub spotlight: bool,
    /// Test hooks that were set but ignored (no `TIDY_HOME`, or a value that does not parse).
    pub ignored: Vec<&'static str>,
}

impl EnvSpec {
    /// Reads the variables through `get`. `home` is the real home folder, used only without
    /// `TIDY_HOME`.
    pub fn from_vars(get: impl Fn(&str) -> Option<OsString>, home: Option<PathBuf>) -> EnvSpec {
        let var = |k: &str| get(k).filter(|v| !v.is_empty());
        let Some(tidy_home) = var(HOME_ENV).map(PathBuf::from) else {
            return EnvSpec {
                tidy_home: None,
                // An unknown home scans an empty system folder rather than the working one.
                root: home.unwrap_or_else(|| PathBuf::from("/var/empty")),
                running: None,
                now: None,
                spotlight: true,
                ignored: TEST_HOOK_VARS
                    .into_iter()
                    .filter(|k| var(k).is_some())
                    .collect(),
            };
        };
        let mut ignored = Vec::new();
        let root = var(SCAN_ROOT_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| tidy_home.join("home"));
        let running = var(TEST_RUNNING_ENV).map(|v| RunningSet::parse(&v.to_string_lossy()));
        let now = var(TEST_NOW_ENV).and_then(|v| {
            let parsed = v.to_string_lossy().trim().parse::<u64>().ok();
            if parsed.is_none() {
                ignored.push(TEST_NOW_ENV);
            }
            parsed
        });
        let spotlight = var(TEST_NO_SPOTLIGHT_ENV).is_some_and(|v| v == "0");
        EnvSpec {
            tidy_home: Some(tidy_home),
            root,
            running,
            now,
            spotlight,
            ignored,
        }
    }
}

/// Everything a scan or clean needs. Cheap to clone: the seams are shared.
#[derive(Clone)]
pub struct Env {
    /// The scan root (the home folder in production). Used as given, not canonicalized; the
    /// trash guard resolves it before every move.
    pub root: PathBuf,
    pub paths: AppPaths,
    pub trasher: Arc<dyn Trasher>,
    pub procs: Arc<dyn ProcessProbe>,
    pub clock: Arc<dyn Clock>,
    pub last_used: Arc<dyn LastUsed>,
    pub settings: Settings,
    /// True under `TIDY_HOME` (or in tests): the folders, the Trash and the root are fakes.
    pub overridden: bool,
}

impl fmt::Debug for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Env")
            .field("root", &self.root)
            .field("paths", &self.paths)
            .field("trasher", &self.trasher.kind())
            .field("settings", &self.settings)
            .field("overridden", &self.overridden)
            .finish_non_exhaustive()
    }
}

impl Env {
    /// The environment for this process, with the conservative [`SystemSeams::default`].
    /// Callers that have the real probe and Spotlight source use [`Env::from_env_with`].
    pub fn from_env() -> Env {
        Self::from_env_with(SystemSeams::default())
    }

    /// The environment for this process. `system` supplies the production process probe and
    /// last-use source; under `TIDY_HOME` the test hooks may replace them.
    pub fn from_env_with(system: SystemSeams) -> Env {
        let spec = EnvSpec::from_vars(|k| std::env::var_os(k), dirs::home_dir());
        Self::build(spec, system)
    }

    /// Builds the environment that `spec` describes. Loads settings.json from the resolved
    /// data folder.
    pub fn build(spec: EnvSpec, system: SystemSeams) -> Env {
        for k in &spec.ignored {
            if spec.tidy_home.is_some() {
                tracing::warn!("{k} has a value that does not parse; ignoring it");
            } else {
                tracing::warn!("{k} is set but TIDY_HOME is not; ignoring it");
            }
        }
        let paths = match &spec.tidy_home {
            Some(home) => AppPaths::under(home),
            None => AppPaths::system(),
        };
        let trasher = make_trasher(spec.tidy_home.is_some(), &paths);
        let procs: Arc<dyn ProcessProbe> = match spec.running {
            Some(set) => Arc::new(FixedProbe::new(set)),
            None => system.procs,
        };
        let clock: Arc<dyn Clock> = match spec.now {
            Some(secs) => Arc::new(FixedClock::new(secs)),
            None => Arc::new(SystemClock),
        };
        let last_used: Arc<dyn LastUsed> = if spec.spotlight {
            system.last_used
        } else {
            Arc::new(FileTimesOnly)
        };
        let settings = Settings::load(&paths);
        Env {
            root: spec.root,
            overridden: paths.overridden,
            paths,
            trasher,
            procs,
            clock,
            last_used,
            settings,
        }
    }

    /// The all-fake environment: `fake_home` as the scan root, Tidy's folders under
    /// `tidy_home`, a [`DirTrash`] in `tidy_home/data/trash`, nothing running, the clock at
    /// [`TEST_NOW`], file times only and default settings. Reads and writes nothing.
    pub fn for_tests(fake_home: &Path, tidy_home: &Path) -> Env {
        let paths = AppPaths::under(tidy_home);
        Env {
            root: fake_home.to_path_buf(),
            trasher: Arc::new(DirTrash::new(paths.dir_trash())),
            paths,
            procs: Arc::new(FixedProbe::default()),
            clock: Arc::new(FixedClock::new(TEST_NOW)),
            last_used: Arc::new(FileTimesOnly),
            settings: Settings::default(),
            overridden: true,
        }
    }

    pub fn with_trasher(mut self, trasher: Arc<dyn Trasher>) -> Env {
        self.trasher = trasher;
        self
    }

    pub fn with_procs(mut self, procs: Arc<dyn ProcessProbe>) -> Env {
        self.procs = procs;
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Env {
        self.clock = clock;
        self
    }

    pub fn with_last_used(mut self, last_used: Arc<dyn LastUsed>) -> Env {
        self.last_used = last_used;
        self
    }

    pub fn with_settings(mut self, settings: Settings) -> Env {
        self.settings = settings;
        self
    }

    /// The clock as Unix seconds.
    pub fn now(&self) -> u64 {
        self.clock.unix_now()
    }

    /// The scan root joined with a relative path.
    pub fn at(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.root.join(rel)
    }
}

/// The only place that picks the macOS Trash: when `TIDY_HOME` is unset. Under `TIDY_HOME`
/// it is the folder Trash in `data/trash`.
fn make_trasher(tidy_home: bool, paths: &AppPaths) -> Arc<dyn Trasher> {
    if tidy_home {
        Arc::new(DirTrash::new(paths.dir_trash()))
    } else {
        Arc::new(crate::trash::SystemTrash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TrasherKind;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn home() -> Option<PathBuf> {
        Some(PathBuf::from("/Users/someone"))
    }

    #[test]
    fn without_tidy_home_the_hooks_are_ignored() {
        let spec = EnvSpec::from_vars(
            vars(&[
                (SCAN_ROOT_ENV, "/tmp/fake"),
                (TEST_RUNNING_ENV, "com.google.Chrome"),
                (TEST_NOW_ENV, "5"),
            ]),
            home(),
        );
        assert_eq!(spec.tidy_home, None);
        assert_eq!(spec.root, PathBuf::from("/Users/someone"));
        assert_eq!(spec.running, None);
        assert_eq!(spec.now, None);
        assert!(spec.spotlight);
        assert_eq!(
            spec.ignored,
            [SCAN_ROOT_ENV, TEST_RUNNING_ENV, TEST_NOW_ENV]
        );
        // An empty TIDY_HOME is the same as none.
        let spec = EnvSpec::from_vars(vars(&[(HOME_ENV, "")]), None);
        assert_eq!(spec.tidy_home, None);
        assert_eq!(spec.root, PathBuf::from("/var/empty"));
        assert!(spec.ignored.is_empty());
    }

    #[test]
    fn tidy_home_defaults() {
        let spec = EnvSpec::from_vars(vars(&[(HOME_ENV, "/tmp/th")]), home());
        assert_eq!(spec.tidy_home, Some(PathBuf::from("/tmp/th")));
        assert_eq!(spec.root, PathBuf::from("/tmp/th/home"));
        assert_eq!(spec.running, None);
        assert_eq!(spec.now, None);
        assert!(!spec.spotlight, "file times only by default (C22)");
        assert!(spec.ignored.is_empty());
    }

    #[test]
    fn tidy_home_honors_every_hook() {
        let spec = EnvSpec::from_vars(
            vars(&[
                (HOME_ENV, "/tmp/th"),
                (SCAN_ROOT_ENV, "/tmp/fake/home"),
                (TEST_RUNNING_ENV, "com.google.Chrome,npm"),
                (TEST_NOW_ENV, "1792764131"),
                (TEST_NO_SPOTLIGHT_ENV, "0"),
            ]),
            home(),
        );
        assert_eq!(spec.root, PathBuf::from("/tmp/fake/home"));
        assert_eq!(
            spec.running,
            Some(RunningSet::parse("com.google.chrome,npm"))
        );
        assert_eq!(spec.now, Some(1_792_764_131));
        assert!(spec.spotlight);
        // A bad clock value is ignored and reported; an empty scan root falls back.
        let spec = EnvSpec::from_vars(
            vars(&[
                (HOME_ENV, "/tmp/th"),
                (SCAN_ROOT_ENV, ""),
                (TEST_NOW_ENV, "soon"),
                (TEST_NO_SPOTLIGHT_ENV, "1"),
            ]),
            home(),
        );
        assert_eq!(spec.root, PathBuf::from("/tmp/th/home"));
        assert_eq!(spec.now, None);
        assert_eq!(spec.ignored, [TEST_NOW_ENV]);
        assert!(!spec.spotlight);
    }

    #[test]
    fn build_under_tidy_home_uses_only_fakes() {
        let tmp = tempfile::tempdir().unwrap();
        let th = tmp.path().join("tidy");
        let spec = EnvSpec::from_vars(
            vars(&[
                (HOME_ENV, th.to_str().unwrap()),
                (TEST_RUNNING_ENV, "com.google.Chrome"),
                (TEST_NOW_ENV, "1000"),
            ]),
            home(),
        );
        let env = Env::build(spec, SystemSeams::default());
        assert!(env.overridden);
        assert_eq!(env.root, th.join("home"));
        assert_eq!(env.paths, AppPaths::under(&th));
        assert_eq!(env.trasher.kind(), TrasherKind::Dir);
        assert!(env.procs.running().contains("com.google.chrome"));
        assert!(!env.procs.running().is_unknown());
        assert_eq!(env.now(), 1000);
        assert_eq!(env.last_used.last_used(tmp.path()), None);
        assert_eq!(env.settings, Settings::default());
        // Building reads settings but writes nothing.
        assert!(!th.exists());
        assert!(format!("{env:?}").contains("Dir"));
    }

    #[test]
    fn build_under_tidy_home_without_a_running_list_uses_the_system_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = EnvSpec::from_vars(vars(&[(HOME_ENV, tmp.path().to_str().unwrap())]), None);
        let env = Env::build(spec, SystemSeams::default());
        assert!(env.procs.running().is_unknown());
    }

    #[test]
    fn the_trasher_follows_tidy_home() {
        let paths = AppPaths::under(Path::new("/tmp/th"));
        assert_eq!(make_trasher(true, &paths).kind(), TrasherKind::Dir);
        // Only built here, never used.
        assert_eq!(make_trasher(false, &paths).kind(), TrasherKind::System);
    }

    #[test]
    fn for_tests_is_all_fake_and_adjustable() {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env::for_tests(&tmp.path().join("home"), &tmp.path().join("tidy"));
        assert!(env.overridden);
        assert_eq!(env.now(), TEST_NOW);
        assert!(env.procs.running().is_empty());
        assert_eq!(env.trasher.kind(), TrasherKind::Dir);
        assert_eq!(env.at("Library"), tmp.path().join("home/Library"));
        let clock = Arc::new(FixedClock::new(10));
        let probe = Arc::new(FixedProbe::parse("Xcode"));
        let env = env
            .with_clock(clock.clone())
            .with_procs(probe.clone())
            .with_settings(Settings {
                cleanup_scanned_once: true,
                ..Settings::default()
            });
        clock.advance(5);
        assert_eq!(env.now(), 15);
        clock.set(3);
        assert_eq!(env.now(), 3);
        assert!(env.procs.running().contains("xcode"));
        probe.set(RunningSet::default());
        assert!(!env.procs.running().contains("xcode"));
        assert!(env.settings.cleanup_scanned_once);
    }

    #[test]
    fn file_time_is_the_later_of_mtime_and_atime() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        let file = std::fs::File::options().write(true).open(&f).unwrap();
        let old = from_unix_secs(1_000_000);
        let newer = from_unix_secs(2_000_000);
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(old)
                .set_accessed(newer),
        )
        .unwrap();
        let meta = std::fs::metadata(&f).unwrap();
        assert_eq!(file_time(&meta), Some(newer));
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(newer)
                .set_accessed(old),
        )
        .unwrap();
        assert_eq!(file_time(&std::fs::metadata(&f).unwrap()), Some(newer));
    }
}
