//! The memory engine: grouping on fake process tables, and one read-only look at the real Mac.
//!
//! Nothing here signals a process. The only processes the tests start are a `/bin/sleep`
//! child, which they stop by its own handle, and the read-only `/usr/bin/top` samples the
//! real probe takes.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tidy_core::env::{FixedMemory, MemoryProbe, NoMemory};
use tidy_core::memory::{LibcMemory, OTHER_ID, SYSTEM_ID, TOP_ROWS, group, parse_top, snapshot};
use tidy_core::model::{
    AppMemory, MemoryKind, Pressure, ProcInfo, SYSTEM_PROCESSES, SwapUsage, TIDY_BUNDLE_ID, VmStats,
};

const GB: u64 = 1 << 30;
const MB: u64 = 1 << 20;

fn proc(pid: i32, ppid: i32, name: &str, path: Option<&str>, mb: Option<u64>) -> ProcInfo {
    ProcInfo {
        pid,
        ppid,
        responsible: None,
        uid: 501,
        name: name.to_string(),
        path: path.map(PathBuf::from),
        footprint: mb.map(|m| m * MB),
        top_footprint: None,
    }
}

fn responsible(mut p: ProcInfo, r: i32) -> ProcInfo {
    p.responsible = Some(r);
    p
}

/// Bundle ids for the fake apps, by folder name.
fn bundle(app: &Path) -> Option<String> {
    let name = app.file_stem()?.to_string_lossy().to_lowercase();
    Some(match name.as_str() {
        "tidy" => TIDY_BUNDLE_ID.to_string(),
        "finder" => "com.apple.finder".to_string(),
        other => format!("test.{}", other.replace(' ', "-")),
    })
}

fn run(procs: &[ProcInfo], own: i32) -> Vec<AppMemory> {
    group(procs, own, &mut bundle)
}

fn row<'a>(rows: &'a [AppMemory], name: &str) -> &'a AppMemory {
    rows.iter().find(|r| r.name == name).unwrap_or_else(|| {
        panic!(
            "no row {name}: {:?}",
            rows.iter().map(|r| &r.name).collect::<Vec<_>>()
        )
    })
}

const CHROME: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const CHROME_HELPER: &str = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/1/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper";

#[test]
fn helpers_are_grouped_under_the_responsible_app() {
    // A WebKit style helper outside any .app, reached only through the responsible pid.
    let procs = [
        proc(
            100,
            1,
            "Safari",
            Some("/Applications/Safari.app/Contents/MacOS/Safari"),
            Some(600),
        ),
        responsible(
            proc(
                101,
                1,
                "com.apple.WebKit.WebContent",
                Some(
                    "/System/Library/Frameworks/WebKit.framework/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent",
                ),
                Some(400),
            ),
            100,
        ),
        responsible(
            proc(
                102,
                1,
                "com.apple.WebKit.Networking",
                Some(
                    "/System/Library/Frameworks/WebKit.framework/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking",
                ),
                Some(100),
            ),
            100,
        ),
    ];
    let rows = run(&procs, 1);
    assert_eq!(rows.len(), 1);
    let safari = &rows[0];
    assert_eq!(safari.name, "Safari");
    assert_eq!(safari.kind, MemoryKind::App);
    assert_eq!(safari.footprint, 1100 * MB);
    assert_eq!(safari.pid, 100);
    assert_eq!(safari.pids, [100, 101, 102]);
    assert_eq!(safari.helpers, 2);
    assert_eq!(safari.id, "app:/Applications/Safari.app");
    assert_eq!(safari.bundle_id.as_deref(), Some("test.safari"));
    assert!(safari.quittable);
}

#[test]
fn the_responsible_pid_wins_over_the_helpers_own_app() {
    // A helper that lives in another bundle but that macOS holds Chrome responsible for.
    let procs = [
        proc(10, 1, "Google Chrome", Some(CHROME), Some(300)),
        responsible(
            proc(
                11,
                1,
                "Other Helper",
                Some("/Applications/Other.app/Contents/MacOS/Other Helper"),
                Some(50),
            ),
            10,
        ),
    ];
    let rows = run(&procs, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "Google Chrome");
    assert_eq!(rows[0].helpers, 1);
}

