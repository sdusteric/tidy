//! The memory engine (M2, design section 5.3): the real [`MemoryProbe`] and the grouping of a
//! process table into the rows of "Top apps by memory".
//!
//! [`LibcMemory`] reads, and only reads:
//!
//! * `sysctlbyname` for `kern.memorystatus_vm_pressure_level`, `vm.swapusage`, `hw.memsize`
//!   and `hw.pagesize`;
//! * `host_statistics64(HOST_VM_INFO64)` for the page counts `vm_stat` prints;
//! * `proc_listpids`, `proc_pidpath`, `proc_name`, `proc_pidinfo(PROC_PIDTBSDINFO)` for the
//!   parent pid and owner, and `proc_pid_rusage(RUSAGE_INFO_V4)` for `ri_phys_footprint`;
//! * `responsibility_get_pid_responsible_for_pid`, looked up with `dlsym` because it is a
//!   private symbol. When it is missing, grouping uses the parent chain and the path;
//! * `/usr/bin/top -l 1 -stats pid,mem`, for the processes macOS does not let an app read
//!   (see below).
//!
//! Nothing here signals a process, except a `top` it started itself that hangs past its time
//! limit, which it stops by its own handle. Quitting is the app's job (`NSRunningApplication`).
//!
//! [`group`] is pure: it takes the process table and gives the rows, so tests run it on a fake
//! table. The order it tries for each process is:
//!
//! 1. the responsible process, when it is in the table and runs from an `.app`;
//! 2. the process's own outermost `.app` (a bundle's main executable is its own app, even when
//!    a terminal started it);
//! 3. the parent chain up to launchd, the first ancestor that runs from an `.app`;
//! 4. otherwise the process stands alone and the row shows its name.
//!
//! Processes whose footprint cannot be read (root-owned, or another user's) are gathered into
//! one [`SYSTEM_PROCESSES`] row with no Quit. macOS refuses an app `proc_pid_rusage`,
//! `PROC_PIDTASKINFO`, `PROC_PIDTASKALLINFO` and `memorystatus_control` for those processes
//! with EPERM, and an estimate from the `vm_stat` page counts does not work (footprints count
//! shared and graphics memory, so the readable footprints alone add up to more than the
//! anonymous and compressed pages; measured on the development Mac, 2026-10-24). `/usr/bin/top`
//! ships with macOS setuid root, so [`LibcMemory`] runs it, read-only, for their sizes: one
//! `top -l 1 -stats pid,mem` sample, the same MEM figure `top` and Activity Monitor show
//! ([`ProcInfo::top_footprint`]). A sample costs about a second of CPU, so it is taken at most
//! every [`TOP_REFRESH`], in the background after the first one. The System row sums those
//! sizes and names its processes, WindowServer first ([`AppMemory::members`]). When `top`
//! gives nothing the row has no size and still names them.

use std::collections::{HashMap, HashSet};
use std::ffi::{CString, c_void};
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::env::{Env, MemoryProbe};
use crate::model::{
    AppMemory, MemoryKind, MemorySnapshot, PageStats, ProcInfo, SYSTEM_PROCESSES, SwapUsage,
    VmStats,
};
use crate::owners::bundle_id_of;
use crate::procs::{list_pids, outer_app, pid_name, pid_path};

/// How many rows the list shows before the rest are summed into "Other".
pub const TOP_ROWS: usize = 15;

/// The id and name of the row that sums everything below the top [`TOP_ROWS`].
pub const OTHER_ID: &str = "other";
pub const OTHER_NAME: &str = "Other";

/// The id of the System processes row.
pub const SYSTEM_ID: &str = "system";

/// Processes that never get a Quit button besides the ones on [`crate::model::NEVER_QUIT`].
const ALSO_NEVER_QUIT: &[&str] = &["kernel_task"];

