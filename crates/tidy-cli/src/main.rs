//! tidyctl: a headless driver of tidy-core (design section 9). It prints one JSON object per
//! line on stdout, each with an `"event"` key; the last line is the result. Logs go to
//! stderr (filter in `TIDY_LOG`, default `warn`).
//!
//! | Command | Does |
//! |---|---|
//! | `scan` | Cleanup scan: progress lines, then `{"event":"plan",...}` |
//! | `large [--min-mb N] [--not-opened] [--folders [REL_PATH]]` | Large files scan |
//! | `clean --safe` or `clean --ids ID,...` | Scan, then move the safe items (or the named ids) to the Trash |
//! | `trash PATH...` | Move paths to the Trash through the Large files guard |
//! | `empty-trash` | Delete for good the items Tidy moved to the Trash |
//! | `history [--last N]` | Print history entries |
//! | `memory` | One memory snapshot: `{"event":"memory",...}` |
//! | `security [--include-updates]` | The Security checks: progress lines, then `{"event":"security",...}` |
//! | `checkup [--include-updates]` | Cleanup scan, Security checks and a memory snapshot, then `{"event":"checkup",...}` |
//! | `explain ID` | Cleanup scan, then the details of one item: `{"event":"explain",...}` |
//! | `system-data` | What Tidy can measure of System Data: `{"event":"system_data",...}` |
//!
//! Exit codes: 0 done, 1 error, 2 bad usage, 3 some items were skipped or failed (for
//! `checkup`: a part could not be done), 4 another Tidy holds the lock, 5 refused. The
//! destructive commands (`clean`, `trash`, `empty-trash`) refuse with 5 unless `TIDY_HOME` is
//! set or `--real` is given (C23), and they take the instance lock; the read-only ones never
//! do, so they work while the app runs. `memory`, `security` and `checkup` move nothing:
//! `security` and `checkup` save their results (`security/<id>.json`, one per run, and `checkups.jsonl`)
//! in Tidy's data folder, which is under `TIDY_HOME` when it is set. Without
//! `--include-updates`, `softwareupdate` never runs.
//!
//! Progress lines carry counts only, not the path being measured: a scan looks at folders it
//! then declines to offer, and the output never names those.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value, json};
use tidy_core::api::{Command, Event};
use tidy_core::env::{SystemRunner, SystemSeams};
use tidy_core::large::{self, FilesFilter};
use tidy_core::lastused::Spotlight;
use tidy_core::memory::LibcMemory;
use tidy_core::model::{
    Checkup, FIRST_CHECK_SENTENCE, FolderNode, HistoryEntry, ItemResult, LARGE_RECORD_MIN,
    LargeKind, LargeScan, MB, TrasherKind,
};
use tidy_core::paths::{AppPaths, HOME_ENV};
use tidy_core::procs::LibcProbe;
use tidy_core::reminder::ReminderDate;
use tidy_core::{Env, InstanceLock, LockError, history};
use tokio::sync::mpsc::UnboundedReceiver;

const USAGE: &str = "usage: tidyctl <command>

commands:
  scan                                   Cleanup scan, prints the plan
  large [--min-mb N] [--not-opened] [--folders [REL_PATH]]
                                         Large files scan
  clean (--safe | --ids ID,...) [--real] Scan, then move items to the Trash
  trash PATH... [--real]                 Move paths to the Trash
  empty-trash [--real]                   Delete the items Tidy moved to the Trash
  history [--last N]                     Print history entries
  memory                                 Memory figures and top apps by memory
  security [--include-updates]           Security checks (softwareupdate only when asked)
  checkup [--include-updates]            Cleanup scan, Security checks and memory
  explain ID                             Why a Cleanup item is offered and what rebuilds it
  system-data                            What Tidy can measure of System Data (read-only)

Destructive commands need TIDY_HOME, or --real to use the real home folder.";