#[test]
fn the_parent_chain_is_the_fallback_without_a_responsible_pid() {
    // node under a shell-less helper chain: 30 -> 20 -> 10 (the app). 30 and 20 run from
    // /usr, so only the chain reaches the app.
    let procs = [
        proc(
            10,
            1,
            "Cursor",
            Some("/Applications/Cursor.app/Contents/MacOS/Cursor"),
            Some(500),
        ),
        proc(20, 10, "node", Some("/usr/local/bin/node"), Some(200)),
        proc(30, 20, "esbuild", None, Some(40)),
        // A process under launchd with no app anywhere stands alone.
        proc(
            40,
            1,
            "postgres",
            Some("/opt/homebrew/bin/postgres"),
            Some(80),
        ),
    ];
    let rows = run(&procs, 1);
    let cursor = row(&rows, "Cursor");
    assert_eq!(cursor.pids, [10, 20, 30]);
    assert_eq!(cursor.footprint, 740 * MB);
    let pg = row(&rows, "postgres");
    assert_eq!(pg.kind, MemoryKind::Process);
    assert_eq!(pg.id, "pid:40");
    assert_eq!(pg.helpers, 0);
    assert_eq!(rows.len(), 2);
}

#[test]
fn the_app_path_is_the_last_fallback() {
    // Parent is launchd and there is no responsible pid: the .app prefix of the path groups
    // Chrome's helpers, including a nested helper app (the outermost .app counts).
    let procs = [
        proc(10, 1, "Google Chrome", Some(CHROME), Some(300)),
        proc(
            11,
            1,
            "Google Chrome Helper",
            Some(CHROME_HELPER),
            Some(150),
        ),
        proc(
            12,
            1,
            "Google Chrome Helper",
            Some(CHROME_HELPER),
            Some(150),
        ),
    ];
    let rows = run(&procs, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "Google Chrome");
    assert_eq!(rows[0].pid, 10);
    assert_eq!(rows[0].pids, [10, 11, 12]);
    assert_eq!(rows[0].helpers, 2);
    assert_eq!(rows[0].footprint, 600 * MB);
    assert_eq!(
        rows[0].app_path.as_deref(),
        Some(Path::new("/Applications/Google Chrome.app"))
    );
}

#[test]
fn an_app_started_from_a_terminal_is_still_its_own_row_without_responsibility() {
    let procs = [
        proc(5, 1, "zsh", Some("/bin/zsh"), Some(5)),
        proc(
            6,
            5,
            "Foo",
            Some("/Applications/Foo.app/Contents/MacOS/Foo"),
            Some(90),
        ),
    ];
    let rows = run(&procs, 1);
    assert_eq!(row(&rows, "Foo").pids, [6]);
    assert_eq!(row(&rows, "zsh").kind, MemoryKind::Process);
}

#[test]
fn a_responsible_pid_that_is_gone_or_not_an_app_falls_back() {
    let procs = [
        proc(
            10,
            1,
            "Foo",
            Some("/Applications/Foo.app/Contents/MacOS/Foo"),
            Some(100),
        ),
        // Responsible pid 999 is not in the table; the parent chain finds Foo.
        responsible(proc(11, 10, "tool", Some("/usr/bin/tool"), Some(10)), 999),
        // Responsible is a plain daemon: not an app, so the parent chain decides.
        proc(50, 1, "daemon", Some("/usr/sbin/daemon"), Some(7)),
        responsible(proc(51, 10, "child", Some("/usr/bin/child"), Some(3)), 50),
        // Responsible is itself: ignored.
        responsible(proc(60, 1, "self", Some("/usr/bin/self"), Some(2)), 60),
    ];
    let rows = run(&procs, 1);
    assert_eq!(row(&rows, "Foo").pids, [10, 11, 51]);
    assert_eq!(row(&rows, "daemon").pids, [50]);
    assert_eq!(row(&rows, "self").pids, [60]);
}

