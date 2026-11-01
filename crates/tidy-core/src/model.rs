//! The value types shared by the core, tidyctl and the UI: cleanup plans, large-file scans,
//! the folder tree and history entries, and for M2 the memory snapshot, the security report
//! and the check up summary. Everything here is plain data with serde, so the CLI can print it
//! as JSON and the fixtures can write the expected results with the same types.
//!
//! Sizes are bytes. `bytes_alloc` is the space on disk (what moving the item to the Trash and
//! emptying the Trash would free), `bytes_logical` is the sum of file lengths. Times are Unix
//! seconds (UTC).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Seconds since the Unix epoch for `t`. Times before the epoch read as 0.
pub fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `SystemTime` for Unix seconds.
pub fn from_unix_secs(secs: u64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_secs(secs)
}

/// Formats Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (UTC), the form history ids use.
pub fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a (year, month, day) in the proleptic Gregorian calendar
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------- Cleanup ----------

/// The five Cleanup groups, in the order the screen shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    /// System and app caches.
    Caches,
    Logs,
    Developer,
    /// AI models and large app data. Every item is Review or Report only.
    AiModels,
    /// Old installers (`.dmg`, `.pkg`) in Downloads. Every item is Review: never ticked,
    /// never part of the one-click clean.
    Installers,
    /// Items Tidy moved to the Trash earlier. Emptied by its own confirmed action, never by
    /// Clean.
    Trash,
}

impl Group {
    /// Every group in display order.
    pub const ALL: [Group; 6] = [
        Group::Caches,
        Group::Logs,
        Group::Developer,
        Group::AiModels,
        Group::Installers,
        Group::Trash,
    ];

    /// The name on the group header.
    pub fn label(self) -> &'static str {
        match self {
            Group::Caches => "System and app caches",
            Group::Logs => "Logs",
            Group::Developer => "Developer",
            Group::AiModels => "AI models and large app data",
            Group::Installers => "Old installers",
            Group::Trash => "Trash",
        }
    }

    /// Whether the group header starts expanded (C5: AI models starts collapsed).
    pub fn starts_expanded(self) -> bool {
        self != Group::AiModels
    }
}

/// How an item may be cleaned. A fourth class, Excluded, never reaches a plan: an excluded
/// path is simply not an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// Ticked by default and part of "Clean safe items".
    Safe,
    /// Shown unticked with a "Review" tag and a reason. Cleaned only when the user ticks it,
    /// after a confirm dialog.
    Review,
    /// Shown with no checkbox and "Tidy won't remove this". Never cleaned.
    ReportOnly,
}

impl Class {
    /// The tag text on the row, if any.
    pub fn tag(self) -> Option<&'static str> {
        match self {
            Class::Safe => None,
            Class::Review => Some("Review"),
            Class::ReportOnly => Some("Report only"),
        }
    }

    /// Whether a row of this class has a checkbox at all.
    pub fn selectable(self) -> bool {
        self != Class::ReportOnly
    }
}

/// One row of a Cleanup plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Stable id: `<rule id>:<path relative to the scan root>`, for example
    /// `cache.app:Library/Caches/com.example.Idle`. The CLI and tests name items by it.
    pub id: String,
    pub group: Group,
    pub class: Class,
    /// Ticked in the plan as scanned: Safe and not blocked. The UI keeps its own ticks after
    /// that.
    pub ticked: bool,
    /// Why the item cannot be cleaned right now, shown in the warning color, for example
    /// "Quit Google Chrome first". A blocked item is unticked and disabled.
    #[serde(default)]
    pub blocked: Option<String>,
    /// The app or tool name when known, otherwise the folder name. For a folded row of small
    /// matches: "38 small items".
    pub name: String,
    /// The rule's one-line reason, for the meta line ("Cache, rebuilt by the app").
    pub reason: String,
    /// The folder or file this row moves to the Trash. For a folded row, the folder that holds
    /// the members.
    pub path: PathBuf,
    pub bytes_alloc: u64,
    pub bytes_logical: u64,
    /// The newest mtime of any file inside the target (Unix seconds), which the age rules use.
    /// None when the target holds no files.
    #[serde(default)]
    pub newest_mtime: Option<u64>,
    /// For a folded row ("38 small items, 12 MB"): the paths that clean as one unit. Empty for
    /// a plain row, which cleans `path` itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<PathBuf>,
}

impl Item {
    /// The rule id: the part of [`Item::id`] before the first `:`.
    pub fn rule_id(&self) -> &str {
        self.id.split_once(':').map_or(self.id.as_str(), |(r, _)| r)
    }

    /// The path relative to the scan root: the part of [`Item::id`] after the first `:`.
    pub fn rel_path(&self) -> &str {
        self.id.split_once(':').map_or("", |(_, p)| p)
    }

    /// Builds the id for a rule and a path relative to the scan root.
    pub fn make_id(rule: &str, rel: &Path) -> String {
        format!("{rule}:{}", rel.to_string_lossy())
    }

    /// Whether the user could tick it now: selectable and not blocked.
    pub fn enabled(&self) -> bool {
        self.class.selectable() && self.blocked.is_none()
    }

    /// The paths a clean of this row moves: the members of a folded row, or the path itself.
    pub fn paths(&self) -> Vec<PathBuf> {
        if self.members.is_empty() {
            vec![self.path.clone()]
        } else {
            self.members.clone()
        }
    }
}

/// Totals for the items Tidy moved to the Trash that are still there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashSummary {
    pub items: u64,
    pub bytes: u64,
}

/// The result of a Cleanup scan. A scan only produces a plan; nothing moves until the user
/// asks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The scan root the ids are relative to.
    pub root: PathBuf,
    /// When the scan finished.
    pub scanned_at: u64,
    /// Items sorted by group, then by size (largest first), then by id.
    pub items: Vec<Item>,
    /// How many files the scan measured.
    #[serde(default)]
    pub files_seen: u64,
    /// One plain line per folder the scan could not read ("Skipped Documents (access not
    /// allowed). ...").
    #[serde(default)]
    pub skipped: Vec<String>,
    /// The items from Tidy's earlier cleans that are still in the Trash.
    #[serde(default)]
    pub trash: TrashSummary,
}

impl Plan {
    /// An empty plan for `root`, scanned at `now`.
    pub fn empty(root: impl Into<PathBuf>, now: u64) -> Plan {
        Plan {
            root: root.into(),
            scanned_at: now,
            ..Plan::default()
        }
    }

    /// Puts the items in display order: group, then size (largest first), then id.
    pub fn sort(&mut self) {
        self.items.sort_by(|a, b| {
            a.group
                .cmp(&b.group)
                .then(b.bytes_alloc.cmp(&a.bytes_alloc))
                .then(a.id.cmp(&b.id))
        });
    }

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// The items ticked in the plan as scanned (Safe and not blocked).
    pub fn ticked(&self) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(|i| i.ticked)
    }

    /// The sum of ticked items: the "Clean 6.3 GB" figure right after a scan.
    pub fn safe_bytes(&self) -> u64 {
        self.ticked().map(|i| i.bytes_alloc).sum()
    }

    /// The sum of Review items: the "34 GB more to review" figure.
    pub fn review_bytes(&self) -> u64 {
        self.items
            .iter()
            .filter(|i| i.class == Class::Review)
            .map(|i| i.bytes_alloc)
            .sum()
    }

    /// The items of one group, in plan order.
    pub fn group(&self, group: Group) -> impl Iterator<Item = &Item> {
        self.items.iter().filter(move |i| i.group == group)
    }

    /// The groups that have at least one item, in display order. Empty groups are hidden.
    pub fn groups(&self) -> Vec<Group> {
        Group::ALL
            .into_iter()
            .filter(|g| self.items.iter().any(|i| i.group == *g))
            .collect()
    }
}