/// The refusal for a destructive command without `TIDY_HOME` or `--real`.
const REFUSAL: &str = "Refusing to use the real home folder. Set TIDY_HOME, or pass --real.";

mod code {
    pub const OK: u8 = 0;
    pub const ERROR: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const PARTIAL: u8 = 3;
    pub const LOCKED: u8 = 4;
    pub const REFUSED: u8 = 5;
}

#[derive(Debug, PartialEq, Eq)]
enum Selection {
    Safe,
    Ids(Vec<String>),
}

#[derive(Debug, PartialEq, Eq)]
enum Cmd {
    Help,
    Scan,
    Large {
        min_mb: Option<u64>,
        not_opened: bool,
        /// `--folders`, with the folder to show (empty for the home folder).
        folders: Option<String>,
    },
    Clean {
        selection: Selection,
        real: bool,
    },
    Trash {
        paths: Vec<PathBuf>,
        real: bool,
    },
    EmptyTrash {
        real: bool,
    },
    History {
        last: Option<usize>,
    },
    Memory,
    Security {
        include_updates: bool,
    },
    Checkup {
        include_updates: bool,
    },
    Explain {
        id: String,
    },
    SystemData,
}

impl Cmd {
    /// For a destructive command: whether `--real` was given.
    fn real(&self) -> Option<bool> {
        match self {
            Cmd::Clean { real, .. } | Cmd::Trash { real, .. } | Cmd::EmptyTrash { real } => {
                Some(*real)
            }
            _ => None,
        }
    }
}

fn parse(args: &[String]) -> Result<Cmd, String> {
    let Some((name, rest)) = args.split_first() else {
        return Err("no command given".into());
    };
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        return Ok(Cmd::Help);
    }
    let mut it = rest.iter().peekable();
    let number = |flag: &str, v: Option<&String>| -> Result<u64, String> {
        v.and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| format!("{flag} needs a whole number"))
    };
    match name.as_str() {
        "help" | "-h" | "--help" => Ok(Cmd::Help),
        "scan" => match it.next() {
            None => Ok(Cmd::Scan),
            Some(a) => Err(format!("scan takes no arguments, got {a}")),
        },
        "large" => {
            let (mut min_mb, mut not_opened, mut folders) = (None, false, None);
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--min-mb" => min_mb = Some(number("--min-mb", it.next())?),
                    "--not-opened" => not_opened = true,
                    "--folders" => {
                        let rel = it.next_if(|v| !v.starts_with("--")).cloned();
                        folders = Some(rel.unwrap_or_default().trim_matches('/').to_string());
                    }
                    other => return Err(format!("large: unknown argument {other}")),
                }
            }
            Ok(Cmd::Large {
                min_mb,
                not_opened,
                folders,
            })
        }
        "clean" => {
            let (mut safe, mut ids, mut real) = (false, Vec::new(), false);
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--safe" => safe = true,
                    "--ids" => {
                        let list = it.next().ok_or("--ids needs a list of item ids")?;
                        ids.extend(
                            list.split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(String::from),
                        );
                    }
                    "--real" => real = true,
                    other => return Err(format!("clean: unknown argument {other}")),
                }
            }
            let selection = match (safe, ids.is_empty()) {
                (true, true) => Selection::Safe,
                (false, false) => Selection::Ids(ids),
                (true, false) => return Err("clean takes --safe or --ids, not both".into()),
                (false, true) => return Err("clean needs --safe or --ids ID,...".into()),
            };
            Ok(Cmd::Clean { selection, real })
        }
        "trash" => {
            let (mut paths, mut real, mut only_paths) = (Vec::new(), false, false);
            for a in it {
                match a.as_str() {
                    "--" if !only_paths => only_paths = true,
                    "--real" if !only_paths => real = true,
                    s if s.starts_with("--") && !only_paths => {
                        return Err(format!("trash: unknown argument {s}"));
                    }
                    s => paths.push(PathBuf::from(s)),
                }
            }
            if paths.is_empty() {
                return Err("trash needs at least one path".into());
            }
            Ok(Cmd::Trash { paths, real })
        }
        "empty-trash" => {
            let mut real = false;
            for a in it {
                match a.as_str() {
                    "--real" => real = true,
                    other => return Err(format!("empty-trash: unknown argument {other}")),
                }
            }
            Ok(Cmd::EmptyTrash { real })
        }
        "history" => {
            let mut last = None;
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--last" => last = Some(number("--last", it.next())? as usize),
                    other => return Err(format!("history: unknown argument {other}")),
                }
            }
            Ok(Cmd::History { last })
        }
        "memory" => match it.next() {
            None => Ok(Cmd::Memory),
            Some(a) => Err(format!("memory takes no arguments, got {a}")),
        },
        "security" | "checkup" => {
            let mut include_updates = false;
            for a in it {
                match a.as_str() {
                    "--include-updates" => include_updates = true,
                    other => return Err(format!("{name}: unknown argument {other}")),
                }
            }
            Ok(if name == "security" {
                Cmd::Security { include_updates }
            } else {
                Cmd::Checkup { include_updates }
            })
        }
        "explain" => match (it.next(), it.next()) {
            (Some(id), None) if !id.starts_with("--") => Ok(Cmd::Explain { id: id.clone() }),
            (None, _) => Err("explain needs an item id".into()),
            _ => Err("explain takes one item id".into()),
        },
        "system-data" => match it.next() {
            None => Ok(Cmd::SystemData),
            Some(a) => Err(format!("system-data takes no arguments, got {a}")),
        },
        other => Err(format!("unknown command {other}")),
    }
}

fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_env("TIDY_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse(&args) {
        Ok(cmd) => cmd,
        Err(msg) => {
            print_line(&json!({"event": "error", "what": "usage", "message": msg}));
            eprintln!("tidyctl: {msg}\n{USAGE}");
            return ExitCode::from(code::USAGE);
        }
    };
    if cmd.real() == Some(false) && !tidy_home_set() {
        print_line(&json!({"event": "error", "what": "refused", "message": REFUSAL}));
        eprintln!("tidyctl: {REFUSAL}");
        return ExitCode::from(code::REFUSED);
    }
    ExitCode::from(run(cmd))
}

fn tidy_home_set() -> bool {
    std::env::var_os(HOME_ENV).is_some_and(|v| !v.is_empty())
}

fn run(cmd: Cmd) -> u8 {
    match cmd {
        Cmd::Help => {
            print_line(&json!({"event": "help", "usage": USAGE}));
            eprintln!("{USAGE}");
            code::OK
        }
        Cmd::History { last } => history_cmd(last),
        other => with_core(other),
    }
}

// ---------- Output ----------

/// Prints one JSON line. A closed stdout is not an error worth more than ignoring.
fn print_line(v: &Value) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// `{"event": name}` merged with the fields of `value` (an object).
fn tagged<T: Serialize>(name: &str, value: &T) -> Value {
    let mut map = match serde_json::to_value(value) {
        Ok(Value::Object(m)) => m,
        Ok(other) => {
            let mut m = Map::new();
            m.insert("value".into(), other);
            m
        }
        Err(e) => {
            let mut m = Map::new();
            m.insert("serialize_error".into(), Value::String(e.to_string()));
            m
        }
    };
    map.insert("event".into(), Value::String(name.into()));
    Value::Object(map)
}