#[test]
fn unreadable_processes_are_summed_into_one_system_row() {
    let procs = [
        proc(
            10,
            1,
            "Foo",
            Some("/Applications/Foo.app/Contents/MacOS/Foo"),
            Some(100),
        ),
        // Root-owned: no footprint. One is even a Foo helper by responsibility; it still goes
        // to the System row because Tidy has no size for it.
        proc(1, 0, "launchd", Some("/sbin/launchd"), None),
        proc(
            90,
            1,
            "mds",
            Some("/System/Library/Frameworks/CoreServices.framework/mds"),
            None,
        ),
        responsible(proc(91, 1, "rootHelper", None, None), 10),
    ];
    let rows = run(&procs, 1);
    assert_eq!(rows.len(), 2);
    let sys = row(&rows, SYSTEM_PROCESSES);
    assert_eq!(sys.id, SYSTEM_ID);
    assert_eq!(sys.kind, MemoryKind::System);
    assert_eq!(sys.pids, [1, 90, 91]);
    assert_eq!(sys.helpers, 3);
    assert_eq!(sys.footprint, 0);
    assert_eq!(sys.pid, 0);
    assert!(!sys.quittable);
    assert_eq!(row(&rows, "Foo").pids, [10]);
    // The System row comes last.
    assert_eq!(rows.last().unwrap().id, SYSTEM_ID);
    // It names what it holds, by name.
    assert_eq!(sys.members, ["launchd", "mds", "rootHelper"]);
}

/// WindowServer runs as another user, so macOS gives Tidy no size for it (see the memory
/// module docs). It is often the largest process `top` shows, so the System row names it
/// first, then kernel_task, then the rest by name, each name once.
#[test]
fn the_system_row_names_windowserver_first() {
    let procs = [
        proc(1, 0, "launchd", Some("/sbin/launchd"), None),
        proc(400, 1, "mds_stores", None, None),
        proc(
            622,
            1,
            "WindowServer",
            Some("/System/Library/PrivateFrameworks/SkyLight.framework/Resources/WindowServer"),
            None,
        ),
        proc(0, 0, "kernel_task", None, None),
        proc(401, 1, "mds_stores", None, None),
        proc(
            10,
            1,
            "Foo",
            Some("/Applications/Foo.app/Contents/MacOS/Foo"),
            Some(100),
        ),
    ];
    let rows = run(&procs, 1);
    let sys = row(&rows, SYSTEM_PROCESSES);
    assert_eq!(
        sys.members,
        ["WindowServer", "kernel_task", "launchd", "mds_stores"]
    );
    assert_eq!(sys.pids, [0, 1, 400, 401, 622]);
    assert_eq!(sys.helpers, 5);
    assert!(!sys.quittable);
    // The other rows carry no names.
    assert!(row(&rows, "Foo").members.is_empty());
}

fn sized(mut p: ProcInfo, top_mb: u64) -> ProcInfo {
    p.top_footprint = Some(top_mb * MB);
    p
}

