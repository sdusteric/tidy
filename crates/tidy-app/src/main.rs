//! Tidy application entry point: wiring only. The UI lives in the `tidy_app` library and the
//! engines in `tidy_core`.
//!
//! `tidy --self-test` prints a JSON report and exits without creating any GPUI object, window or
//! Dock icon. It never scans the real home folder.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tidy_core::env::{SystemRunner, SystemSeams};
use tidy_core::lastused::Spotlight;
use tidy_core::memory::LibcMemory;
use tidy_core::procs::{LibcOpenFiles, LibcProbe};
use tidy_core::walk::{WalkOpts, measure, measure_reference};
use tidy_core::{AppPaths, Env, InstanceLock, LargeScan, LockError, Settings};
use tracing_subscriber::EnvFilter;

fn main() {
    if std::env::args().skip(1).any(|a| a == "--self-test") {
        std::process::exit(self_test());
    }
    run_app();
}

// ---------- --self-test ----------

/// True when the current user may write to `dir`, or to the nearest existing ancestor when `dir`
/// does not exist yet (the app creates it at startup). Checks permissions only; nothing is
/// created.
fn writable(dir: &Path) -> bool {
    let mut probe: Option<&Path> = Some(dir);
    while let Some(p) = probe {
        if let Ok(meta) = std::fs::metadata(p) {
            if !meta.is_dir() {
                return false;
            }
            let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else {
                return false;
            };
            // SAFETY: `c` is a valid NUL-terminated path.
            return unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) } == 0;
        }
        probe = p.parent().filter(|p| !p.as_os_str().is_empty());
    }
    false
}

/// Builds a small tree in a fresh folder under the system temp dir, measures it with the
/// production walker and the `std::fs` reference walker, and removes exactly the paths it
/// created. Returns the report and whether both walkers agree with the expected totals.
fn walker_check() -> (serde_json::Value, bool) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("tidy-selftest-{}-{nanos}", std::process::id()));
    let sub = dir.join("sub");
    let (a, b, link, sym) = (
        dir.join("a.bin"),
        sub.join("b.bin"),
        sub.join("b-link.bin"),
        dir.join("a-symlink"),
    );
    let built = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&sub)?;
        std::fs::write(&a, vec![0x5a_u8; 8192])?;
        std::fs::write(&b, vec![0xa5_u8; 4096])?;
        std::fs::hard_link(&b, &link)?;
        std::os::unix::fs::symlink(&a, &sym)?;
        Ok(())
    })();
    let result = match built {
        Ok(()) => {
            let opts = WalkOpts::default();
            let fast = measure(&dir, &opts);
            let reference = measure_reference(&dir, &opts);
            // Two regular files; the hard link counts once and the symlink counts nothing.
            let ok = fast == reference && fast.files == 2 && fast.logical == 8192 + 4096;
            (
                serde_json::json!({
                    "ok": ok,
                    "files": fast.files,
                    "logical_bytes": fast.logical,
                    "allocated_bytes": fast.alloc,
                    "matches_reference": fast == reference,
                }),
                ok,
            )
        }
        Err(e) => (
            serde_json::json!({ "ok": false, "error": e.to_string() }),
            false,
        ),
    };
    for file in [&sym, &link, &b, &a] {
        let _ = std::fs::remove_file(file);
    }
    for folder in [&sub, &dir] {
        let _ = std::fs::remove_dir(folder);
    }
    result
}

/// Builds the self-test report. Reads settings without taking the instance lock, renaming files
/// or creating directories.
fn self_report() -> (serde_json::Value, bool) {
    let paths = AppPaths::from_env();
    let settings = Settings::load(&paths);
    let data_writable = writable(&paths.data);
    let (walker, walker_ok) = walker_check();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    // Volume attributes only: nothing in the home folder is read.
    let free = tidy_app::macos::disk_info(&home);
    let bundle_id = {
        use objc2_foundation::NSBundle;
        NSBundle::mainBundle()
            .bundleIdentifier()
            .map(|s| s.to_string())
    };
    let ok = data_writable && walker_ok;
    let report = serde_json::json!({
        "ok": ok,
        "version": env!("CARGO_PKG_VERSION"),
        "bundle_id": bundle_id,
        "tidy_home_override": paths.overridden,
        "paths": {
            "data": paths.data,
            "cache": paths.cache,
            "logs": paths.logs,
        },
        "data_dir_writable": data_writable,
        "settings": settings,
        "free_space": free.map(|d| serde_json::json!({
            "volume": d.name,
            "total_bytes": d.total,
            "available_bytes": d.available,
        })),
        "walker": walker,
    });
    (report, ok)
}

fn self_test() -> i32 {
    let (report, ok) = self_report();
    match serde_json::to_string_pretty(&report) {
        Ok(text) => println!("{text}"),
        Err(e) => {
            eprintln!("tidy: cannot print the self-test report: {e}");
            return 1;
        }
    }
    if ok { 0 } else { 1 }
}

// ---------- logging ----------

