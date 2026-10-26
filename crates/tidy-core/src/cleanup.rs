//! The Cleanup scan: the rule table of `rules` applied to the scan root, producing a [`Plan`]
//! (design section 6), and the clean-time re-check of one item.
//!
//! A scan only reads. It runs in four steps:
//!
//! 1. **Gather.** Every rule target is expanded to candidate paths: glob patterns, the
//!    folders of `Library/Caches`, the Electron and Chromium cache folders, and the project
//!    build output found by walking the project roots (never into `.git`, never into a match,
//!    never through a symlink, at most [`PROJECT_MAX_DEPTH`] levels).
//! 2. **Dates.** The newest file mtime inside each candidate (and, for project rules, in the
//!    project folder) decides the 24-hour skip, the age rules and the keep-newest policy.
//! 3. **Sizes.** `walk::measure` gives allocated and logical bytes of what is left.
//! 4. **Assemble.** Blocked items (the owner is running) are unticked with "Quit <App>
//!    first"; matches under 1 MB fold into one "N small items" row per rule and folder.
//!
//! The date and size steps run on a few threads; the progress callback is called on the
//! calling thread only, so it needs no `Sync`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::env::Env;
use crate::model::{Class, Group, Item, ItemResult, Plan, RunningSet, TrashSummary};
use crate::owners::{InstalledApps, Owner as ResolvedOwner};
use crate::paths::{is_denied, is_denied_rel};
use crate::procs::uses_path;
use crate::rules::{
    self, Age, Keep, KeepBy, MIN_ROW_BYTES, Markers, Name, PROJECT_MAX_DEPTH, PROJECT_RULES,
    ProjectRule, RECENT_SECS, RULES, Rule, SMALL_SUFFIX, Target, UNKNOWN_CACHE,
};
use crate::store::{read_json, read_json_lines, write_json_atomic};
use crate::trash::{Checked, check};
use crate::walk::{Size, WalkOpts, device_of, measure};

/// The signature line Cargo writes into `CACHEDIR.TAG`.
const CARGO_TAG_SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55";
/// Markers of a Chromium or Electron user-data folder.
const CHROMIUM_MARKERS: &[&str] = &[
    "Local State",
    "Preferences",
    "Local Storage",
    "Session Storage",
    "Cookies",
    "GPUCache",
    "Code Cache",
    "blob_storage",
];

/// The skip reason when the process list cannot be read at clean time.
pub const PROCS_UNKNOWN: &str = "Tidy couldn't read which apps are open, skipped";

/// What a running scan has measured so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanupProgress {
    pub files: u64,
    pub bytes: u64,
    /// The path being measured, when there is one.
    pub current: Option<PathBuf>,
}

// ---------- Public API ----------

/// How long one rule may take in a scan (gathering, dates and sizes together) before it is
/// stopped and reported as skipped. The rest of the scan goes on.
pub const RULE_BUDGET: Duration = Duration::from_secs(30);

/// The time each rule may take: [`RULE_BUDGET`] unless a rule id is listed. Project build
/// output is budgeted as one rule, `proj`.
#[derive(Clone, Debug)]
pub struct ScanBudget {
    pub default: Duration,
    pub rules: Vec<(&'static str, Duration)>,
}

impl Default for ScanBudget {
    fn default() -> ScanBudget {
        ScanBudget {
            default: RULE_BUDGET,
            rules: Vec::new(),
        }
    }
}

impl ScanBudget {
    fn of(&self, rule: &str) -> Duration {
        self.rules
            .iter()
            .find(|(id, _)| *id == rule)
            .map_or(self.default, |(_, d)| *d)
    }
}

/// The budget key of a candidate's rule: every project rule shares `proj`.
fn budget_key(rule_id: &'static str) -> &'static str {
    if rule_id.starts_with("proj.") {
        PROJECTS_KEY
    } else {
        rule_id
    }
}

const PROJECTS_KEY: &str = "proj";

/// The plain name of a rule in a "Skipped ..." line.
fn budget_title(key: &str) -> String {
    let named = match key {
        PROJECTS_KEY => "build output in your projects",
        "cache.app" | "cache.unknown" => "app caches",
        "cache.electron" => "app caches in Application Support",
        "cache.electron.sw" => "website data",
        "cache.electron.scripts" => "service worker scripts",
        "logs.app" => "app logs",
        "logs.diag" => "crash reports",
        "dev.cache" => "tool caches",
        "dev.jetbrains" => "JetBrains caches",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_string();
    }
    match rules::rule(key) {
        Some(r) if !r.label.is_empty() => r.label.to_string(),
        _ => key.to_string(),
    }
}

/// The line a rule that ran out of time leaves in [`Plan::skipped`].
pub fn timed_out_line(key: &str) -> String {
    format!(
        "Skipped {} because it took too long. Scan again to try it.",
        budget_title(key)
    )
}

/// Runs the Cleanup rules against `env.root` and returns the plan.
///
/// `progress` is called on the calling thread every so often while sizes are measured.
/// Setting `cancel` stops the scan at the next folder; the plan then holds only what was
/// finished, and the caller should discard it. A finished scan is saved to
/// `last-cleanup.json` (a failure to save is logged, not fatal).
pub fn scan_cleanup(
    env: &Env,
    progress: &dyn Fn(CleanupProgress),
    cancel: &Arc<AtomicBool>,
) -> Plan {
    scan_cleanup_within(env, progress, cancel, &ScanBudget::default())
}

/// [`scan_cleanup`] with a time budget per rule. A rule that runs out of time is stopped, its
/// items are left out, and the plan's `skipped` says so; the other rules are not affected.
pub fn scan_cleanup_within(
    env: &Env,
    progress: &dyn Fn(CleanupProgress),
    cancel: &Arc<AtomicBool>,
    budget: &ScanBudget,
) -> Plan {
    let now = env.now();
    let mut plan = Plan::empty(env.root.clone(), now);
    let ctx = Ctx::new(env, cancel.clone(), budget.clone());
    progress(CleanupProgress::default());

    let cands = gather(&ctx, &mut plan.skipped);
    if ctx.cancelled() {
        return plan;
    }

    // Step 2: dates.
    let mut cands = run_parallel(&ctx, cands, progress, |c| c.stat(&ctx));
    keep_newest(&mut cands);
    cands.retain(|c| c.passes_age(&ctx));
    if ctx.cancelled() {
        return plan;
    }

    // Step 3: sizes.
    let mut cands = run_parallel(&ctx, cands, progress, |c| c.size(&ctx));
    if ctx.cancelled() {
        return plan;
    }

    // A rule that ran out of time offers nothing, and says so.
    let over = ctx.clocks.over_keys();
    cands.retain(|c| !over.contains(&budget_key(c.rule_id)));
    for key in &over {
        tracing::warn!("cleanup rule {key} ran out of time");
        plan.skipped.push(timed_out_line(key));
    }

    // Step 4: assemble.
    plan.files_seen = cands.iter().map(|c| c.size.files).sum();
    plan.items = assemble(&ctx, cands);
    plan.sort();
    plan.trash = trash_summary(env);
    if let Err(e) = save_last_cleanup(env, &plan) {
        tracing::warn!("cannot save the cleanup plan: {e}");
    }
    plan
}

/// Re-checks one item right before its move (design 6.2, rule 5): the path exists, its real
/// path is inside the scan root, it is not on the denylist or the ignore list, the rule's
/// markers are still present and the owner is not running.
///
/// Returns the path to move: the item's path, or the holder folder of a folded row (use
/// [`recheck_paths`] to get the members). The error is the reason shown on the skipped row.
pub fn recheck(env: &Env, item: &Item) -> Result<PathBuf, String> {
    Rechecker::new(env).check(item)?;
    Ok(item.path.clone())
}

/// [`recheck`] for every path the item moves. Members of a folded row that fail a check are
/// left out; when none is left the first reason is the error.
pub fn recheck_paths(env: &Env, item: &Item) -> Result<Vec<PathBuf>, String> {
    Rechecker::new(env).check(item)
}

/// Saves the plan to `last-cleanup.json`.
pub fn save_last_cleanup(env: &Env, plan: &Plan) -> io::Result<()> {
    write_json_atomic(&env.paths.last_cleanup_file(), plan)
}

/// The plan saved by the last finished scan, if there is one and it reads. A plan for another
/// scan root is not returned.
pub fn load_last_cleanup(env: &Env) -> Option<Plan> {
    match read_json::<Plan>(&env.paths.last_cleanup_file()) {
        Ok(Some(plan)) if plan.root == env.root => Some(plan),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!("cannot read the saved cleanup plan: {e}");
            None
        }
    }
}

/// The items Tidy moved to the Trash that are still there, from history.jsonl. Items whose
/// state cannot be read (macOS may refuse `~/.Trash` without Full Disk Access) count as there.
pub fn trash_summary(env: &Env) -> TrashSummary {
    let entries: Vec<crate::model::HistoryEntry> =
        read_json_lines(&env.paths.history_file()).unwrap_or_default();
    let mut seen = HashSet::new();
    let mut summary = TrashSummary::default();
    for item in entries.iter().flat_map(|e| e.items.iter()) {
        if item.result != ItemResult::Trashed {
            continue;
        }
        let Some(tp) = &item.trash_path else { continue };
        if !seen.insert(tp.clone()) {
            continue;
        }
        match tp.symlink_metadata() {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            _ => {
                summary.items += 1;
                summary.bytes += item.bytes;
            }
        }
    }
    summary
}

// ---------- Context ----------

struct Ctx<'a> {
    env: &'a Env,
    root: PathBuf,
    now: u64,
    running: RunningSet,
    apps: InstalledApps,
    cancel: Arc<AtomicBool>,
    walk: WalkOpts,
    device: Option<u64>,
    project_ages: Mutex<HashMap<PathBuf, Option<u64>>>,
    /// The tracked paths of each Git repository the project guard read, by work tree.
    git_indexes: Mutex<HashMap<PathBuf, GitIndex>>,
    /// The time each rule has used.
    clocks: RuleClocks,
    files: AtomicU64,
    bytes: AtomicU64,
    current: Mutex<Option<PathBuf>>,
}