/// The System row names these first when they are in it: the large macOS processes that
/// `top` and Activity Monitor show near the top and that Tidy cannot measure itself.
pub const SYSTEM_NAMED_FIRST: &[&str] = &["WindowServer", "kernel_task"];

/// The longest a `top` sample may be reused for the System row's sizes.
pub const TOP_REFRESH: Duration = Duration::from_secs(20);

/// How long one `top` sample may take before Tidy stops it.
const TOP_TIMEOUT: Duration = Duration::from_secs(5);

// ---------- Snapshots ----------

/// Reads the probe once and builds the snapshot: the figures, and the rows grouped by app.
/// Errors when the system-wide figures cannot be read (the fake [`crate::env::NoMemory`]
/// always errors).
pub fn snapshot(probe: &dyn MemoryProbe, at: u64) -> io::Result<MemorySnapshot> {
    let vm = probe.vm()?;
    let mut snap = MemorySnapshot::from_vm(at, &vm);
    snap.apps = group(&probe.processes(), probe.own_pid(), &mut bundle_id_of);
    Ok(snap)
}

/// [`snapshot`] with the environment's probe and clock.
pub fn take(env: &Env) -> io::Result<MemorySnapshot> {
    snapshot(env.memory.as_ref(), env.now())
}

// ---------- Clear file cache ----------

/// The one script Tidy runs with administrator rights (design section 21): macOS's own
/// `purge`, behind macOS's own password prompt. [`crate::env::SystemRunner`] runs `osascript`
/// with nothing else, and never under `TIDY_HOME` or in tests.
pub const PURGE_SCRIPT: &str = "do shell script \"/usr/sbin/purge\" with prompt \"Tidy wants to clear the file cache.\" with administrator privileges";
/// `osascript`, which shows the password prompt and runs [`PURGE_SCRIPT`].
pub const OSASCRIPT: &str = "/usr/bin/osascript";
/// How long the prompt may stay open before Tidy gives up on it.
pub const PURGE_TIMEOUT: Duration = Duration::from_secs(180);
/// What the Memory screen says when the purge did not run for another reason than a cancel.
pub const FILE_CACHE_FAILED: &str = "macOS didn't clear the file cache.";

/// How a Clear File Cache ended.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum FileCacheResult {
    /// `purge` ran. The file cache (file-backed pages) before and right after, in bytes.
    Cleared {
        before: u64,
        after: u64,
    },
    /// The password prompt was canceled; nothing ran.
    Canceled,
    Failed {
        message: String,
    },
}

/// The file cache in `vm`: file-backed pages times the page size.
pub fn file_cache_bytes(vm: &VmStats) -> u64 {
    vm.pages.external.saturating_mul(vm.pages.page_size)
}

/// Runs `purge` behind macOS's password prompt, and reads the file cache before and after.
/// Only ever on a click and a confirm; nothing else in Tidy runs as root.
pub fn clear_file_cache(env: &Env) -> FileCacheResult {
    let read = || env.memory.vm().ok().map(|vm| file_cache_bytes(&vm));
    let before = read();
    match env
        .runner
        .run(OSASCRIPT, &["-e", PURGE_SCRIPT], PURGE_TIMEOUT)
    {
        Ok(out) if out.success() => FileCacheResult::Cleared {
            before: before.unwrap_or(0),
            after: read().unwrap_or(0),
        },
        Ok(out) if out.stderr.contains("(-128)") => FileCacheResult::Canceled,
        Ok(out) => {
            tracing::warn!("purge failed: {}", out.stderr.trim());
            FileCacheResult::Failed {
                message: FILE_CACHE_FAILED.to_string(),
            }
        }
        Err(e) => {
            tracing::warn!("purge did not run: {e}");
            FileCacheResult::Failed {
                message: FILE_CACHE_FAILED.to_string(),
            }
        }
    }
}

// ---------- Grouping ----------