/// `top` is setuid root, so it reads what an app cannot: the System row sums the sizes it
/// gave, takes its place among the rows by that size (never inside "Other"), and names the
/// largest processes inside first after WindowServer and kernel_task.
#[test]
fn the_system_row_sums_the_sizes_top_gave() {
    let ws = "/System/Library/PrivateFrameworks/SkyLight.framework/Resources/WindowServer";
    let procs = [
        proc(
            10,
            1,
            "Foo",
            Some("/Applications/Foo.app/Contents/MacOS/Foo"),
            Some(3000),
        ),
        proc(
            20,
            1,
            "Bar",
            Some("/Applications/Bar.app/Contents/MacOS/Bar"),
            Some(100),
        ),
        sized(proc(622, 1, "WindowServer", Some(ws), None), 1500),
        sized(proc(1, 0, "launchd", Some("/sbin/launchd"), None), 30),
        sized(proc(400, 1, "mds_stores", None, None), 200),
        sized(proc(401, 1, "mds_stores", None, None), 100),
        sized(proc(0, 0, "kernel_task", None, None), 70),
        // `top` did not list this one: it counts for nothing.
        proc(402, 1, "coreaudiod", None, None),
    ];
    let rows = run(&procs, 1);
    let sys = row(&rows, SYSTEM_PROCESSES);
    assert_eq!(sys.footprint, (1500 + 30 + 200 + 100 + 70) * MB);
    assert_eq!(sys.pids, [0, 1, 400, 401, 402, 622]);
    assert!(!sys.quittable);
    // Between Foo (3000 MB) and Bar (100 MB).
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Foo", SYSTEM_PROCESSES, "Bar"]);
    // WindowServer and kernel_task first, then mds_stores (300 MB in all) before launchd and
    // coreaudiod (no size), each name once.
    assert_eq!(
        sys.members,
        [
            "WindowServer",
            "kernel_task",
            "mds_stores",
            "launchd",
            "coreaudiod"
        ]
    );

    // With more rows than fit, the System row stays above "Other", whatever its size.
    let mut many: Vec<ProcInfo> = (0..20)
        .map(|i| {
            proc(
                100 + i,
                1,
                &format!("Tool{i}"),
                Some(&format!("/usr/local/bin/tool{i}")),
                Some(500 + i as u64),
            )
        })
        .collect();
    many.push(sized(proc(622, 1, "WindowServer", Some(ws), None), 1));
    let rows = run(&many, 1);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids.len(), TOP_ROWS + 2);
    assert_eq!(ids[TOP_ROWS], SYSTEM_ID);
    assert_eq!(ids[TOP_ROWS + 1], OTHER_ID);
    assert!(!row(&rows, "Other").pids.contains(&622));
}

/// A `top -l 1 -s 0 -F -stats pid,mem` sample captured on the development Mac (2026-10-24),
/// plus the other unit letters and the delta-mode suffixes `top` prints.
#[test]
fn top_samples_parse() {
    let text = "Processes: 880 total, 3 running, 1 stuck, 876 sleeping, 7523 threads \n\
                2026/10/24 02:06:43\n\
                Load Avg: 3.54, 3.67, 3.78 \n\
                CPU usage: 9.44% user, 8.3% sys, 82.52% idle \n\
                MemRegions: 1402100 total, 16G resident, 0B private, 3860M shared.\n\
                PhysMem: 45G used (6168M wired, 8724M compressor), 1916M unused.\n\
                VM: 469T vsize, 0B framework vsize, 10594879(0) swapins, 14292371(0) swapouts.\n\
                Networks: packets: 448613707/581G in, 179079166/205G out.\n\
                Disks: 513776204/15T read, 188395843/4049G written.\n\
                \n\
                PID  MEM  \n\
                622  1495M\n\
                1    33M  \n\
                0    69M  \n\
                77   9776K\n\
                78   1.5G+\n\
                79   512B-\n\
                80   ?\n";
    let sizes = parse_top(text);
    assert_eq!(sizes.get(&622), Some(&(1495 * MB)));
    assert_eq!(sizes.get(&1), Some(&(33 * MB)));
    assert_eq!(sizes.get(&0), Some(&(69 * MB)));
    assert_eq!(sizes.get(&77), Some(&(9776 * 1024)));
    assert_eq!(sizes.get(&78), Some(&(3 * GB / 2)));
    assert_eq!(sizes.get(&79), Some(&512));
    assert_eq!(sizes.get(&80), None);
    assert_eq!(sizes.len(), 6);
    // The header lines above PID MEM are never read as processes, and no header means nothing.
    assert!(parse_top("PhysMem: 45G used\n622  1495M\n").is_empty());
    assert!(parse_top("").is_empty());
}

