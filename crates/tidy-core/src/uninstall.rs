//! The uninstaller (design section 19): the apps in the application folders, the files each
//! one left in the home folder's Library, and the move of both to the Trash.
//!
//! - [`list_apps`] reads every `*.app` in `/Applications` and `~/Applications` (one level
//!   down too; only `<root>/Applications` under `TIDY_HOME`). Apple's apps, Tidy and the copy
//!   of Tidy that is running are left out and never offered (C47).
//! - [`find_leftovers`] looks for one app's leftovers, read-only, only when the user opens
//!   the app (C48). An entry belongs to the app when its name is one of the app's ids (its
//!   bundle id and its helpers') or starts with one followed by a dot, unless a longer id of
//!   another installed app also matches it (C49).
//! - [`uninstall`] finds the leftovers again from scratch and moves only the asked-for items
//!   that are still in that fresh result. It has its own allowlist guard instead of
//!   [`crate::trash::check`]: the app lives outside the scan root, and `Containers` and
//!   `Group Containers` are on the Cleanup denylist (C50). The shared last gate
//!   ([`MoveGate`]: the ignore list and open files) still runs before every move.

use std::ffi::CString;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::Event;
use crate::clean::{
    GONE, IGNORE_LIST_STOPPED, IGNORE_LIST_UNREADABLE, MoveGate, STOPPED, finish, move_checked,
    record, touches_tidy,
};
use crate::env::{Env, file_time};
use crate::model::{
    Action, HistoryEntry, ItemResult, LaunchKind, Scope, Signer, Source, UseSource, unix_secs,
};
use crate::owners::{InstalledApp, InstalledApps, Owner, bundle_id_of, read_app};
use crate::security::codesign::{Signature, Signatures};
use crate::security::launchd::read_job;
use crate::trash::Refusal;
use crate::walk::{WalkOpts, device_of, measure};

/// The skip reason for an id that the fresh look did not find, or found but cannot move.
pub const NOT_FOUND_AGAIN: &str = "Tidy didn't find this again. Open the app again first.";
/// The note on a Report only row outside the home folder (C52).
pub const NEEDS_ADMIN_NOTE: &str =
    "Needs an administrator, so Tidy leaves it. Remove it in Finder.";
/// The note on an app bundle Tidy's user cannot rename (C51).
pub const APP_NEEDS_ADMIN: &str = "Tidy can't move this app without an administrator. Drag it to the Trash in Finder, which asks for your password.";
/// The skip reason when macOS refused the move of the app itself.
pub const APP_MOVE_REFUSED: &str =
    "macOS didn't let Tidy move the app. Drag it to the Trash in Finder.";

/// Words that name a vendor or a framework, never one app, so a folder with that name is not
/// taken as an app's by name.
const VENDOR_WORDS: [&str; 9] = [
    "google",
    "microsoft",
    "adobe",
    "apple",
    "mozilla",
    "jetbrains",
    "electron",
    "python",
    "java",
];

/// Helper bundles inside an app whose ids count as the app's: (folder, extension).
const HELPER_FOLDERS: [(&str, &str); 3] = [
    ("Contents/PlugIns", "appex"),
    ("Contents/Library/LoginItems", "app"),
    ("Contents/Helpers", "app"),
];

/// The Library folders of [`LeftoverKind::System`] rows (Report only).
const SYSTEM_FOLDERS: [&str; 4] = [
    "LaunchAgents",
    "LaunchDaemons",
    "PrivilegedHelperTools",
    "Application Support",
];

// ---------- Model ----------

/// A helper bundle inside an app (a login item, an extension).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Helper {
    pub bundle_id: String,
    pub executable: Option<String>,
}

/// One app the uninstaller lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppEntry {
    pub path: PathBuf,
    pub name: String,
    pub bundle_id: String,
    pub version: Option<String>,
    pub executable: Option<String>,
    /// The team id of its signature, for shared containers.
    pub team_id: Option<String>,
    /// The developer name from its signature.
    pub team_name: Option<String>,
    /// Allocated bytes of the bundle.
    pub bytes: u64,
    /// Unix seconds: Spotlight's last-use date, else the bundle's file time.
    pub last_used: Option<u64>,
    pub used_source: UseSource,
    pub helpers: Vec<Helper>,
    /// Tidy's user can rename the bundle (C51).
    pub movable: bool,
}