/// Groups a process table into rows, largest footprint first: up to [`TOP_ROWS`] app or
/// process rows, then one "Other" row summing the rest (when there is a rest). The System
/// processes row (when any process was unreadable) takes its place among the top rows by the
/// size `top` gave it, and comes last when it has no size.
///
/// `own_pid` is Tidy's own pid: the row that holds it has no Quit. `bundle_id` reads the
/// bundle id of an `.app` folder; it is called only for the rows that are kept.
pub fn group(
    procs: &[ProcInfo],
    own_pid: i32,
    bundle_id: &mut dyn FnMut(&Path) -> Option<String>,
) -> Vec<AppMemory> {
    let by_pid: HashMap<i32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();

    // Unreadable processes go to the System row whatever app they belong to: only `top` has a
    // size for them, and they cannot be quit.
    let mut system: Vec<&ProcInfo> = Vec::new();
    let mut apps: HashMap<PathBuf, Vec<&ProcInfo>> = HashMap::new();
    let mut singles: Vec<&ProcInfo> = Vec::new();
    for p in procs {
        if p.footprint.is_none() {
            system.push(p);
            continue;
        }
        match owning_app(p, &by_pid) {
            Some(app) => apps.entry(app).or_default().push(p),
            None => singles.push(p),
        }
    }

    let mut rows: Vec<AppMemory> = Vec::new();
    for (app, members) in apps {
        rows.push(app_row(&app, &members, own_pid));
    }
    for p in singles {
        rows.push(AppMemory {
            id: format!("pid:{}", p.pid),
            name: p.name.clone(),
            kind: MemoryKind::Process,
            pid: p.pid,
            pids: vec![p.pid],
            bundle_id: None,
            app_path: None,
            footprint: p.footprint.unwrap_or(0),
            helpers: 0,
            // Quit is a polite terminate for an app; a bare process has no such thing.
            quittable: false,
            members: Vec::new(),
        });
    }
    // Largest first; equal sizes by name and id so the order is stable between refreshes.
    rows.sort_by(|a, b| {
        b.footprint
            .cmp(&a.footprint)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });

    let rest = rows.split_off(rows.len().min(TOP_ROWS));
    for row in &mut rows {
        if row.kind == MemoryKind::App
            && let Some(app) = &row.app_path
        {
            row.bundle_id = bundle_id(app);
            row.quittable = row.quittable
                && !row.bundle_id.as_deref().is_some_and(protected)
                && !row.bundle_id.as_deref().is_some_and(AppMemory::never_quit);
        }
    }
    if !rest.is_empty() {
        let mut pids: Vec<i32> = rest.iter().flat_map(|r| r.pids.iter().copied()).collect();
        pids.sort_unstable();
        rows.push(AppMemory {
            id: OTHER_ID.to_string(),
            name: OTHER_NAME.to_string(),
            kind: MemoryKind::Process,
            pid: 0,
            helpers: u32::try_from(pids.len()).unwrap_or(u32::MAX),
            pids,
            bundle_id: None,
            app_path: None,
            footprint: rest.iter().map(|r| r.footprint).sum(),
            quittable: false,
            members: Vec::new(),
        });
    }
    if !system.is_empty() {
        let members = system_members(&system);
        let mut pids: Vec<i32> = system.iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        // macOS does not let an app read these footprints (see the module docs); the sizes
        // are the ones `top` reported, and 0 when it gave none.
        let footprint: u64 = system.iter().filter_map(|p| p.top_footprint).sum();
        let row = AppMemory {
            id: SYSTEM_ID.to_string(),
            name: SYSTEM_PROCESSES.to_string(),
            kind: MemoryKind::System,
            pid: 0,
            helpers: u32::try_from(pids.len()).unwrap_or(u32::MAX),
            pids,
            bundle_id: None,
            app_path: None,
            footprint,
            quittable: false,
            members,
        };
        // Among the top rows by size (never inside "Other"); last when it has no size.
        let at = if footprint == 0 {
            rows.len()
        } else {
            rows.iter()
                .position(|r| r.id == OTHER_ID || r.footprint < footprint)
                .unwrap_or(rows.len())
        };
        rows.insert(at, row);
    }
    rows
}

