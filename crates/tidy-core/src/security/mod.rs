//! The Security checks of design section 5.4: read-only, no prompts, no network of Tidy's own
//! (`softwareupdate --list` runs only when asked for).
//!
//! - [`system`]: FileVault, the firewall, Gatekeeper, SIP, XProtect and macOS updates.
//! - [`launchd`]: launch agent and daemon plists and `launchctl print gui/<uid>`.
//! - [`codesign`]: who signed each program and whether Apple notarized it.
//! - [`chrome`]: extensions from each Chrome profile's `Secure Preferences`.
//! - [`diff`]: the New and Changed marks against the previous week's saved report.
//!
//! [`run`] does one pass over [`Sources`] and returns the report; [`run_and_save`] also
//! compares it with the latest earlier week and saves it as `security/<week>.json`. A check
//! that cannot run is gray and says why; nothing here asks for access.

pub mod cf;
pub mod chrome;
pub mod codesign;
pub mod diff;
pub mod launchd;
pub mod system;

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::env::{CommandRunner, Env};
use crate::model::{
    CheckId, CheckResult, ChromeExtension, LaunchKind, LoginItem, Mark, Scope, SecurityReport,
    Signer, Status, run_id,
};
use crate::paths::AppPaths;
use crate::store;
use codesign::{CodeSign, Signature, Signatures};
use launchd::LaunchdRoots;

/// Where one Security run reads from, and how it checks signatures. Tests point every folder
/// at fixtures and may swap the signature checker.
#[derive(Clone)]
pub struct Sources {
    pub launchd: LaunchdRoots,
    /// The XProtect Info.plists; the newest counts.
    pub xprotect: Vec<PathBuf>,
    /// Chrome's data folder ([`chrome::chrome_root`]).
    pub chrome: PathBuf,
    /// The user whose login session `launchctl print gui/<uid>` lists.
    pub uid: u32,
    pub signatures: Arc<dyn Signatures>,
}

impl std::fmt::Debug for Sources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sources")
            .field("launchd", &self.launchd)
            .field("xprotect", &self.xprotect)
            .field("chrome", &self.chrome)
            .field("uid", &self.uid)
            .finish_non_exhaustive()
    }
}

impl Sources {
    /// The real sources for the home folder `home`: its LaunchAgents and Chrome folders, the
    /// system LaunchAgents and LaunchDaemons, XProtect, this process's user, and
    /// Security.framework for signatures.
    pub fn for_home(home: &Path) -> Sources {
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        Sources {
            launchd: LaunchdRoots::for_home(home),
            xprotect: system::XPROTECT_PLISTS.iter().map(PathBuf::from).collect(),
            chrome: chrome::chrome_root(home),
            uid,
            signatures: Arc::new(CodeSign),
        }
    }

    /// The sources for `env`: the user folders come from the scan root, so under
    /// `TIDY_HOME` they are the fake home's.
    pub fn for_env(env: &Env) -> Sources {
        Sources::for_home(&env.root)
    }

    pub fn with_signatures(mut self, signatures: Arc<dyn Signatures>) -> Sources {
        self.signatures = signatures;
        self
    }
}

/// Called with `(done, total)` as the run goes: the checks, then one step per signature.
pub type Progress<'a> = &'a mut dyn FnMut(usize, usize);

/// The steps that are not signature checks: one per [`CheckId`].
const FIXED_STEPS: usize = CheckId::ALL.len();