impl<'a> Ctx<'a> {
    /// The project guard's Git check, with each repository's index read once per scan.
    fn git_guard(&self, parent: &Path, output: &Path) -> Option<Guard> {
        let repo = find_repo(&self.root, parent)?;
        let index = {
            let mut cache = self.git_indexes.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .entry(repo.clone())
                .or_insert_with(|| read_git_index(&repo))
                .clone()
        };
        index_guard(&index, &repo, output)
    }

    fn new(env: &'a Env, cancel: Arc<AtomicBool>, budget: ScanBudget) -> Ctx<'a> {
        let mut walk = WalkOpts::for_root(&env.root);
        walk.cancel = Some(cancel.clone());
        Ctx {
            env,
            root: env.root.clone(),
            now: env.now(),
            running: env.procs.running(),
            apps: InstalledApps::load(env),
            cancel,
            device: device_of(&env.root),
            walk,
            project_ages: Mutex::new(HashMap::new()),
            git_indexes: Mutex::new(HashMap::new()),
            clocks: RuleClocks::new(budget),
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            current: Mutex::new(None),
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn rel<'p>(&self, path: &'p Path) -> Option<&'p Path> {
        path.strip_prefix(&self.root).ok()
    }

    /// Whether a path may be a candidate at all: inside the root, not denied, excluded or
    /// ignored by the user.
    fn allowed(&self, path: &Path) -> bool {
        let Some(rel) = self.rel(path) else {
            return false;
        };
        !rel.as_os_str().is_empty()
            && !is_denied_rel(rel)
            && !rules::is_excluded_rel(rel)
            && !self.env.settings.is_ignored(path)
    }

    /// Whether a path is a real file or folder on the root's device (not a symlink).
    fn plain(&self, meta: &std::fs::Metadata) -> bool {
        !meta.file_type().is_symlink() && self.device.is_none_or(|d| d == meta.dev())
    }

    /// The seconds since `mtime`, or 0 for a time in the future.
    fn age_secs(&self, mtime: u64) -> u64 {
        self.now.saturating_sub(mtime)
    }

    /// The newest mtime in the project folder `dir`, without build output and `.git`. None
    /// when the walk was stopped (the scan was cancelled or `key` ran out of time): a partial
    /// walk may have missed a recent file, so it is never used or kept.
    fn project_newest(&self, dir: &Path, key: &'static str) -> Option<Option<u64>> {
        if let Some(v) = self
            .project_ages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(dir)
        {
            return Some(*v);
        }
        let halt = || self.cancelled() || self.clocks.over(key);
        let walk = walk_dates(
            dir,
            self,
            None,
            &|name: &str| rules::is_output_name(name),
            None,
            &halt,
        );
        if !walk.complete {
            return None;
        }
        self.project_ages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir.to_path_buf(), walk.newest);
        Some(walk.newest)
    }
}

// ---------- Time budget ----------

/// The time each rule has used, and a stop flag per rule that its walks watch.
struct RuleClocks {
    budget: ScanBudget,
    rules: Mutex<HashMap<&'static str, RuleClock>>,
}

#[derive(Default)]
struct RuleClock {
    /// Time used by finished work.
    spent: Duration,
    /// Work going on now, by token.
    active: Vec<(u64, std::time::Instant)>,
    next: u64,
    /// Set when the rule ran out of time (or the scan was cancelled): its walks stop.
    stop: Arc<AtomicBool>,
    over: bool,
}

impl RuleClock {
    fn used(&self, now: std::time::Instant) -> Duration {
        self.spent
            + self
                .active
                .iter()
                .map(|(_, at)| now.saturating_duration_since(*at))
                .sum::<Duration>()
    }
}