/// The distinct names in the System row: [`SYSTEM_NAMED_FIRST`] in that order, then the rest
/// largest first (a name's size is the sum `top` gave its processes), then by name.
fn system_members(procs: &[&ProcInfo]) -> Vec<String> {
    let mut sizes: HashMap<&str, u64> = HashMap::new();
    for p in procs {
        *sizes.entry(p.name.as_str()).or_default() += p.top_footprint.unwrap_or(0);
    }
    let mut names: Vec<(&str, u64)> = sizes.into_iter().collect();
    let rank = |n: &str| {
        SYSTEM_NAMED_FIRST
            .iter()
            .position(|k| k.eq_ignore_ascii_case(n))
            .unwrap_or(SYSTEM_NAMED_FIRST.len())
    };
    names.sort_by(|(a, sa), (b, sb)| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| sb.cmp(sa))
            .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
            .then_with(|| a.cmp(b))
    });
    names.into_iter().map(|(n, _)| n.to_string()).collect()
}

/// The row for one `.app` and the processes grouped under it.
fn app_row(app: &Path, members: &[&ProcInfo], own_pid: i32) -> AppMemory {
    let main = main_process(app, members);
    let mut pids: Vec<i32> = members
        .iter()
        .map(|p| p.pid)
        .filter(|pid| *pid != main)
        .collect();
    pids.sort_unstable();
    pids.insert(0, main);
    let name = app
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.to_string_lossy().into_owned());
    AppMemory {
        id: format!("app:{}", app.display()),
        quittable: !protected(&name) && !AppMemory::never_quit(&name) && !pids.contains(&own_pid),
        name,
        kind: MemoryKind::App,
        pid: main,
        helpers: u32::try_from(pids.len() - 1).unwrap_or(u32::MAX),
        pids,
        bundle_id: None,
        app_path: Some(app.to_path_buf()),
        footprint: members.iter().filter_map(|p| p.footprint).sum(),
        members: Vec::new(),
    }
}

/// Names on top of [`crate::model::NEVER_QUIT`].
fn protected(name_or_id: &str) -> bool {
    ALSO_NEVER_QUIT
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name_or_id))
}

/// The process Quit is sent to: one that runs from `Contents/MacOS` of the app (lowest pid
/// first), else the lowest pid.
fn main_process(app: &Path, members: &[&ProcInfo]) -> i32 {
    let macos = app.join("Contents/MacOS");
    members
        .iter()
        .filter(|p| p.path.as_deref().is_some_and(|x| x.starts_with(&macos)))
        .map(|p| p.pid)
        .min()
        .or_else(|| members.iter().map(|p| p.pid).min())
        .unwrap_or(0)
}

/// The `.app` a process belongs to, by the order in the module docs.
fn owning_app(p: &ProcInfo, by_pid: &HashMap<i32, &ProcInfo>) -> Option<PathBuf> {
    // 1. The process macOS holds responsible for it.
    if let Some(r) = p.responsible
        && r > 0
        && r != p.pid
        && let Some(app) = by_pid.get(&r).and_then(|q| app_of(q))
    {
        return Some(app);
    }
    // 2. Its own bundle.
    if let Some(app) = app_of(p) {
        return Some(app);
    }
    // 3. The parent chain, up to launchd.
    let mut seen: HashSet<i32> = HashSet::from([p.pid]);
    let mut next = p.ppid;
    while next > 1 && seen.insert(next) {
        let Some(parent) = by_pid.get(&next) else {
            break;
        };
        if let Some(app) = app_of(parent) {
            return Some(app);
        }
        next = parent.ppid;
    }
    None
}

fn app_of(p: &ProcInfo) -> Option<PathBuf> {
    p.path.as_deref().and_then(outer_app)
}