/// Logs to <logs>/tidy.log. Falls back to stderr when the file cannot be opened, or when
/// `to_file` is false (a demo without TIDY_HOME must not write into the real log folder).
fn init_logging(paths: &AppPaths, to_file: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file = to_file
        .then(|| {
            std::fs::create_dir_all(&paths.logs)
                .and_then(|()| {
                    OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(paths.log_file())
                })
                .ok()
        })
        .flatten();
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false);
    match file {
        Some(file) => builder.with_writer(Mutex::new(file)).init(),
        None => builder.with_writer(std::io::stderr).init(),
    }
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}");
        eprintln!("tidy: panic: {info}");
    }));
}

// ---------- the app ----------

fn run_app() {
    use gpui_kit::App;

    let demo = tidy_app::demo::active();
    // A demo never reads the environment's folders: no core, no lock, no disk access.
    let paths = AppPaths::from_env();
    init_logging(&paths, demo.is_none() || paths.overridden);
    tracing::info!(
        "starting Tidy {} (home override: {})",
        env!("CARGO_PKG_VERSION"),
        paths.overridden
    );

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("tidy-tokio")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("cannot start the async runtime: {e}");
            eprintln!("tidy: cannot start the async runtime: {e}");
            std::process::exit(1);
        }
    };

    if let Some(scenario) = demo {
        run_demo(scenario, rt);
        return;
    }

    let env = Env::from_env_with(SystemSeams {
        procs: Arc::new(LibcProbe::new()),
        last_used: Arc::new(Spotlight),
        // M2: memory figures and the process table from libc, and the read-only system
        // commands of the Security checks (never softwareupdate unless asked for).
        memory: Arc::new(LibcMemory),
        runner: Arc::new(SystemRunner),
        // Read right before each move: a file a process holds open is skipped.
        open_files: Arc::new(LibcOpenFiles),
    });
    // The app holds the instance lock for its whole life. macOS normally routes a second
    // launch to the running instance, so this is a second guard.
    let _lock = match InstanceLock::acquire(&env.paths.lock_file()) {
        Ok(lock) => lock,
        Err(LockError::AlreadyRunning) => {
            tracing::info!(
                "Tidy is already running for {}; exiting",
                env.paths.data.display()
            );
            return;
        }
        Err(e) => {
            tracing::error!("cannot take the instance lock: {e}");
            eprintln!("tidy: cannot start: {e}");
            std::process::exit(1);
        }
    };

    let settings = env.settings.clone();
    let clock = env.clock.clone();
    let home = env.root.clone();
    // The last Large files result is shown until the user rescans (C11).
    let cached_large: Option<LargeScan> = tidy_core::store::read_json(&env.paths.last_large_file())
        .ok()
        .flatten();
    let (core, events) = tidy_core::start(env, rt.handle().clone());
    let app = gpui_kit::application().with_assets(tidy_app::AppAssets);
    app.on_reopen(|cx: &mut App| tidy_app::reopen(cx));

    app.run(move |cx: &mut App| {
        apply_appearance_override(cx);
        gpui_kit::init(cx);
        tidy_app::init(cx);
        tidy_app::install(rt, core, settings, clock, cached_large, &home, events, cx);
        // Sparkle, once the app has finished launching. Off in background, shot and test runs
        // and with TIDY_NO_UPDATES=1 (see tidy_app::updates::decide).
        tidy_app::updates::start(cx);
        // `kill <pid>` quits the same way as Cmd-Q.
        tidy_app::quit_on_signals(cx);

        if tidy_app::open_main_window(cx).is_some() {
            tracing::info!("main window opened");
        } else {
            tracing::error!("main window did not open");
        }

        if !tidy_app::app::background_mode() {
            cx.activate(true);
        }
    });
}

/// TIDY_APPEARANCE=dark|light forces the appearance of every window, so screenshots can show
/// both. Must run before `gpui_kit::init`, which reads the appearance once. (A launch argument
/// like -AppleInterfaceStyle is not honored by GPUI.)
fn apply_appearance_override(cx: &mut gpui_kit::App) {
    use gpui_kit::WindowAppearance;

    if let Ok(v) = std::env::var("TIDY_APPEARANCE") {
        cx.set_window_appearance(match v.as_str() {
            "dark" => Some(WindowAppearance::Dark),
            "light" => Some(WindowAppearance::Light),
            _ => None,
        });
    }
}

/// Runs a fixed demo scenario (TIDY_DEMO) for screenshots. See `tidy_app::demo`.
fn run_demo(scenario: tidy_app::demo::Scenario, rt: tokio::runtime::Runtime) {
    use gpui_kit::App;

    tracing::info!("demo mode: scenario={}", scenario.name());
    let app = gpui_kit::application().with_assets(tidy_app::AppAssets);
    app.run(move |cx: &mut App| {
        apply_appearance_override(cx);
        gpui_kit::init(cx);
        tidy_app::init(cx);
        // Never starts Sparkle: Check up shows a stand-in with Sparkle's defaults.
        tidy_app::updates::install(
            std::rc::Rc::new(tidy_app::updates::FakeUpdates::new(true, true)),
            cx,
        );
        // Never activates: the window is created hidden.
        tidy_app::demo::start(scenario, rt, cx);
    });
}
