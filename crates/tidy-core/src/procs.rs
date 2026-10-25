//! The real process probe: `proc_listpids`, `proc_pidpath` and `proc_name` from libc (C20), so
//! the app and tidyctl see the same list without AppKit.
//!
//! For each process the set gets its executable name, its `proc_name`, and the bundle id from
//! the `Info.plist` of the outermost `.app` that holds the executable (so a Chrome helper
//! counts as Chrome). Two extras help rules that name a tool rather than an app:
//!
//! * A `node` or `bun` process also adds the name of the script it runs, so `npm` and `npx`
//!   (scripts run by node) are seen.
//! * An executable that lies inside the user's home folder also adds its full path, in
//!   lowercase. [`uses_path`] uses those to tell whether a version folder under `~/.local` is
//!   in use.
//!
//! When the process list cannot be read the probe says [`RunningSet::unknown`], so nothing
//! with an owner is cleaned on a guess.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::env::ProcessProbe;
use crate::model::RunningSet;
use crate::owners::bundle_id_of;

/// Reads the real process list.
#[derive(Clone, Debug, Default)]
pub struct LibcProbe {
    /// The home folder: only executables inside it add their path to the set.
    home: Option<PathBuf>,
}

impl LibcProbe {
    /// A probe that records executable paths inside the current user's home folder.
    pub fn new() -> LibcProbe {
        LibcProbe {
            home: dirs::home_dir(),
        }
    }

    /// A probe that records executable paths inside `home`.
    pub fn with_home(home: impl Into<PathBuf>) -> LibcProbe {
        LibcProbe {
            home: Some(home.into()),
        }
    }
}

impl ProcessProbe for LibcProbe {
    fn running(&self) -> RunningSet {
        let Some(pids) = list_pids() else {
            tracing::warn!("cannot read the process list");
            return RunningSet::unknown();
        };
        let mut set = RunningSet::default();
        let mut bundle_ids: HashMap<PathBuf, Option<String>> = HashMap::new();
        for pid in pids {
            if let Some(name) = pid_name(pid) {
                set.insert(&name);
            }
            let Some(exe) = pid_path(pid) else { continue };
            let exe = PathBuf::from(exe);
            let file = exe
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            set.insert(&file);
            if let Some(app) = outer_app(&exe) {
                let id = bundle_ids
                    .entry(app.clone())
                    .or_insert_with(|| bundle_id_of(&app));
                if let Some(id) = id {
                    set.insert(id);
                }
            }
            if self.home.as_ref().is_some_and(|h| exe.starts_with(h)) {
                set.insert(&exe.to_string_lossy());
            }
            if is_script_host(&file)
                && let Some(args) = pid_args(pid)
                && let Some(script) = script_name(&args)
            {
                set.insert(&script);
                if let Some(short) = script.strip_suffix("-cli") {
                    set.insert(short);
                }
            }
        }
        set
    }
}

/// The outermost `.app` folder in an executable path, if any: `/Applications/Google
/// Chrome.app` for a helper deep inside it.
pub fn outer_app(exe: &Path) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    for c in exe.components() {
        path.push(c);
        if matches!(c, std::path::Component::Normal(s)
            if s.to_string_lossy().to_ascii_lowercase().ends_with(".app"))
        {
            return Some(path);
        }
    }
    None
}

/// Whether a process with this executable name runs a script named in its arguments: Node and
/// Bun (npm, npx, tsserver), PHP (Composer), Ruby (gem, bundle), Python (Poetry, pip) and the
/// shells (pyenv, rbenv and their build plugins are shell scripts).
fn is_script_host(file: &str) -> bool {
    matches!(
        file,
        "node" | "bun" | "php" | "ruby" | "bash" | "sh" | "zsh"
    ) || file
        .strip_prefix("python")
        .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
        || file == "Python"
}

/// The base name of the script a `node` process runs, without its extension: the first
/// argument after the program that does not start with a dash.
pub fn script_name(args: &[String]) -> Option<String> {
    let arg = args.iter().skip(1).find(|a| !a.starts_with('-'))?;
    let file = Path::new(arg).file_name()?.to_string_lossy().into_owned();
    let stem = file.rsplit_once('.').map_or(file.as_str(), |(s, _)| s);
    (!stem.is_empty()).then(|| stem.to_string())
}

/// Whether a running process uses `path`: its executable is `path` or lies inside it. Only
/// executables inside the home folder are recorded (see the module docs). An unknown process
/// list counts as in use.
pub fn uses_path(running: &RunningSet, path: &Path) -> bool {
    if running.is_unknown() {
        return true;
    }
    let p = path.to_string_lossy().to_ascii_lowercase();
    let inside = format!("{}/", p.trim_end_matches('/'));
    running
        .ids()
        .any(|id| id.starts_with('/') && (id == p || id.starts_with(&inside)))
}

