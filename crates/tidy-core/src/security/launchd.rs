//! Launch agents and daemons: the plists in `~/Library/LaunchAgents`,
//! `/Library/LaunchAgents` and `/Library/LaunchDaemons` (XML or binary), and the services
//! `launchctl print gui/<uid>` lists for the user's login session.
//!
//! The folders are passed in ([`LaunchdRoots`]), so tests read fixture folders. Only reads:
//! nothing here loads, unloads or moves a job.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::cf::{Plist, read_plist};
use crate::env::CommandRunner;
use crate::model::{LaunchKind, Scope};

pub const LAUNCHCTL: &str = "/bin/launchctl";
/// How long `launchctl print` may take.
pub const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(10);

/// The three folders the Login items check reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchdRoots {
    /// `~/Library/LaunchAgents`.
    pub user_agents: PathBuf,
    /// `/Library/LaunchAgents`.
    pub system_agents: PathBuf,
    /// `/Library/LaunchDaemons`.
    pub system_daemons: PathBuf,
}

impl LaunchdRoots {
    /// The real folders for the home folder `home`.
    pub fn for_home(home: &Path) -> LaunchdRoots {
        LaunchdRoots {
            user_agents: home.join("Library/LaunchAgents"),
            system_agents: PathBuf::from("/Library/LaunchAgents"),
            system_daemons: PathBuf::from("/Library/LaunchDaemons"),
        }
    }

    /// The folders with their scope and kind.
    pub fn folders(&self) -> [(&Path, Scope, LaunchKind); 3] {
        [
            (&self.user_agents, Scope::User, LaunchKind::Agent),
            (&self.system_agents, Scope::System, LaunchKind::Agent),
            (&self.system_daemons, Scope::System, LaunchKind::Daemon),
        ]
    }
}

/// One launchd job from a plist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// `Label`, or the file name without `.plist` when the plist has none.
    pub label: String,
    pub plist: PathBuf,
    /// `Program`, else the first item of `ProgramArguments`. None when the plist names
    /// neither (a `BundleProgram` inside an app, for example).
    pub program: Option<PathBuf>,
    /// `RunAtLoad`.
    pub runs_at_load: bool,
    /// `KeepAlive` is true or a set of conditions.
    pub keep_alive: bool,
    /// `Disabled`.
    pub disabled: bool,
    pub scope: Scope,
    pub kind: LaunchKind,
}

/// Turns a parsed plist into a [`Job`]. None when it is not a dictionary.
pub fn job_from_plist(value: &Plist, plist: &Path, scope: Scope, kind: LaunchKind) -> Option<Job> {
    value.as_dict()?;
    let label = value
        .get("Label")
        .and_then(Plist::as_str)
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .or_else(|| plist.file_stem().map(|s| s.to_string_lossy().into_owned()))?;
    let program = value
        .get("Program")
        .and_then(Plist::as_str)
        .or_else(|| {
            value
                .get("ProgramArguments")
                .and_then(Plist::as_array)
                .and_then(|a| a.first())
                .and_then(Plist::as_str)
        })
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    let keep_alive = match value.get("KeepAlive") {
        Some(Plist::Bool(b)) => *b,
        Some(Plist::Dict(d)) => !d.is_empty(),
        _ => false,
    };
    Some(Job {
        label,
        plist: plist.to_path_buf(),
        program,
        runs_at_load: value
            .get("RunAtLoad")
            .and_then(Plist::as_bool)
            .unwrap_or(false),
        keep_alive,
        disabled: value
            .get("Disabled")
            .and_then(Plist::as_bool)
            .unwrap_or(false),
        scope,
        kind,
    })
}

/// Reads one plist file into a [`Job`].
pub fn read_job(plist: &Path, scope: Scope, kind: LaunchKind) -> io::Result<Job> {
    let value = read_plist(plist)?;
    job_from_plist(&value, plist, scope, kind)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the plist is not a dictionary"))
}

/// What reading one folder gave.
#[derive(Debug, Default)]
pub struct FolderRead {
    /// Sorted by label.
    pub jobs: Vec<Job>,
    /// The error that stopped the folder from being listed. A missing folder is not an error.
    pub error: Option<io::Error>,
    /// Plists that did not parse (logged, then skipped).
    pub skipped: Vec<PathBuf>,
}

/// Reads every `*.plist` in `dir` (not hidden, not in subfolders).
pub fn read_folder(dir: &Path, scope: Scope, kind: LaunchKind) -> FolderRead {
    let mut out = FolderRead::default();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            !name.starts_with('.') && name.ends_with(".plist")
        })
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    for f in files {
        match read_job(&f, scope, kind) {
            Ok(job) => out.jobs.push(job),
            Err(e) => {
                tracing::debug!("launchd: skipping {}: {e}", f.display());
                out.skipped.push(f);
            }
        }
    }
    out.jobs.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// Everything read from the three folders.
