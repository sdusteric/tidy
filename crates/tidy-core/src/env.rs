//! The environment every scan and clean runs in: the scan root, Tidy's folders, settings, the
//! local time zone, and six seams (traits) that tests replace with fakes.
//!
//! | Seam | Production | Fake |
//! |---|---|---|
//! | [`Trasher`] | `SystemTrash` (NSFileManager) | [`DirTrash`] (a folder) |
//! | [`ProcessProbe`] | the libc probe in `procs` | [`FixedProbe`] |
//! | [`Clock`] | [`SystemClock`] | [`FixedClock`] |
//! | [`LastUsed`] | Spotlight in `lastused` | [`FileTimesOnly`] |
//! | [`MemoryProbe`] (M2) | the libc and mach probe in `memory` | [`FixedMemory`] |
//! | [`CommandRunner`] (M2) | [`SystemRunner`] | [`ReplayRunner`] |
//!
//! [`Env::from_env`] builds the production variant, or the `TIDY_HOME` variant when that
//! variable is set (design section 8.4). The test hooks (`TIDY_SCAN_ROOT`,
//! `TIDY_TEST_RUNNING`, `TIDY_TEST_NOW`, `TIDY_TEST_NO_SPOTLIGHT`, and from M2
//! `TIDY_TEST_COMMANDS`, `TIDY_TEST_MEMORY`, `TIDY_TEST_TZ`) are honored only under
//! `TIDY_HOME`; without it they are ignored and logged. [`Env::for_tests`] builds the all-fake
//! variant and never touches the real home folder.
//!
//! The production memory probe and command runner only read: the memory probe lists
//! processes, and the runner is used for read-only status commands (`fdesetup status`,
//! `spctl --status` and the like, and `tmutil listlocalsnapshots /` for the System Data
//! sheet). [`SystemRunner`] refuses `softwareupdate` and `tmutil` under `TIDY_HOME` and in
//! this crate's tests, and refuses `docker` and every `tmutil` verb but the listing always.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::Metadata;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::model::{ProcInfo, RunningSet, VmStats, from_unix_secs, unix_secs};
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

/// A JSON file of captured command outputs (see [`ReplayRunner::from_file`]) that replaces
/// the real command runner, so Security and Check up can run on fixtures. Only under
/// `TIDY_HOME`.
pub const TEST_COMMANDS_ENV: &str = "TIDY_TEST_COMMANDS";
/// A JSON file with a fake process table and vm stats (see [`FixedMemory::from_file`]) that
/// replaces the real memory probe. Only under `TIDY_HOME`.
pub const TEST_MEMORY_ENV: &str = "TIDY_TEST_MEMORY";
/// The local time zone for the reminder date: an IANA name (`America/Los_Angeles`) or a
/// POSIX TZ string. Only under `TIDY_HOME`.
pub const TEST_TZ_ENV: &str = "TIDY_TEST_TZ";