// ---------- Open files ----------

/// Reads which files the user's own processes hold open, with libproc and no extra rights:
/// `proc_listpids` for the processes of this user, `PROC_PIDLISTFDS` for each one's file
/// descriptors, and `PROC_PIDFDVNODEPATHINFO` for the path behind each file or folder
/// descriptor. Tidy's own process is left out, so a scan that is walking a folder does not
/// block its own clean.
///
/// It fails closed: when the process list cannot be read, or the descriptors of one of the
/// user's processes cannot be read for any reason other than that process having exited, the
/// whole read is an error and the caller skips the move.
#[derive(Clone, Copy, Debug, Default)]
pub struct LibcOpenFiles;

impl crate::env::OpenFilesProbe for LibcOpenFiles {
    fn open_files(&self) -> std::io::Result<crate::env::OpenFiles> {
        read_open_files()
    }
}

/// `PROC_UID_ONLY` in `sys/proc_info.h`: the processes whose effective user is `arg`.
const PROC_UID_ONLY: u32 = 4;
/// `PROC_PIDFDVNODEPATHINFO` in `sys/proc_info.h`.
const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

/// `struct proc_fileinfo` in `sys/proc_info.h`.
#[repr(C)]
#[derive(Clone, Copy)]
struct ProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: i64,
    fi_type: i32,
    fi_guardflags: u32,
}

/// `struct vnode_fdinfowithpath` in `sys/proc_info.h`.
#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: libc::vnode_info_path,
}

fn read_open_files() -> std::io::Result<crate::env::OpenFiles> {
    use std::io;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let pids =
        list_pids_of_user(uid).ok_or_else(|| io::Error::other("cannot read the process list"))?;
    let me = std::process::id() as i32;
    let mut files = Vec::new();
    for pid in pids {
        if pid == me {
            continue;
        }
        let fds = match pid_fds(pid) {
            Ok(fds) => fds,
            // The process ended between the two calls.
            Err(e) if e.raw_os_error() == Some(libc::ESRCH) => continue,
            Err(e) => {
                let name = pid_name(pid).unwrap_or_else(|| pid.to_string());
                return Err(io::Error::new(
                    e.kind(),
                    format!("cannot read the open files of {name}: {e}"),
                ));
            }
        };
        let mut name: Option<String> = None;
        for fd in fds {
            if fd.proc_fdtype != libc::PROX_FDTYPE_VNODE as u32 {
                continue;
            }
            let Some(path) = fd_path(pid, fd.proc_fd) else {
                // A descriptor closed between the calls, or one without a path.
                continue;
            };
            let holder = name
                .get_or_insert_with(|| pid_name(pid).unwrap_or_else(|| pid.to_string()))
                .clone();
            files.push(crate::env::OpenFile { path, pid, holder });
        }
    }
    Ok(crate::env::OpenFiles::new(files))
}

fn list_pids_of_user(uid: u32) -> Option<Vec<i32>> {
    // SAFETY: as in `list_pids`, with the user filter.
    unsafe {
        let need = libc::proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0);
        if need <= 0 {
            return None;
        }
        let cap = need as usize / size_of::<i32>() + 64;
        let mut pids = vec![0i32; cap];
        let got = libc::proc_listpids(
            PROC_UID_ONLY,
            uid,
            pids.as_mut_ptr().cast(),
            (cap * size_of::<i32>()) as libc::c_int,
        );
        if got <= 0 {
            return None;
        }
        pids.truncate(got as usize / size_of::<i32>());
        pids.retain(|p| *p > 0);
        Some(pids)
    }
}

/// The file descriptors of a process. ESRCH means it has exited.
fn pid_fds(pid: i32) -> std::io::Result<Vec<libc::proc_fdinfo>> {
    let entry = size_of::<libc::proc_fdinfo>();
    // SAFETY: a null buffer asks for the size; the second call writes at most `bytes` bytes
    // of `proc_fdinfo` entries and returns how many bytes it wrote.
    unsafe {
        let need = libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0);
        if need <= 0 {
            return Err(last_error_or_gone(pid));
        }
        // Room for descriptors opened between the two calls.
        let cap = need as usize / entry + 32;
        let mut fds = vec![
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0
            };
            cap
        ];
        let got = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            (cap * entry) as libc::c_int,
        );
        if got <= 0 {
            return Err(last_error_or_gone(pid));
        }
        fds.truncate(got as usize / entry);
        Ok(fds)
    }
}