// ---------- Running processes ----------

/// Bundle ids and process names that are running. Matching ignores ASCII case, because bundle
/// ids are case-insensitive in practice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningSet {
    /// Lowercased bundle ids and process names.
    ids: BTreeSet<String>,
    /// True when the real process list could not be read. Every owner then counts as
    /// running, so nothing with an owner is cleaned on a guess.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    unknown: bool,
}

impl RunningSet {
    pub fn new<I, S>(ids: I) -> RunningSet
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        RunningSet {
            ids: ids
                .into_iter()
                .map(|s| s.as_ref().trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect(),
            unknown: false,
        }
    }

    /// Parses a comma-separated list, as `TIDY_TEST_RUNNING` gives it.
    pub fn parse(list: &str) -> RunningSet {
        RunningSet::new(list.split(','))
    }

    /// A set for "the process list is unknown": [`RunningSet::contains`] is always true.
    pub fn unknown() -> RunningSet {
        RunningSet {
            ids: BTreeSet::new(),
            unknown: true,
        }
    }

    pub fn is_unknown(&self) -> bool {
        self.unknown
    }

    /// Whether a bundle id or process name is running.
    pub fn contains(&self, id: &str) -> bool {
        self.unknown || self.ids.contains(&id.to_ascii_lowercase())
    }

    /// Whether any of `ids` is running.
    pub fn any<S: AsRef<str>>(&self, ids: &[S]) -> bool {
        ids.iter().any(|s| self.contains(s.as_ref()))
    }

    pub fn insert(&mut self, id: &str) {
        let id = id.trim().to_ascii_lowercase();
        if !id.is_empty() {
            self.ids.insert(id);
        }
    }

    /// The lowercased ids, sorted.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.ids.iter().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        !self.unknown && self.ids.is_empty()
    }
}

// ---------- Large files ----------

/// The default size threshold for Large files (500 MB).
pub const LARGE_DEFAULT_MIN: u64 = 500 * MB;
/// The smallest file a large scan records (C13), so every threshold in the menu can be
/// applied to a saved scan without rescanning.
pub const LARGE_RECORD_MIN: u64 = 50 * MB;
/// The threshold menu: 100 MB, 500 MB, 1 GB, 5 GB.
pub const LARGE_THRESHOLDS: [u64; 4] = [100 * MB, 500 * MB, 1000 * MB, 5000 * MB];
/// "Not opened lately" means older than this many days (C12).
pub const NOT_OPENED_DAYS: u32 = 90;

/// One megabyte as Finder counts it (10^6 bytes).
pub const MB: u64 = 1_000_000;

/// What a large item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LargeKind {
    File,
    /// A bundle (`.app`, `.framework` and so on): one item with the total of its contents.
    Package,
    Folder,
}

/// Where a last-use date came from, so the row can say "Opened" or "Modified".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UseSource {
    /// Spotlight's `kMDItemLastUsedDate`: "Opened 8 months ago".
    Spotlight,
    /// The later of mtime and atime: "Modified 2 years ago".
    FileTimes,
}

/// One row of the Files list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LargeItem {
    pub path: PathBuf,
    /// The path relative to the scan root, with `/` separators. Also the item's id.
    pub rel: String,
    pub name: String,
    pub kind: LargeKind,
    pub bytes_alloc: u64,
    pub bytes_logical: u64,
    /// The last-use date (Unix seconds). None when neither Spotlight nor the file times gave
    /// one; such an item counts as old.
    #[serde(default)]
    pub last_used: Option<u64>,
    pub used_source: UseSource,
    /// Under `Library`: the row carries the "App data" tag and the dialog warns that the app
    /// may stop working.
    #[serde(default)]
    pub app_data: bool,
}

impl LargeItem {
    /// Whether the item was not used for `days` days before `now`. Items with no date count
    /// as old.
    pub fn not_opened_since(&self, now: u64, days: u32) -> bool {
        let cutoff = now.saturating_sub(u64::from(days) * 86_400);
        self.last_used.is_none_or(|t| t < cutoff)
    }
}

/// One node of the folder tree that Folders mode drills into. Only children at or above the
/// scan's record minimum are kept; the rest are summed into `rest_count` and `rest_alloc`,
/// so a saved tree can be refiltered for any menu threshold.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderNode {
    pub name: String,
    /// Relative to the scan root, `/` separators, empty for the root itself.
    pub rel: String,
    pub kind: Option<LargeKind>,
    pub bytes_alloc: u64,
    pub bytes_logical: u64,
    /// Files counted inside (hard links once).
    pub files: u64,
    /// Children at or above the record minimum, largest first. Packages and files have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<FolderNode>,
    /// Children below the record minimum, folded: how many and their total.
    #[serde(default)]
    pub rest_count: u64,
    #[serde(default)]
    pub rest_alloc: u64,
}

impl FolderNode {
    /// The node at `rel` (`/` separators, empty for this node), if the tree kept it.
    pub fn find(&self, rel: &str) -> Option<&FolderNode> {
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            return Some(self);
        }
        let mut node = self;
        for part in rel.split('/').filter(|p| !p.is_empty()) {
            node = node.children.iter().find(|c| c.name == part)?;
        }
        Some(node)
    }

    /// The children at or above `min`, largest first, and the folded rest (count, bytes)
    /// that includes the children below `min`.
    pub fn split_at(&self, min: u64) -> (Vec<&FolderNode>, u64, u64) {
        let mut shown = Vec::new();
        let mut count = self.rest_count;
        let mut bytes = self.rest_alloc;
        for c in &self.children {
            if c.bytes_alloc >= min {
                shown.push(c);
            } else {
                count += 1;
                bytes += c.bytes_alloc;
            }
        }
        shown.sort_by(|a, b| b.bytes_alloc.cmp(&a.bytes_alloc).then(a.name.cmp(&b.name)));
        (shown, count, bytes)
    }
}

/// The result of a Large files scan, saved to `last-large.json` so later visits reuse it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LargeScan {
    pub root: PathBuf,
    pub scanned_at: u64,
    /// The smallest file or package recorded in `items` and kept in `tree`.
    pub record_min: u64,
    /// Files and packages at or above `record_min`, largest first. Hard-linked files appear
    /// once.
    pub items: Vec<LargeItem>,
    /// Folder totals from the scan root down.
    pub tree: FolderNode,
    #[serde(default)]
    pub files_seen: u64,
    /// Plain lines for folders the scan could not read.
    #[serde(default)]
    pub skipped: Vec<String>,
}

impl LargeScan {
    /// An empty scan of `root` at `now`.
    pub fn empty(root: impl Into<PathBuf>, now: u64, record_min: u64) -> LargeScan {
        LargeScan {
            root: root.into(),
            scanned_at: now,
            record_min,
            ..LargeScan::default()
        }
    }

    /// Biggest: items at or above `min`, largest first.
    pub fn biggest(&self, min: u64) -> Vec<&LargeItem> {
        let mut v: Vec<&LargeItem> = self.items.iter().filter(|i| i.bytes_alloc >= min).collect();
        v.sort_by(|a, b| b.bytes_alloc.cmp(&a.bytes_alloc).then(a.rel.cmp(&b.rel)));
        v
    }