impl AppEntry {
    /// The bundle ids and process names that mean the app is open: its own and its helpers'.
    pub fn owner(&self) -> Owner {
        let mut ids = vec![self.bundle_id.clone()];
        ids.extend(self.executable.clone());
        for h in &self.helpers {
            ids.push(h.bundle_id.clone());
            ids.extend(h.executable.clone());
        }
        Owner {
            name: self.name.clone(),
            ids,
        }
    }
}

/// The apps of a listing, sorted by name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppList {
    pub apps: Vec<AppEntry>,
    /// How many apps were left out (Apple's and Tidy).
    pub left_out: usize,
    /// When it was listed, Unix seconds.
    pub at: u64,
}

/// What kind of file a row of an uninstall is, in the order the rows are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftoverKind {
    App,
    AppSupport,
    Caches,
    Preferences,
    Container,
    GroupContainer,
    AppScripts,
    SavedState,
    Logs,
    WebData,
    WebKit,
    LaunchAgent,
    /// Outside the home folder: Report only (C52).
    System,
}

impl LeftoverKind {
    /// The row's label.
    pub fn label(self) -> &'static str {
        match self {
            LeftoverKind::App => "App",
            LeftoverKind::AppSupport => "Application Support",
            LeftoverKind::Caches => "Caches",
            LeftoverKind::Preferences => "Preferences",
            LeftoverKind::Container => "Container",
            LeftoverKind::GroupContainer => "Shared container",
            LeftoverKind::AppScripts => "Application scripts",
            LeftoverKind::SavedState => "Saved window state",
            LeftoverKind::Logs => "Logs",
            LeftoverKind::WebData => "Web data",
            LeftoverKind::WebKit => "WebKit data",
            LeftoverKind::LaunchAgent => "Launch agent",
            LeftoverKind::System => "System file",
        }
    }

    /// The rule recorded in history.
    pub fn rule(self) -> &'static str {
        match self {
            LeftoverKind::App => "uninstall.app",
            LeftoverKind::AppSupport => "uninstall.app-support",
            LeftoverKind::Caches => "uninstall.caches",
            LeftoverKind::Preferences => "uninstall.preferences",
            LeftoverKind::Container => "uninstall.container",
            LeftoverKind::GroupContainer => "uninstall.group-container",
            LeftoverKind::AppScripts => "uninstall.app-scripts",
            LeftoverKind::SavedState => "uninstall.saved-state",
            LeftoverKind::Logs => "uninstall.logs",
            LeftoverKind::WebData => "uninstall.web-data",
            LeftoverKind::WebKit => "uninstall.webkit",
            LeftoverKind::LaunchAgent => "uninstall.launch-agent",
            LeftoverKind::System => "uninstall.system",
        }
    }

    /// The Library folders (relative to the scan root's `Library`) this kind is found in.
    fn folders(self) -> &'static [&'static str] {
        match self {
            LeftoverKind::App | LeftoverKind::System => &[],
            LeftoverKind::AppSupport => &["Application Support"],
            LeftoverKind::Caches => &["Caches"],
            LeftoverKind::Preferences => &["Preferences", "Preferences/ByHost"],
            LeftoverKind::Container => &["Containers"],
            LeftoverKind::GroupContainer => &["Group Containers"],
            LeftoverKind::AppScripts => &["Application Scripts"],
            LeftoverKind::SavedState => &["Saved Application State"],
            LeftoverKind::Logs => &["Logs"],
            LeftoverKind::WebData => &["HTTPStorages"],
            LeftoverKind::WebKit => &["WebKit"],
            LeftoverKind::LaunchAgent => &["LaunchAgents"],
        }
    }

    /// Whether apps keep folders of this kind under their name.
    fn by_name(self) -> bool {
        matches!(
            self,
            LeftoverKind::AppSupport | LeftoverKind::Caches | LeftoverKind::Logs
        )
    }
}

/// One row of an uninstall: the app itself or one leftover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leftover {
    /// The absolute path as a string: the id the UI and tidyctl name it by.
    pub id: String,
    pub path: PathBuf,
    pub kind: LeftoverKind,
    /// Allocated bytes; None when Tidy could not read inside it.
    pub bytes: Option<u64>,
    /// Ticked when the detail opens.
    pub ticked: bool,
    /// False for Report only rows, which are never moved.
    pub selectable: bool,
    /// Matched by the app's name rather than an id.
    pub by_name: bool,
    /// Why it is unticked or Report only ("Shared with Word").
    pub note: Option<String>,
}