// ---------- The real probe ----------

/// The production [`MemoryProbe`]: libc, mach and sysctl, all read-only.
#[derive(Clone, Copy, Debug, Default)]
pub struct LibcMemory;

impl LibcMemory {
    pub fn new() -> LibcMemory {
        LibcMemory
    }
}

impl MemoryProbe for LibcMemory {
    fn processes(&self) -> Vec<ProcInfo> {
        let Some(pids) = list_pids() else {
            tracing::warn!("cannot read the process list");
            return Vec::new();
        };
        let responsible = responsible_fn();
        let mut procs: Vec<ProcInfo> = pids
            .into_iter()
            .filter_map(|pid| read_proc(pid, responsible))
            .collect();
        if procs.iter().any(|p| p.footprint.is_none()) {
            let sizes = top_sizes();
            for p in procs.iter_mut().filter(|p| p.footprint.is_none()) {
                p.top_footprint = sizes.get(&p.pid).copied();
            }
        }
        procs
    }

    fn vm(&self) -> io::Result<VmStats> {
        let level = sysctl_u32(c"kern.memorystatus_vm_pressure_level")?;
        let total_bytes = sysctl_u64(c"hw.memsize")?;
        Ok(VmStats {
            pressure_level: level,
            total_bytes,
            swap: swap_usage()?,
            pages: page_stats()?,
        })
    }
}

type ResponsibleFn = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;

/// `responsibility_get_pid_responsible_for_pid`, or None when this macOS does not have it.
fn responsible_fn() -> Option<ResponsibleFn> {
    static F: OnceLock<Option<ResponsibleFn>> = OnceLock::new();
    *F.get_or_init(|| {
        let name = CString::new("responsibility_get_pid_responsible_for_pid").ok()?;
        // SAFETY: dlsym only looks a name up in the loaded images.
        let sym = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        if sym.is_null() {
            tracing::debug!("responsibility_get_pid_responsible_for_pid is not available");
            return None;
        }
        // SAFETY: the symbol has this C signature (int responsibility_get_pid_responsible_for_pid(pid_t)).
        Some(unsafe { std::mem::transmute::<*mut c_void, ResponsibleFn>(sym) })
    })
}

/// Reads one process. None when it has gone, or when libproc refuses everything about it.
fn read_proc(pid: i32, responsible: Option<ResponsibleFn>) -> Option<ProcInfo> {
    let (ppid, uid, comm) = bsd_info(pid)?;
    let footprint = footprint(pid);
    if footprint == Footprint::Gone {
        return None;
    }
    let path = pid_path(pid).map(PathBuf::from);
    let name = pid_name(pid)
        .or_else(|| {
            path.as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        })
        .or(comm)
        .unwrap_or_else(|| pid.to_string());
    let responsible = responsible.and_then(|f| {
        // SAFETY: a plain function of a pid with no pointers; -1 means failure.
        let r = unsafe { f(pid) };
        (r > 0).then_some(r)
    });
    Some(ProcInfo {
        pid,
        ppid,
        responsible,
        uid,
        name,
        path,
        footprint: match footprint {
            Footprint::Bytes(b) => Some(b),
            _ => None,
        },
        top_footprint: None,
    })
}

// ---------- top, for the processes an app cannot read ----------

/// The last `top` sample: pid to MEM in bytes.
#[derive(Default)]
struct TopCache {
    read_at: Option<Instant>,
    sizes: Arc<HashMap<i32, u64>>,
    running: bool,
}

fn top_cache() -> &'static Mutex<TopCache> {
    static CACHE: OnceLock<Mutex<TopCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(TopCache::default()))
}