#[test]
fn tidy_finder_windowserver_loginwindow_and_kernel_task_have_no_quit() {
    let procs = [
        proc(
            2,
            1,
            "Tidy",
            Some("/Applications/Tidy.app/Contents/MacOS/tidy"),
            Some(90),
        ),
        proc(
            3,
            1,
            "Finder",
            Some("/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder"),
            Some(300),
        ),
        proc(
            4,
            1,
            "WindowServer",
            Some("/System/Library/PrivateFrameworks/SkyLight.framework/Resources/WindowServer"),
            Some(500),
        ),
        proc(
            5,
            1,
            "loginwindow",
            Some("/System/Library/CoreServices/loginwindow.app/Contents/MacOS/loginwindow"),
            Some(40),
        ),
        proc(6, 1, "kernel_task", Some("/kernel"), Some(900)),
        proc(
            7,
            1,
            "Notes",
            Some("/System/Applications/Notes.app/Contents/MacOS/Notes"),
            Some(60),
        ),
    ];
    // Tidy is found by its bundle id here; its pid is not 2 for the app that reads it.
    let rows = run(&procs, 4242);
    for name in [
        "Tidy",
        "Finder",
        "WindowServer",
        "loginwindow",
        "kernel_task",
    ] {
        assert!(!row(&rows, name).quittable, "{name} must have no Quit");
    }
    assert!(row(&rows, "Notes").quittable);
}

#[test]
fn the_row_that_holds_tidys_own_pid_has_no_quit() {
    let procs = [
        proc(
            77,
            1,
            "Foo",
            Some("/Applications/Foo.app/Contents/MacOS/Foo"),
            Some(90),
        ),
        proc(
            78,
            1,
            "Bar",
            Some("/Applications/Bar.app/Contents/MacOS/Bar"),
            Some(80),
        ),
    ];
    let rows = run(&procs, 77);
    assert!(!row(&rows, "Foo").quittable);
    assert!(row(&rows, "Bar").quittable);
}

#[test]
fn a_process_without_an_app_has_no_quit() {
    let procs = [proc(40, 1, "node", Some("/usr/local/bin/node"), Some(80))];
    let rows = run(&procs, 1);
    assert!(!rows[0].quittable);
}

#[test]
fn rows_are_sorted_by_footprint_with_the_top_fifteen_and_other() {
    let mut procs = Vec::new();
    for i in 0..20u64 {
        procs.push(proc(
            100 + i as i32,
            1,
            &format!("Tool{i}"),
            Some(&format!("/usr/local/bin/tool{i}")),
            Some(10 + i),
        ));
    }
    procs.push(proc(1, 0, "launchd", Some("/sbin/launchd"), None));
    let rows = run(&procs, 1);
    // 15 + Other + System.
    assert_eq!(rows.len(), TOP_ROWS + 2);
    let sizes: Vec<u64> = rows[..TOP_ROWS].iter().map(|r| r.footprint).collect();
    let mut sorted = sizes.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(sizes, sorted);
    assert_eq!(rows[0].name, "Tool19");
    let other = &rows[TOP_ROWS];
    assert_eq!(other.id, OTHER_ID);
    assert_eq!(other.name, "Other");
    assert_eq!(other.helpers, 5);
    // The five smallest: 10..=14 MB.
    assert_eq!(other.footprint, (10 + 11 + 12 + 13 + 14) * MB);
    assert!(!other.quittable);
    assert_eq!(rows.last().unwrap().id, SYSTEM_ID);
}

#[test]
fn no_other_row_when_everything_fits() {
    let procs = [proc(40, 1, "node", Some("/usr/local/bin/node"), Some(80))];
    let rows = run(&procs, 1);
    assert!(rows.iter().all(|r| r.id != OTHER_ID));
    assert_eq!(rows.len(), 1);
}

#[test]
fn bundle_ids_are_read_only_for_the_rows_that_are_kept() {
    let mut procs = Vec::new();
    for i in 0..30 {
        procs.push(proc(
            100 + i,
            1,
            "x",
            Some(&format!("/Applications/App{i:02}.app/Contents/MacOS/x")),
            Some(10 + i as u64),
        ));
    }
    let mut asked = 0;
    let rows = group(&procs, 1, &mut |_| {
        asked += 1;
        None
    });
    assert_eq!(asked, TOP_ROWS);
    assert_eq!(rows.len(), TOP_ROWS + 1);
}