impl RuleClocks {
    fn new(budget: ScanBudget) -> RuleClocks {
        RuleClocks {
            budget,
            rules: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<&'static str, RuleClock>> {
        self.rules.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts timing work for `key`. None when the rule has no time left.
    fn begin(&self, key: &'static str) -> Option<u64> {
        let budget = self.budget.of(key);
        let mut rules = self.lock();
        let clock = rules.entry(key).or_default();
        if clock.over || clock.used(std::time::Instant::now()) >= budget {
            clock.over = true;
            clock.stop.store(true, Ordering::Relaxed);
            return None;
        }
        let token = clock.next;
        clock.next += 1;
        clock.active.push((token, std::time::Instant::now()));
        Some(token)
    }

    fn end(&self, key: &'static str, token: u64) {
        let mut rules = self.lock();
        let clock = rules.entry(key).or_default();
        if let Some(i) = clock.active.iter().position(|(t, _)| *t == token) {
            let (_, at) = clock.active.swap_remove(i);
            clock.spent += at.elapsed();
        }
    }

    /// Whether `key` has run out of time, checked against the clock now (so a walk can stop
    /// in the middle without waiting for [`RuleClocks::tick`]).
    fn over(&self, key: &'static str) -> bool {
        let budget = self.budget.of(key);
        let mut rules = self.lock();
        let clock = rules.entry(key).or_default();
        if !clock.over && clock.used(std::time::Instant::now()) >= budget {
            clock.over = true;
            clock.stop.store(true, Ordering::Relaxed);
        }
        clock.over
    }

    /// The stop flag of `key`, for the walker's `cancel`.
    fn stop_flag(&self, key: &'static str) -> Arc<AtomicBool> {
        self.lock().entry(key).or_default().stop.clone()
    }

    /// Called every few milliseconds while work runs: stops every rule that ran out of time,
    /// and every rule when the scan is cancelled.
    fn tick(&self, cancelled: bool) {
        let now = std::time::Instant::now();
        for (key, clock) in self.lock().iter_mut() {
            if cancelled {
                clock.stop.store(true, Ordering::Relaxed);
            } else if !clock.over && clock.used(now) >= self.budget.of(key) {
                clock.over = true;
                clock.stop.store(true, Ordering::Relaxed);
            }
        }
    }

    /// The rules that ran out of time, sorted.
    fn over_keys(&self) -> Vec<&'static str> {
        let mut keys: Vec<&'static str> = self
            .lock()
            .iter()
            .filter(|(_, c)| c.over)
            .map(|(k, _)| *k)
            .collect();
        keys.sort_unstable();
        keys
    }
}

// ---------- Candidates ----------

#[derive(Clone, Debug)]
struct Cand {
    /// The rule id shown in the item id (`cache.app` or `cache.unknown` for cache folders).
    rule_id: &'static str,
    group: Group,
    class: Class,
    age: Age,
    keep: Option<(KeepBy, PathBuf, String)>,
    in_use: bool,
    owner: Option<ResolvedOwner>,
    name: String,
    reason: &'static str,
    path: PathBuf,
    /// The folder a folded row of this rule is made for.
    fold_dir: PathBuf,
    /// `{app}` in the reason.
    app: String,
    /// The project folder for [`Age::Project`].
    project: Option<PathBuf>,
    /// The folder that holds a project build output, for the project guard.
    output_parent: Option<PathBuf>,
    // Filled in by the scan.
    newest: Option<u64>,
    project_newest: Option<u64>,
    dropped: bool,
    size: Size,
}

impl Cand {
    fn new(rule: &Rule, path: PathBuf) -> Cand {
        let fold_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let app = fold_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Cand {
            rule_id: rule.id,
            group: rule.group,
            class: rule.class,
            age: rule.age,
            keep: None,
            in_use: rule.in_use,
            owner: None,
            name: String::new(),
            reason: rule.reason,
            path,
            fold_dir,
            app,
            project: None,
            output_parent: None,
            newest: None,
            project_newest: None,
            dropped: false,
            size: Size::default(),
        }
    }

    /// Step 2 for one candidate: the project's newest mtime, then the target's.
    fn stat(&mut self, ctx: &Ctx) {
        if ctx.cancelled() {
            self.dropped = true;
            return;
        }
        *ctx.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.path.clone());
        let key = budget_key(self.rule_id);
        if let (Age::Project(days), Some(project)) = (self.age, &self.project) {
            let Some(newest) = ctx.project_newest(project, key) else {
                // The walk stopped early: its date is not known.
                self.dropped = true;
                return;
            };
            self.project_newest = newest;
            if let Some(t) = self.project_newest
                && ctx.age_secs(t) <= u64::from(days) * 86_400
            {
                self.dropped = true;
                return;
            }
        }
        // Without a keep-newest policy a file inside the 24-hour window ends the walk early:
        // the candidate is dropped either way.
        let stop = self
            .keep
            .is_none()
            .then(|| ctx.now.saturating_sub(RECENT_SECS));
        let halt = || ctx.cancelled() || ctx.clocks.over(key);
        let Some(parent) = self.output_parent.clone() else {
            let walk = walk_dates(&self.path, ctx, stop, &|_| false, None, &halt);
            self.newest = walk.newest;
            self.dropped |= !walk.complete;
            return;
        };
        // Build output: the same walk also looks for what the project guard refuses.
        let walk = walk_dates(&self.path, ctx, stop, &|_| false, Some(&guard_name), &halt);
        let (newest, flagged) = (walk.newest, walk.guard);
        self.newest = newest;
        if !walk.complete {
            self.dropped = true;
            return;
        }
        if let Some(guard) = flagged {
            tracing::info!(path = %self.path.display(), "project guard: {}", guard.reason());
            self.dropped = true;
            return;
        }
        let recent = newest.is_some_and(|t| ctx.age_secs(t) <= RECENT_SECS);
        if !recent && let Some(guard) = ctx.git_guard(&parent, &self.path) {
            tracing::info!(path = %self.path.display(), "project guard: {}", guard.reason());
            self.dropped = true;
        }
    }

    /// Whether the candidate survives the 24-hour skip, its age rule and the in-use check.
    fn passes_age(&self, ctx: &Ctx) -> bool {
        if self.dropped {
            return false;
        }
        if let Some(t) = self.newest
            && ctx.age_secs(t) <= RECENT_SECS
        {
            return false;
        }
        if let (Age::Target(days), Some(t)) = (self.age, self.newest)
            && ctx.age_secs(t) <= u64::from(days) * 86_400
        {
            return false;
        }
        if self.in_use && uses_path(&ctx.running, &self.path) {
            return false;
        }
        true
    }

    /// Step 3 for one candidate.
    fn size(&mut self, ctx: &Ctx) {
        if ctx.cancelled() {
            self.dropped = true;
            return;
        }
        *ctx.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.path.clone());
        let key = budget_key(self.rule_id);
        let mut walk = ctx.walk.clone();
        walk.cancel = Some(ctx.clocks.stop_flag(key));
        self.size = measure(&self.path, &walk);
        if ctx.clocks.over(key) {
            // Stopped part way: the size is not the whole folder's.
            self.dropped = true;
            return;
        }
        // The move refuses a folder that holds a file Tidy never touches (a cookie store, a
        // `*.sqlite`), so the scan does not offer it either (design 6.3, 18.1).
        if self.size.files > 0 && crate::trash::holds_denied_name(&self.path).unwrap_or(true) {
            tracing::info!(path = %self.path.display(), "holds files Tidy never touches");
            self.dropped = true;
        }
        ctx.files.fetch_add(self.size.files, Ordering::Relaxed);
        ctx.bytes.fetch_add(self.size.alloc, Ordering::Relaxed);
    }

    /// The age in days the reason shows: since the project's or the target's newest file.
    fn days(&self, ctx: &Ctx) -> u64 {
        let t = match self.age {
            Age::Project(_) => self.project_newest.or(self.newest),
            _ => self.newest,
        };
        t.map_or(0, |t| ctx.age_secs(t) / 86_400)
    }
}

/// Runs `work` on every candidate on a few threads, calling `progress` on this thread.
fn run_parallel(
    ctx: &Ctx,
    cands: Vec<Cand>,
    progress: &dyn Fn(CleanupProgress),
    work: impl Fn(&mut Cand) + Sync,
) -> Vec<Cand> {
    let slots: Vec<Mutex<Cand>> = cands.into_iter().map(Mutex::new).collect();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 8)
        .min(slots.len().max(1));
    let active = AtomicUsize::new(threads);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(slot) = slots.get(i) else { break };
                    let mut c = slot.lock().unwrap_or_else(|e| e.into_inner());
                    if c.dropped {
                        continue;
                    }
                    let key = budget_key(c.rule_id);
                    let Some(token) = ctx.clocks.begin(key) else {
                        c.dropped = true;
                        continue;
                    };
                    work(&mut c);
                    ctx.clocks.end(key, token);
                }
                active.fetch_sub(1, Ordering::Release);
            });
        }
        while active.load(Ordering::Acquire) > 0 {
            report(ctx, progress);
            ctx.clocks.tick(ctx.cancelled());
            std::thread::sleep(Duration::from_millis(40));
        }
    });
    report(ctx, progress);
    slots
        .into_iter()
        .map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()))
        .collect()
}

fn report(ctx: &Ctx, progress: &dyn Fn(CleanupProgress)) {
    progress(CleanupProgress {
        files: ctx.files.load(Ordering::Relaxed),
        bytes: ctx.bytes.load(Ordering::Relaxed),
        current: ctx
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
    });
}