/// One Security run at `env.now()`, compared with `previous` (the latest earlier week, or
/// None for the first run). `include_updates` runs `softwareupdate --list`; without it the
/// macOS updates row is gray and says it is checked on demand. Does not save.
pub fn run(
    env: &Env,
    sources: &Sources,
    include_updates: bool,
    previous: Option<&SecurityReport>,
    progress: Progress<'_>,
) -> SecurityReport {
    let runner: &dyn CommandRunner = env.runner.as_ref();
    let now = env.now();
    let mut report = SecurityReport::empty(now);
    report.updates_checked = include_updates;

    // The login items first, so the total is known before the slow part.
    let read = launchd::read_roots(&sources.launchd);
    let loaded = match launchd::print_gui(runner, sources.uid) {
        Ok(domain) => Some(domain),
        Err(e) => {
            tracing::debug!("launchctl print: {e}");
            None
        }
    };
    let programs: BTreeSet<PathBuf> = read
        .jobs
        .iter()
        .filter_map(|j| j.program.as_deref().map(resolve_program))
        .collect();
    let total = FIXED_STEPS + programs.len();
    let mut done = 0;
    let mut step = |progress: &mut dyn FnMut(usize, usize)| {
        done += 1;
        progress(done, total);
    };
    progress(0, total);

    let mut rows: HashMap<CheckId, CheckResult> = HashMap::new();
    rows.insert(CheckId::FileVault, system::filevault(runner));
    step(progress);
    rows.insert(CheckId::Firewall, system::firewall(runner));
    step(progress);
    rows.insert(CheckId::Gatekeeper, system::gatekeeper(runner));
    step(progress);
    rows.insert(CheckId::Sip, system::sip(runner));
    step(progress);
    rows.insert(CheckId::XProtect, system::xprotect(&sources.xprotect, now));
    step(progress);
    rows.insert(
        CheckId::MacosUpdates,
        system::updates(runner, include_updates),
    );
    step(progress);

    // Signatures, one program at a time.
    let mut signatures: HashMap<PathBuf, Signature> = HashMap::new();
    for program in &programs {
        signatures.insert(program.clone(), sources.signatures.check(program));
        step(progress);
    }
    report.login_items = login_items(&read.jobs, loaded.as_ref(), &signatures);

    let mut chrome = chrome::read_all(&sources.chrome);
    report.extensions = std::mem::take(&mut chrome.extensions);

    diff::mark_items(&mut report, previous);
    sort_login_items(&mut report.login_items);

    rows.insert(
        CheckId::LoginItems,
        login_items_row(&report.login_items, &read, report.first_run),
    );
    step(progress);
    rows.insert(
        CheckId::ChromeExtensions,
        chrome_row(&chrome, &report.extensions, report.first_run),
    );
    step(progress);

    report.checks = CheckId::ALL
        .iter()
        .filter_map(|id| rows.remove(id))
        .collect();
    diff::mark_checks(&mut report, previous);
    report
}

/// Runs the checks for `env` (see [`run`]), compares them with the latest week before this
/// one and saves the report as `security/<week>.json`. The report is returned even when
/// saving fails (the error is logged).
pub fn run_and_save(env: &Env, include_updates: bool, progress: Progress<'_>) -> SecurityReport {
    run_and_save_with(env, &Sources::for_env(env), include_updates, progress)
}

/// [`run_and_save`] with explicit sources (tests pass fixture folders).
pub fn run_and_save_with(
    env: &Env,
    sources: &Sources,
    include_updates: bool,
    progress: Progress<'_>,
) -> SecurityReport {
    let id = run_id(env.now());
    let previous = previous_report(&env.paths, &id);
    let report = run(env, sources, include_updates, previous.as_ref(), progress);
    if let Err(e) = save_report(&env.paths, &report) {
        tracing::warn!("cannot save the security report: {e}");
    }
    report
}

/// A program as launchd would find it: a bare name resolves against the default `PATH`.
fn resolve_program(program: &Path) -> PathBuf {
    if program.is_absolute() {
        return program.to_path_buf();
    }
    for dir in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        let p = Path::new(dir).join(program);
        if p.is_file() {
            return p;
        }
    }
    program.to_path_buf()
}