#[test]
fn a_cyclic_or_missing_parent_chain_ends() {
    let procs = [
        proc(10, 11, "a", Some("/usr/bin/a"), Some(1)),
        proc(11, 10, "b", Some("/usr/bin/b"), Some(1)),
        proc(12, 4000, "c", Some("/usr/bin/c"), Some(1)),
    ];
    assert_eq!(run(&procs, 1).len(), 3);
}

#[test]
fn snapshot_carries_the_figures_the_verdict_and_the_rows() {
    let fake = FixedMemory::default();
    fake.set_vm(VmStats {
        pressure_level: 2,
        total_bytes: 16 * GB,
        swap: SwapUsage {
            total: 4 * GB,
            used: 2 * GB,
            free: 2 * GB,
        },
        pages: tidy_core::model::PageStats {
            page_size: 16384,
            compressor: 100_000,
            ..Default::default()
        },
    });
    fake.set_processes(vec![proc(
        40,
        1,
        "node",
        Some("/usr/local/bin/node"),
        Some(80),
    )]);
    fake.set_own_pid(40);
    let snap = snapshot(&fake, 1234).unwrap();
    assert_eq!(snap.at, 1234);
    assert_eq!(snap.pressure, Pressure::Warning);
    assert_eq!(
        snap.verdict(),
        "Memory is tight. Quitting a large app below will help."
    );
    assert_eq!(snap.swap.used, 2 * GB);
    assert_eq!(snap.compressed_bytes, 100_000 * 16384);
    assert_eq!(snap.apps.len(), 1);
}