/// The error of the last libproc call, or ESRCH when the process is no longer there (libproc
/// does not always set errno for that).
fn last_error_or_gone(pid: i32) -> std::io::Error {
    let e = std::io::Error::last_os_error();
    // SAFETY: kill with signal 0 only checks that the process exists; nothing is sent.
    let alive = unsafe { libc::kill(pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    if !alive {
        return std::io::Error::from_raw_os_error(libc::ESRCH);
    }
    match e.raw_os_error() {
        Some(0) | None => std::io::Error::from_raw_os_error(libc::EPERM),
        _ => e,
    }
}

/// The path of a file or folder descriptor, or None when it has none.
fn fd_path(pid: i32, fd: i32) -> Option<PathBuf> {
    let mut info = std::mem::MaybeUninit::<VnodeFdInfoWithPath>::zeroed();
    let size = size_of::<VnodeFdInfoWithPath>() as libc::c_int;
    // SAFETY: the buffer is `size` bytes of a zeroed `vnode_fdinfowithpath`; the call writes
    // at most that many bytes and returns how many.
    let got = unsafe {
        libc::proc_pidfdinfo(
            pid,
            fd,
            PROC_PIDFDVNODEPATHINFO,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if got != size {
        return None;
    }
    // SAFETY: the call filled the whole struct.
    let info = unsafe { info.assume_init() };
    let raw: Vec<u8> = info
        .pvip
        .vip_path
        .iter()
        .flatten()
        .map(|c| *c as u8)
        .take_while(|b| *b != 0)
        .collect();
    if raw.is_empty() {
        return None;
    }
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&raw)))
}

// ---------- libproc ----------

pub(crate) fn list_pids() -> Option<Vec<i32>> {
    const PROC_ALL_PIDS: u32 = 1;
    // SAFETY: proc_listpids with a null buffer returns the size needed; with a buffer it
    // writes at most `buffersize` bytes of i32 pids and returns the bytes written.
    unsafe {
        let need = libc::proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0);
        if need <= 0 {
            return None;
        }
        // Room for processes that start between the two calls.
        let cap = need as usize / size_of::<i32>() + 64;
        let mut pids = vec![0i32; cap];
        let got = libc::proc_listpids(
            PROC_ALL_PIDS,
            0,
            pids.as_mut_ptr().cast(),
            (cap * size_of::<i32>()) as libc::c_int,
        );
        if got <= 0 {
            return None;
        }
        pids.truncate(got as usize / size_of::<i32>());
        pids.retain(|p| *p > 0);
        Some(pids)
    }
}

pub(crate) fn pid_path(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is `buffersize` bytes; the call returns the length it wrote.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub(crate) fn pid_name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 64];
    // SAFETY: the buffer is `buffersize` bytes; the call returns the length it wrote.
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

/// The arguments of a process (`argv`), read with `sysctl(KERN_PROCARGS2)`. None when the
/// process is gone or not readable.
fn pid_args(pid: i32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a null buffer asks for the size; the second call gets at most `size` bytes.
    unsafe {
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
            || size == 0
        {
            return None;
        }
        let mut buf = vec![0u8; size];
        if libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        buf.truncate(size);
        parse_procargs(&buf)
    }
}