/// The sizes `top` reports, by pid. The first call waits for a sample; later calls answer
/// from the last one at once and, when it is older than [`TOP_REFRESH`], start a new sample
/// in the background. A sample that fails keeps the sizes from before.
fn top_sizes() -> Arc<HashMap<i32, u64>> {
    let mut cache = top_cache().lock().unwrap_or_else(PoisonError::into_inner);
    if cache.running {
        return cache.sizes.clone();
    }
    match cache.read_at {
        None => {
            cache.running = true;
            drop(cache);
            store_top(read_top());
            top_cache()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .sizes
                .clone()
        }
        Some(at) if at.elapsed() >= TOP_REFRESH => {
            cache.running = true;
            let sizes = cache.sizes.clone();
            drop(cache);
            let spawned = std::thread::Builder::new()
                .name("tidy-top".to_string())
                .spawn(|| store_top(read_top()));
            if spawned.is_err() {
                store_top(None);
            }
            sizes
        }
        Some(_) => cache.sizes.clone(),
    }
}

fn store_top(sizes: Option<HashMap<i32, u64>>) {
    let mut cache = top_cache().lock().unwrap_or_else(PoisonError::into_inner);
    cache.read_at = Some(Instant::now());
    cache.running = false;
    if let Some(sizes) = sizes {
        cache.sizes = Arc::new(sizes);
    }
}

/// Runs one `top -l 1 -stats pid,mem` sample and reads it. None when `top` is missing, fails,
/// or takes longer than [`TOP_TIMEOUT`] (it is then stopped by its own pid).
fn read_top() -> Option<HashMap<i32, u64>> {
    let mut child = Command::new("/usr/bin/top")
        .args(["-l", "1", "-s", "0", "-F", "-stats", "pid,mem"])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| tracing::warn!("cannot run top: {e}"))
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).map(|_| text)
    });
    let deadline = Instant::now() + TOP_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) | Err(_) => {
                tracing::warn!("top took longer than {TOP_TIMEOUT:?}; stopping it");
                // Our own child, stopped by its handle.
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let text = reader.join().ok()?.ok()?;
    if !status?.success() {
        tracing::warn!("top failed");
        return None;
    }
    let sizes = parse_top(&text);
    (!sizes.is_empty()).then_some(sizes)
}

/// Reads the process lines of a `top -l 1 -stats pid,mem` sample: the lines after the
/// `PID  MEM` header, each a pid and a size such as `1495M`, `9776K` or `1.2G+`.
pub fn parse_top(text: &str) -> HashMap<i32, u64> {
    let mut sizes = HashMap::new();
    let mut lines = text.lines();
    let header = lines.by_ref().find(|l| {
        let mut words = l.split_whitespace();
        words.next() == Some("PID") && words.next() == Some("MEM")
    });
    if header.is_none() {
        return sizes;
    }
    for line in lines {
        let mut words = line.split_whitespace();
        let (Some(pid), Some(mem)) = (words.next(), words.next()) else {
            continue;
        };
        if let (Ok(pid), Some(bytes)) = (pid.parse::<i32>(), parse_top_size(mem)) {
            sizes.insert(pid, bytes);
        }
    }
    sizes
}

/// A `top` size: a number, a unit (B, K, M, G, T, P; powers of 1024) and, in delta mode, a
/// trailing `+` or `-`.
fn parse_top_size(s: &str) -> Option<u64> {
    let s = s.trim_end_matches(['+', '-']);
    let unit = s.chars().last()?;
    let shift = match unit {
        'B' => 0,
        'K' => 10,
        'M' => 20,
        'G' => 30,
        'T' => 40,
        'P' => 50,
        _ => return None,
    };
    let number: f64 = s[..s.len() - 1].parse().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    // A positive, finite size; `as` saturates at u64::MAX.
    Some((number * (1u64 << shift) as f64).round() as u64)
}

#[derive(PartialEq, Eq)]
enum Footprint {
    Bytes(u64),
    /// The process refused (EPERM): a root-owned or other-user process.
    Denied,
    /// The process exited between the listing and now.
    Gone,
}