// ---------- Keep newest ----------

fn keep_key(by: KeepBy, name: &str) -> String {
    match by {
        KeepBy::Folder => String::new(),
        KeepBy::BeforeLastDash => name
            .rsplit_once('-')
            .map_or(name, |(model, _)| model)
            .to_string(),
        KeepBy::DeviceModel => name
            .split_whitespace()
            .next()
            .filter(|w| w.contains(','))
            .unwrap_or("")
            .to_string(),
    }
}

/// Drops the newest candidate of every keep group (same rule, same parent folder, same key).
fn keep_newest(cands: &mut Vec<Cand>) {
    let mut groups: BTreeMap<(&'static str, PathBuf, String), Vec<usize>> = BTreeMap::new();
    for (i, c) in cands.iter().enumerate() {
        if c.dropped {
            continue;
        }
        if let Some((_, parent, key)) = &c.keep {
            groups
                .entry((c.rule_id, parent.clone(), key.clone()))
                .or_default()
                .push(i);
        }
    }
    let mut kept = HashSet::new();
    for members in groups.values() {
        let newest = members.iter().copied().max_by(|a, b| {
            let (ca, cb) = (&cands[*a], &cands[*b]);
            (ca.newest.unwrap_or(0), file_name_of(&ca.path))
                .cmp(&(cb.newest.unwrap_or(0), file_name_of(&cb.path)))
        });
        kept.extend(newest);
    }
    let mut i = 0;
    cands.retain(|_| {
        let keep = !kept.contains(&i);
        i += 1;
        keep
    });
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---------- Assemble ----------

fn assemble(ctx: &Ctx, cands: Vec<Cand>) -> Vec<Item> {
    struct Bucket {
        first: usize,
        size: Size,
        members: Vec<PathBuf>,
        newest: Option<u64>,
    }
    let mut items: Vec<Item> = Vec::new();
    let mut buckets: BTreeMap<(&'static str, PathBuf), Bucket> = BTreeMap::new();
    let cands: Vec<Cand> = cands.into_iter().filter(|c| !c.dropped).collect();
    for (i, c) in cands.iter().enumerate() {
        if c.size.files == 0 && c.size.alloc == 0 {
            continue;
        }
        let blocked = c.owner.as_ref().and_then(|o| o.blocked_by(&ctx.running));
        if c.size.alloc < MIN_ROW_BYTES {
            // A small blocked match is not worth a row of its own or a place in a fold.
            if blocked.is_some() {
                continue;
            }
            let b = buckets
                .entry((c.rule_id, c.fold_dir.clone()))
                .or_insert_with(|| Bucket {
                    first: i,
                    size: Size::default(),
                    members: Vec::new(),
                    newest: None,
                });
            b.size += c.size;
            b.members.push(c.path.clone());
            b.newest = b.newest.max(c.newest);
            continue;
        }
        items.push(Item {
            id: item_id(ctx, c.rule_id, &c.path),
            group: c.group,
            class: c.class,
            ticked: c.class == Class::Safe && blocked.is_none(),
            blocked,
            name: c.name.clone(),
            reason: rules::fill_reason(c.reason, &c.app, Some(c.days(ctx))),
            path: c.path.clone(),
            bytes_alloc: c.size.alloc,
            bytes_logical: c.size.logical,
            newest_mtime: c.newest,
            members: Vec::new(),
        });
    }
    for ((rule_id, dir), b) in buckets {
        if b.size.alloc < MIN_ROW_BYTES {
            continue;
        }
        let c = &cands[b.first];
        let mut members = b.members;
        members.sort();
        items.push(Item {
            id: item_id(ctx, &format!("{rule_id}{SMALL_SUFFIX}"), &dir),
            group: c.group,
            class: c.class,
            ticked: c.class == Class::Safe,
            blocked: None,
            name: format!("{} small items", members.len()),
            reason: rules::fill_reason(c.reason, &c.app, None),
            path: dir,
            bytes_alloc: b.size.alloc,
            bytes_logical: b.size.logical,
            newest_mtime: b.newest,
            members,
        });
    }
    drop_nested(items)
}

fn item_id(ctx: &Ctx, rule_id: &str, path: &Path) -> String {
    let rel = ctx.rel(path).unwrap_or(path);
    Item::make_id(rule_id, rel)
}

/// An item inside another item's folder is dropped, so no byte is counted or moved twice. A
/// folded row is judged by its holder folder only when it is the outer one.
fn drop_nested(items: Vec<Item>) -> Vec<Item> {
    let outer: Vec<PathBuf> = items
        .iter()
        .filter(|i| i.members.is_empty())
        .map(|i| i.path.clone())
        .collect();
    items
        .into_iter()
        .filter(|i| {
            let paths = i.paths();
            !paths
                .iter()
                .all(|p| outer.iter().any(|o| o != p && p.starts_with(o)))
        })
        .collect()
}

// ---------- Gathering ----------

fn gather(ctx: &Ctx, skipped: &mut Vec<String>) -> Vec<Cand> {
    let mut out: Vec<Cand> = Vec::new();
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    let mut push = |out: &mut Vec<Cand>, c: Cand| {
        if claimed.insert(c.path.clone()) {
            out.push(c);
        }
    };
    for rule in RULES {
        if ctx.cancelled() {
            break;
        }
        if rule.protected && !ctx.env.settings.large_scanned_once {
            // A folder macOS protects is read only once the user has let Tidy in (design 7).
            continue;
        }
        let key = budget_key(rule.id);
        let Some(token) = ctx.clocks.begin(key) else {
            continue;
        };
        for target in rule.targets {
            match *target {
                Target::Glob(pattern) => {
                    for path in expand_glob(&ctx.root, pattern, rule.skip) {
                        if let Some(c) = glob_cand(ctx, rule, path) {
                            push(&mut out, c);
                        }
                    }
                }
                Target::Files(pattern) => {
                    for dir in expand_glob(&ctx.root, pattern, rule.skip) {
                        if rule.protected
                            && let Err(e) = std::fs::read_dir(&dir)
                            && e.kind() == io::ErrorKind::PermissionDenied
                        {
                            skipped.push(skipped_line(ctx, &dir));
                            continue;
                        }
                        for c in file_cands(ctx, rule, &dir) {
                            push(&mut out, c);
                        }
                    }
                }
                Target::CacheFolders(dir) => {
                    for c in cache_folder_cands(ctx, rule, &ctx.root.join(dir)) {
                        push(&mut out, c);
                    }
                }
                Target::Chromium(subs) => {
                    for c in chromium_cands(ctx, rule, subs) {
                        push(&mut out, c);
                    }
                }
            }
        }
        ctx.clocks.end(key, token);
    }
    if let Some(token) = ctx.clocks.begin(PROJECTS_KEY) {
        let mut projects = Vec::new();
        project_cands(ctx, &mut projects, skipped);
        ctx.clocks.end(PROJECTS_KEY, token);
        for c in projects {
            push(&mut out, c);
        }
    }
    out
}

/// A plain folder or file inside the root that rules may offer, with its metadata.
fn usable(ctx: &Ctx, path: &Path) -> Option<std::fs::Metadata> {
    let meta = path.symlink_metadata().ok()?;
    (ctx.plain(&meta) && ctx.allowed(path)).then_some(meta)
}

fn display_name(rule: &Rule, path: &Path, owner: Option<&ResolvedOwner>) -> String {
    let entry = &file_name_of(path);
    match rule.name {
        Name::Entry => entry.to_string(),
        Name::ParentEntry => match path.parent().map(file_name_of) {
            Some(parent) if !parent.is_empty() => format!("{parent}: {entry}"),
            _ => entry.to_string(),
        },
        Name::Label => {
            if !rule.label.is_empty() {
                rule.label.to_string()
            } else if let Some(o) = owner {
                o.name.clone()
            } else {
                entry.to_string()
            }
        }
        Name::LabelEntry | Name::LabelParen => {
            let label = if !rule.label.is_empty() {
                rule.label.to_string()
            } else if let Some(o) = owner {
                o.name.clone()
            } else {
                String::new()
            };
            match (rule.name, label.is_empty()) {
                (_, true) => entry.to_string(),
                (Name::LabelParen, false) => format!("{label} ({entry})"),
                _ => format!("{label}: {entry}"),
            }
        }
        Name::HuggingFace => entry
            .strip_prefix("models--")
            .unwrap_or(entry)
            .replace("--", "/"),
    }
}

fn rule_owner(ctx: &Ctx, rule: &Rule, folder_name: &str) -> Option<ResolvedOwner> {
    match rule.owner {
        rules::Owner::None => None,
        rules::Owner::Fixed(info) => Some(info.to_owner()),
        rules::Owner::Folder => ctx.apps.resolve(folder_name, true),
    }
}

fn keep_for(rule: &Rule, path: &Path) -> Option<(KeepBy, PathBuf, String)> {
    let Keep::Newest(by) = rule.keep else {
        return None;
    };
    let parent = path.parent()?.to_path_buf();
    Some((by, parent, keep_key(by, &file_name_of(path))))
}

fn glob_cand(ctx: &Ctx, rule: &Rule, path: PathBuf) -> Option<Cand> {
    usable(ctx, &path)?;
    let entry = file_name_of(&path);
    let owner = rule_owner(ctx, rule, &entry);
    let mut c = Cand::new(rule, path.clone());
    c.name = display_name(rule, &path, owner.as_ref());
    c.app = owner.as_ref().map_or(entry, |o| o.name.clone());
    c.owner = owner;
    c.keep = keep_for(rule, &path);
    Some(c)
}

/// Whether `path` has one of `exts` (any file when `exts` is empty).
fn has_ext(path: &Path, exts: &[&str]) -> bool {
    exts.is_empty()
        || path
            .extension()
            .is_some_and(|x| exts.iter().any(|e| x.eq_ignore_ascii_case(e)))
}

/// The files directly inside `dir`, one candidate each (Logs, Homebrew downloads).
fn file_cands(ctx: &Ctx, rule: &Rule, dir: &Path) -> Vec<Cand> {
    let Some(meta) = usable(ctx, dir) else {
        return Vec::new();
    };
    if !meta.is_dir() {
        return Vec::new();
    }
    let folder = file_name_of(dir);
    let owner = rule_owner(ctx, rule, &folder);
    let app = owner.as_ref().map_or(folder, |o| o.name.clone());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        let path = e.path();
        if !has_ext(&path, rule.exts) || usable(ctx, &path).is_none() {
            continue;
        }
        let mut c = Cand::new(rule, path.clone());
        c.name = display_name(rule, &path, owner.as_ref());
        c.app = app.clone();
        c.owner = owner.clone();
        c.fold_dir = dir.to_path_buf();
        out.push(c);
    }
    out
}

/// The folders directly inside `dir` of `Library/Caches` style: Safe when the owner is known,
/// Review otherwise.
fn cache_folder_cands(ctx: &Ctx, rule: &Rule, dir: &Path) -> Vec<Cand> {
    if !real_dir_under(&ctx.root, dir) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || rules::skipped(rule.skip, &name) {
            continue;
        }
        let path = e.path();
        if usable(ctx, &path).is_none() {
            continue;
        }
        // Cache folders are matched by bundle id or table name, not by app name.
        let owner = ctx.apps.resolve(&name, false);
        let mut c = match &owner {
            Some(_) => Cand::new(rule, path),
            None => Cand::new(&UNKNOWN_CACHE, path),
        };
        c.name = owner
            .as_ref()
            .map_or_else(|| name.clone(), |o| o.name.clone());
        c.app = c.name.clone();
        c.owner = owner;
        out.push(c);
    }
    out
}

fn is_chromium_dir(dir: &Path, markers: &[&str]) -> bool {
    markers
        .iter()
        .any(|m| dir.join(m).symlink_metadata().is_ok())
}

/// Profile folders of a Chromium user-data folder.
fn chromium_profiles(root_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n == "Default"
                || n == "Guest Profile"
                || n == "System Profile"
                || n.starts_with("Profile ")
        })
        .map(|e| e.path())
        .collect()
}