/// An app and everything Tidy would move with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallPlan {
    pub app: AppEntry,
    /// The app row first, then the leftovers by kind, Report only rows last.
    pub items: Vec<Leftover>,
    /// "Quit Slack first" while the app or a helper runs (C54).
    pub blocked: Option<String>,
    /// The scan root the paths are under, so the screen can show them from `~`.
    pub root: PathBuf,
}

impl UninstallPlan {
    pub fn item(&self, id: &str) -> Option<&Leftover> {
        self.items.iter().find(|i| i.id == id)
    }
}

// ---------- Listing ----------

/// One bundle with what the uninstaller needs to know about it.
struct Bundle {
    app: InstalledApp,
    sign: Signature,
    helpers: Vec<Helper>,
    left_out: bool,
}

/// Every app in the application folders, with the left out ones marked.
fn bundles(env: &Env, signer: &dyn Signatures) -> Vec<Bundle> {
    let own = own_bundle();
    app_paths(env)
        .into_iter()
        .filter_map(|path| {
            // An empty or dotless id could match unrelated names (`.GlobalPreferences`), so such
            // a bundle is neither listed nor matched.
            let app = read_app(&path).filter(|a| real_id(&a.bundle_id))?;
            let sign = signer.check(&path);
            let id = app.bundle_id.to_ascii_lowercase();
            let left_out = id.starts_with("com.apple.")
                || id.starts_with("dev.sdusteric.tidy")
                || sign.signer == Signer::Apple
                || own
                    .as_ref()
                    .is_some_and(|o| path.canonicalize().is_ok_and(|p| &p == o));
            let helpers = helpers(&path);
            Some(Bundle {
                app,
                sign,
                helpers,
                left_out,
            })
        })
        .collect()
}

/// The `*.app` folders directly in each application folder and one level down. Links are not
/// followed.
fn app_paths(env: &Env) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in InstalledApps::folders(env) {
        for path in plain_children(&dir) {
            if is_app(&path) {
                out.push(path);
            } else {
                out.extend(plain_children(&path).into_iter().filter(|p| is_app(p)));
            }
        }
    }
    out
}

/// Whether `id` can stand for one app: not empty and with a dot, as reverse-DNS ids have.
fn real_id(id: &str) -> bool {
    !id.is_empty() && id.contains('.') && !id.starts_with('.')
}

/// Whether `id` names something of Apple's, which the uninstaller never matches.
fn is_apple(id: &str) -> bool {
    id.to_ascii_lowercase().starts_with("com.apple.")
}

/// The folders (not links) directly in `dir`, sorted.
fn plain_children(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

fn is_app(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("app"))
}

/// The app bundle this process runs from, resolved. None for tidyctl and tests.
fn own_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    crate::procs::outer_app(&exe)?.canonicalize().ok()
}

fn helpers(app: &Path) -> Vec<Helper> {
    let mut out = Vec::new();
    for (folder, ext) in HELPER_FOLDERS {
        let Ok(entries) = std::fs::read_dir(app.join(folder)) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)))
            .collect();
        paths.sort();
        out.extend(
            paths
                .iter()
                .filter_map(|p| read_app(p))
                .filter(|h| real_id(&h.bundle_id) && !is_apple(&h.bundle_id))
                .map(|h| Helper {
                    bundle_id: h.bundle_id,
                    executable: h.executable,
                }),
        );
    }
    out
}

/// Whether this process may write `path` (rename it, for a folder).
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated string that outlives the call.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Allocated bytes of `path`, staying on its volume. None when it cannot be read.
fn size_of(path: &Path) -> Option<u64> {
    let meta = path.symlink_metadata().ok()?;
    if meta.file_type().is_symlink() {
        return Some(0);
    }
    if meta.is_dir() && std::fs::read_dir(path).is_err() {
        return None;
    }
    let opts = WalkOpts {
        device: device_of(path),
        ..WalkOpts::default()
    };
    Some(measure(path, &opts).alloc)
}