    /// Not opened lately: [`LargeScan::biggest`] keeping only items not used for `days`
    /// days before `now`.
    pub fn not_opened(&self, min: u64, now: u64, days: u32) -> Vec<&LargeItem> {
        self.biggest(min)
            .into_iter()
            .filter(|i| i.not_opened_since(now, days))
            .collect()
    }
}

// ---------- History ----------

/// What a history entry records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Clean on the Cleanup screen (or `tidyctl clean`).
    Clean,
    /// Move to Trash on the Large files screen (or `tidyctl trash`).
    Trash,
    /// Empty Trash: permanent deletion of the items Tidy moved to the Trash.
    EmptyTrash,
    /// Uninstall on the Uninstaller screen (or `tidyctl uninstall`): an app and its
    /// leftovers to the Trash.
    Uninstall,
}

/// Which screen started the action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Cleanup,
    Large,
    Uninstaller,
}

/// Which Trash the items went to, so a reader can prove a test never used the real one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrasherKind {
    /// The macOS Trash through NSFileManager.
    System,
    /// A plain folder (`$TIDY_HOME/data/trash`), or a test double.
    Dir,
}

/// What happened to one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemResult {
    /// Moved to the Trash; `trash_path` says where.
    Trashed,
    /// A check failed right before the move; `reason` says which.
    Skipped,
    /// The move or the deletion failed; `reason` holds the message.
    Failed,
    /// Deleted for good by Empty Trash.
    Deleted,
    /// Already gone.
    Missing,
}

/// Which file system object sits at a path: its device, inode and birth time. Recorded for
/// each item right after it reaches the Trash, so that Empty Trash can tell the item Tidy
/// moved from a different one that later took the same name in the Trash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
    /// The birth time in nanoseconds since 1970.
    pub birth_ns: i64,
}

impl FileId {
    /// The identity of what is at `path` itself (a symlink is not followed).
    pub fn of(path: &Path) -> std::io::Result<FileId> {
        use std::os::macos::fs::MetadataExt;
        let m = path.symlink_metadata()?;
        Ok(FileId {
            dev: m.st_dev(),
            ino: m.st_ino(),
            birth_ns: m
                .st_birthtime()
                .saturating_mul(1_000_000_000)
                .saturating_add(m.st_birthtime_nsec()),
        })
    }
}

/// One item of a history entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryItem {
    /// The original path.
    pub path: PathBuf,
    /// Where it went in the Trash (from `trashItemAtURL`'s `resultingItemURL`), or None.
    pub trash_path: Option<PathBuf>,
    pub bytes: u64,
    /// The rule id for a cleanup item, `large` for a Large files item, `trash` for Empty Trash.
    pub rule: String,
    pub result: ItemResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What landed at `trash_path`, read right after the move, or read from the original path
    /// right before the move when the Trash path cannot be read. Empty Trash deletes a Trash
    /// path only while it still holds this same object. None when neither could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trash_id: Option<FileId>,
}

/// One line of `history.jsonl`: one user action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// `<UTC time>-<n>`, for example `2026-10-23T14:02:11Z-1`. See [`HistoryEntry::make_id`].
    pub id: String,
    /// Unix seconds.
    pub at: u64,
    pub action: Action,
    pub source: Source,
    pub trasher: TrasherKind,
    pub items: Vec<HistoryItem>,
    /// The bytes that reached the Trash (or, for Empty Trash, that were deleted).
    pub trashed_bytes: u64,
    /// A message for the whole action, such as the Empty Trash refusal from macOS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl HistoryEntry {
    /// The id for the `seq`-th entry written at `at` (seq starts at 1).
    pub fn make_id(at: u64, seq: u32) -> String {
        format!("{}-{seq}", format_utc(at))
    }

    /// How many items were skipped or failed: the "3 skipped" on the History row.
    pub fn skipped(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i.result, ItemResult::Skipped | ItemResult::Failed))
            .count()
    }

    /// The items that reached the Trash.
    pub fn trashed(&self) -> impl Iterator<Item = &HistoryItem> {
        self.items
            .iter()
            .filter(|i| i.result == ItemResult::Trashed)
    }
}

// ---------- Dates for M2 ----------

/// (year, month, day) to days since 1970-01-01 in the proleptic Gregorian calendar (Howard
/// Hinnant's `days_from_civil`).
#[cfg(test)]
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The id of a Security run taken at Unix seconds `secs`: its UTC time as `YYYYMMDD-HHMMSS`
/// (for example `20261023-140211`). Ids sort in time order, and each run is saved under its
/// id (see [`crate::paths::AppPaths::security_file`]).
pub fn run_id(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let t = secs % 86_400;
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

// ---------- Memory (M2) ----------

/// Apps and processes the Memory screen never offers to quit (design section 5.3), by
/// process name or bundle id, compared ignoring ASCII case. Tidy itself is left out by pid
/// ([`crate::env::MemoryProbe::own_pid`]) and by [`TIDY_BUNDLE_ID`].
pub const NEVER_QUIT: &[&str] = &[
    "Finder",
    "com.apple.finder",
    "WindowServer",
    "loginwindow",
    "com.apple.loginwindow",
];

/// Tidy's bundle id.
pub const TIDY_BUNDLE_ID: &str = "dev.sdusteric.tidy";

/// The name of the row that sums the processes Tidy cannot read (owned by root).
pub const SYSTEM_PROCESSES: &str = "System processes";

/// Memory pressure as macOS reports it in `kern.memorystatus_vm_pressure_level`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Pressure {
    #[default]
    Normal,
    Warning,
    Critical,
}

impl Pressure {
    /// From the sysctl value: 1 is normal, 2 warning, 4 critical. 0 or an unknown value
    /// below 2 reads as Normal, so an odd value never raises an alarm.
    pub fn from_level(level: u32) -> Pressure {
        match level {
            4.. => Pressure::Critical,
            2 | 3 => Pressure::Warning,
            _ => Pressure::Normal,
        }
    }

    /// The figure: "Normal", "Warning" or "Critical".
    pub fn label(self) -> &'static str {
        match self {
            Pressure::Normal => "Normal",
            Pressure::Warning => "Warning",
            Pressure::Critical => "Critical",
        }
    }

    /// The verdict line at the top of the Memory screen and on the Check up card.
    pub fn verdict(self) -> &'static str {
        match self {
            Pressure::Normal => {
                "Memory is fine. macOS is using spare memory for caching, which is normal."
            }
            Pressure::Warning => "Memory is tight. Quitting a large app below will help.",
            Pressure::Critical => {
                "Memory is very tight. Your Mac is moving memory to disk, which slows it down."
            }
        }
    }
}

/// Swap use from `vm.swapusage`, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SwapUsage {
    pub total: u64,
    pub used: u64,
    pub free: u64,
}

/// Page counts from `host_statistics64(HOST_VM_INFO64)`, the numbers `vm_stat` prints. Every
/// count is in pages of `page_size` bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageStats {
    /// `vm_page_size` (16384 on Apple silicon).
    pub page_size: u64,
    pub free: u64,
    pub active: u64,
    pub inactive: u64,
    pub speculative: u64,
    pub wired: u64,
    pub purgeable: u64,
    /// File-backed pages ("File-backed pages" in `vm_stat`): the file cache.
    pub external: u64,
    /// Anonymous pages ("Anonymous pages").
    pub internal: u64,
    /// Pages the compressor occupies ("Pages occupied by compressor"). Times `page_size` this
    /// is the Compressed figure.
    pub compressor: u64,
    /// Pages held in the compressor before compression ("Pages stored in compressor").
    pub compressed: u64,
    pub swapins: u64,
    pub swapouts: u64,
}