/// The login items from the plists, plus labels only `launchctl` lists that are not part of
/// macOS (a helper an app registered without a plist in the three folders).
fn login_items(
    jobs: &[launchd::Job],
    loaded: Option<&launchd::Domain>,
    signatures: &HashMap<PathBuf, Signature>,
) -> Vec<LoginItem> {
    let mut items: Vec<LoginItem> = jobs
        .iter()
        .map(|j| {
            let sig = j
                .program
                .as_deref()
                .and_then(|p| signatures.get(&resolve_program(p)))
                .cloned()
                .unwrap_or_else(Signature::unknown);
            LoginItem {
                label: j.label.clone(),
                plist: Some(j.plist.clone()),
                program: j.program.clone(),
                runs_at_load: j.runs_at_load,
                scope: j.scope,
                kind: j.kind,
                signer: sig.signer,
                notarized: sig.notarized,
                mark: None,
            }
        })
        .collect();
    if let Some(domain) = loaded {
        let known: BTreeSet<&str> = jobs.iter().map(|j| j.label.as_str()).collect();
        let mut extra: BTreeSet<&str> = BTreeSet::new();
        for s in &domain.services {
            let label = s.label.as_str();
            let disabled = domain.disabled.get(label).copied().unwrap_or(false);
            if !known.contains(label) && !launchd::is_system_label(label) && !disabled {
                extra.insert(label);
            }
        }
        items.extend(extra.into_iter().map(|label| LoginItem {
            label: label.to_string(),
            plist: None,
            program: None,
            runs_at_load: false,
            scope: Scope::User,
            kind: LaunchKind::Agent,
            signer: Signer::Unknown,
            notarized: None,
            mark: None,
        }));
    }
    items
}

/// New items first, then by label (then scope, so the order is stable).
fn sort_login_items(items: &mut [LoginItem]) {
    items.sort_by(|a, b| {
        let new = |i: &LoginItem| i.mark != Some(Mark::New);
        (new(a), a.label.to_lowercase(), a.scope).cmp(&(new(b), b.label.to_lowercase(), b.scope))
    });
}

/// "1 item" or "3 items".
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The Login items row: amber when an item is new since the last check, not signed, or not
/// notarized; gray when none of the folders could be read.
fn login_items_row(
    items: &[LoginItem],
    read: &launchd::LaunchdRead,
    first_run: bool,
) -> CheckResult {
    let id = CheckId::LoginItems;
    if read.errors.len() == 3 {
        let sentence = if read
            .errors
            .iter()
            .all(|(_, e)| e.kind() == io::ErrorKind::PermissionDenied)
        {
            "Tidy couldn't read this without Full Disk Access."
        } else {
            "Tidy couldn't read the launch agent and daemon folders."
        };
        return CheckResult::new(id, Status::Gray, sentence);
    }
    let new = items.iter().filter(|i| i.mark == Some(Mark::New)).count();
    let unsigned = items
        .iter()
        .filter(|i| i.signer == Signer::Unsigned)
        .count();
    let not_notarized = items
        .iter()
        .filter(|i| i.signer != Signer::Unsigned && i.notarized == Some(false))
        .count();
    let look = items.iter().filter(|i| i.needs_a_look()).count();
    if look > 0 {
        let mut parts = Vec::new();
        if new > 0 {
            parts.push(format!("{new} new since the last check"));
        }
        if unsigned > 0 {
            parts.push(format!("{unsigned} not signed"));
        }
        if not_notarized > 0 {
            parts.push(format!("{not_notarized} not notarized by Apple"));
        }
        let verb = if look == 1 { "needs" } else { "need" };
        return CheckResult::new(
            id,
            Status::Amber,
            format!(
                "{} {verb} a look: {}. Check that you recognize {}.",
                plural(look, "item", "items"),
                parts.join(", "),
                if look == 1 { "it" } else { "them" }
            ),
        );
    }
    let sentence = match (items.len(), first_run) {
        (0, _) => "No login items or background helpers besides the ones macOS ships.".to_string(),
        (n, true) => format!(
            "{}, all signed.",
            plural(
                n,
                "login item or background helper",
                "login items and background helpers"
            )
        ),
        (n, false) => format!(
            "{}, all signed and none new since the last check.",
            plural(
                n,
                "login item or background helper",
                "login items and background helpers"
            )
        ),
    };
    CheckResult::new(id, Status::Green, sentence)
}