fn entry(env: &Env, b: &Bundle, measure_bundle: bool) -> AppEntry {
    let path = &b.app.path;
    let (last_used, used_source) = match env.last_used.last_used(path) {
        Some(t) => (Some(unix_secs(t)), UseSource::Spotlight),
        None => (
            path.symlink_metadata()
                .ok()
                .and_then(|m| file_time(&m))
                .map(unix_secs),
            UseSource::FileTimes,
        ),
    };
    let team_name = match &b.sign.signer {
        Signer::Team(name) => Some(name.clone()),
        _ => None,
    };
    AppEntry {
        path: path.clone(),
        name: b.app.name.clone(),
        bundle_id: b.app.bundle_id.clone(),
        version: b.app.version.clone(),
        executable: b.app.executable.clone(),
        team_id: b.sign.team_id.clone(),
        team_name,
        bytes: if measure_bundle {
            size_of(path).unwrap_or(0)
        } else {
            0
        },
        last_used,
        used_source,
        helpers: b.helpers.clone(),
        movable: writable(path) && path.parent().is_some_and(writable),
    }
}

/// The apps the uninstaller lists, each measured, sorted by name.
pub fn list_apps(env: &Env, signer: &dyn Signatures) -> AppList {
    let all = bundles(env, signer);
    let left_out = all.iter().filter(|b| b.left_out).count();
    let mut apps: Vec<AppEntry> = all
        .iter()
        .filter(|b| !b.left_out)
        .map(|b| entry(env, b, true))
        .collect();
    apps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    AppList {
        apps,
        left_out,
        at: env.now(),
    }
}

/// Where Report only rows are looked for: `/Library`, or `$TIDY_HOME/system/Library` under
/// `TIDY_HOME` and in tests.
pub fn system_library(env: &Env) -> PathBuf {
    if env.overridden {
        env.paths
            .data
            .parent()
            .unwrap_or(&env.paths.data)
            .join("system/Library")
    } else {
        PathBuf::from("/Library")
    }
}

// ---------- Matching ----------

/// Decides which Library entries belong to one app.
struct Matcher {
    /// The app's bundle id and its helpers', lowercase.
    ids: Vec<String>,
    /// Every other installed app's ids, lowercase.
    others: Vec<String>,
    /// The app's names that may match a folder, lowercase (C49).
    names: Vec<String>,
    team: Option<String>,
    /// Names of the other apps signed by the same team.
    team_mates: Vec<String>,
    /// File names of other installed bundles with one of this app's ids (a second copy): what
    /// matches by id is theirs too, so it is left unticked.
    sharers: Vec<String>,
    app_path: PathBuf,
}

/// Whether `name` is `id` or starts with `id` followed by a dot. Both lowercase.
fn hits(name: &str, id: &str) -> bool {
    name == id || (name.starts_with(id) && name.as_bytes().get(id.len()) == Some(&b'.'))
}

fn bundle_ids(b: &Bundle) -> Vec<String> {
    let mut ids = vec![b.app.bundle_id.to_ascii_lowercase()];
    ids.extend(b.helpers.iter().map(|h| h.bundle_id.to_ascii_lowercase()));
    ids
}

fn bundle_names(b: &Bundle) -> Vec<String> {
    let mut names = vec![b.app.name.to_lowercase()];
    if let Some(stem) = b.app.path.file_stem() {
        names.push(stem.to_string_lossy().to_lowercase());
    }
    names.extend(b.app.executable.iter().map(|e| e.to_lowercase()));
    names
}