fn footprint(pid: i32) -> Footprint {
    // SAFETY: rusage_info_v4 is plain data; proc_pid_rusage fills it for RUSAGE_INFO_V4.
    unsafe {
        let mut info: libc::rusage_info_v4 = std::mem::zeroed();
        let rc = libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            (&raw mut info).cast::<libc::rusage_info_t>(),
        );
        if rc == 0 {
            return Footprint::Bytes(info.ri_phys_footprint);
        }
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Footprint::Gone,
        _ => Footprint::Denied,
    }
}

/// The parent pid, owner and short command name of a process, from `PROC_PIDTBSDINFO`.
///
/// macOS refuses `PROC_PIDTBSDINFO` for another user's process (WindowServer runs as
/// `_windowserver`, uid 88) but answers `PROC_PIDT_SHORTBSDINFO`, which has the same three
/// fields, so that is the fallback. A process that refuses both is kept with ppid 0 and
/// uid 0 only while it still exists.
fn bsd_info(pid: i32) -> Option<(i32, u32, Option<String>)> {
    // SAFETY: proc_bsdinfo is plain data; proc_pidinfo writes at most `buffersize` bytes.
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            size,
        );
        if n == size {
            return Some((
                i32::try_from(info.pbi_ppid).unwrap_or(0),
                info.pbi_uid,
                comm_string(&info.pbi_comm),
            ));
        }
    }
    if io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return None;
    }
    if let Some(short) = short_bsd_info(pid) {
        return Some(short);
    }
    // No details: keep the process only if it still exists.
    (io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)).then_some((0, 0, None))
}

/// [`bsd_info`] from `PROC_PIDT_SHORTBSDINFO`, which macOS answers for every user's process.
fn short_bsd_info(pid: i32) -> Option<(i32, u32, Option<String>)> {
    // SAFETY: proc_bsdshortinfo is plain data; proc_pidinfo writes at most `buffersize` bytes.
    unsafe {
        let mut info: libc::proc_bsdshortinfo = std::mem::zeroed();
        let size = size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        let n = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            size,
        );
        (n == size).then(|| {
            (
                i32::try_from(info.pbsi_ppid).unwrap_or(0),
                info.pbsi_uid,
                comm_string(&info.pbsi_comm),
            )
        })
    }
}