/// Whether `dir` and every folder between `root` and it is a real folder, not a symlink, so
/// that listing `dir` never goes through a link (`Library/Caches` swapped for a link).
fn real_dir_under(root: &Path, dir: &Path) -> bool {
    let Ok(rel) = dir.strip_prefix(root) else {
        return false;
    };
    let mut at = root.to_path_buf();
    for c in rel.components() {
        let std::path::Component::Normal(part) = c else {
            return false;
        };
        at.push(part);
        if !at.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            return false;
        }
    }
    true
}

/// Whether every folder of `sub` before its last part (`Crashpad` in `Crashpad/completed`) is a
/// real folder under `base`, not a symlink. `symlink_metadata` on the full path checks only
/// the last part and follows a link in the middle.
fn plain_folders_between(base: &Path, sub: &str) -> bool {
    let parts: Vec<&str> = sub.split('/').collect();
    let mut dir = base.to_path_buf();
    for part in &parts[..parts.len().saturating_sub(1)] {
        dir.push(part);
        if !dir.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            return false;
        }
    }
    true
}

/// The listed cache subfolders of every Electron and Chromium app in `Application Support`,
/// including each Chrome profile.
fn chromium_cands(ctx: &Ctx, rule: &Rule, subs: &[&str]) -> Vec<Cand> {
    let support = ctx.root.join("Library/Application Support");
    if !real_dir_under(&ctx.root, &support) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&support) else {
        return Vec::new();
    };
    let mut apps: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    apps.sort();
    let mut out = Vec::new();
    for app_dir in apps {
        if ctx.cancelled() {
            break;
        }
        if usable(ctx, &app_dir).is_none() {
            continue;
        }
        let folder = file_name_of(&app_dir);
        // The user-data folders: the app folder itself, and sub-folders with a Local State
        // file (Google/Chrome, Dia/User Data).
        let mut user_data: Vec<PathBuf> = Vec::new();
        if is_chromium_dir(&app_dir, CHROMIUM_MARKERS) {
            user_data.push(app_dir.clone());
        }
        if let Ok(children) = std::fs::read_dir(&app_dir) {
            let mut kids: Vec<PathBuf> = children
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .filter(|p| p.join("Local State").symlink_metadata().is_ok())
                .collect();
            kids.sort();
            user_data.extend(kids);
        }
        if user_data.is_empty() {
            continue;
        }
        let owner = ctx.apps.resolve(&folder, true);
        for base in user_data {
            let mut bases = vec![base.clone()];
            bases.extend(chromium_profiles(&base));
            for b in bases {
                for sub in subs {
                    // A two-part target ("Crashpad/completed") must not pass through a link.
                    if !plain_folders_between(&b, sub) {
                        continue;
                    }
                    let path = b.join(sub);
                    let Some(meta) = usable(ctx, &path) else {
                        continue;
                    };
                    if !meta.is_dir() {
                        continue;
                    }
                    let within = path
                        .strip_prefix(&app_dir)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| sub.to_string());
                    let app = owner.as_ref().map_or(folder.clone(), |o| o.name.clone());
                    let mut c = match &owner {
                        Some(_) => Cand::new(rule, path.clone()),
                        None => Cand::new(&UNKNOWN_CACHE, path.clone()),
                    };
                    c.name = format!("{app}: {within}");
                    c.app = app;
                    c.owner = owner.clone();
                    c.fold_dir = app_dir.clone();
                    out.push(c);
                }
            }
        }
    }
    out
}

// ---------- Project build output ----------