impl Matcher {
    /// `system_names` are the names of the apps that come with macOS, which no other app's
    /// folders may be taken for.
    fn new(target: &Bundle, all: &[Bundle], system_names: &[String]) -> Matcher {
        let ids = bundle_ids(target);
        let others_all: Vec<&Bundle> = all
            .iter()
            .filter(|b| b.app.path != target.app.path)
            .collect();
        let others: Vec<String> = others_all
            .iter()
            .flat_map(|b| bundle_ids(b))
            .filter(|id| !ids.contains(id))
            .collect();
        let mut sharers: Vec<String> = others_all
            .iter()
            .filter(|b| bundle_ids(b).iter().any(|id| ids.contains(id)))
            .map(|b| {
                b.app
                    .path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| b.app.name.clone())
            })
            .collect();
        sharers.sort();
        sharers.dedup();
        let mut taken: Vec<String> = others_all.iter().flat_map(|b| bundle_names(b)).collect();
        taken.extend(system_names.iter().map(|n| n.to_lowercase()));
        let mut names: Vec<String> = bundle_names(target)
            .into_iter()
            .filter(|n| n.chars().count() >= 3)
            .filter(|n| !VENDOR_WORDS.contains(&n.as_str()))
            .filter(|n| !taken.contains(n))
            .collect();
        names.dedup();
        let team = target.sign.team_id.clone();
        let mut team_mates: Vec<String> = others_all
            .iter()
            .filter(|b| {
                b.sign.team_id.is_some()
                    && b.sign
                        .team_id
                        .as_deref()
                        .zip(team.as_deref())
                        .is_some_and(|(a, t)| a.eq_ignore_ascii_case(t))
            })
            .map(|b| b.app.name.clone())
            .collect();
        team_mates.sort();
        team_mates.dedup();
        Matcher {
            ids,
            others,
            names,
            team,
            team_mates,
            sharers,
            app_path: target.app.path.clone(),
        }
    }

    /// Whether `name` is one of the app's ids or starts with one and a dot, and no longer id
    /// of another app matches it too.
    fn id(&self, name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        if n.starts_with("com.apple.") {
            return false;
        }
        let Some(best) = self
            .ids
            .iter()
            .filter(|id| hits(&n, id))
            .map(String::len)
            .max()
        else {
            return false;
        };
        !self.others.iter().any(|o| o.len() > best && hits(&n, o))
    }

    /// Whether `name` matches another app's id.
    fn others_id(&self, name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        self.others.iter().any(|o| hits(&n, o))
    }

    fn name(&self, name: &str) -> bool {
        self.names.contains(&name.to_lowercase())
    }
}

/// The folders from the scan root down to `root/rel` when each is a real folder, not a link.
fn plain_dir(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for part in Path::new(rel).components() {
        path.push(part);
        if !path.symlink_metadata().ok()?.is_dir() {
            return None;
        }
    }
    Some(path)
}

/// The names in `dir`, sorted.
fn names_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// "Also used by Slack 2": a second copy of the same app.
fn also_used_note(names: &[String]) -> String {
    match names {
        [a] => format!("Also used by {a}"),
        [a, b] => format!("Also used by {a} and {b}"),
        [a, ..] => format!("Also used by {a} and {} more", names.len() - 1),
        [] => "Also used by another copy of this app".to_string(),
    }
}

/// The names of the apps that come with macOS (`/System/Applications` and its Utilities), so
/// a third-party app called "Dock" never takes Apple's folders by name. None under
/// `TIDY_HOME` and in tests.
fn system_app_names(env: &Env) -> Vec<String> {
    if env.overridden {
        return Vec::new();
    }
    let root = Path::new("/System/Applications");
    let mut out = Vec::new();
    for path in plain_children(root)
        .into_iter()
        .chain(plain_children(&root.join("Utilities")))
    {
        if is_app(&path)
            && let Some(stem) = path.file_stem()
        {
            out.push(stem.to_string_lossy().into_owned());
        }
    }
    out
}

/// "Shared with Word", "Shared with Word and Excel", "Shared with Word, Excel and 2 more".
fn shared_note(names: &[String]) -> String {
    match names {
        [] => "Shared with other apps".to_string(),
        [a] => format!("Shared with {a}"),
        [a, b] => format!("Shared with {a} and {b}"),
        [a, b, rest @ ..] => format!("Shared with {a}, {b} and {} more", rest.len()),
    }
}

fn leftover(path: PathBuf, kind: LeftoverKind) -> Leftover {
    let selectable = kind != LeftoverKind::System;
    Leftover {
        id: path.to_string_lossy().into_owned(),
        bytes: size_of(&path),
        path,
        kind,
        ticked: selectable,
        selectable,
        by_name: false,
        note: (!selectable).then(|| NEEDS_ADMIN_NOTE.to_string()),
    }
}