impl PageStats {
    /// `pages` in bytes.
    pub fn bytes(&self, pages: u64) -> u64 {
        pages.saturating_mul(self.page_size)
    }

    /// The memory the compressor uses: [`PageStats::compressor`] times the page size.
    pub fn compressed_bytes(&self) -> u64 {
        self.bytes(self.compressor)
    }
}

/// The system-wide numbers a [`crate::env::MemoryProbe`] reads in one go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VmStats {
    /// The raw `kern.memorystatus_vm_pressure_level` (1, 2 or 4).
    pub pressure_level: u32,
    /// `hw.memsize`.
    pub total_bytes: u64,
    pub swap: SwapUsage,
    pub pages: PageStats,
}

/// One row of a process table, as a [`crate::env::MemoryProbe`] reports it. The memory engine
/// groups these into [`AppMemory`] rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcInfo {
    pub pid: i32,
    /// The parent pid (1 for processes started by launchd).
    pub ppid: i32,
    /// From `responsibility_get_pid_responsible_for_pid` (looked up with `dlsym`). None when
    /// the function is missing or fails; grouping then falls back to the parent chain and
    /// the `.app` prefix of `path`.
    #[serde(default)]
    pub responsible: Option<i32>,
    pub uid: u32,
    /// `proc_name`, or the executable's file name.
    pub name: String,
    /// `proc_pidpath`. None when it cannot be read.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// `ri_phys_footprint` from `proc_pid_rusage`, in bytes. None when it cannot be read,
    /// which is the case for root-owned processes and other users' processes; those go to
    /// the [`SYSTEM_PROCESSES`] row.
    #[serde(default)]
    pub footprint: Option<u64>,
    /// For a process whose `footprint` is None: the footprint `/usr/bin/top` reported for it
    /// (its MEM column), in bytes. `top` is setuid root, so it can read what an app cannot.
    /// None when `top` did not list the process or could not run. The System processes row
    /// sums these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_footprint: Option<u64>,
}

/// What a Memory row stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// An app bundle with its helper processes.
    App,
    /// A process Tidy could not attribute to an app; the row shows the process name.
    Process,
    /// The one "System processes" row: everything Tidy cannot read. No Quit button.
    System,
}

/// One row of "Top apps by memory".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppMemory {
    /// Stable across refreshes, so the screen can keep "Quit requested" on the right row:
    /// `app:<path of the .app>`, `pid:<pid>` or `system`.
    pub id: String,
    /// The app's name (the `.app` folder without the extension), the process name, or
    /// [`SYSTEM_PROCESSES`].
    pub name: String,
    pub kind: MemoryKind,
    /// The main process: the one Quit is sent to. 0 for the System row.
    pub pid: i32,
    /// Every process in the row, the main one first.
    pub pids: Vec<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    /// The `.app` bundle, for the icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_path: Option<PathBuf>,
    /// The summed physical footprint of `pids`, in bytes. For the System row it is the sum of
    /// the sizes `top` reported for its processes ([`ProcInfo::top_footprint`]), and 0 when
    /// `top` gave none.
    pub footprint: u64,
    /// Processes besides the main one (the "3 helpers" on the row).
    pub helpers: u32,
    /// Whether the row has a Quit button: false for Tidy, [`NEVER_QUIT`] and the System row.
    pub quittable: bool,
    /// For the System row: the names of the processes in it, once each, the large macOS ones
    /// people know (WindowServer, kernel_task) first, then the rest largest first by the size
    /// `top` gave and by name. The row is one sum, so the names say what is inside it. Empty
    /// for the other rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
}

impl AppMemory {
    /// Whether `name_or_id` (a process name or bundle id) is on [`NEVER_QUIT`] or is Tidy.
    pub fn never_quit(name_or_id: &str) -> bool {
        name_or_id.eq_ignore_ascii_case(TIDY_BUNDLE_ID)
            || NEVER_QUIT
                .iter()
                .any(|n| n.eq_ignore_ascii_case(name_or_id))
    }
}

/// A Memory screen refresh, or the Memory part of a check up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemorySnapshot {
    /// Unix seconds.
    pub at: u64,
    pub pressure: Pressure,
    /// The raw `kern.memorystatus_vm_pressure_level`.
    pub pressure_level: u32,
    /// `hw.memsize`.
    pub total_bytes: u64,
    pub swap: SwapUsage,
    /// Compressor pages times page size: the Compressed figure.
    pub compressed_bytes: u64,
    pub pages: PageStats,
    /// Rows by footprint, largest first. The System row is included where its size puts it.
    pub apps: Vec<AppMemory>,
}

impl MemorySnapshot {
    /// A snapshot with no numbers and no rows, taken at `at`.
    pub fn empty(at: u64) -> MemorySnapshot {
        MemorySnapshot {
            at,
            ..MemorySnapshot::default()
        }
    }

    /// The figures of `vm` with no rows yet.
    pub fn from_vm(at: u64, vm: &VmStats) -> MemorySnapshot {
        MemorySnapshot {
            at,
            pressure: Pressure::from_level(vm.pressure_level),
            pressure_level: vm.pressure_level,
            total_bytes: vm.total_bytes,
            swap: vm.swap,
            compressed_bytes: vm.pages.compressed_bytes(),
            pages: vm.pages,
            apps: Vec::new(),
        }
    }

    /// The verdict line for this snapshot's pressure.
    pub fn verdict(&self) -> &'static str {
        self.pressure.verdict()
    }

    pub fn app(&self, id: &str) -> Option<&AppMemory> {
        self.apps.iter().find(|a| a.id == id)
    }
}

// ---------- Security (M2) ----------

/// What the first run says, since it has nothing to compare against (design section 5.4).
pub const FIRST_CHECK_SENTENCE: &str =
    "This is your first check. Next time Tidy will show what changed.";

/// The checks of design section 5.4, in the order the Security screen lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CheckId {
    /// `fdesetup status`.
    #[serde(rename = "filevault")]
    FileVault,
    /// `socketfilterfw --getglobalstate`.
    #[serde(rename = "firewall")]
    Firewall,
    /// `spctl --status`.
    #[serde(rename = "gatekeeper")]
    Gatekeeper,
    /// `csrutil status`.
    #[serde(rename = "sip")]
    Sip,
    /// The date and version of `XProtect.bundle/Contents/Info.plist`.
    #[serde(rename = "xprotect")]
    XProtect,
    /// `softwareupdate --list`, only when asked for (it is slow and goes online).
    #[serde(rename = "macos_updates")]
    MacosUpdates,
    /// Launch agent and daemon plists and `launchctl print gui/<uid>`.
    #[serde(rename = "login_items")]
    LoginItems,
    /// The `Secure Preferences` of every Chrome profile.
    #[serde(rename = "chrome_extensions")]
    ChromeExtensions,
}

impl CheckId {
    /// Every check, in screen order.
    pub const ALL: [CheckId; 8] = [
        CheckId::FileVault,
        CheckId::Firewall,
        CheckId::Gatekeeper,
        CheckId::Sip,
        CheckId::XProtect,
        CheckId::MacosUpdates,
        CheckId::LoginItems,
        CheckId::ChromeExtensions,
    ];