/// The Chrome extensions row: amber when an extension was added since the last check; gray
/// when Chrome is not set up or its settings could not be read.
fn chrome_row(read: &chrome::ChromeRead, exts: &[ChromeExtension], first_run: bool) -> CheckResult {
    let id = CheckId::ChromeExtensions;
    if !read.installed {
        return CheckResult::new(
            id,
            Status::Gray,
            "Chrome isn't set up for this user, so there are no extensions to check.",
        );
    }
    if read.profiles == 0 && !read.errors.is_empty() {
        return CheckResult::new(id, Status::Gray, "Tidy couldn't read Chrome's settings.");
    }
    let added = exts.iter().filter(|e| e.mark == Some(Mark::New)).count();
    if added > 0 {
        let what = if added == 1 {
            "1 extension was".to_string()
        } else {
            format!("{added} extensions were")
        };
        return CheckResult::new(
            id,
            Status::Amber,
            format!(
                "{what} added since the last check. To see {}, open Chrome and choose Window > Extensions.",
                if added == 1 { "it" } else { "them" }
            ),
        );
    }
    let count = plural(exts.len(), "extension", "extensions");
    let profiles = plural(read.profiles, "Chrome profile", "Chrome profiles");
    let sentence = match (exts.len(), first_run) {
        (0, _) => "No extensions are installed in Chrome.".to_string(),
        (_, true) => format!("{count} in {profiles}."),
        (_, false) => format!("{count} in {profiles}, none added since the last check."),
    };
    CheckResult::new(id, Status::Green, sentence)
}

// ---------- Saved reports ----------

/// How many saved runs are kept. Older ones are removed after each save; the marks only
/// need the newest.
pub const KEEP_RUNS: usize = 100;

/// Whether `name` is a saved report's file name: `20261023-140211.json`. Returns the id.
fn run_of_file(name: &str) -> Option<&str> {
    let id = name.strip_suffix(".json")?;
    let b = id.as_bytes();
    let ok = b.len() == 15
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'-'
        && b[9..].iter().all(u8::is_ascii_digit);
    ok.then_some(id)
}

/// The ids of the saved runs, oldest first.
pub fn saved_runs(paths: &AppPaths) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(paths.security_dir()) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| run_of_file(&e.file_name().to_string_lossy()).map(str::to_string))
        .collect();
    ids.sort();
    ids
}

/// Saves `report` as `security/<id>.json`, then removes all but the newest [`KEEP_RUNS`].
pub fn save_report(paths: &AppPaths, report: &SecurityReport) -> io::Result<()> {
    if run_of_file(&format!("{}.json", report.id)).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a run id: {:?}", report.id),
        ));
    }
    store::write_json_atomic(&paths.security_file(&report.id), report)?;
    let runs = saved_runs(paths);
    for old in &runs[..runs.len().saturating_sub(KEEP_RUNS)] {
        if let Err(e) = std::fs::remove_file(paths.security_file(old)) {
            tracing::warn!("cannot remove the old security report {old}: {e}");
        }
    }
    Ok(())
}

/// The saved run `id`. None when there is none or it does not parse (logged).
pub fn load_report(paths: &AppPaths, id: &str) -> Option<SecurityReport> {
    match store::read_json(&paths.security_file(id)) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("cannot read the security report {id}: {e}");
            None
        }
    }
}

/// The newest saved report, for the screen to show before a new run.
pub fn latest_report(paths: &AppPaths) -> Option<SecurityReport> {
    saved_runs(paths)
        .iter()
        .rev()
        .find_map(|id| load_report(paths, id))
}

/// The newest saved report from before run `id`: what that run compares with.
pub fn previous_report(paths: &AppPaths, id: &str) -> Option<SecurityReport> {
    saved_runs(paths)
        .iter()
        .rev()
        .filter(|r| r.as_str() < id)
        .find_map(|r| load_report(paths, r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_file_names_are_strict() {
        assert_eq!(run_of_file("20261023-140211.json"), Some("20261023-140211"));
        assert_eq!(run_of_file("20261023-140211.json.tmp"), None);
        assert_eq!(run_of_file("2026-W43.json"), None);
        assert_eq!(run_of_file("20261023140211.json"), None);
        assert_eq!(run_of_file("notes.json"), None);
    }

    #[test]
    fn bare_programs_resolve_on_the_default_path() {
        assert_eq!(resolve_program(Path::new("ls")), PathBuf::from("/bin/ls"));
        assert_eq!(
            resolve_program(Path::new("/opt/x/y")),
            PathBuf::from("/opt/x/y")
        );
    }
}