/// Parses the `KERN_PROCARGS2` buffer: an `i32` argument count, the executable path, padding
/// NULs, then `argc` NUL-terminated arguments.
pub fn parse_procargs(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
    if !(0..=4096).contains(&argc) {
        return None;
    }
    let mut rest = &buf[4..];
    let end = rest.iter().position(|b| *b == 0)?;
    rest = &rest[end..];
    while let Some((0, tail)) = rest.split_first().map(|(b, t)| (*b, t)) {
        rest = tail;
    }
    let mut args = Vec::new();
    for _ in 0..argc {
        let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
        args.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        if end >= rest.len() {
            break;
        }
        rest = &rest[end + 1..];
    }
    Some(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outer_app_is_the_first_app_folder() {
        assert_eq!(
            outer_app(Path::new(
                "/Applications/Google Chrome.app/Contents/Frameworks/X.framework/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper"
            )),
            Some(PathBuf::from("/Applications/Google Chrome.app"))
        );
        assert_eq!(outer_app(Path::new("/usr/bin/true")), None);
        assert_eq!(outer_app(Path::new("/Users/x/app.app.d/bin")), None);
    }

    #[test]
    fn procargs_parse_into_arguments() {
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/usr/local/bin/node\0\0\0");
        buf.extend_from_slice(b"node\0/opt/homebrew/bin/npm\0install\0PATH=/x\0");
        let args = parse_procargs(&buf).unwrap();
        assert_eq!(args, ["node", "/opt/homebrew/bin/npm", "install"]);
        assert_eq!(script_name(&args).as_deref(), Some("npm"));
        // Flags before the script are skipped, and an extension is dropped.
        let args = ["node", "--no-warnings", "/x/npm-cli.js"].map(String::from);
        assert_eq!(script_name(&args).as_deref(), Some("npm-cli"));
        assert_eq!(script_name(&["node".to_string()]), None);
        assert_eq!(parse_procargs(&[1, 2]), None);
        assert_eq!(parse_procargs(&(-1i32).to_ne_bytes()), None);
    }

    #[test]
    fn script_hosts_cover_the_package_tools() {
        for host in [
            "node",
            "bun",
            "php",
            "ruby",
            "bash",
            "sh",
            "zsh",
            "python",
            "python3",
            "python3.12",
            "Python",
        ] {
            assert!(is_script_host(host), "{host}");
        }
        for other in ["pythonw-ish", "rustc", "Google Chrome", "pythonista"] {
            assert!(!is_script_host(other), "{other}");
        }
        // The tool a host runs is its first argument that is not a flag.
        let gem = ["ruby", "/opt/homebrew/bin/gem", "install", "rails"].map(String::from);
        assert_eq!(script_name(&gem).as_deref(), Some("gem"));
        let pyenv = [
            "bash",
            "/opt/homebrew/Cellar/pyenv/2.4/libexec/pyenv",
            "install",
        ]
        .map(String::from);
        assert_eq!(script_name(&pyenv).as_deref(), Some("pyenv"));
        let composer = ["php", "/opt/homebrew/bin/composer", "install"].map(String::from);
        assert_eq!(script_name(&composer).as_deref(), Some("composer"));
    }

    #[test]
    fn uses_path_matches_the_path_and_what_is_inside_it() {
        let set = RunningSet::new([
            "node",
            "/Users/x/.local/share/claude/versions/2.1.50",
            "/users/x/.local/share/cursor-agent/versions/9/bin/agent",
        ]);
        let v = Path::new("/Users/x/.local/share/claude/versions");
        assert!(uses_path(&set, &v.join("2.1.50")));
        assert!(!uses_path(&set, &v.join("2.1.4")));
        assert!(!uses_path(&set, &v.join("2.1.5")));
        assert!(uses_path(
            &set,
            Path::new("/Users/x/.local/share/cursor-agent/versions/9")
        ));
        assert!(uses_path(&RunningSet::unknown(), &v.join("anything")));
        assert!(!uses_path(&RunningSet::default(), &v.join("2.1.50")));
    }

    /// Reads only the process list and the Info.plist files of apps that are running.
    #[test]
    fn the_real_probe_sees_this_test_process() {
        let probe = LibcProbe::new();
        let set = probe.running();
        assert!(!set.is_unknown());
        assert!(!set.is_empty());
        let exe = std::env::current_exe().unwrap();
        let name = exe
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        assert!(
            set.contains(&name),
            "{name} not in {:?}",
            set.ids().collect::<Vec<_>>()
        );
        // launchd is always running.
        assert!(set.contains("launchd"));
    }

    #[test]
    fn the_real_probe_sees_a_child_by_name_and_script() {
        // A child this test starts and stops by its own handle.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let set = LibcProbe::new().running();
        let seen = set.contains("sleep");
        let _ = child.kill();
        let _ = child.wait();
        assert!(seen);
    }

    #[test]
    fn home_paths_are_recorded_only_inside_home() {
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        let inside = LibcProbe::with_home(exe.parent().unwrap()).running();
        assert!(uses_path(&inside, &exe));
        let elsewhere = LibcProbe::with_home("/nonexistent-home").running();
        assert!(!uses_path(&elsewhere, &exe));
    }
    /// Reads the open files of this user's processes (read-only, no signal) and finds a file
    /// this test holds open in a child process it starts and stops itself.
    #[test]
    fn the_open_file_probe_sees_a_file_a_child_holds_open() {
        use crate::env::OpenFilesProbe;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap().join("cache");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("held.bin");
        std::fs::write(&file, b"x").unwrap();
        // `tail -f` keeps the file open until it is killed.
        let mut child = std::process::Command::new("/usr/bin/tail")
            .arg("-f")
            .arg(&file)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Give it a moment to open the file.
        let mut seen = None;
        for _ in 0..50 {
            match LibcOpenFiles.open_files() {
                Ok(open) => {
                    if let Some(f) = open.inside(&dir) {
                        seen = Some((f.pid, f.holder.clone()));
                        break;
                    }
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("the probe failed: {e}");
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let pid = child.id() as i32;
        let _ = child.kill();
        let _ = child.wait();
        let (holder_pid, holder) = seen.expect("the held file is listed");
        assert_eq!(holder_pid, pid);
        assert_eq!(holder, "tail");
        // Once the child is gone, nothing holds it.
        let after = LibcOpenFiles.open_files().unwrap();
        assert!(after.inside(&dir).is_none());
        assert!(!after.is_empty(), "other processes hold files too");
    }

    #[test]
    fn the_vnode_path_struct_has_the_kernel_size() {
        // sizeof(struct vnode_fdinfowithpath) in sys/proc_info.h.
        assert_eq!(size_of::<VnodeFdInfoWithPath>(), 1200);
    }
}