    /// The row title.
    pub fn title(self) -> &'static str {
        match self {
            CheckId::FileVault => "FileVault",
            CheckId::Firewall => "Firewall",
            CheckId::Gatekeeper => "Gatekeeper",
            CheckId::Sip => "System Integrity Protection",
            CheckId::XProtect => "XProtect",
            CheckId::MacosUpdates => "macOS updates",
            CheckId::LoginItems => "Login items and background helpers",
            CheckId::ChromeExtensions => "Chrome extensions",
        }
    }

    /// The System Settings page an amber row's button opens, per the table in 5.4. None for
    /// SIP (turned back on in Recovery) and Chrome extensions (only Chrome can open its page).
    pub fn link(self) -> Option<SettingsLink> {
        match self {
            CheckId::FileVault | CheckId::Gatekeeper => Some(SettingsLink::PrivacySecurity),
            CheckId::Firewall => Some(SettingsLink::Network),
            CheckId::XProtect | CheckId::MacosUpdates => Some(SettingsLink::SoftwareUpdate),
            CheckId::LoginItems => Some(SettingsLink::LoginItems),
            CheckId::Sip | CheckId::ChromeExtensions => None,
        }
    }
}

/// The status dot of a check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// As it should be.
    Green,
    /// Something to look at. The sentence says what is off and what it means.
    Amber,
    /// Could not check. The sentence says why. Never a button that asks for access.
    Gray,
}

/// A System Settings page a button opens. The app opens the URL only from a click.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsLink {
    PrivacySecurity,
    /// The Network page. The firewall has its own page under it, but its URL anchor is not
    /// verified, so the button opens Network.
    Network,
    SoftwareUpdate,
    LoginItems,
    /// Notifications, for the weekly reminder when permission is denied.
    Notifications,
}

impl SettingsLink {
    /// The `x-apple.systempreferences:` URL (macOS 13 and later).
    pub fn url(self) -> &'static str {
        match self {
            SettingsLink::PrivacySecurity => {
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension"
            }
            SettingsLink::Network => {
                "x-apple.systempreferences:com.apple.Network-Settings.extension"
            }
            SettingsLink::SoftwareUpdate => {
                "x-apple.systempreferences:com.apple.Software-Update-Settings.extension"
            }
            SettingsLink::LoginItems => {
                "x-apple.systempreferences:com.apple.LoginItems-Settings.extension"
            }
            SettingsLink::Notifications => {
                "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=dev.sdusteric.tidy"
            }
        }
    }

    /// The button label.
    pub fn label(self) -> &'static str {
        match self {
            SettingsLink::PrivacySecurity => "Open Privacy & Security",
            SettingsLink::Network => "Open Network Settings",
            SettingsLink::SoftwareUpdate => "Open Software Update",
            SettingsLink::LoginItems => "Open Login Items",
            SettingsLink::Notifications => "Open Notifications Settings",
        }
    }
}

/// How a row compares with the last check's results.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    /// Not in the last check's results.
    New,
    /// In the last check's results with a different status (a check) or program or signer (a
    /// login item).
    Changed,
}

impl Mark {
    /// The tag text: "New" or "Changed".
    pub fn label(self) -> &'static str {
        match self {
            Mark::New => "New",
            Mark::Changed => "Changed",
        }
    }
}

/// One row of the Security screen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: CheckId,
    pub status: Status,
    /// Usually [`CheckId::title`].
    pub title: String,
    /// One plain sentence, at most two: for green what is on, for amber what is off and what
    /// it means, for gray why Tidy could not check. No scare words (the copy rules of design
    /// section 5.4).
    pub sentence: String,
    /// The button of an amber row. None for green and gray rows and for checks without a
    /// page ([`CheckId::link`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<SettingsLink>,
    /// A short fact compared week over week, such as the XProtect version or the update
    /// names. Not shown as a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<Mark>,
}

impl CheckResult {
    /// A row with the check's title, no detail and no mark. The action is the check's link
    /// when `status` is amber, otherwise None.
    pub fn new(id: CheckId, status: Status, sentence: impl Into<String>) -> CheckResult {
        CheckResult {
            id,
            status,
            title: id.title().to_string(),
            sentence: sentence.into(),
            action: if status == Status::Amber {
                id.link()
            } else {
                None
            },
            detail: None,
            mark: None,
        }
    }
}

/// Whose a launch agent or daemon is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// `~/Library/LaunchAgents` or the user's launchd domain. Only these can go to the Trash.
    User,
    /// `/Library/LaunchAgents` or `/Library/LaunchDaemons`.
    System,
}

/// Whether a launchd job runs per user (agent) or for the whole Mac (daemon).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchKind {
    Agent,
    Daemon,
}

/// Who signed a program.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signer {
    /// Signed by Apple (platform binary or Apple certificate).
    Apple,
    /// A Developer ID or App Store signature: the team name (or team id when the name is
    /// not available).
    Team(String),
    /// No signature, or an ad-hoc one.
    Unsigned,
    /// The program is missing or could not be checked.
    Unknown,
}

impl Signer {
    /// What the row shows: "Apple", the team name, "Not signed" or "Unknown".
    pub fn label(&self) -> &str {
        match self {
            Signer::Apple => "Apple",
            Signer::Team(name) => name,
            Signer::Unsigned => "Not signed",
            Signer::Unknown => "Unknown",
        }
    }
}

/// A launch agent or daemon: a login item or background helper.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginItem {
    /// The plist's `Label`.
    pub label: String,
    /// The plist it came from. None for a job only `launchctl print` lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plist: Option<PathBuf>,
    /// `Program`, or the first item of `ProgramArguments`. None when the plist names
    /// neither (for example a bundled helper registered by an app).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<PathBuf>,
    /// `RunAtLoad`.
    pub runs_at_load: bool,
    pub scope: Scope,
    pub kind: LaunchKind,
    pub signer: Signer,
    /// From `SecAssessment` (or the stapled ticket). None when it could not be checked or
    /// does not apply (Apple programs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notarized: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<Mark>,
}

impl LoginItem {
    /// The key that matches an item with the last check's: scope and label.
    pub fn key(&self) -> String {
        let scope = match self.scope {
            Scope::User => "user",
            Scope::System => "system",
        };
        format!("{scope}:{}", self.label)
    }

    /// Whether the item makes the Login items check amber: new since the last check, not
    /// signed, or not notarized.
    pub fn needs_a_look(&self) -> bool {
        self.mark == Some(Mark::New)
            || self.signer == Signer::Unsigned
            || self.notarized == Some(false)
    }

    /// Whether "Move to Trash" is offered: user agents with a plist only.
    pub fn can_trash(&self) -> bool {
        self.scope == Scope::User && self.kind == LaunchKind::Agent && self.plist.is_some()
    }
}

/// A Chrome extension from a profile's `Secure Preferences`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChromeExtension {
    /// The 32-letter extension id.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The profile folder name (`Default`, `Profile 1`).
    pub profile: String,
    /// False when the extension is installed but turned off.
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<Mark>,
}

impl ChromeExtension {
    /// The key that matches an extension with the last check's: profile and id.
    pub fn key(&self) -> String {
        format!("{}/{}", self.profile, self.id)
    }
}