/// The variables that only count under `TIDY_HOME`.
pub const TEST_HOOK_VARS: [&str; 7] = [
    SCAN_ROOT_ENV,
    TEST_RUNNING_ENV,
    TEST_NOW_ENV,
    TEST_NO_SPOTLIGHT_ENV,
    TEST_COMMANDS_ENV,
    TEST_MEMORY_ENV,
    TEST_TZ_ENV,
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

/// The process table and the system-wide memory figures (M2, design section 5.3). Read-only:
/// nothing here quits or signals a process; the app quits apps itself through
/// `NSRunningApplication`.
pub trait MemoryProbe: Send + Sync {
    /// Every process the probe can see, in any order. Details it cannot read are None (a
    /// root-owned process has no footprint).
    fn processes(&self) -> Vec<ProcInfo>;

    /// Pressure, swap, total memory and page counts.
    fn vm(&self) -> io::Result<VmStats>;

    /// The pid of the running Tidy (or tidyctl), which never gets a Quit button.
    fn own_pid(&self) -> i32 {
        i32::try_from(std::process::id()).unwrap_or(0)
    }
}

/// What a command printed and how it ended.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    /// The exit code. None when a signal ended it.
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

impl CommandOutput {
    /// Exit code 0 with `stdout`.
    pub fn ok(stdout: impl Into<String>) -> CommandOutput {
        CommandOutput {
            status: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// Exit code `status` with `stdout` and `stderr`.
    pub fn exit(
        status: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> CommandOutput {
        CommandOutput {
            status: Some(status),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    /// Whether it exited with 0.
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Runs a program and collects its output (M2: the Security checks).
///
/// Errors: `NotFound` when the program does not exist (or a fake has no reply for it),
/// `TimedOut` when it ran past `timeout` (the real runner then stops it and everything it
/// started), `PermissionDenied` when the runner refuses the program. A check turns any error
/// into a gray row that says why.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> io::Result<CommandOutput>;
}

/// `program` and `args` as one line, the key [`ReplayRunner`] replies by:
/// `/usr/bin/fdesetup status`.
pub fn command_line(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
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

// ---------- Memory fakes ----------

/// The contents of a [`FixedMemory`]: also the format of the `TIDY_TEST_MEMORY` file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryTable {
    /// The pid that plays Tidy. 0 means none of the processes is Tidy.
    pub own_pid: i32,
    pub processes: Vec<ProcInfo>,
    pub vm: VmStats,
}

/// A memory probe that says what it is told. Shared through an `Arc`, a test can change the
/// process table between two snapshots (an app quits, a helper starts).
#[derive(Debug, Default)]
pub struct FixedMemory(Mutex<MemoryTable>);

impl FixedMemory {
    pub fn new(table: MemoryTable) -> FixedMemory {
        FixedMemory(Mutex::new(table))
    }

    /// Reads a [`MemoryTable`] from a JSON file.
    pub fn from_file(file: &Path) -> io::Result<FixedMemory> {
        let bytes = std::fs::read(file)?;
        let table: MemoryTable = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        Ok(FixedMemory::new(table))
    }

    fn table(&self) -> std::sync::MutexGuard<'_, MemoryTable> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_processes(&self, processes: Vec<ProcInfo>) {
        self.table().processes = processes;
    }

    pub fn set_vm(&self, vm: VmStats) {
        self.table().vm = vm;
    }

    pub fn set_own_pid(&self, pid: i32) {
        self.table().own_pid = pid;
    }
}

impl MemoryProbe for FixedMemory {
    fn processes(&self) -> Vec<ProcInfo> {
        self.table().processes.clone()
    }

    fn vm(&self) -> io::Result<VmStats> {
        Ok(self.table().vm)
    }

    fn own_pid(&self) -> i32 {
        self.table().own_pid
    }
}

/// A probe for when no real one is available: no processes, and the figures cannot be read.
/// The default in [`SystemSeams`] until the real probe is passed in.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoMemory;

impl MemoryProbe for NoMemory {
    fn processes(&self) -> Vec<ProcInfo> {
        Vec::new()
    }

    fn vm(&self) -> io::Result<VmStats> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "memory figures are not available",
        ))
    }
}

// ---------- Command runners ----------

/// How a [`ReplayRunner`] answers one command line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Replay {
    /// The captured output.
    Output(CommandOutput),
    /// An error instead of output.
    Error(ReplayError),
}

/// The errors a [`ReplayRunner`] can play back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayError {
    /// The program ran past its timeout.
    TimedOut,
    /// The program does not exist.
    NotFound,
    /// The runner refused the program.
    Refused,
}

impl ReplayError {
    fn to_io(self, line: &str) -> io::Error {
        match self {
            ReplayError::TimedOut => {
                io::Error::new(io::ErrorKind::TimedOut, format!("{line} timed out"))
            }
            ReplayError::NotFound => {
                io::Error::new(io::ErrorKind::NotFound, format!("{line}: not found"))
            }
            ReplayError::Refused => {
                io::Error::new(io::ErrorKind::PermissionDenied, format!("{line}: refused"))
            }
        }
    }
}

/// The fake command runner: replays captured outputs by command line and records every
/// call, so a test can prove which programs ran (and that `softwareupdate` did not).
///
/// A call is matched by its full [`command_line`] first, then by the same line with the
/// program's file name only (`fdesetup status`), so captures work whether or not the caller
/// uses full paths. A call with no reply fails with `NotFound`.
#[derive(Debug, Default)]
pub struct ReplayRunner {
    replies: Mutex<HashMap<String, Replay>>,
    calls: Mutex<Vec<String>>,
}