/// The JSON line for a core event. Large results are printed by the `large` command itself.
fn event_line(ev: &Event) -> Value {
    match ev {
        Event::ScanProgress {
            kind, files, bytes, ..
        } => json!({"event": "progress", "kind": kind, "files": files, "bytes": bytes}),
        Event::CleanupPlan(plan) => tagged("plan", plan),
        Event::LargeResult(scan) => json!({
            "event": "large_result", "root": scan.root, "items": scan.items.len(),
        }),
        Event::ScanStopped { kind } => json!({"event": "stopped", "kind": kind}),
        Event::CleanProgress { done, total } => {
            json!({"event": "clean_progress", "done": done, "total": total})
        }
        Event::ItemTrashed { id, trash_path } => {
            json!({"event": "trashed", "id": id, "trash_path": trash_path})
        }
        Event::ItemSkipped { id, reason } => {
            json!({"event": "skipped", "id": id, "reason": reason})
        }
        Event::CleanDone(entry) | Event::EmptyTrashDone(entry) => tagged("done", entry),
        Event::RunningChanged(set) => {
            json!({"event": "running", "ids": set.ids().collect::<Vec<_>>()})
        }
        Event::SettingsChanged(s) => tagged("settings", s),
        Event::MemorySnapshot(snap) => tagged("memory", snap),
        Event::SecurityProgress { done, total } => {
            json!({"event": "security_progress", "done": done, "total": total})
        }
        Event::SecurityResult(report) => tagged("security", report),
        Event::CheckupDone { checkup, reminder } => checkup_line(checkup, reminder),
        Event::Failed { what, message } => {
            json!({"event": "error", "what": what, "message": message})
        }
    }
}

// ---------- Commands that use the core ----------

fn with_core(cmd: Cmd) -> u8 {
    let env = Env::from_env_with(SystemSeams {
        procs: Arc::new(LibcProbe::new()),
        last_used: Spotlight::shared(),
        // Under TIDY_HOME, TIDY_TEST_MEMORY and TIDY_TEST_COMMANDS replace these two.
        memory: Arc::new(LibcMemory::new()),
        runner: Arc::new(SystemRunner),
    });
    let _lock = if cmd.real().is_some() {
        match InstanceLock::acquire(&env.paths.lock_file()) {
            Ok(lock) => Some(lock),
            Err(LockError::AlreadyRunning) => {
                print_line(&json!({
                    "event": "error", "what": "lock",
                    "message": "Tidy is already running. Quit it or wait for it to finish.",
                }));
                return code::LOCKED;
            }
            Err(e) => {
                print_line(&json!({"event": "error", "what": "lock", "message": e.to_string()}));
                return code::ERROR;
            }
        }
    } else {
        None
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            print_line(&json!({"event": "error", "what": "runtime", "message": e.to_string()}));
            return code::ERROR;
        }
    };
    let (core, events) = tidy_core::start(env.clone(), rt.handle().clone());
    let mut session = Session { rt: &rt, events };
    let status = match cmd {
        Cmd::Scan => {
            core.send(Command::ScanCleanup);
            match session.plan() {
                Some(_) => code::OK,
                None => code::ERROR,
            }
        }
        Cmd::Large {
            min_mb,
            not_opened,
            folders,
        } => {
            let min = min_mb
                .map(|m| m.saturating_mul(MB))
                .unwrap_or(env.settings.large_min_bytes);
            // Folders mode keeps every node so each folder total can be reported.
            let record_min = if folders.is_some() {
                1
            } else {
                min.clamp(1, LARGE_RECORD_MIN)
            };
            core.send(Command::ScanLarge {
                record_min: Some(record_min),
            });
            match session.large() {
                Some(scan) => match folders {
                    Some(rel) => print_folders(&scan, &rel, min),
                    None => print_files(&env, &scan, not_opened, min),
                },
                None => code::ERROR,
            }
        }
        Cmd::Clean { selection, .. } => {
            core.send(Command::ScanCleanup);
            match session.plan() {
                Some(plan) => {
                    let item_ids = match selection {
                        Selection::Safe => plan.ticked().map(|i| i.id.clone()).collect(),
                        Selection::Ids(ids) => ids,
                    };
                    core.send(Command::Clean { item_ids });
                    session.done()
                }
                None => code::ERROR,
            }
        }
        Cmd::Trash { paths, .. } => {
            core.send(Command::TrashPaths { paths });
            session.done()
        }
        Cmd::EmptyTrash { .. } => {
            core.send(Command::EmptyTidyTrash);
            session.done()
        }
        Cmd::Memory => {
            core.send(Command::SnapshotMemory);
            session.memory()
        }
        Cmd::Security { include_updates } => {
            core.send(Command::RunSecurity { include_updates });
            session.security()
        }
        Cmd::Checkup { include_updates } => {
            core.send(Command::RunCheckup { include_updates });
            session.checkup()
        }
        Cmd::Explain { id } => {
            core.send(Command::ScanCleanup);
            match session.plan() {
                Some(plan) => match plan.item(&id) {
                    Some(item) => {
                        let mut line = tagged("explain", &tidy_core::explain::explain(item));
                        line["id"] = Value::String(id);
                        print_line(&line);
                        code::OK
                    }
                    None => {
                        print_line(&json!({
                            "event": "error", "what": "explain",
                            "message": format!("no item {id} in the plan"),
                        }));
                        code::ERROR
                    }
                },
                None => code::ERROR,
            }
        }
        Cmd::SystemData => match rt.block_on(core.system_data()) {
            Some(report) => {
                let mut line = tagged("system_data", &report);
                line["measured_bytes"] = json!(report.measured_bytes());
                print_line(&line);
                code::OK
            }
            None => code::ERROR,
        },
        Cmd::Help | Cmd::History { .. } => unreachable!("handled in run"),
    };
    rt.block_on(core.shutdown());
    status
}