/// The leftovers of the app `m` describes, under the scan root's Library.
fn find_in_library(env: &Env, m: &Matcher) -> Vec<Leftover> {
    use LeftoverKind::*;
    let mut out = Vec::new();
    for kind in [
        AppSupport,
        Caches,
        Preferences,
        Container,
        GroupContainer,
        AppScripts,
        SavedState,
        Logs,
        WebData,
        WebKit,
        LaunchAgent,
    ] {
        for folder in kind.folders() {
            let Some(dir) = plain_dir(&env.root, &format!("Library/{folder}")) else {
                continue;
            };
            for name in names_in(&dir) {
                let path = dir.join(&name);
                if touches_tidy(env, &path) {
                    continue;
                }
                let stem = |suffix: &str| {
                    name.strip_suffix(suffix)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                };
                let item = match kind {
                    Preferences => {
                        let Some(s) = stem(".plist") else { continue };
                        if !m.id(&s) {
                            continue;
                        }
                        leftover(path, kind)
                    }
                    SavedState => {
                        let Some(s) = stem(".savedState") else {
                            continue;
                        };
                        if !m.id(&s) {
                            continue;
                        }
                        leftover(path, kind)
                    }
                    LaunchAgent => {
                        let Some(s) = stem(".plist") else { continue };
                        let job = read_job(&path, Scope::User, LaunchKind::Agent).ok();
                        let ours = m.id(&s)
                            || job.as_ref().is_some_and(|j| {
                                m.id(&j.label)
                                    || j.program.as_ref().is_some_and(|p| {
                                        p.starts_with(&m.app_path)
                                            && !p
                                                .components()
                                                .any(|c| c == std::path::Component::ParentDir)
                                    })
                            });
                        if !ours {
                            continue;
                        }
                        leftover(path, kind)
                    }
                    GroupContainer => {
                        let lower = name.to_ascii_lowercase();
                        if let Some(rest) = lower.strip_prefix("group.") {
                            if !m.id(rest) {
                                continue;
                            }
                            leftover(path, kind)
                        } else {
                            let Some((team, rest)) = name.split_once('.') else {
                                continue;
                            };
                            if !m
                                .team
                                .as_deref()
                                .is_some_and(|t| t.eq_ignore_ascii_case(team))
                            {
                                continue;
                            }
                            if m.id(rest) || m.team_mates.is_empty() {
                                leftover(path, kind)
                            } else if m.others_id(rest) {
                                continue;
                            } else {
                                let mut l = leftover(path, kind);
                                l.ticked = false;
                                l.note = Some(shared_note(&m.team_mates));
                                l
                            }
                        }
                    }
                    _ if m.id(&name) => leftover(path, kind),
                    _ if kind.by_name() && m.name(&name) => {
                        let mut l = leftover(path, kind);
                        l.by_name = true;
                        l
                    }
                    _ => continue,
                };
                out.push(item);
            }
        }
    }
    out
}

/// Report only rows in `/Library` (C52).
fn find_in_system(env: &Env, m: &Matcher) -> Vec<Leftover> {
    let root = system_library(env);
    let mut out = Vec::new();
    for folder in SYSTEM_FOLDERS {
        let dir = root.join(folder);
        for name in names_in(&dir) {
            let key = name.strip_suffix(".plist").unwrap_or(&name);
            if m.id(key) {
                out.push(leftover(dir.join(&name), LeftoverKind::System));
            }
        }
    }
    out
}

/// The app at `app` and its leftovers, or None when `app` is not an app the uninstaller lists
/// (left out, gone, or outside the application folders).
pub fn find_leftovers(env: &Env, signer: &dyn Signatures, app: &Path) -> Option<UninstallPlan> {
    let all = bundles(env, signer);
    let target = all.iter().find(|b| b.app.path == app && !b.left_out)?;
    let entry = entry(env, target, true);
    let m = Matcher::new(target, &all, &system_app_names(env));
    let mut items = vec![Leftover {
        id: entry.path.to_string_lossy().into_owned(),
        path: entry.path.clone(),
        kind: LeftoverKind::App,
        bytes: Some(entry.bytes),
        ticked: entry.movable,
        selectable: entry.movable,
        by_name: false,
        note: (!entry.movable).then(|| APP_NEEDS_ADMIN.to_string()),
    }];
    let mut found = find_in_library(env, &m);
    if !m.sharers.is_empty() {
        for item in found.iter_mut().filter(|i| i.ticked) {
            item.ticked = false;
            item.note = Some(also_used_note(&m.sharers));
        }
    }
    found.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.path.cmp(&b.path)));
    items.extend(found);
    items.extend(find_in_system(env, &m));
    let blocked = entry.owner().blocked_by(&env.procs.running());
    Some(UninstallPlan {
        app: entry,
        items,
        blocked,
        root: env.root.clone(),
    })
}

// ---------- Moving ----------

type Refused = (ItemResult, String);