impl ReplayRunner {
    pub fn new() -> ReplayRunner {
        ReplayRunner::default()
    }

    /// Reads replies from a JSON object that maps command lines to outputs:
    /// `{"/usr/bin/fdesetup status": {"status": 0, "stdout": "FileVault is On.\n"},
    /// "softwareupdate --list": "timed_out"}`.
    pub fn from_file(file: &Path) -> io::Result<ReplayRunner> {
        let bytes = std::fs::read(file)?;
        let replies: HashMap<String, Replay> =
            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        Ok(ReplayRunner {
            replies: Mutex::new(replies),
            calls: Mutex::new(Vec::new()),
        })
    }

    /// Adds a reply for `line` (builder form).
    pub fn with(self, line: &str, output: CommandOutput) -> ReplayRunner {
        self.set(line, Replay::Output(output));
        self
    }

    /// Adds an error reply for `line` (builder form).
    pub fn with_error(self, line: &str, error: ReplayError) -> ReplayRunner {
        self.set(line, Replay::Error(error));
        self
    }

    /// Sets or replaces the reply for `line`.
    pub fn set(&self, line: &str, reply: Replay) {
        self.replies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(line.to_string(), reply);
    }

    /// Every command line run so far, in order.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Whether any call ran `program` (compared by file name).
    pub fn ran(&self, program: &str) -> bool {
        let want = file_name(program);
        self.calls()
            .iter()
            .any(|c| file_name(c.split(' ').next().unwrap_or("")) == want)
    }
}

impl CommandRunner for ReplayRunner {
    fn run(&self, program: &str, args: &[&str], _timeout: Duration) -> io::Result<CommandOutput> {
        let line = command_line(program, args);
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(line.clone());
        let short = command_line(file_name(program), args);
        let reply = {
            let replies = self.replies.lock().unwrap_or_else(|e| e.into_inner());
            replies.get(&line).or_else(|| replies.get(&short)).cloned()
        };
        match reply {
            Some(Replay::Output(out)) => Ok(out),
            Some(Replay::Error(e)) => Err(e.to_io(&line)),
            None => Err(ReplayError::NotFound.to_io(&line)),
        }
    }
}

/// The last part of a program path.
fn file_name(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// How long [`SystemRunner`] waits for the output pipes after the program ended (a
/// grandchild may still hold them).
const PIPE_GRACE: Duration = Duration::from_millis(500);

/// The real command runner: `std::process` with a timeout. The program runs in its own
/// process group with stdin closed; on timeout the runner kills that group (only processes
/// it started) and returns `TimedOut`.
///
/// It refuses `softwareupdate` when `TIDY_HOME` is set and in this crate's tests, as a second
/// guard behind the Security check's own "only when asked" rule.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunner;

/// The one `tmutil` call Tidy makes: list the local snapshots of the startup volume. It only
/// reads (design 17.2, C30).
pub const TMUTIL_LIST: [&str; 2] = ["listlocalsnapshots", "/"];

/// Whether [`SystemRunner`] refuses `program` with `args`:
///
/// - `softwareupdate` and `tmutil` under `TIDY_HOME` (or in tests), so tests and agents never
///   ask the real Mac;
/// - `tmutil` with anything but [`TMUTIL_LIST`], always: Tidy never changes snapshots;
/// - `docker`, always: Tidy only shows Docker commands for the user to copy (design section 4).
pub fn runner_refuses(program: &str, args: &[&str], tidy_home_set: bool) -> bool {
    let guarded = tidy_home_set || cfg!(test);
    match file_name(program) {
        "softwareupdate" => guarded,
        "tmutil" => guarded || args != TMUTIL_LIST,
        "docker" => true,
        _ => false,
    }
}

impl CommandRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> io::Result<CommandOutput> {
        let tidy_home_set = std::env::var_os(HOME_ENV).is_some_and(|v| !v.is_empty());
        if runner_refuses(program, args, tidy_home_set) {
            return Err(ReplayError::Refused.to_io(&command_line(program, args)));
        }
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let stdout = read_in_thread(child.stdout.take());
        let stderr = read_in_thread(child.stderr.take());
        // The group id is the child's pid (process_group(0)): only what this runner started.
        let pgid = libc::pid_t::try_from(child.id()).ok();
        let kill_group = || {
            if let Some(pgid) = pgid {
                // SAFETY: killpg on the process group this runner created for the child.
                unsafe {
                    libc::killpg(pgid, libc::SIGKILL);
                }
            }
        };
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                kill_group();
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "{} did not finish in {} s",
                        command_line(program, args),
                        timeout.as_secs_f32()
                    ),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut out = stdout.recv_timeout(PIPE_GRACE);
        let mut err = stderr.recv_timeout(PIPE_GRACE);
        if matches!(out, Err(RecvTimeoutError::Timeout))
            || matches!(err, Err(RecvTimeoutError::Timeout))
        {
            // Something the program started in the background still holds the pipes.
            kill_group();
            if matches!(out, Err(RecvTimeoutError::Timeout)) {
                out = stdout.recv_timeout(PIPE_GRACE);
            }
            if matches!(err, Err(RecvTimeoutError::Timeout)) {
                err = stderr.recv_timeout(PIPE_GRACE);
            }
        }
        Ok(CommandOutput {
            status: status.code(),
            stdout: out.unwrap_or_default(),
            stderr: err.unwrap_or_default(),
        })
    }
}