/// Reads core events, printing each one, until the result of the command arrives.
struct Session<'a> {
    rt: &'a tokio::runtime::Runtime,
    events: UnboundedReceiver<Event>,
}

impl Session<'_> {
    fn next(&mut self) -> Option<Event> {
        let events = &mut self.events;
        self.rt.block_on(events.recv())
    }

    /// Prints events until the plan (Some) or a stop or failure (None).
    fn plan(&mut self) -> Option<tidy_core::Plan> {
        while let Some(ev) = self.next() {
            match ev {
                Event::CleanupPlan(plan) => {
                    print_line(&event_line(&Event::CleanupPlan(plan.clone())));
                    return Some(plan);
                }
                ev @ (Event::Failed { .. } | Event::ScanStopped { .. }) => {
                    print_line(&event_line(&ev));
                    return None;
                }
                ev => print_line(&event_line(&ev)),
            }
        }
        None
    }

    /// Prints progress until the large scan arrives (not printed here).
    fn large(&mut self) -> Option<LargeScan> {
        while let Some(ev) = self.next() {
            match ev {
                Event::LargeResult(scan) => return Some(scan),
                ev @ (Event::Failed { .. } | Event::ScanStopped { .. }) => {
                    print_line(&event_line(&ev));
                    return None;
                }
                ev => print_line(&event_line(&ev)),
            }
        }
        None
    }

    /// Prints events until a clean, trash or Empty Trash ends, and returns the exit code.
    fn done(&mut self) -> u8 {
        let mut skipped = false;
        while let Some(ev) = self.next() {
            print_line(&event_line(&ev));
            match ev {
                Event::ItemSkipped { .. } => skipped = true,
                Event::CleanDone(entry) | Event::EmptyTrashDone(entry) => {
                    return if skipped || !all_done(&entry) {
                        code::PARTIAL
                    } else {
                        code::OK
                    };
                }
                Event::Failed { .. } => return code::ERROR,
                _ => {}
            }
        }
        code::ERROR
    }

    /// Prints the memory snapshot, or the error.
    fn memory(&mut self) -> u8 {
        while let Some(ev) = self.next() {
            print_line(&event_line(&ev));
            match ev {
                Event::MemorySnapshot(_) => return code::OK,
                Event::Failed { .. } => return code::ERROR,
                _ => {}
            }
        }
        code::ERROR
    }

    /// Prints progress until the Security report (0) or a failure (1).
    fn security(&mut self) -> u8 {
        while let Some(ev) = self.next() {
            print_line(&event_line(&ev));
            match ev {
                Event::SecurityResult(_) => return code::OK,
                Event::Failed { .. } => return code::ERROR,
                _ => {}
            }
        }
        code::ERROR
    }

    /// Prints every part's events until the check up ends. 0 when all three parts are in
    /// it, 3 when one could not be done, 1 when the check up failed or was not saved.
    fn checkup(&mut self) -> u8 {
        let mut not_saved = false;
        loop {
            // After a check up failure the result may still follow (a save that failed) or
            // not (the run itself failed): wait a little, not forever.
            let ev = if not_saved {
                let events = &mut self.events;
                match self.rt.block_on(async {
                    tokio::time::timeout(std::time::Duration::from_secs(5), events.recv()).await
                }) {
                    Ok(Some(ev)) => ev,
                    _ => return code::ERROR,
                }
            } else {
                match self.next() {
                    Some(ev) => ev,
                    None => return code::ERROR,
                }
            };
            print_line(&event_line(&ev));
            match ev {
                Event::Failed { what, .. } if what == "checkup" => not_saved = true,
                Event::CheckupDone { checkup, .. } => {
                    return if not_saved {
                        code::ERROR
                    } else if checkup.cleanup.is_none()
                        || checkup.security.is_none()
                        || checkup.memory.is_none()
                    {
                        code::PARTIAL
                    } else {
                        code::OK
                    };
                }
                _ => {}
            }
        }
    }
}