fn project_cands(ctx: &Ctx, out: &mut Vec<Cand>, skipped: &mut Vec<String>) {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for root in ctx.env.settings.project_root_paths(&ctx.root) {
        // Walk the folder under its on-disk spelling, once however many spellings are listed.
        let root = on_disk_spelling(&ctx.root, &root);
        if !seen.insert(root.clone()) {
            continue;
        }
        let Ok(meta) = root.symlink_metadata() else {
            continue;
        };
        if !meta.is_dir() || !ctx.plain(&meta) || !ctx.allowed(&root) {
            continue;
        }
        if !real_dir_under(&ctx.root, &root) {
            continue;
        }
        match std::fs::read_dir(&root) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                skipped.push(skipped_line(ctx, &root));
                continue;
            }
            Err(_) => continue,
            Ok(_) => {}
        }
        visit_project_dir(ctx, &root, &root, 0, out);
    }
}

/// `path` (inside `root`) with each part spelled as the folder that holds it spells it. On a
/// case-insensitive volume a settings entry such as `Documents/projects` finds
/// `Documents/Projects`; the scan must carry the real spelling, or the clean-time check
/// (which compares resolved paths) would call the item a link. Only the letter case of a name
/// changes: no link is followed, and a part that does not exist keeps its spelling.
fn on_disk_spelling(root: &Path, path: &Path) -> PathBuf {
    let Ok(rel) = path.strip_prefix(root) else {
        return path.to_path_buf();
    };
    let mut at = root.to_path_buf();
    for c in rel.components() {
        let std::path::Component::Normal(part) = c else {
            return path.to_path_buf();
        };
        let mut found = None;
        if let Ok(entries) = std::fs::read_dir(&at) {
            for e in entries.flatten() {
                let name = e.file_name();
                if name == part {
                    found = Some(name);
                    break;
                }
                if found.is_none()
                    && name.to_string_lossy().to_lowercase()
                        == part.to_string_lossy().to_lowercase()
                {
                    found = Some(name);
                }
            }
        }
        match found {
            // A different name is the same entry only when the volume ignores case.
            Some(name) if name == part || at.join(part).symlink_metadata().is_ok() => at.push(name),
            _ => at.push(part),
        }
    }
    at
}

fn skipped_line(ctx: &Ctx, root: &Path) -> String {
    let rel = ctx.rel(root).unwrap_or(root);
    let first = rel
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = if matches!(first.as_str(), "Documents" | "Desktop" | "Downloads") {
        first
    } else {
        rel.to_string_lossy().into_owned()
    };
    format!(
        "Skipped {name} (access not allowed). You can allow it in System Settings > Privacy & Security > Files and Folders."
    )
}

/// Looks at the folders directly inside `dir`: a match becomes a candidate and is not entered,
/// any other folder is walked (unless its name is build output or `.git`).
fn visit_project_dir(ctx: &Ctx, dir: &Path, top: &Path, depth: usize, out: &mut Vec<Cand>) {
    if ctx.cancelled() || depth > PROJECT_MAX_DEPTH || ctx.clocks.over(PROJECTS_KEY) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut kids: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    kids.sort();
    for (name, path) in kids {
        if name == ".git" || usable(ctx, &path).is_none() {
            continue;
        }
        // A project is a folder directly inside a project root.
        let project_top = if depth == 0 { path.as_path() } else { top };
        if let Some((rule, class)) = match_project(dir, &path, &name) {
            out.push(project_cand(rule, class, dir, &path, project_top));
            continue;
        }
        if rules::is_output_name(&name) {
            continue;
        }
        visit_project_dir(ctx, &path, project_top, depth + 1, out);
    }
}

fn project_cand(
    rule: &'static ProjectRule,
    class: Class,
    parent: &Path,
    path: &Path,
    top: &Path,
) -> Cand {
    let owner = match rule.owner {
        rules::Owner::Fixed(info) => Some(info.to_owner()),
        _ => None,
    };
    let project_name = file_name_of(parent);
    let folder = file_name_of(path);
    Cand {
        rule_id: rule.id,
        group: Group::Developer,
        class,
        age: rule.age,
        keep: None,
        in_use: false,
        owner,
        name: format!("{project_name}/{folder}"),
        reason: rule.reason,
        path: path.to_path_buf(),
        fold_dir: top.to_path_buf(),
        app: project_name,
        project: matches!(rule.age, Age::Project(_)).then(|| parent.to_path_buf()),
        output_parent: Some(parent.to_path_buf()),
        newest: None,
        project_newest: None,
        dropped: false,
        size: Size::default(),
    }
}

/// The first project row whose folder name and markers fit, and the class it gives.
fn match_project(parent: &Path, path: &Path, name: &str) -> Option<(&'static ProjectRule, Class)> {
    PROJECT_RULES
        .iter()
        .filter(|r| r.names.contains(&name))
        .find_map(|r| markers_class(r, parent, path, name).map(|c| (r, c)))
}

/// The class a project row gives this folder, or None when its markers are missing.
fn markers_class(rule: &ProjectRule, parent: &Path, path: &Path, name: &str) -> Option<Class> {
    let has = |names: &[&str]| names.iter().any(|n| has_entry(parent, n));
    match rule.markers {
        Markers::ParentHasAny(names) => has(names).then_some(rule.class),
        Markers::CargoTarget => {
            let tag = std::fs::read_to_string(path.join("CACHEDIR.TAG")).unwrap_or_default();
            (has_entry(parent, "Cargo.toml") || tag.contains(CARGO_TAG_SIGNATURE))
                .then_some(rule.class)
        }
        Markers::Pods => {
            if !has_entry(parent, "Podfile") {
                None
            } else if has_entry(parent, "Podfile.lock") {
                Some(rule.class)
            } else {
                Some(Class::Review)
            }
        }
        Markers::Venv => (path.join("pyvenv.cfg").symlink_metadata().is_ok()
            && has(&["pyproject.toml", "requirements.txt", "uv.lock"]))
        .then_some(rule.class),
        Markers::PythonSource => has_extension(parent, "py").then_some(rule.class),
        Markers::XcodeProject => (has_extension(parent, "xcodeproj")
            || has_extension(parent, "xcworkspace"))
        .then_some(rule.class),
        Markers::GitIgnored(names) => {
            (has(names) && gitignore_names(parent, name)).then_some(rule.class)
        }
        Markers::WithLockfile(files, locks) => (has(files) && has(locks)).then_some(rule.class),
    }
}

/// Whether `dir` holds an entry called `name`; a name that ends with `*` is a prefix.
fn has_entry(dir: &Path, name: &str) -> bool {
    match name.strip_suffix('*') {
        None => dir.join(name).symlink_metadata().is_ok(),
        Some(prefix) => std::fs::read_dir(dir).is_ok_and(|rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(prefix))
        }),
    }
}

fn has_extension(dir: &Path, ext: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case(ext))
        })
    })
}

/// Whether the project's `.gitignore` names the folder: `dist`, `/dist`, `dist/`, `/dist/`,
/// `**/dist`. Comments and negations do not count.
fn gitignore_names(project: &Path, folder: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(project.join(".gitignore")) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            return false;
        }
        let line = line.trim_start_matches("**/").trim_start_matches('/');
        line.trim_end_matches('/') == folder
    })
}

// ---------- Globs ----------

/// Whether a path segment pattern (`a`, `*`, `pre*`, or `a|b*`) matches a name.
fn segment_matches(pattern: &str, name: &str) -> bool {
    pattern.split('|').any(|alt| {
        if alt == "*" {
            !name.starts_with('.')
        } else if let Some(prefix) = alt.strip_suffix('*') {
            name.to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase())
        } else {
            alt.eq_ignore_ascii_case(name)
        }
    })
}