/// The allowlist guard (C50), run on the fresh plan right before each move. Returns the
/// resolved path to move: the resolved parent folder joined with the entry's own name, so a
/// leftover that is a link is moved as the link.
fn guard(env: &Env, plan: &UninstallPlan, item: &Leftover) -> Result<PathBuf, Refused> {
    let path = &item.path;
    let meta = match path.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err((ItemResult::Missing, GONE.to_string()));
        }
        Err(_) => {
            return Err((
                ItemResult::Skipped,
                Refusal::Unresolvable.reason().to_string(),
            ));
        }
    };
    let skip = |r: Refusal| Err((ItemResult::Skipped, r.reason().to_string()));
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return skip(Refusal::Outside);
    };
    let Ok(parent) = parent.canonicalize() else {
        return skip(Refusal::Unresolvable);
    };
    match item.kind {
        LeftoverKind::System => Err((ItemResult::Skipped, NEEDS_ADMIN_NOTE.to_string())),
        LeftoverKind::App => {
            if !meta.is_dir() {
                return skip(Refusal::Linked);
            }
            let folders: Vec<PathBuf> = InstalledApps::folders(env)
                .iter()
                .filter_map(|f| f.canonicalize().ok())
                .collect();
            let in_folder = folders.contains(&parent)
                || (!is_app(&parent)
                    && parent
                        .parent()
                        .is_some_and(|g| folders.iter().any(|f| f == g)));
            if !in_folder {
                return skip(Refusal::Outside);
            }
            if bundle_id_of(path).as_deref() != Some(plan.app.bundle_id.as_str()) {
                return Err((ItemResult::Skipped, NOT_FOUND_AGAIN.to_string()));
            }
            if !plan.app.movable {
                return Err((ItemResult::Skipped, APP_NEEDS_ADMIN.to_string()));
            }
            Ok(parent.join(name))
        }
        kind => {
            let (Ok(root), Ok(rel)) = (env.root.canonicalize(), path.strip_prefix(&env.root))
            else {
                return skip(Refusal::Outside);
            };
            let folder = rel.parent().unwrap_or(Path::new(""));
            let expected = kind
                .folders()
                .iter()
                .map(|f| Path::new("Library").join(f))
                .find(|f| f == folder);
            match expected {
                Some(f) if root.join(&f) == parent => {}
                Some(_) => return skip(Refusal::Linked),
                None => return skip(Refusal::Outside),
            }
            let at = parent.join(name);
            if touches_tidy(env, &at) {
                return skip(Refusal::Denied);
            }
            Ok(at)
        }
    }
}

/// Moves `app` and the items named by `ids` to the Trash (C50). The leftovers are found again
/// first; an id that is not in that fresh look, or not movable, is skipped with
/// [`NOT_FOUND_AGAIN`]. While the app or a helper runs, every item is skipped (C54). The app
/// goes first, then the leftovers in list order. Writes one `uninstall` history entry (none
/// when `ids` is empty) and returns it.
pub fn uninstall(
    env: &Env,
    signer: &dyn Signatures,
    app: &Path,
    ids: &[String],
    events: &mut dyn FnMut(Event),
) -> HistoryEntry {
    uninstall_until(env, signer, app, ids, events, None)
}