/// The result line of `checkup`: the check up as saved, the next reminder date, and the
/// three cards in words (null for a part that could not be done).
fn checkup_line(checkup: &Checkup, reminder: &ReminderDate) -> Value {
    let security = checkup.security.as_ref();
    json!({
        "event": "checkup",
        "checkup": checkup,
        "reminder": reminder,
        "cleanup_total": checkup.cleanup.map(|c| c.safe_bytes),
        "cleanup_items": checkup.cleanup.map(|c| c.safe_items),
        "security_summary": security.map(|s| s.headline()),
        "security_first_check": security.filter(|s| s.first_run).map(|_| FIRST_CHECK_SENTENCE),
        "memory_verdict": checkup.memory.map(|m| m.verdict()),
    })
}

/// Whether every item of the entry did what the action is for.
fn all_done(entry: &HistoryEntry) -> bool {
    entry.message.is_none()
        && entry
            .items
            .iter()
            .all(|i| matches!(i.result, ItemResult::Trashed | ItemResult::Deleted))
}

// ---------- large ----------

fn print_files(env: &Env, scan: &LargeScan, not_opened: bool, min: u64) -> u8 {
    let filter = if not_opened {
        FilesFilter::NotOpened
    } else {
        FilesFilter::Biggest
    };
    let days = env.settings.not_opened_days;
    let now = env.now();
    let items = large::files(scan, filter, min, now, days);
    let total: u64 = items.iter().map(|i| i.bytes_alloc).sum();
    print_line(&json!({
        "event": "large",
        "mode": "files",
        "filter": filter,
        "min_bytes": min,
        "not_opened_days": days,
        "now": now,
        "root": scan.root,
        "scanned_at": scan.scanned_at,
        "files_seen": scan.files_seen,
        "skipped": scan.skipped,
        "count": items.len(),
        "bytes_alloc": total,
        "items": items,
    }));
    code::OK
}

/// One folder total for the `folders` list and the rows.
fn node_line(n: &FolderNode) -> Value {
    json!({
        "rel": n.rel,
        "name": n.name,
        "kind": n.kind,
        "bytes_alloc": n.bytes_alloc,
        "bytes_logical": n.bytes_logical,
        "files": n.files,
    })
}