/// One Security run, saved as `security/<id>.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityReport {
    /// [`run_id`] of `at`, the file name the report is saved under. Reports saved before
    /// M2 shipped named this `week`.
    #[serde(alias = "week")]
    pub id: String,
    /// Unix seconds.
    pub at: u64,
    /// No earlier check to compare against: the screen shows [`FIRST_CHECK_SENTENCE`] and
    /// nothing is marked.
    pub first_run: bool,
    /// The id of the run the marks compare against, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compared_to: Option<String>,
    /// When that run happened (Unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compared_at: Option<u64>,
    /// Whether `softwareupdate --list` ran. When false the macOS updates row is gray and
    /// says it is checked on demand.
    pub updates_checked: bool,
    /// One row per check, in [`CheckId::ALL`] order.
    pub checks: Vec<CheckResult>,
    /// New items first, then by label.
    pub login_items: Vec<LoginItem>,
    pub extensions: Vec<ChromeExtension>,
}

impl SecurityReport {
    /// A report with no rows, taken at `at`.
    pub fn empty(at: u64) -> SecurityReport {
        SecurityReport {
            id: run_id(at),
            at,
            ..SecurityReport::default()
        }
    }

    pub fn check(&self, id: CheckId) -> Option<&CheckResult> {
        self.checks.iter().find(|c| c.id == id)
    }

    /// How many checks are amber: "2 things to look at".
    pub fn amber(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == Status::Amber)
            .count()
    }

    /// How many checks could not run.
    pub fn gray(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == Status::Gray)
            .count()
    }

    /// "All good", "1 thing to look at" or "N things to look at".
    pub fn headline(&self) -> String {
        security_headline(self.amber())
    }
}

/// The Security summary for `amber` amber checks.
pub fn security_headline(amber: usize) -> String {
    match amber {
        0 => "All good".to_string(),
        1 => "1 thing to look at".to_string(),
        n => format!("{n} things to look at"),
    }
}

// ---------- Check up (M2) ----------

/// The Cleanup card of a check up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckupCleanup {
    /// The ticked total: "6.3 GB ready to clean".
    pub safe_bytes: u64,
    pub safe_items: usize,
    /// The Review total.
    pub review_bytes: u64,
}

impl CheckupCleanup {
    pub fn from_plan(plan: &Plan) -> CheckupCleanup {
        CheckupCleanup {
            safe_bytes: plan.safe_bytes(),
            safe_items: plan.ticked().count(),
            review_bytes: plan.review_bytes(),
        }
    }
}

/// The Security card of a check up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckupSecurity {
    /// The report's id, so the card can open that report.
    #[serde(alias = "week")]
    pub id: String,
    pub amber: usize,
    pub gray: usize,
    pub first_run: bool,
}

impl CheckupSecurity {
    pub fn from_report(report: &SecurityReport) -> CheckupSecurity {
        CheckupSecurity {
            id: report.id.clone(),
            amber: report.amber(),
            gray: report.gray(),
            first_run: report.first_run,
        }
    }

    /// "All good" or "2 things to look at".
    pub fn headline(&self) -> String {
        security_headline(self.amber)
    }
}

/// The Memory card of a check up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckupMemory {
    pub pressure: Pressure,
    pub swap_used: u64,
    pub compressed_bytes: u64,
}

impl CheckupMemory {
    pub fn from_snapshot(s: &MemorySnapshot) -> CheckupMemory {
        CheckupMemory {
            pressure: s.pressure,
            swap_used: s.swap.used,
            compressed_bytes: s.compressed_bytes,
        }
    }

    /// The verdict line.
    pub fn verdict(&self) -> &'static str {
        self.pressure.verdict()
    }
}