#[derive(Debug, Default)]
pub struct LaunchdRead {
    pub jobs: Vec<Job>,
    /// One per folder that could not be listed.
    pub errors: Vec<(PathBuf, io::Error)>,
}

/// Reads the three folders of `roots`.
pub fn read_roots(roots: &LaunchdRoots) -> LaunchdRead {
    let mut out = LaunchdRead::default();
    for (dir, scope, kind) in roots.folders() {
        let read = read_folder(dir, scope, kind);
        if let Some(e) = read.error {
            out.errors.push((dir.to_path_buf(), e));
        }
        out.jobs.extend(read.jobs);
    }
    out
}

// ---------- launchctl print ----------

/// One service line of `launchctl print`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub label: String,
    /// The pid while it runs. None when it is loaded but not running (`0` or `-`).
    pub pid: Option<i32>,
}

/// What `launchctl print gui/<uid>` says about the login session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Domain {
    /// The `services` block, in output order.
    pub services: Vec<Service>,
    /// The `disabled services` block: label to true when disabled.
    pub disabled: BTreeMap<String, bool>,
}

impl Domain {
    /// Every loaded label.
    pub fn labels(&self) -> BTreeSet<&str> {
        self.services.iter().map(|s| s.label.as_str()).collect()
    }
}

/// Parses `launchctl print gui/<uid>` output. Blocks are introduced by a line ending in
/// `= {` and closed by `}`; the `services` block holds `pid status label` lines, the
/// `disabled services` block `"label" => enabled|disabled` lines.
pub fn parse_print(text: &str) -> Domain {
    let mut out = Domain::default();
    // The block names from the top-level domain down to the current line.
    let mut stack: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_suffix("= {") {
            stack.push(name.trim().to_string());
            continue;
        }
        if t == "}" {
            stack.pop();
            continue;
        }
        // Only the blocks right under the domain (depth 2: "gui/501", then the block).
        if stack.len() != 2 {
            continue;
        }
        match stack[1].as_str() {
            "services" => {
                let parts: Vec<&str> = t.split_whitespace().collect();
                if parts.len() >= 3 {
                    let label = parts[2..].join(" ");
                    let pid = parts[0].parse::<i32>().ok().filter(|p| *p > 0);
                    out.services.push(Service { label, pid });
                }
            }
            "disabled services" => {
                if let Some((label, state)) = t.split_once("=>") {
                    let label = label.trim().trim_matches('"').to_string();
                    if !label.is_empty() {
                        out.disabled.insert(label, state.trim() == "disabled");
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Runs `launchctl print gui/<uid>` and parses it.
pub fn print_gui(runner: &dyn CommandRunner, uid: u32) -> io::Result<Domain> {
    let target = format!("gui/{uid}");
    let out = runner.run(LAUNCHCTL, &["print", &target], LAUNCHCTL_TIMEOUT)?;
    if !out.success() {
        return Err(io::Error::other(format!(
            "launchctl print {target} ended with an error"
        )));
    }
    Ok(parse_print(&out.stdout))
}

/// Whether a label `launchctl` lists belongs to macOS or is an app's own window session
/// rather than a helper someone installed: Apple's services, running apps
/// (`application.<bundle id>.<n>`), camera extensions and the ssh agent.
pub fn is_system_label(label: &str) -> bool {
    const PREFIXES: [&str; 4] = [
        "com.apple.",
        "application.",
        "CMIOExtension.",
        "com.openssh.",
    ];
    PREFIXES.iter().any(|p| label.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_fall_back_to_the_file_name() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Program</key><string>/bin/ls</string></dict></plist>"#;
        let value = super::super::cf::parse_plist(xml).unwrap();
        let job = job_from_plist(
            &value,
            Path::new("/x/com.example.nolabel.plist"),
            Scope::User,
            LaunchKind::Agent,
        )
        .unwrap();
        assert_eq!(job.label, "com.example.nolabel");
        assert_eq!(job.program.as_deref(), Some(Path::new("/bin/ls")));
        assert!(!job.runs_at_load && !job.keep_alive && !job.disabled);
    }

    #[test]
    fn a_missing_folder_is_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let read = read_folder(&tmp.path().join("nope"), Scope::User, LaunchKind::Agent);
        assert!(read.jobs.is_empty());
        assert!(read.error.is_none());
    }

    #[test]
    fn system_labels_are_told_apart() {
        assert!(is_system_label("com.apple.Finder"));
        assert!(is_system_label("application.com.example.App.123.456"));
        assert!(!is_system_label("com.example.helper"));
    }
}