/// Expands a pattern relative to `root` to the paths that exist. Intermediate folders must be
/// real folders (a symlink is never followed). Names in `skip` never match the last segment.
fn expand_glob(root: &Path, pattern: &str, skip: &[&str]) -> Vec<PathBuf> {
    let segments: Vec<&str> = pattern.split('/').collect();
    let mut current = vec![root.to_path_buf()];
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        let mut next = Vec::new();
        for base in &current {
            if i > 0 && !base.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                continue;
            }
            for alt in seg.split('|') {
                if alt.ends_with('*') {
                    let Ok(entries) = std::fs::read_dir(base) else {
                        continue;
                    };
                    for e in entries.flatten() {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if segment_matches(alt, &name) && !(last && rules::skipped(skip, &name)) {
                            next.push(e.path());
                        }
                    }
                } else if !(last && rules::skipped(skip, alt)) {
                    let p = base.join(alt);
                    if p.symlink_metadata().is_ok() {
                        next.push(p);
                    }
                }
            }
        }
        current = next;
    }
    current.sort();
    current.dedup();
    current
}

/// Whether the components of `rel` fit a glob pattern from the front (the whole pattern must be
/// used and the whole path consumed).
fn glob_fits(pattern: &str, rel: &Path) -> bool {
    let segs: Vec<&str> = pattern.split('/').collect();
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    segs.len() == parts.len() && segs.iter().zip(&parts).all(|(s, p)| segment_matches(s, p))
}

// ---------- Newest mtime ----------

/// Looks at an entry's name and kind (`true` for a folder) for what the project guard refuses.
type GuardFn = dyn Fn(&str, bool) -> Option<Guard>;

/// What [`walk_dates`] found.
struct DateWalk {
    newest: Option<u64>,
    /// The first entry the flag refused, if any.
    guard: Option<Guard>,
    /// False when `halt` stopped the walk before it saw everything.
    complete: bool,
}

/// The newest mtime (Unix seconds) of any regular file inside `path` (or of `path` itself when
/// it is a file). Symlinks are not followed, other devices and denylisted paths are skipped,
/// `.DS_Store` is ignored, and folders for which `skip_dir` is true are not entered.
///
/// With `stop_at`, the walk ends as soon as a file with an mtime at or after it is found; the
/// value returned is then a lower bound that is at least `stop_at`. With `flag`, it also ends
/// at the first entry whose name and kind (`true` for a folder) `flag` refuses. `halt` is asked
/// before each folder; when it says stop, the result is marked incomplete.
fn walk_dates(
    path: &Path,
    ctx: &Ctx,
    stop_at: Option<u64>,
    skip_dir: &dyn Fn(&str) -> bool,
    flag: Option<&GuardFn>,
    halt: &dyn Fn() -> bool,
) -> DateWalk {
    let done = |newest, guard| DateWalk {
        newest,
        guard,
        complete: true,
    };
    let Ok(meta) = path.symlink_metadata() else {
        return done(None, None);
    };
    if !ctx.plain(&meta) {
        return done(None, None);
    }
    let mtime = |m: &std::fs::Metadata| m.mtime().max(0) as u64;
    if meta.is_file() {
        return done(Some(mtime(&meta)), None);
    }
    if !meta.is_dir() {
        return done(None, None);
    }
    let mut best: Option<u64> = None;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if halt() {
            return DateWalk {
                newest: best,
                guard: None,
                complete: false,
            };
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if !ctx.plain(&m) {
                continue;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if let Some(g) = flag.and_then(|f| f(&name, m.is_dir())) {
                return done(best, Some(g));
            }
            let child = e.path();
            if is_denied(&ctx.root, &child) {
                continue;
            }
            if m.is_dir() {
                if !skip_dir(&name) {
                    stack.push(child);
                }
            } else if m.is_file() && name != ".DS_Store" {
                let t = mtime(&m);
                best = best.max(Some(t));
                if stop_at.is_some_and(|s| t >= s) {
                    return done(best, None);
                }
            }
        }
    }
    done(best, None)
}

// ---------- Project guard ----------

/// Why a build output folder is never offered or moved, even with its markers present: it
/// holds something a build does not make again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// A `*-keypair.json` file (a Solana or Anchor deploy key, for example).
    Keypair,
    /// A `.git` folder or file: a repository of its own.
    NestedGit,
    /// Git tracks files inside it.
    Tracked,
    /// The project's Git index could not be read, so Tidy cannot tell.
    IndexUnreadable,
}

impl Guard {
    /// The skip reason at clean time.
    pub fn reason(self) -> &'static str {
        match self {
            Guard::Keypair => "Holds a keypair file, skipped",
            Guard::NestedGit => "Holds a Git repository, skipped",
            Guard::Tracked => "Git tracks files inside it, skipped",
            Guard::IndexUnreadable => "Tidy couldn't read the project's Git index, skipped",
        }
    }
}

/// The names the project guard refuses inside build output.
fn guard_name(name: &str, _is_dir: bool) -> Option<Guard> {
    if name == ".git" {
        Some(Guard::NestedGit)
    } else if name.to_ascii_lowercase().ends_with("-keypair.json") {
        Some(Guard::Keypair)
    } else {
        None
    }
}

/// The paths of a Git index, or why they could not be read.
type GitIndex = Result<std::sync::Arc<Vec<Vec<u8>>>, ()>;

/// The nearest folder at or above `dir`, inside `root`, that holds `.git`.
fn find_repo(root: &Path, dir: &Path) -> Option<PathBuf> {
    let mut at = dir;
    loop {
        if !at.starts_with(root) {
            return None;
        }
        if at.join(".git").symlink_metadata().is_ok() {
            return Some(at.to_path_buf());
        }
        at = at.parent()?;
    }
}

/// Reads the tracked paths of the repository whose work tree is `repo`. A repository with no
/// index yet tracks nothing; any other failure is an error.
fn read_git_index(repo: &Path) -> GitIndex {
    let file = crate::gitindex::index_of(repo).map_err(|e| {
        tracing::warn!(repo = %repo.display(), "cannot find the Git index: {e}");
    })?;
    match std::fs::read(&file) {
        Ok(bytes) => crate::gitindex::tracked_paths(&bytes)
            .map(std::sync::Arc::new)
            .map_err(|e| tracing::warn!(index = %file.display(), "{e}")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(std::sync::Arc::new(Vec::new())),
        Err(e) => {
            tracing::warn!(index = %file.display(), "cannot read the Git index: {e}");
            Err(())
        }
    }
}

fn index_guard(index: &GitIndex, repo: &Path, output: &Path) -> Option<Guard> {
    let Ok(paths) = index else {
        return Some(Guard::IndexUnreadable);
    };
    let rel = output.strip_prefix(repo).ok()?;
    let rel: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    crate::gitindex::tracks_inside(paths, &rel.join("/")).then_some(Guard::Tracked)
}

/// The project guard for one build output folder, read fresh (the clean-time re-check): a
/// keypair file or a `.git` anywhere inside, or a file Git tracks inside.
pub fn project_guard(root: &Path, output: &Path) -> Option<Guard> {
    let mut stack = vec![output.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name();
            if let Some(g) = guard_name(&name.to_string_lossy(), ft.is_dir()) {
                return Some(g);
            }
            if ft.is_dir() {
                stack.push(e.path());
            }
        }
    }
    let parent = output.parent()?;
    let repo = find_repo(root, parent)?;
    index_guard(&read_git_index(&repo), &repo, output)
}

// ---------- Re-check at clean time ----------

/// Re-checks items right before they move. Build it once for a clean: it reads the process
/// list and (when an owner has to be looked up) the installed apps once.
pub struct Rechecker<'a> {
    env: &'a Env,
    running: RunningSet,
    apps: OnceLock<InstalledApps>,
}