/// One check up: one line of `checkups.jsonl`. A part that failed or was stopped is None
/// and its card says so.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkup {
    /// Unix seconds when it finished. The weekly reminder counts from here.
    pub at: u64,
    /// Whether "Include macOS updates" was ticked, so `softwareupdate --list` ran.
    #[serde(default)]
    pub include_updates: bool,
    #[serde(default)]
    pub cleanup: Option<CheckupCleanup>,
    #[serde(default)]
    pub security: Option<CheckupSecurity>,
    #[serde(default)]
    pub memory: Option<CheckupMemory>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, group: Group, class: Class, bytes: u64) -> Item {
        Item {
            id: id.into(),
            group,
            class,
            ticked: class == Class::Safe,
            blocked: None,
            name: id.into(),
            reason: "r".into(),
            path: PathBuf::from("/h").join(id.split_once(':').unwrap().1),
            bytes_alloc: bytes,
            bytes_logical: bytes,
            newest_mtime: Some(1),
            members: Vec::new(),
        }
    }

    #[test]
    fn utc_format_matches_the_doc_example() {
        assert_eq!(format_utc(1_792_764_131), "2026-10-23T14:02:11Z");
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        // A leap day and the end of a leap year.
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_735_689_599), "2024-12-31T23:59:59Z");
        assert_eq!(
            HistoryEntry::make_id(1_792_764_131, 1),
            "2026-10-23T14:02:11Z-1"
        );
    }

    #[test]
    fn unix_secs_round_trip() {
        assert_eq!(unix_secs(from_unix_secs(1_792_764_131)), 1_792_764_131);
        assert_eq!(unix_secs(UNIX_EPOCH - std::time::Duration::from_secs(5)), 0);
    }

    #[test]
    fn item_id_parts() {
        let i = item(
            "cache.app:Library/Caches/a:b",
            Group::Caches,
            Class::Safe,
            1,
        );
        assert_eq!(i.rule_id(), "cache.app");
        assert_eq!(i.rel_path(), "Library/Caches/a:b");
        assert_eq!(
            Item::make_id("dev.npm", Path::new(".npm/_cacache")),
            "dev.npm:.npm/_cacache"
        );
        assert_eq!(i.paths(), vec![PathBuf::from("/h/Library/Caches/a:b")]);
        let mut folded = i.clone();
        folded.members = vec!["/h/x".into(), "/h/y".into()];
        assert_eq!(folded.paths().len(), 2);
    }

    #[test]
    fn serde_names_are_snake_case() {
        assert_eq!(
            serde_json::to_string(&Class::ReportOnly).unwrap(),
            "\"report_only\""
        );
        assert_eq!(
            serde_json::to_string(&Group::AiModels).unwrap(),
            "\"ai_models\""
        );
        assert_eq!(
            serde_json::to_string(&Action::EmptyTrash).unwrap(),
            "\"empty_trash\""
        );
        assert_eq!(serde_json::to_string(&TrasherKind::Dir).unwrap(), "\"dir\"");
        assert_eq!(
            serde_json::to_string(&ItemResult::Trashed).unwrap(),
            "\"trashed\""
        );
        assert_eq!(
            serde_json::to_string(&UseSource::FileTimes).unwrap(),
            "\"file_times\""
        );
    }

    #[test]
    fn plan_sorts_and_totals() {
        let mut p = Plan::empty("/h", 10);
        p.items = vec![
            item("dev.npm:.npm/_cacache", Group::Developer, Class::Safe, 5),
            item("cache.app:Library/Caches/b", Group::Caches, Class::Safe, 2),
            item("cache.app:Library/Caches/a", Group::Caches, Class::Safe, 9),
            item(
                "cache.unknown:Library/Caches/u",
                Group::Caches,
                Class::Review,
                7,
            ),
            item(
                "ai.hf:.cache/huggingface/hub/models--x",
                Group::AiModels,
                Class::Review,
                100,
            ),
            item("vm:.colima", Group::AiModels, Class::ReportOnly, 1000),
        ];
        p.items[1].blocked = Some("Quit B first".into());
        p.items[1].ticked = false;
        p.sort();
        let ids: Vec<&str> = p.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "cache.app:Library/Caches/a",
                "cache.unknown:Library/Caches/u",
                "cache.app:Library/Caches/b",
                "dev.npm:.npm/_cacache",
                "vm:.colima",
                "ai.hf:.cache/huggingface/hub/models--x",
            ]
        );
        assert_eq!(p.safe_bytes(), 9 + 5);
        assert_eq!(p.review_bytes(), 7 + 100);
        assert_eq!(
            p.groups(),
            [Group::Caches, Group::Developer, Group::AiModels]
        );
        assert_eq!(p.group(Group::Caches).count(), 3);
        assert!(!p.item("cache.app:Library/Caches/b").unwrap().enabled());
        assert!(!p.item("vm:.colima").unwrap().enabled());
        assert!(p.item("cache.unknown:Library/Caches/u").unwrap().enabled());
        assert!(p.item("missing:x").is_none());
    }

    #[test]
    fn plan_round_trips_and_old_fields_default() {
        let mut p = Plan::empty("/h", 10);
        p.items.push(item(
            "dev.npm:.npm/_cacache",
            Group::Developer,
            Class::Safe,
            5,
        ));
        p.trash = TrashSummary { items: 2, bytes: 3 };
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("members"), "empty members are not written");
        assert_eq!(serde_json::from_str::<Plan>(&json).unwrap(), p);
        let minimal = r#"{"root":"/h","scanned_at":1,"items":[]}"#;
        let back: Plan = serde_json::from_str(minimal).unwrap();
        assert_eq!(back.trash, TrashSummary::default());
        assert!(back.skipped.is_empty());
    }

    #[test]
    fn group_labels_and_expansion() {
        assert_eq!(
            Group::ALL.map(Group::label)[3],
            "AI models and large app data"
        );
        assert!(!Group::AiModels.starts_expanded());
        assert!(Group::Caches.starts_expanded());
        assert_eq!(Class::Review.tag(), Some("Review"));
        assert_eq!(Class::Safe.tag(), None);
    }

    #[test]
    fn running_set_matching() {
        let r = RunningSet::parse("com.google.Chrome, npm ,,Xcode");
        assert!(r.contains("com.google.chrome"));
        assert!(r.contains("COM.GOOGLE.CHROME"));
        assert!(r.contains("npm"));
        assert!(r.contains("xcode"));
        assert!(!r.contains("cargo"));
        assert!(r.any(&["cargo", "npm"]));
        assert_eq!(r.ids().count(), 3);
        assert!(RunningSet::default().is_empty());
        let u = RunningSet::unknown();
        assert!(u.contains("anything"));
        assert!(!u.is_empty());
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("unknown"));
        assert_eq!(serde_json::from_str::<RunningSet>(&json).unwrap(), r);
        let mut m = RunningSet::default();
        m.insert(" Ollama ");
        m.insert("");
        assert_eq!(m.ids().collect::<Vec<_>>(), ["ollama"]);
    }

    fn large(rel: &str, bytes: u64, used: Option<u64>) -> LargeItem {
        LargeItem {
            path: PathBuf::from("/h").join(rel),
            rel: rel.into(),
            name: rel.rsplit('/').next().unwrap().into(),
            kind: LargeKind::File,
            bytes_alloc: bytes,
            bytes_logical: bytes,
            last_used: used,
            used_source: UseSource::FileTimes,
            app_data: rel.starts_with("Library/"),
        }
    }

    #[test]
    fn large_filters() {
        let now = 1_000 * 86_400;
        let mut s = LargeScan::empty("/h", now, LARGE_RECORD_MIN);
        s.items = vec![
            large("Movies/old.mov", 900 * MB, Some(now - 200 * 86_400)),
            large("Movies/new.mov", 2000 * MB, Some(now - 3 * 86_400)),
            large("a.zip", 120 * MB, None),
            large("Library/x.bin", 600 * MB, Some(now - 91 * 86_400)),
        ];
        let rels = |v: Vec<&LargeItem>| v.into_iter().map(|i| i.rel.clone()).collect::<Vec<_>>();
        assert_eq!(
            rels(s.biggest(LARGE_DEFAULT_MIN)),
            ["Movies/new.mov", "Movies/old.mov", "Library/x.bin"]
        );
        assert_eq!(
            rels(s.not_opened(100 * MB, now, NOT_OPENED_DAYS)),
            ["Movies/old.mov", "Library/x.bin", "a.zip"]
        );
        assert!(s.items[3].app_data);
    }

    #[test]
    fn folder_tree_find_and_split() {
        let leaf = |name: &str, rel: &str, b: u64| FolderNode {
            name: name.into(),
            rel: rel.into(),
            kind: Some(LargeKind::File),
            bytes_alloc: b,
            bytes_logical: b,
            files: 1,
            ..FolderNode::default()
        };
        let lib = FolderNode {
            name: "Library".into(),
            rel: "Library".into(),
            kind: Some(LargeKind::Folder),
            bytes_alloc: 700,
            children: vec![leaf("a", "Library/a", 600), leaf("b", "Library/b", 60)],
            rest_count: 3,
            rest_alloc: 40,
            ..FolderNode::default()
        };
        let root = FolderNode {
            children: vec![lib],
            bytes_alloc: 700,
            ..FolderNode::default()
        };
        assert_eq!(root.find("").unwrap().bytes_alloc, 700);
        assert_eq!(root.find("Library/a").unwrap().bytes_alloc, 600);
        assert_eq!(root.find("/Library/").unwrap().name, "Library");
        assert!(root.find("Library/zzz").is_none());
        let (shown, n, b) = root.find("Library").unwrap().split_at(100);
        assert_eq!(shown.len(), 1);
        assert_eq!((n, b), (4, 100));
        let (shown, n, b) = root.find("Library").unwrap().split_at(50);
        assert_eq!(
            shown.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!((n, b), (3, 40));
    }

    #[test]
    fn history_entry_matches_the_doc_shape() {
        let line = r#"{"id":"2026-10-23T14:02:11Z-1","at":1792764131,"action":"clean","source":"cleanup","trasher":"system",
 "items":[{"path":"/Users/x/Library/Caches/node-gyp","trash_path":"/Users/x/.Trash/node-gyp","bytes":135266304,"rule":"dev.node-gyp","result":"trashed"},
          {"path":"/Users/x/Library/Caches/Google/Chrome/Default","trash_path":null,"bytes":1857028096,"rule":"cache.chrome","result":"skipped","reason":"Google Chrome is running"}],
 "trashed_bytes":135266304}"#;
        let e: HistoryEntry = serde_json::from_str(line).unwrap();
        assert_eq!(e.action, Action::Clean);
        assert_eq!(e.source, Source::Cleanup);
        assert_eq!(e.trasher, TrasherKind::System);
        assert_eq!(e.items.len(), 2);
        assert_eq!(e.skipped(), 1);
        assert_eq!(e.trashed().count(), 1);
        assert_eq!(
            e.items[1].reason.as_deref(),
            Some("Google Chrome is running")
        );
        assert_eq!(e.message, None);
        let again = serde_json::to_value(&e).unwrap();
        assert_eq!(again["items"][0]["trash_path"], "/Users/x/.Trash/node-gyp");
        assert!(again["items"][0].get("reason").is_none());
        assert!(again["items"][1]["trash_path"].is_null());
        assert!(again.get("message").is_none());
        assert_eq!(serde_json::from_value::<HistoryEntry>(again).unwrap(), e);
    }

    // ----- M2 -----

    #[test]
    fn run_ids() {
        // TEST_NOW is 2026-10-23 14:02:11 UTC.
        assert_eq!(run_id(1_792_764_131), "20261023-140211");
        assert_eq!(run_id(0), "19700101-000000");
        assert_eq!(run_id(86_399), "19700101-235959");
        assert!(run_id(1_792_764_131) < run_id(1_792_764_132));
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2024, 12, 31), (2026, 10, 23)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
    }

    #[test]
    fn pressure_levels_and_verdicts() {
        assert_eq!(Pressure::from_level(1), Pressure::Normal);
        assert_eq!(Pressure::from_level(0), Pressure::Normal);
        assert_eq!(Pressure::from_level(2), Pressure::Warning);
        assert_eq!(Pressure::from_level(4), Pressure::Critical);
        assert!(Pressure::Normal.verdict().starts_with("Memory is fine."));
        assert!(Pressure::Warning.verdict().starts_with("Memory is tight."));
        assert!(
            Pressure::Critical
                .verdict()
                .starts_with("Memory is very tight.")
        );
        assert_eq!(
            serde_json::to_string(&Pressure::Warning).unwrap(),
            "\"warning\""
        );
        let vm = VmStats {
            pressure_level: 2,
            total_bytes: 16 << 30,
            swap: SwapUsage {
                total: 2 << 30,
                used: 1 << 30,
                free: 1 << 30,
            },
            pages: PageStats {
                page_size: 16_384,
                compressor: 1000,
                ..PageStats::default()
            },
        };
        let s = MemorySnapshot::from_vm(5, &vm);
        assert_eq!(s.pressure, Pressure::Warning);
        assert_eq!(s.compressed_bytes, 16_384_000);
        assert_eq!(s.swap.used, 1 << 30);
        assert_eq!(CheckupMemory::from_snapshot(&s).verdict(), s.verdict());
    }

    #[test]
    fn never_quit_list() {
        for n in [
            "Finder",
            "com.apple.finder",
            "WindowServer",
            "loginwindow",
            TIDY_BUNDLE_ID,
        ] {
            assert!(AppMemory::never_quit(n), "{n}");
        }
        assert!(AppMemory::never_quit("FINDER"));
        assert!(!AppMemory::never_quit("Safari"));
    }

    #[test]
    fn check_ids_and_links() {
        let names: Vec<String> = CheckId::ALL
            .iter()
            .map(|c| serde_json::to_string(c).unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "\"filevault\"",
                "\"firewall\"",
                "\"gatekeeper\"",
                "\"sip\"",
                "\"xprotect\"",
                "\"macos_updates\"",
                "\"login_items\"",
                "\"chrome_extensions\"",
            ]
        );
        assert_eq!(CheckId::Sip.link(), None);
        assert_eq!(CheckId::Firewall.link(), Some(SettingsLink::Network));
        for l in [
            SettingsLink::PrivacySecurity,
            SettingsLink::Network,
            SettingsLink::SoftwareUpdate,
            SettingsLink::LoginItems,
            SettingsLink::Notifications,
        ] {
            assert!(l.url().starts_with("x-apple.systempreferences:com.apple."));
            assert!(l.label().starts_with("Open "));
        }
        // Only amber rows get a button.
        let amber = CheckResult::new(CheckId::FileVault, Status::Amber, "FileVault is off.");
        assert_eq!(amber.action, Some(SettingsLink::PrivacySecurity));
        assert_eq!(amber.title, "FileVault");
        let green = CheckResult::new(CheckId::FileVault, Status::Green, "FileVault is on.");
        assert_eq!(green.action, None);
        let gray = CheckResult::new(CheckId::Firewall, Status::Gray, "Tidy could not read this.");
        assert_eq!(gray.action, None);
    }

    fn login(scope: Scope, signer: Signer) -> LoginItem {
        LoginItem {
            label: "com.example.agent".into(),
            plist: Some("/h/Library/LaunchAgents/com.example.agent.plist".into()),
            program: Some("/usr/local/bin/agent".into()),
            runs_at_load: true,
            scope,
            kind: LaunchKind::Agent,
            signer,
            notarized: None,
            mark: None,
        }
    }

    #[test]
    fn login_items() {
        let mut i = login(Scope::User, Signer::Team("Example Inc".into()));
        assert_eq!(i.key(), "user:com.example.agent");
        assert!(!i.needs_a_look());
        assert!(i.can_trash());
        i.mark = Some(Mark::New);
        assert!(i.needs_a_look());
        i.mark = None;
        i.notarized = Some(false);
        assert!(i.needs_a_look());
        assert!(login(Scope::User, Signer::Unsigned).needs_a_look());
        assert!(!login(Scope::User, Signer::Apple).needs_a_look());
        let sys = login(Scope::System, Signer::Apple);
        assert_eq!(sys.key(), "system:com.example.agent");
        assert!(!sys.can_trash(), "only user agents go to the Trash");
        assert_eq!(Signer::Unsigned.label(), "Not signed");
        assert_eq!(Signer::Team("Example Inc".into()).label(), "Example Inc");
        assert_eq!(
            serde_json::to_value(Signer::Team("T".into())).unwrap(),
            serde_json::json!({"team": "T"})
        );
        assert_eq!(serde_json::to_value(Signer::Apple).unwrap(), "apple");
        let back: LoginItem = serde_json::from_str(&serde_json::to_string(&i).unwrap()).unwrap();
        assert_eq!(back, i);
    }

    #[test]
    fn security_report_counts_and_headlines() {
        let mut r = SecurityReport::empty(1_792_764_131);
        assert_eq!(r.id, "20261023-140211");
        assert_eq!(r.headline(), "All good");
        r.checks = vec![
            CheckResult::new(CheckId::FileVault, Status::Green, "On."),
            CheckResult::new(CheckId::Firewall, Status::Amber, "Off."),
            CheckResult::new(CheckId::MacosUpdates, Status::Gray, "Checked on demand."),
        ];
        assert_eq!((r.amber(), r.gray()), (1, 1));
        assert_eq!(r.headline(), "1 thing to look at");
        r.checks
            .push(CheckResult::new(CheckId::Sip, Status::Amber, "Off."));
        assert_eq!(r.headline(), "2 things to look at");
        assert_eq!(r.check(CheckId::Sip).unwrap().status, Status::Amber);
        let c = CheckupSecurity::from_report(&r);
        assert_eq!((c.amber, c.gray, c.id.as_str()), (2, 1, "20261023-140211"));
        assert_eq!(c.headline(), "2 things to look at");
        let back: SecurityReport =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        let e = ChromeExtension {
            id: "a".repeat(32),
            name: "Example".into(),
            version: None,
            profile: "Profile 1".into(),
            enabled: true,
            mark: Some(Mark::New),
        };
        assert_eq!(e.key(), format!("Profile 1/{}", "a".repeat(32)));
        assert_eq!(Mark::Changed.label(), "Changed");
    }

    #[test]
    fn checkup_parts() {
        let plan = Plan {
            items: vec![
                item("cache.a:Library/Caches/a", Group::Caches, Class::Safe, 100),
                item("cache.b:Library/Caches/b", Group::Caches, Class::Review, 40),
            ],
            ..Plan::default()
        };
        let c = CheckupCleanup::from_plan(&plan);
        assert_eq!((c.safe_bytes, c.safe_items, c.review_bytes), (100, 1, 40));
        // An old line with missing parts still reads.
        let old: Checkup = serde_json::from_str(r#"{"at": 5}"#).unwrap();
        assert_eq!(
            old,
            Checkup {
                at: 5,
                ..Checkup::default()
            }
        );
    }
}