/// A NUL-terminated `comm` field, or None when it is empty.
fn comm_string(comm: &[libc::c_char]) -> Option<String> {
    let bytes: Vec<u8> = comm
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

// ---------- sysctl and mach ----------

fn sysctl_raw(name: &std::ffi::CStr, out: *mut c_void, len: &mut usize) -> io::Result<()> {
    // SAFETY: `out` points to `*len` writable bytes (the callers pass a local of that size).
    let rc = unsafe { libc::sysctlbyname(name.as_ptr(), out, len, std::ptr::null_mut(), 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn sysctl_u32(name: &std::ffi::CStr) -> io::Result<u32> {
    let mut v: u32 = 0;
    let mut len = size_of::<u32>();
    sysctl_raw(name, (&raw mut v).cast(), &mut len)?;
    Ok(v)
}

fn sysctl_u64(name: &std::ffi::CStr) -> io::Result<u64> {
    let mut v: u64 = 0;
    let mut len = size_of::<u64>();
    sysctl_raw(name, (&raw mut v).cast(), &mut len)?;
    Ok(v)
}

/// `vm.swapusage`, the numbers `sysctl vm.swapusage` prints.
fn swap_usage() -> io::Result<SwapUsage> {
    // SAFETY: xsw_usage is plain data.
    let mut x: libc::xsw_usage = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::xsw_usage>();
    sysctl_raw(c"vm.swapusage", (&raw mut x).cast(), &mut len)?;
    Ok(SwapUsage {
        total: x.xsu_total,
        used: x.xsu_used,
        free: x.xsu_avail,
    })
}

/// `host_statistics64(HOST_VM_INFO64)` and the page size, laid out like `vm_stat`.
fn page_stats() -> io::Result<PageStats> {
    let page_size = sysctl_u64(c"hw.pagesize")?;
    // SAFETY: vm_statistics64 is plain data; the count tells the kernel how many 32-bit words
    // fit, and it writes no more. libc marks mach_host_self deprecated in favor of the mach2
    // crate; one call does not justify a new dependency.
    #[allow(deprecated)]
    let st = unsafe {
        let mut st: libc::vm_statistics64 = std::mem::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        let rc = libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            (&raw mut st).cast::<i32>(),
            &mut count,
        );
        if rc != 0 {
            return Err(io::Error::other(format!("host_statistics64 failed: {rc}")));
        }
        st
    };
    let n = |v: u32| u64::from(v);
    Ok(PageStats {
        page_size,
        // vm_stat prints free pages without the speculative ones.
        free: n(st.free_count).saturating_sub(n(st.speculative_count)),
        active: n(st.active_count),
        inactive: n(st.inactive_count),
        speculative: n(st.speculative_count),
        wired: n(st.wire_count),
        purgeable: n(st.purgeable_count),
        external: n(st.external_page_count),
        internal: n(st.internal_page_count),
        compressor: n(st.compressor_page_count),
        compressed: st.total_uncompressed_pages_in_compressor,
        swapins: st.swapins,
        swapouts: st.swapouts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: i32, ppid: i32, name: &str, path: Option<&str>, fp: Option<u64>) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            responsible: None,
            uid: 501,
            name: name.to_string(),
            path: path.map(PathBuf::from),
            footprint: fp,
            top_footprint: None,
        }
    }

    #[test]
    fn main_process_prefers_contents_macos() {
        let app = Path::new("/Applications/Foo.app");
        let a = p(
            5,
            1,
            "helper",
            Some("/Applications/Foo.app/Contents/Frameworks/H"),
            Some(1),
        );
        let b = p(
            9,
            1,
            "Foo",
            Some("/Applications/Foo.app/Contents/MacOS/Foo"),
            Some(1),
        );
        assert_eq!(main_process(app, &[&a, &b]), 9);
        assert_eq!(main_process(app, &[&a]), 5);
    }

    #[test]
    fn parent_loops_do_not_hang() {
        let a = p(10, 11, "a", Some("/usr/bin/a"), Some(1));
        let b = p(11, 10, "b", Some("/usr/bin/b"), Some(1));
        let rows = group(&[a, b], 1, &mut |_| None);
        assert_eq!(rows.len(), 2);
    }

    /// Reads the real process table (read-only). For a process Tidy cannot measure, the
    /// owner and parent come from `PROC_PIDT_SHORTBSDINFO` instead of being taken as uid 0 and
    /// ppid 0. WindowServer, when it runs, is owned by its own user, not root.
    #[test]
    fn unreadable_processes_keep_their_owner_and_parent() {
        let me = unsafe { libc::getuid() };
        let procs = LibcMemory.processes();
        assert!(!procs.is_empty());
        for p in procs.iter().filter(|p| p.footprint.is_none()) {
            let Some((ppid, uid, _)) = short_bsd_info(p.pid) else {
                continue; // it exited since
            };
            if p.pid > 1 {
                assert_eq!((p.ppid, p.uid), (ppid, uid), "{p:?}");
                assert!(p.ppid > 0, "{p:?}");
            }
        }
        if let Some(ws) = procs.iter().find(|p| p.name == "WindowServer") {
            assert_ne!(ws.uid, 0, "{ws:?}");
            if ws.uid != me {
                assert_eq!(ws.footprint, None, "{ws:?}");
            }
        }
        // Tidy's own process is readable and owned by this user.
        let own = std::process::id() as i32;
        let mine = procs.iter().find(|p| p.pid == own).expect("own process");
        assert_eq!(mine.uid, me);
        assert!(mine.footprint.is_some());
    }
}