impl<'a> Rechecker<'a> {
    pub fn new(env: &'a Env) -> Rechecker<'a> {
        Rechecker {
            env,
            running: env.procs.running(),
            apps: OnceLock::new(),
        }
    }

    /// Checks the item and returns the paths still to move, or the reason it is skipped.
    pub fn check(&self, item: &Item) -> Result<Vec<PathBuf>, String> {
        if !item.class.selectable() {
            return Err("Tidy won't remove this".to_string());
        }
        // An unreadable process list means "unknown", never "nothing is running".
        if self.running.is_unknown() {
            return Err(PROCS_UNKNOWN.to_string());
        }
        let paths = item.paths();
        let mut ok = Vec::new();
        let mut first_err: Option<String> = None;
        for p in paths {
            match self.check_path(item, &p) {
                Ok(()) => ok.push(p),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        if ok.is_empty() {
            return Err(first_err.unwrap_or_else(|| "No longer there".to_string()));
        }
        Ok(ok)
    }

    fn check_path(&self, item: &Item, path: &Path) -> Result<(), String> {
        let env = self.env;
        let rel = path
            .strip_prefix(&env.root)
            .map_err(|_| crate::trash::Refusal::Outside.reason().to_string())?
            .to_path_buf();
        let at = match check(&env.root, path) {
            Checked::Inside(at) => at,
            Checked::Missing => return Err("No longer there".to_string()),
            Checked::Refused(why) => return Err(why.reason().to_string()),
        };
        // The resolved path must be the scanned one: no link anywhere between the root and
        // the item, or the move would take the link's target instead.
        let canonical_root = env
            .root
            .canonicalize()
            .map_err(|_| crate::trash::Refusal::Unresolvable.reason().to_string())?;
        if at != canonical_root.join(&rel) {
            return Err(crate::trash::Refusal::Linked.reason().to_string());
        }
        if path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err("Now a link, skipped".to_string());
        }
        if env.settings.is_ignored(path) {
            return Err("On your ignore list".to_string());
        }
        if is_denied_rel(&rel) || rules::is_excluded_rel(&rel) {
            return Err(crate::trash::Refusal::Denied.reason().to_string());
        }
        let rule_id = item.rule_id();
        let name = file_name_of(path);
        if let Some(rule) = rules::project_rule(rule_id) {
            let parent = path.parent().ok_or("Not a full path")?;
            if !rule.names.contains(&name.as_str())
                || markers_class(rule, parent, path, &name).is_none()
            {
                return Err("The project files are gone, skipped".to_string());
            }
            if let Some(guard) = project_guard(&env.root, path) {
                return Err(guard.reason().to_string());
            }
            if let rules::Owner::Fixed(info) = rule.owner {
                self.owner_running(&info.to_owner())?;
            }
            return Ok(());
        }
        let rule = rules::rule(rule_id).ok_or("Tidy doesn't know this rule")?;
        if !self.path_fits(rule, &rel) {
            return Err("Not where the rule expects it, skipped".to_string());
        }
        let owner = match rule.owner {
            rules::Owner::None => None,
            rules::Owner::Fixed(info) => Some(info.to_owner()),
            rules::Owner::Folder => self.folder_owner(rule, &rel),
        };
        if let Some(o) = owner {
            self.owner_running(&o)?;
        }
        if rule.in_use && uses_path(&self.running, path) {
            return Err("In use, skipped".to_string());
        }
        Ok(())
    }

    fn owner_running(&self, owner: &ResolvedOwner) -> Result<(), String> {
        match owner.blocked_by(&self.running) {
            Some(_) => Err(format!("{} opened, skipped", owner.name)),
            None => Ok(()),
        }
    }

    fn apps(&self) -> &InstalledApps {
        self.apps.get_or_init(|| InstalledApps::load(self.env))
    }

    /// The owner of a path under an `Owner::Folder` rule, resolved the way the scan does.
    fn folder_owner(&self, rule: &Rule, rel: &Path) -> Option<ResolvedOwner> {
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let has_files_target = rule.targets.iter().any(|t| matches!(t, Target::Files(_)));
        let has_chromium = rule
            .targets
            .iter()
            .any(|t| matches!(t, Target::Chromium(_)));
        let has_cache = rule
            .targets
            .iter()
            .any(|t| matches!(t, Target::CacheFolders(_)));
        if has_files_target {
            // Library/Logs/<app>/<file>
            let app = parts.get(parts.len().checked_sub(2)?)?;
            self.apps().resolve(app, true)
        } else if has_chromium {
            // Library/Application Support/<app>/...
            self.apps().resolve(parts.get(2)?, true)
        } else if has_cache {
            self.apps().resolve(parts.last()?, false)
        } else {
            self.apps().resolve(parts.last()?, true)
        }
    }

    /// Whether `rel` is a place the rule (or a later reclassification of it) can produce.
    fn path_fits(&self, rule: &Rule, rel: &Path) -> bool {
        // An unknown-owner cache row borrows the cache rules' targets.
        let rules_to_try: Vec<&Rule> = if rule.id == UNKNOWN_CACHE.id {
            RULES
                .iter()
                .filter(|r| {
                    r.targets
                        .iter()
                        .any(|t| matches!(t, Target::CacheFolders(_) | Target::Chromium(_)))
                })
                .collect()
        } else {
            vec![rule]
        };
        rules_to_try.iter().any(|r| {
            r.targets.iter().any(|t| match *t {
                Target::Glob(p) => glob_fits(p, rel),
                Target::Files(p) => rel.parent().is_some_and(|parent| glob_fits(p, parent)),
                Target::CacheFolders(dir) => {
                    rel.parent().is_some_and(|parent| parent == Path::new(dir))
                }
                Target::Chromium(subs) => subs.iter().any(|s| {
                    let rel = rel.to_string_lossy();
                    rel.starts_with("Library/Application Support/")
                        && rel.ends_with(&format!("/{s}"))
                }),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_match_names_prefixes_and_alternatives() {
        assert!(segment_matches("*", "anything"));
        assert!(!segment_matches("*", ".hidden"));
        assert!(segment_matches("models--*", "models--org--name"));
        assert!(segment_matches("AndroidStudio*", "androidstudio2024.1"));
        assert!(segment_matches("a|b", "B"));
        assert!(!segment_matches("a|b", "c"));
        assert!(glob_fits("Library/Caches/*", Path::new("Library/Caches/x")));
        assert!(!glob_fits(
            "Library/Caches/*",
            Path::new("Library/Caches/x/y")
        ));
        assert!(!glob_fits("Library/Caches/*", Path::new("Library/Other/x")));
    }

    #[test]
    fn gitignore_lines_name_a_folder() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".gitignore"),
            "# build\nnode_modules/\n/dist\n!keep\n**/out\n  build/  \n",
        )
        .unwrap();
        for name in ["node_modules", "dist", "out", "build"] {
            assert!(gitignore_names(tmp.path(), name), "{name}");
        }
        assert!(!gitignore_names(tmp.path(), "keep"));
        assert!(!gitignore_names(tmp.path(), "target"));
        assert!(!gitignore_names(&tmp.path().join("none"), "dist"));
    }

    #[test]
    fn keep_keys_group_by_model_and_browser() {
        assert_eq!(
            keep_key(KeepBy::DeviceModel, "iPhone15,2 17.5 (21F90)"),
            "iPhone15,2"
        );
        assert_eq!(keep_key(KeepBy::DeviceModel, "17.5 (21F90)"), "");
        assert_eq!(
            keep_key(KeepBy::BeforeLastDash, "chromium-1148"),
            "chromium"
        );
        assert_eq!(
            keep_key(KeepBy::BeforeLastDash, "chromium_headless_shell-1148"),
            "chromium_headless_shell"
        );
        assert_eq!(keep_key(KeepBy::BeforeLastDash, "plain"), "plain");
        assert_eq!(keep_key(KeepBy::Folder, "anything"), "");
    }
}