/// [`uninstall`] that stops before the next item once `stop` is set (Tidy is quitting); the
/// items left are skipped.
pub fn uninstall_until(
    env: &Env,
    signer: &dyn Signatures,
    app: &Path,
    ids: &[String],
    events: &mut dyn FnMut(Event),
    stop: Option<&std::sync::atomic::AtomicBool>,
) -> HistoryEntry {
    let plan = find_leftovers(env, signer, app);
    let mut order: Vec<(String, Option<&Leftover>)> = Vec::new();
    if let Some(p) = &plan {
        for item in p.items.iter().filter(|i| i.selectable) {
            if ids.contains(&item.id) {
                order.push((item.id.clone(), Some(item)));
            }
        }
    }
    for id in ids {
        if !order.iter().any(|(o, _)| o == id) {
            order.push((id.clone(), None));
        }
    }
    let total = order.len();
    let mut out = Vec::new();
    let mut halted = false;
    for (n, (id, item)) in order.into_iter().enumerate() {
        let path = PathBuf::from(&id);
        let rule = item.map_or(LeftoverKind::App.rule(), |i| i.kind.rule());
        let bytes = item.and_then(|i| i.bytes).unwrap_or(0);
        let stopped = stop.is_some_and(|s| s.load(std::sync::atomic::Ordering::Relaxed));
        let at: Result<PathBuf, Refused> = match (item, &plan) {
            _ if stopped => Err((ItemResult::Skipped, STOPPED.to_string())),
            (Some(i), Some(p)) => {
                if let Some(blocked) = &p.blocked {
                    Err((ItemResult::Skipped, blocked.clone()))
                } else if halted {
                    Err((ItemResult::Skipped, IGNORE_LIST_UNREADABLE.to_string()))
                } else {
                    match MoveGate::read(env) {
                        Ok(gate) => guard(env, p, i).and_then(|at| gate.allows(&path, at)),
                        Err(reason) => {
                            halted = true;
                            Err((ItemResult::Skipped, reason))
                        }
                    }
                }
            }
            _ => Err((ItemResult::Skipped, NOT_FOUND_AGAIN.to_string())),
        };
        let is_app = item.is_some_and(|i| i.kind == LeftoverKind::App);
        match at.and_then(|at| move_checked(env, &at)) {
            Ok((trash_path, moved_id)) => {
                out.push(record(
                    &path,
                    trash_path.clone(),
                    moved_id,
                    bytes,
                    rule,
                    ItemResult::Trashed,
                    None,
                ));
                events(Event::ItemTrashed { id, trash_path });
            }
            Err((result, reason)) => {
                let reason = if is_app && result == ItemResult::Failed && refused_by_macos(&reason)
                {
                    APP_MOVE_REFUSED.to_string()
                } else {
                    reason
                };
                out.push(record(
                    &path,
                    None,
                    None,
                    bytes,
                    rule,
                    result,
                    Some(reason.clone()),
                ));
                events(Event::ItemSkipped { id, reason });
            }
        }
        events(Event::CleanProgress { done: n + 1, total });
    }
    let message = halted.then(|| IGNORE_LIST_STOPPED.to_string());
    finish(env, Action::Uninstall, Source::Uninstaller, out, message)
}

/// Whether a move error is macOS refusing access (the reason [`crate::trash::error_reason`]
/// gives for EPERM and EACCES).
fn refused_by_macos(reason: &str) -> bool {
    reason == crate::trash::error_reason(&io::Error::from_raw_os_error(libc::EPERM))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_refuses_a_folder_swapped_for_a_link_after_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        let (home, tidy) = (dir.path().join("home"), dir.path().join("tidy"));
        let app = home.join("Applications/Box.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>com.example.box</string></dict></plist>",
        )
        .unwrap();
        let cache = home.join("Library/Caches/com.example.box");
        std::fs::create_dir_all(&cache).unwrap();
        let env = Env::for_tests(&home, &tidy);
        let signer = crate::security::codesign::CodeSign;
        let plan = find_leftovers(&env, &signer, &app).unwrap();
        let item = plan.item(&cache.to_string_lossy()).unwrap().clone();
        assert!(guard(&env, &plan, &item).is_ok());

        // Between the plan and the move, Library/Caches becomes a link to somewhere else that
        // holds an entry with the same name.
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(outside.join("com.example.box")).unwrap();
        std::fs::remove_dir_all(home.join("Library/Caches")).unwrap();
        std::os::unix::fs::symlink(&outside, home.join("Library/Caches")).unwrap();
        let refused = guard(&env, &plan, &item).unwrap_err();
        assert_eq!(refused.0, ItemResult::Skipped);
        assert_eq!(refused.1, Refusal::Linked.reason());
    }

    #[test]
    fn hits_needs_a_dot_boundary() {
        assert!(hits("com.foo", "com.foo"));
        assert!(hits("com.foo.helper", "com.foo"));
        assert!(!hits("com.foobar", "com.foo"));
        assert!(!hits("com.fo", "com.foo"));
    }

    #[test]
    fn shared_notes_name_up_to_two_apps() {
        let n = |v: &[&str]| shared_note(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(n(&["Word"]), "Shared with Word");
        assert_eq!(n(&["Word", "Excel"]), "Shared with Word and Excel");
        assert_eq!(
            n(&["Word", "Excel", "Outlook", "Teams"]),
            "Shared with Word, Excel and 2 more"
        );
    }

    #[test]
    fn every_kind_has_a_label_and_a_rule() {
        use LeftoverKind::*;
        for k in [
            App,
            AppSupport,
            Caches,
            Preferences,
            Container,
            GroupContainer,
            AppScripts,
            SavedState,
            Logs,
            WebData,
            WebKit,
            LaunchAgent,
            System,
        ] {
            assert!(!k.label().is_empty());
            assert!(k.rule().starts_with("uninstall."));
        }
    }
}