/// Reads a pipe to the end on its own thread, so a full stderr never blocks stdout.
fn read_in_thread(pipe: Option<impl Read + Send + 'static>) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
        });
    }
    rx
}

/// The production seams that live in their own modules (libc, Spotlight and mach FFI), so
/// the caller passes them to [`Env::from_env_with`]. The default is the conservative set:
/// [`UnknownProbe`], [`FileTimesOnly`], [`NoMemory`] and an empty [`ReplayRunner`] (every
/// command fails, so every Security check is gray). The app and tidyctl pass the real ones.
#[derive(Clone)]
pub struct SystemSeams {
    pub procs: Arc<dyn ProcessProbe>,
    pub last_used: Arc<dyn LastUsed>,
    pub memory: Arc<dyn MemoryProbe>,
    pub runner: Arc<dyn CommandRunner>,
}

impl Default for SystemSeams {
    fn default() -> SystemSeams {
        SystemSeams {
            procs: Arc::new(UnknownProbe),
            last_used: Arc::new(FileTimesOnly),
            memory: Arc::new(NoMemory),
            runner: Arc::new(ReplayRunner::new()),
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
    /// `TIDY_TEST_COMMANDS`: captured command outputs replacing the real runner.
    pub commands: Option<PathBuf>,
    /// `TIDY_TEST_MEMORY`: a fake process table replacing the real memory probe.
    pub memory: Option<PathBuf>,
    /// `TIDY_TEST_TZ`: the local time zone.
    pub tz: Option<String>,
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
                commands: None,
                memory: None,
                tz: None,
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
        let commands = var(TEST_COMMANDS_ENV).map(PathBuf::from);
        let memory = var(TEST_MEMORY_ENV).map(PathBuf::from);
        let tz = var(TEST_TZ_ENV).map(|v| v.to_string_lossy().into_owned());
        EnvSpec {
            tidy_home: Some(tidy_home),
            root,
            running,
            now,
            spotlight,
            commands,
            memory,
            tz,
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
    /// The process table and memory figures (M2).
    pub memory: Arc<dyn MemoryProbe>,
    /// Runs the Security checks' status commands (M2).
    pub runner: Arc<dyn CommandRunner>,
    /// The local time zone, for the reminder date: the system's, or `TIDY_TEST_TZ`, or UTC
    /// in [`Env::for_tests`].
    pub tz: TimeZone,
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
            .field("tz", &self.tz.iana_name())
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
        let memory: Arc<dyn MemoryProbe> = match &spec.memory {
            Some(file) => match FixedMemory::from_file(file) {
                Ok(m) => Arc::new(m),
                Err(e) => {
                    // The caller asked for a fake: an unreadable file gives an empty one,
                    // never the real probe.
                    tracing::warn!("{TEST_MEMORY_ENV}: cannot read {}: {e}", file.display());
                    Arc::new(FixedMemory::default())
                }
            },
            None => system.memory,
        };
        let runner: Arc<dyn CommandRunner> = match &spec.commands {
            Some(file) => match ReplayRunner::from_file(file) {
                Ok(r) => Arc::new(r),
                Err(e) => {
                    tracing::warn!("{TEST_COMMANDS_ENV}: cannot read {}: {e}", file.display());
                    Arc::new(ReplayRunner::new())
                }
            },
            None => system.runner,
        };
        let tz = match &spec.tz {
            Some(name) => parse_tz(name).unwrap_or_else(|| {
                tracing::warn!("{TEST_TZ_ENV}: unknown time zone {name:?}; using the system's");
                TimeZone::system()
            }),
            None => TimeZone::system(),
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
            memory,
            runner,
            tz,
            settings,
        }
    }

    /// The all-fake environment: `fake_home` as the scan root, Tidy's folders under
    /// `tidy_home`, a [`DirTrash`] in `tidy_home/data/trash`, nothing running, the clock at
    /// [`TEST_NOW`], file times only, an empty [`FixedMemory`], an empty [`ReplayRunner`],
    /// UTC and default settings. Reads and writes nothing.
    pub fn for_tests(fake_home: &Path, tidy_home: &Path) -> Env {
        let paths = AppPaths::under(tidy_home);
        Env {
            root: fake_home.to_path_buf(),
            trasher: Arc::new(DirTrash::new(paths.dir_trash())),
            paths,
            procs: Arc::new(FixedProbe::default()),
            clock: Arc::new(FixedClock::new(TEST_NOW)),
            last_used: Arc::new(FileTimesOnly),
            memory: Arc::new(FixedMemory::default()),
            runner: Arc::new(ReplayRunner::new()),
            tz: TimeZone::UTC,
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

    pub fn with_memory(mut self, memory: Arc<dyn MemoryProbe>) -> Env {
        self.memory = memory;
        self
    }

    pub fn with_runner(mut self, runner: Arc<dyn CommandRunner>) -> Env {
        self.runner = runner;
        self
    }

    pub fn with_tz(mut self, tz: TimeZone) -> Env {
        self.tz = tz;
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

/// A time zone from an IANA name (`America/Los_Angeles`, read from the system's zoneinfo)
/// or, failing that, a POSIX TZ string (`PST8PDT,M3.2.0,M11.1.0`).
pub fn parse_tz(name: &str) -> Option<TimeZone> {
    TimeZone::get(name).or_else(|_| TimeZone::posix(name)).ok()
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
    fn m2_hooks_count_only_under_tidy_home() {
        let spec = EnvSpec::from_vars(
            vars(&[
                (TEST_COMMANDS_ENV, "/tmp/c.json"),
                (TEST_MEMORY_ENV, "/tmp/m.json"),
                (TEST_TZ_ENV, "America/Los_Angeles"),
            ]),
            home(),
        );
        assert_eq!(spec.commands, None);
        assert_eq!(spec.memory, None);
        assert_eq!(spec.tz, None);
        assert_eq!(
            spec.ignored,
            [TEST_COMMANDS_ENV, TEST_MEMORY_ENV, TEST_TZ_ENV]
        );
        let spec = EnvSpec::from_vars(
            vars(&[
                (HOME_ENV, "/tmp/th"),
                (TEST_COMMANDS_ENV, "/tmp/c.json"),
                (TEST_MEMORY_ENV, "/tmp/m.json"),
                (TEST_TZ_ENV, "America/Los_Angeles"),
            ]),
            home(),
        );
        assert_eq!(spec.commands, Some(PathBuf::from("/tmp/c.json")));
        assert_eq!(spec.memory, Some(PathBuf::from("/tmp/m.json")));
        assert_eq!(spec.tz.as_deref(), Some("America/Los_Angeles"));
        assert!(spec.ignored.is_empty());
    }

    #[test]
    fn build_loads_the_m2_fakes_from_files() {
        let tmp = tempfile::tempdir().unwrap();
        let th = tmp.path().join("tidy");
        let commands = tmp.path().join("commands.json");
        std::fs::write(
            &commands,
            br#"{"/usr/bin/fdesetup status": {"status": 0, "stdout": "FileVault is On.\n"},
                 "softwareupdate --list": "timed_out"}"#,
        )
        .unwrap();
        let memory = tmp.path().join("memory.json");
        std::fs::write(
            &memory,
            br#"{"own_pid": 7, "processes": [{"pid": 7, "ppid": 1, "uid": 501, "name": "Tidy",
                 "footprint": 1000}], "vm": {"pressure_level": 2, "total_bytes": 8}}"#,
        )
        .unwrap();
        let spec = EnvSpec::from_vars(
            vars(&[
                (HOME_ENV, th.to_str().unwrap()),
                (TEST_COMMANDS_ENV, commands.to_str().unwrap()),
                (TEST_MEMORY_ENV, memory.to_str().unwrap()),
                (TEST_TZ_ENV, "PST8PDT,M3.2.0,M11.1.0"),
            ]),
            home(),
        );
        let env = Env::build(spec, SystemSeams::default());
        let out = env
            .runner
            .run("/usr/bin/fdesetup", &["status"], Duration::from_secs(1))
            .unwrap();
        assert_eq!(out, CommandOutput::ok("FileVault is On.\n"));
        let e = env
            .runner
            .run(
                "/usr/sbin/softwareupdate",
                &["--list"],
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert_eq!(env.memory.own_pid(), 7);
        assert_eq!(env.memory.processes()[0].footprint, Some(1000));
        assert_eq!(env.memory.vm().unwrap().pressure_level, 2);
        // A POSIX zone: 2026-07-01 is daylight time (UTC-7).
        let ts = jiff::Timestamp::from_second(1_782_907_200).unwrap();
        assert_eq!(env.tz.to_offset(ts).seconds(), -7 * 3600);

        // Unreadable files give empty fakes, never the real probe or runner.
        let spec = EnvSpec::from_vars(
            vars(&[
                (HOME_ENV, th.to_str().unwrap()),
                (TEST_COMMANDS_ENV, "/nonexistent/c.json"),
                (TEST_MEMORY_ENV, "/nonexistent/m.json"),
            ]),
            home(),
        );
        let env = Env::build(spec, SystemSeams::default());
        assert!(env.memory.processes().is_empty());
        assert_eq!(env.memory.own_pid(), 0);
        let e = env
            .runner
            .run("/bin/echo", &["hi"], Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(!th.exists());
    }

    #[test]
    fn default_seams_are_conservative() {
        let s = SystemSeams::default();
        assert!(s.memory.processes().is_empty());
        assert_eq!(
            s.memory.vm().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        let e = s
            .runner
            .run("/usr/bin/fdesetup", &["status"], Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        let tmp = tempfile::tempdir().unwrap();
        let env = Env::for_tests(&tmp.path().join("home"), &tmp.path().join("tidy"));
        assert!(env.memory.processes().is_empty());
        assert_eq!(env.tz, TimeZone::UTC);
    }

    #[test]
    fn replay_runner_replays_and_records() {
        let r = ReplayRunner::new()
            .with(
                "/usr/bin/fdesetup status",
                CommandOutput::ok("FileVault is On.\n"),
            )
            .with(
                "spctl --status",
                CommandOutput::exit(1, "assessments disabled\n", ""),
            )
            .with_error("/usr/bin/csrutil status", ReplayError::Refused);
        let t = Duration::from_secs(1);
        assert!(
            r.run("/usr/bin/fdesetup", &["status"], t)
                .unwrap()
                .success()
        );
        // Matched by the file name when the full line has no reply.
        let out = r.run("/usr/sbin/spctl", &["--status"], t).unwrap();
        assert_eq!(out.status, Some(1));
        assert!(!out.success());
        assert_eq!(
            r.run("/usr/bin/csrutil", &["status"], t)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            r.run("/usr/bin/fdesetup", &["list"], t).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            r.calls(),
            [
                "/usr/bin/fdesetup status",
                "/usr/sbin/spctl --status",
                "/usr/bin/csrutil status",
                "/usr/bin/fdesetup list",
            ]
        );
        assert!(r.ran("spctl"));
        assert!(r.ran("/usr/bin/fdesetup"));
        assert!(!r.ran("softwareupdate"));
    }

    #[test]
    fn fixed_memory_can_change_between_reads() {
        let m = FixedMemory::default();
        assert!(m.processes().is_empty());
        let p = ProcInfo {
            pid: 42,
            ppid: 1,
            uid: 501,
            name: "Safari".into(),
            path: Some("/Applications/Safari.app/Contents/MacOS/Safari".into()),
            responsible: Some(42),
            footprint: Some(3_000_000),
            top_footprint: None,
        };
        m.set_processes(vec![p.clone()]);
        m.set_vm(VmStats {
            pressure_level: 4,
            ..VmStats::default()
        });
        m.set_own_pid(9);
        assert_eq!(m.processes(), [p]);
        assert_eq!(m.vm().unwrap().pressure_level, 4);
        assert_eq!(m.own_pid(), 9);
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            command_line(
                "/usr/libexec/ApplicationFirewall/socketfilterfw",
                &["--getglobalstate"]
            ),
            "/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate"
        );
        assert_eq!(command_line("csrutil", &[]), "csrutil");
    }

    #[test]
    fn system_runner_refuses_softwareupdate_under_tidy_home_and_in_tests() {
        assert!(runner_refuses(
            "/usr/sbin/softwareupdate",
            &["--list"],
            true
        ));
        assert!(
            runner_refuses("softwareupdate", &[], false),
            "always in tests"
        );
        assert!(!runner_refuses("/usr/bin/fdesetup", &["status"], true));
        let e = SystemRunner
            .run(
                "/usr/sbin/softwareupdate",
                &["--list"],
                Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn system_runner_never_changes_snapshots_or_runs_docker() {
        // tmutil: only the listing, and never under TIDY_HOME or in tests.
        assert!(runner_refuses("/usr/bin/tmutil", &TMUTIL_LIST, true));
        assert!(
            runner_refuses("tmutil", &TMUTIL_LIST, false),
            "always in tests"
        );
        for args in [
            &["deletelocalsnapshots", "/"][..],
            &["thinlocalsnapshots", "/", "1", "4"],
            &["listlocalsnapshots", "/Volumes/Other"],
            &[],
        ] {
            assert!(runner_refuses("/usr/bin/tmutil", args, false), "{args:?}");
        }
        // docker: never, with any arguments.
        for args in [&["system", "df"][..], &["system", "prune"], &[]] {
            assert!(runner_refuses("/usr/local/bin/docker", args, false));
        }
        assert!(!runner_refuses("/usr/bin/csrutil", &["status"], false));
        // The guard is in the runner itself, before anything is spawned.
        let e = SystemRunner
            .run("/usr/bin/tmutil", &TMUTIL_LIST, Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        let e = SystemRunner
            .run("docker", &["system", "df"], Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    }

    /// Runs only programs it names itself (`/bin/echo`, `/bin/sh`, `/bin/sleep`) and stops
    /// only the processes it started.
    #[test]
    fn system_runner_collects_output_and_exit_codes() {
        let t = Duration::from_secs(10);
        let out = SystemRunner.run("/bin/echo", &["hello"], t).unwrap();
        assert_eq!(out, CommandOutput::ok("hello\n"));
        let out = SystemRunner
            .run("/bin/sh", &["-c", "echo out; echo err >&2; exit 3"], t)
            .unwrap();
        assert_eq!(out, CommandOutput::exit(3, "out\n", "err\n"));
        let e = SystemRunner
            .run("/nonexistent/tidy-test-program", &[], t)
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn system_runner_stops_a_program_at_the_timeout() {
        let start = Instant::now();
        let e = SystemRunner
            .run("/bin/sleep", &["30"], Duration::from_millis(200))
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(5));
        // A background child that keeps the pipes open does not hold the runner either.
        let start = Instant::now();
        let out = SystemRunner
            .run(
                "/bin/sh",
                &["-c", "/bin/sleep 30 & echo started"],
                Duration::from_secs(10),
            )
            .unwrap();
        assert!(out.success());
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn time_zones_parse_by_name_or_posix_string() {
        assert!(parse_tz("America/Los_Angeles").is_some());
        assert!(parse_tz("PST8PDT,M3.2.0,M11.1.0").is_some());
        assert!(parse_tz("Not/AZone").is_none());
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