fn print_folders(scan: &LargeScan, rel: &str, min: u64) -> u8 {
    let Some(view) = large::folder_view(scan, rel, min) else {
        print_line(&json!({
            "event": "error", "what": "large",
            "message": format!("The scan has no folder at {rel:?}."),
        }));
        return code::ERROR;
    };
    // Every folder and package at or below the shown folder, parents first.
    let mut folders = Vec::new();
    let mut stack = vec![view.node];
    while let Some(n) = stack.pop() {
        if matches!(n.kind, Some(LargeKind::Folder | LargeKind::Package)) {
            folders.push(node_line(n));
        }
        stack.extend(n.children.iter().rev());
    }
    print_line(&json!({
        "event": "folders",
        "rel": view.node.rel,
        "min_bytes": min,
        "root": scan.root,
        "scanned_at": scan.scanned_at,
        "crumbs": view.crumbs,
        "node": node_line(view.node),
        "rows": view.rows.iter().map(|n| node_line(n)).collect::<Vec<_>>(),
        "smaller_count": view.smaller_count,
        "smaller_bytes": view.smaller_bytes,
        "folders": folders,
    }));
    code::OK
}

// ---------- history ----------

fn history_cmd(last: Option<usize>) -> u8 {
    let paths = AppPaths::from_env();
    let entries = match last {
        Some(n) => history::last(&paths, n),
        None => history::read(&paths),
    };
    for e in &entries {
        print_line(&tagged("history", e));
    }
    let kind = if paths.overridden {
        TrasherKind::Dir
    } else {
        TrasherKind::System
    };
    let summary = history::trash_summary(&paths, kind);
    print_line(&json!({
        "event": "summary",
        "entries": entries.len(),
        "in_trash_items": summary.items,
        "in_trash_bytes": summary.bytes,
    }));
    code::OK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_every_command() {
        assert_eq!(parse(&args("scan")), Ok(Cmd::Scan));
        assert_eq!(
            parse(&args("large --min-mb 4 --not-opened")),
            Ok(Cmd::Large {
                min_mb: Some(4),
                not_opened: true,
                folders: None
            })
        );
        assert_eq!(
            parse(&args("large --folders")),
            Ok(Cmd::Large {
                min_mb: None,
                not_opened: false,
                folders: Some(String::new())
            })
        );
        assert_eq!(
            parse(&args("large --folders Library/Caches/ --min-mb 1")),
            Ok(Cmd::Large {
                min_mb: Some(1),
                not_opened: false,
                folders: Some("Library/Caches".into())
            })
        );
        assert_eq!(
            parse(&args("clean --safe")),
            Ok(Cmd::Clean {
                selection: Selection::Safe,
                real: false
            })
        );
        assert_eq!(
            parse(&args("clean --ids a:b,c:d --ids e:f --real")),
            Ok(Cmd::Clean {
                selection: Selection::Ids(vec!["a:b".into(), "c:d".into(), "e:f".into()]),
                real: true
            })
        );
        assert_eq!(
            parse(&args("trash /a /b -- --real")),
            Ok(Cmd::Trash {
                paths: vec!["/a".into(), "/b".into(), "--real".into()],
                real: false
            })
        );
        assert_eq!(
            parse(&args("empty-trash --real")),
            Ok(Cmd::EmptyTrash { real: true })
        );
        assert_eq!(
            parse(&args("history --last 3")),
            Ok(Cmd::History { last: Some(3) })
        );
        assert_eq!(parse(&args("clean --help")), Ok(Cmd::Help));
        assert_eq!(parse(&args("memory")), Ok(Cmd::Memory));
        assert_eq!(parse(&args("system-data")), Ok(Cmd::SystemData));
        assert!(parse(&args("system-data now")).is_err());
        assert_eq!(
            parse(&args("explain cache.app:Library/Caches/x")),
            Ok(Cmd::Explain {
                id: "cache.app:Library/Caches/x".into()
            })
        );
        assert!(parse(&args("explain")).is_err());
        assert!(parse(&args("explain a b")).is_err());
        assert_eq!(Cmd::SystemData.real(), None);
        assert_eq!(
            parse(&args("security")),
            Ok(Cmd::Security {
                include_updates: false
            })
        );
        assert_eq!(
            parse(&args("security --include-updates")),
            Ok(Cmd::Security {
                include_updates: true
            })
        );
        assert_eq!(
            parse(&args("checkup")),
            Ok(Cmd::Checkup {
                include_updates: false
            })
        );
        assert_eq!(
            parse(&args("checkup --include-updates")),
            Ok(Cmd::Checkup {
                include_updates: true
            })
        );
    }

    #[test]
    fn rejects_bad_usage() {
        for bad in [
            "",
            "nope",
            "scan extra",
            "large --min-mb",
            "large --min-mb x",
            "clean",
            "clean --safe --ids a:b",
            "clean --ids",
            "trash",
            "trash --force /a",
            "empty-trash now",
            "history --last",
            "memory --all",
            "security --updates",
            "checkup now",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn only_destructive_commands_need_the_guard() {
        assert_eq!(Cmd::Scan.real(), None);
        assert_eq!(Cmd::History { last: None }.real(), None);
        assert_eq!(Cmd::EmptyTrash { real: false }.real(), Some(false));
        assert_eq!(Cmd::Memory.real(), None);
        assert_eq!(
            Cmd::Checkup {
                include_updates: false
            }
            .real(),
            None
        );
        assert!(!REFUSAL.contains('\u{2014}'));
    }

    #[test]
    fn events_have_an_event_key() {
        let entry = HistoryEntry {
            id: "x-1".into(),
            at: 1,
            action: tidy_core::model::Action::Clean,
            source: tidy_core::model::Source::Cleanup,
            trasher: TrasherKind::Dir,
            items: vec![],
            trashed_bytes: 0,
            message: None,
        };
        let line = event_line(&Event::CleanDone(entry));
        assert_eq!(line["event"], "done");
        assert_eq!(line["trasher"], "dir");
        let line = event_line(&Event::ScanProgress {
            kind: tidy_core::ScanKind::Cleanup,
            files: 3,
            bytes: 4,
            current: Some("/h/secret".into()),
        });
        assert_eq!(line["event"], "progress");
        assert!(!line.to_string().contains("secret"));
    }

    #[test]
    fn the_checkup_line_names_the_three_parts() {
        use tidy_core::model::{CheckupCleanup, CheckupMemory, CheckupSecurity, Pressure};
        let reminder = ReminderDate {
            at: 1,
            year: 2026,
            month: 10,
            day: 30,
            hour: 10,
            minute: 0,
        };
        let full = Checkup {
            at: 5,
            include_updates: false,
            cleanup: Some(CheckupCleanup {
                safe_bytes: 6_300_000_000,
                safe_items: 4,
                review_bytes: 0,
            }),
            security: Some(CheckupSecurity {
                id: "20261023-140211".into(),
                amber: 2,
                gray: 1,
                first_run: true,
            }),
            memory: Some(CheckupMemory {
                pressure: Pressure::Warning,
                swap_used: 0,
                compressed_bytes: 0,
            }),
        };
        let line = event_line(&Event::CheckupDone {
            checkup: full.clone(),
            reminder,
        });
        assert_eq!(line["event"], "checkup");
        assert_eq!(line["cleanup_total"], 6_300_000_000u64);
        assert_eq!(line["security_summary"], "2 things to look at");
        assert_eq!(line["security_first_check"], FIRST_CHECK_SENTENCE);
        assert_eq!(line["memory_verdict"], Pressure::Warning.verdict());
        assert_eq!(line["reminder"]["day"], 30);
        assert_eq!(line["checkup"]["at"], 5);
        let empty = checkup_line(&Checkup::default(), &reminder);
        assert!(empty["cleanup_total"].is_null());
        assert!(empty["security_summary"].is_null());
        assert!(empty["memory_verdict"].is_null());
    }
}