#[test]
fn snapshot_errors_when_the_figures_cannot_be_read() {
    let err = snapshot(&NoMemory, 1).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

// ---------- The real Mac, read-only ----------

#[test]
fn the_real_probe_reads_sane_numbers() {
    // A child this test starts and stops by its own handle, so there is a known process.
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let child_pid = child.id() as i32;

    let probe: Arc<dyn MemoryProbe> = Arc::new(LibcMemory::new());
    let started = Instant::now();
    let snap = snapshot(probe.as_ref(), 1);
    let procs = probe.processes();
    let _ = child.kill();
    let _ = child.wait();
    let took = started.elapsed();
    let snap = snap.unwrap();

    assert!(took.as_secs() < 3, "took {took:?}");
    assert!(snap.total_bytes >= 2 * GB, "{}", snap.total_bytes);
    assert!(
        matches!(snap.pressure_level, 1 | 2 | 4),
        "{}",
        snap.pressure_level
    );
    assert!(matches!(snap.pages.page_size, 4096 | 16384));
    assert!(snap.pages.wired > 0 && snap.pages.active > 0);
    assert!(snap.compressed_bytes < snap.total_bytes);
    assert_eq!(
        snap.compressed_bytes,
        snap.pages.compressor * snap.pages.page_size
    );
    assert!(snap.swap.used <= snap.swap.total || snap.swap.total == 0);

    // This process and the sleep child are readable and have a footprint.
    let me = procs
        .iter()
        .find(|p| p.pid == std::process::id() as i32)
        .expect("this process is listed");
    assert!(me.footprint.unwrap_or(0) > MB);
    assert_eq!(me.uid, unsafe { libc::getuid() });
    let sleep = procs
        .iter()
        .find(|p| p.pid == child_pid)
        .expect("child listed");
    assert_eq!(sleep.ppid, std::process::id() as i32);
    assert_eq!(sleep.name, "sleep");
    assert!(sleep.footprint.is_some());
    // launchd is listed and unreadable by a normal user, so its size comes from `top`.
    let launchd = procs.iter().find(|p| p.pid == 1).expect("launchd listed");
    assert_eq!(launchd.footprint, None);
    assert!(launchd.top_footprint.unwrap_or(0) > 0, "{launchd:?}");
    // WindowServer runs as another user: `top` gives it a size too.
    if let Some(ws) = procs.iter().find(|p| p.name == "WindowServer") {
        assert_eq!(ws.footprint, None, "{ws:?}");
        assert!(ws.top_footprint.unwrap_or(0) > 10 * MB, "{ws:?}");
    }
    // A readable process never carries a `top` size.
    assert!(
        procs
            .iter()
            .all(|p| p.footprint.is_none() || p.top_footprint.is_none())
    );

    // Rows: sorted, the System row is not quittable and has a size, and the largest row is
    // real.
    assert!(!snap.apps.is_empty());
    assert!(snap.apps.len() <= TOP_ROWS + 2);
    assert!(snap.apps[0].footprint > 0);
    let readable: Vec<&AppMemory> = snap
        .apps
        .iter()
        .filter(|a| a.kind != MemoryKind::System && a.id != OTHER_ID)
        .collect();
    assert!(
        readable
            .windows(2)
            .all(|w| w[0].footprint >= w[1].footprint)
    );
    let sys = snap.apps.iter().find(|a| a.kind == MemoryKind::System);
    if let Some(sys) = sys {
        assert!(!sys.quittable);
        assert!(sys.pids.contains(&1));
        assert!(sys.footprint > 0, "{sys:?}");
    }
    // Every row's pids are real processes from the same table.
    for a in &snap.apps {
        assert!(!a.quittable || a.kind == MemoryKind::App);
    }
}

// ---------- Clear file cache ----------

/// A memory probe whose file cache shrinks by 2 GB after the first read, as `purge` would.
struct Shrinking(std::sync::atomic::AtomicU32);

impl MemoryProbe for Shrinking {
    fn processes(&self) -> Vec<ProcInfo> {
        Vec::new()
    }
    fn vm(&self) -> io::Result<tidy_core::model::VmStats> {
        let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut vm = tidy_core::model::VmStats::default();
        vm.pages.page_size = 16_384;
        let gb = 1_000_000_000 / 16_384;
        vm.pages.external = if n == 0 { 5 * gb } else { 3 * gb };
        Ok(vm)
    }
}

fn purge_env(reply: tidy_core::env::Replay) -> (tempfile::TempDir, tidy_core::Env) {
    use tidy_core::env::{ReplayRunner, command_line};
    let dir = tempfile::tempdir().unwrap();
    let runner = ReplayRunner::new();
    runner.set(
        &command_line(
            "/usr/bin/osascript",
            &["-e", tidy_core::memory::PURGE_SCRIPT],
        ),
        reply,
    );
    let env = tidy_core::Env::for_tests(&dir.path().join("home"), &dir.path().join("tidy"))
        .with_runner(Arc::new(runner))
        .with_memory(Arc::new(Shrinking(Default::default())));
    (dir, env)
}

#[test]
fn clearing_the_file_cache_reports_what_left() {
    use tidy_core::env::{CommandOutput, Replay};
    use tidy_core::memory::{FileCacheResult, clear_file_cache};
    let (_dir, env) = purge_env(Replay::Output(CommandOutput::ok("")));
    match clear_file_cache(&env) {
        FileCacheResult::Cleared { before, after } => {
            assert!(before > after, "{before} {after}");
            assert_eq!(before - after, 2_000_000_000 / 16_384 * 16_384);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_canceled_password_prompt_clears_nothing() {
    use tidy_core::env::{CommandOutput, Replay};
    use tidy_core::memory::{FileCacheResult, clear_file_cache};
    let (_dir, env) = purge_env(Replay::Output(CommandOutput::exit(
        1,
        "",
        "execution error: User canceled. (-128)",
    )));
    assert_eq!(clear_file_cache(&env), FileCacheResult::Canceled);
    let (_dir, env) = purge_env(Replay::Output(CommandOutput::exit(1, "", "purge: oops")));
    assert!(matches!(
        clear_file_cache(&env),
        FileCacheResult::Failed { .. }
    ));
}
