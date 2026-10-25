//! "System Data on this Mac" (design 17.2): what Tidy can measure of the space that System
//! Settings counts as System Data, without Full Disk Access.
//!
//! Everything here only reads. Folders are measured with [`walk::measure`] (denylist on, never
//! following symlinks or crossing volumes), and local Time Machine snapshots are listed with
//! the read-only `tmutil listlocalsnapshots /` through the [`crate::env::CommandRunner`] seam.
//! Tidy never runs `docker` or any other `tmutil` verb; it shows those commands for the user to
//! copy (design section 4, C30). The report never claims that Apple's System Data figure will
//! shrink (C29): macOS computes that figure its own way.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::env::{Env, TMUTIL_LIST};
use crate::walk::{self, Inodes, WalkOpts};

/// The `tmutil` Tidy runs, with [`TMUTIL_LIST`].
pub const TMUTIL: &str = "/usr/bin/tmutil";
/// How long the snapshot listing may take.
pub const TMUTIL_TIMEOUT: Duration = Duration::from_secs(10);

/// The rows of the sheet, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SysKind {
    Caches,
    Logs,
    Xcode,
    Simulators,
    VirtualMachines,
    IosBackups,
    Docker,
    Snapshots,
}

impl SysKind {
    pub const ALL: [SysKind; 8] = [
        SysKind::Caches,
        SysKind::Logs,
        SysKind::Xcode,
        SysKind::Simulators,
        SysKind::VirtualMachines,
        SysKind::IosBackups,
        SysKind::Docker,
        SysKind::Snapshots,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SysKind::Caches => "Caches",
            SysKind::Logs => "Logs",
            SysKind::Xcode => "Xcode build data",
            SysKind::Simulators => "Simulators",
            SysKind::VirtualMachines => "Virtual machines",
            SysKind::IosBackups => "iPhone and iPad backups",
            SysKind::Docker => "Docker",
            SysKind::Snapshots => "Local Time Machine snapshots",
        }
    }

    /// The folders a row adds up, relative to the home folder. Empty for the rows Tidy
    /// does not measure (Docker, snapshots).
    pub fn folders(self) -> &'static [&'static str] {
        match self {
            SysKind::Caches => &["Library/Caches"],
            SysKind::Logs => &["Library/Logs"],
            SysKind::Xcode => &[
                "Library/Developer/Xcode/DerivedData",
                "Library/Developer/Xcode/Archives",
                "Library/Developer/Xcode/iOS DeviceSupport",
                "Library/Developer/Xcode/watchOS DeviceSupport",
                "Library/Developer/Xcode/tvOS DeviceSupport",
                "Library/Developer/Xcode/visionOS DeviceSupport",
                "Library/Developer/Xcode/xrOS DeviceSupport",
                "Library/Developer/Xcode/UserData/Previews",
            ],
            SysKind::Simulators => &["Library/Developer/CoreSimulator"],
            SysKind::VirtualMachines => &[
                ".colima",
                ".lima",
                ".android/avd",
                "Library/Application Support/Claude/vm_bundles",
            ],
            SysKind::IosBackups => &["Library/Application Support/MobileSync/Backup"],
            SysKind::Docker | SysKind::Snapshots => &[],
        }
    }

    /// Whether Tidy only reports this row and never offers to move it.
    pub fn report_only(self) -> bool {
        matches!(self, SysKind::Docker | SysKind::Snapshots)
    }
}

/// How big a row is, as far as Tidy can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "bytes")]
pub enum SysSize {
    /// Allocated bytes Tidy could read.
    Measured(u64),
    /// Tidy does not open where it lives (Docker's app container).
    NotMeasured,
    /// macOS keeps it behind Full Disk Access, which Tidy never asks for.
    NeedsFullDiskAccess,
    /// macOS does not report it (snapshot sizes).
    NotReported,
    /// The listing found nothing.
    None,
    /// The listing could not run.
    NotListed,
}

impl SysSize {
    /// The text in the size column when there is no number.
    pub fn label(self) -> Option<&'static str> {
        match self {
            SysSize::Measured(_) => None,
            SysSize::NotMeasured => Some("Not measured"),
            SysSize::NeedsFullDiskAccess => Some("Needs Full Disk Access"),
            SysSize::NotReported => Some("Size not reported"),
            SysSize::None => Some("None right now"),
            SysSize::NotListed => Some("Not listed"),
        }
    }

    pub fn bytes(self) -> u64 {
        match self {
            SysSize::Measured(b) => b,
            _ => 0,
        }
    }
}

/// A command the row suggests. Tidy never runs it (except the snapshot listing, which only
/// reads); the sheet has a Copy button for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyCommand {
    /// What it does, in a few words.
    pub label: String,
    pub command: String,
}

impl CopyCommand {
    fn new(label: &str, command: &str) -> CopyCommand {
        CopyCommand {
            label: label.to_string(),
            command: command.to_string(),
        }
    }
}

/// One row of the sheet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SysRow {
    pub kind: SysKind,
    pub title: String,
    /// What it is and what to do about it, one or two sentences.
    pub note: String,
    pub size: SysSize,
    pub report_only: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CopyCommand>,
}

/// The local Time Machine snapshots `tmutil` listed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshots {
    /// Snapshot names, oldest first as `tmutil` prints them.
    pub names: Vec<String>,
}

impl Snapshots {
    /// Reads the output of `tmutil listlocalsnapshots /`: a header line ("Snapshots for disk
    /// /:") and one snapshot name per line (`com.apple.TimeMachine.2026-10-25-101112.local`).
    pub fn parse(stdout: &str) -> Snapshots {
        Snapshots {
            names: stdout
                .lines()
                .map(str::trim)
                .filter(|l| l.starts_with("com.apple."))
                .map(str::to_string)
                .collect(),
        }
    }

    /// The newest snapshot's date as "Oct 25 at 10:11", from the name. None when no name
    /// carries a date.
    pub fn newest(&self) -> Option<String> {
        self.names
            .iter()
            .filter_map(|n| snapshot_date(n))
            .max()
            .map(|(_, mo, d, h, mi)| format!("{} {d} at {h:02}:{mi:02}", MONTHS[(mo - 1) as usize]))
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `(year, month, day, hour, minute)` from a name such as
/// `com.apple.TimeMachine.2026-10-25-101112.local`.
pub fn snapshot_date(name: &str) -> Option<(u32, u32, u32, u32, u32)> {
    let stamp = name
        .split('.')
        .find(|part| part.len() == 17 && part.as_bytes()[4] == b'-')?;
    let num = |r: std::ops::Range<usize>| stamp.get(r)?.parse::<u32>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi) = (num(11..13)?, num(13..15)?);
    ((1..=12).contains(&mo) && (1..=31).contains(&d) && h < 24 && mi < 60)
        .then_some((y, mo, d, h, mi))
}

/// The whole sheet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemDataReport {
    /// When it was measured (Unix seconds).
    pub at: u64,
    pub rows: Vec<SysRow>,
}

impl SystemDataReport {
    /// The sum of the rows Tidy measured.
    pub fn measured_bytes(&self) -> u64 {
        self.rows.iter().map(|r| r.size.bytes()).sum()
    }

    pub fn row(&self, kind: SysKind) -> Option<&SysRow> {
        self.rows.iter().find(|r| r.kind == kind)
    }
}

/// The row about everything else, which has no number.
pub const REST_TITLE: &str = "Everything else";
pub const REST_NOTE: &str = "macOS itself, other apps' private data, and Mail, Messages and Photos libraries. Tidy can't measure these without Full Disk Access, and it doesn't ask for it.";
/// The sentence under the rows. It never promises that the System Data figure will shrink.
pub const FOOTER: &str = "Moving things to the Trash frees disk space, but macOS computes System Data its own way, so that figure may not drop by the same amount. Caches also grow back as apps use them.";
/// The sentence above the rows.
pub const LEAD: &str = "System Settings counts caches, logs, developer files, virtual machines and snapshots as System Data. Here is the part Tidy can measure without Full Disk Access.";

/// Whether a folder can be listed: None when it does not exist, Some(false) when macOS keeps
/// it closed.
fn readable(path: &Path) -> Option<bool> {
    match path.symlink_metadata() {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Some(false),
        Err(_) => return None,
        Ok(m) if !m.is_dir() => return None,
        Ok(_) => {}
    }
    match std::fs::read_dir(path) {
        Ok(_) => Some(true),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Some(false),
        Err(_) => None,
    }
}

/// Measures the System Data rows under `env.root` and lists the local snapshots. Read-only.
pub fn measure(env: &Env) -> SystemDataReport {
    let inodes = Arc::new(Inodes::new());
    let opts = WalkOpts {
        inodes: Some(inodes),
        ..WalkOpts::for_root(&env.root)
    };
    let mut rows = Vec::new();
    for kind in SysKind::ALL {
        let row = match kind {
            SysKind::Docker => docker_row(env),
            SysKind::Snapshots => Some(snapshot_row(&list_snapshots(env))),
            SysKind::IosBackups => backups_row(env, &opts),
            _ => measured_row(env, kind, &opts),
        };
        rows.extend(row);
    }
    SystemDataReport {
        at: env.now(),
        rows,
    }
}

fn note(kind: SysKind) -> &'static str {
    match kind {
        SysKind::Caches => {
            "Apps keep these to load faster. Cleanup offers the ones Tidy knows are safe."
        }
        SysKind::Logs => "App logs and crash reports. Cleanup offers the old ones.",
        SysKind::Xcode => {
            "Build data, archives and device symbols. Cleanup offers old build data; archives and device symbols wait for your review."
        }
        SysKind::Simulators => "Simulator devices and what you installed on them.",
        SysKind::VirtualMachines => {
            "Disks of Colima, Lima, Android emulators and Claude. Remove them from the app that made them."
        }
        SysKind::IosBackups => {
            "Finder shows and removes them: select the device, then Manage Backups."
        }
        SysKind::Docker => {
            "Docker keeps images, containers and build cache in one disk image inside its app container, which Tidy doesn't open. Run these in Terminal to see and reclaim that space."
        }
        SysKind::Snapshots => {
            "macOS removes them after 24 hours, or sooner when the disk runs low, and doesn't report their size."
        }
    }
}

fn commands(kind: SysKind) -> Vec<CopyCommand> {
    match kind {
        SysKind::Simulators => vec![CopyCommand::new(
            "Remove simulators whose runtime is gone",
            "xcrun simctl delete unavailable",
        )],
        SysKind::Docker => vec![
            CopyCommand::new("See what Docker uses", "docker system df"),
            CopyCommand::new(
                "Remove stopped containers, unused images and build cache",
                "docker system prune",
            ),
        ],
        SysKind::Snapshots => vec![CopyCommand::new(
            "List them",
            &format!("tmutil {}", TMUTIL_LIST.join(" ")),
        )],
        _ => Vec::new(),
    }
}

fn row(kind: SysKind, note: String, size: SysSize) -> SysRow {
    SysRow {
        kind,
        title: kind.title().to_string(),
        note,
        size,
        report_only: kind.report_only(),
        commands: commands(kind),
    }
}

/// A row that adds up its folders. Left out when they hold nothing.
fn measured_row(env: &Env, kind: SysKind, opts: &WalkOpts) -> Option<SysRow> {
    let bytes: u64 = kind
        .folders()
        .iter()
        .map(|rel| walk::measure(&env.at(rel), opts).alloc)
        .sum();
    (bytes > 0).then(|| row(kind, note(kind).to_string(), SysSize::Measured(bytes)))
}

/// Backups: measured when the folder can be read, "Needs Full Disk Access" when macOS keeps
/// it closed, left out when there is none.
fn backups_row(env: &Env, opts: &WalkOpts) -> Option<SysRow> {
    let path = env.at(SysKind::IosBackups.folders()[0]);
    match readable(&path)? {
        true => measured_row(env, SysKind::IosBackups, opts),
        false => Some(row(
            SysKind::IosBackups,
            format!(
                "macOS keeps this folder behind Full Disk Access. {}",
                note(SysKind::IosBackups)
            ),
            SysSize::NeedsFullDiskAccess,
        )),
    }
}

/// Docker: shown when `~/.docker` exists. Its disk image lives in Docker's app container,
/// which Tidy never opens, so it has no size.
fn docker_row(env: &Env) -> Option<SysRow> {
    env.at(".docker")
        .symlink_metadata()
        .ok()
        .filter(|m| m.is_dir())
        .map(|_| {
            row(
                SysKind::Docker,
                note(SysKind::Docker).to_string(),
                SysSize::NotMeasured,
            )
        })
}

/// The snapshot listing, or None when `tmutil` could not run (refused under `TIDY_HOME`, not
/// found, timed out, or a non-zero exit).
pub fn list_snapshots(env: &Env) -> Option<Snapshots> {
    match env.runner.run(TMUTIL, &TMUTIL_LIST, TMUTIL_TIMEOUT) {
        Ok(out) if out.success() => Some(Snapshots::parse(&out.stdout)),
        Ok(out) => {
            tracing::debug!("tmutil listlocalsnapshots exited {:?}", out.status);
            None
        }
        Err(e) => {
            tracing::debug!("tmutil listlocalsnapshots: {e}");
            None
        }
    }
}

/// The snapshot row for a listing (None: it could not run).
pub fn snapshot_row(listing: &Option<Snapshots>) -> SysRow {
    let base = note(SysKind::Snapshots);
    match listing {
        None => row(
            SysKind::Snapshots,
            format!("Tidy couldn't list them this time. {base}"),
            SysSize::NotListed,
        ),
        Some(s) if s.names.is_empty() => row(
            SysKind::Snapshots,
            "None right now. macOS makes them while Time Machine backs up and removes them on its own.".to_string(),
            SysSize::None,
        ),
        Some(s) => {
            let count = match s.names.len() {
                1 => "1 snapshot".to_string(),
                n => format!("{n} snapshots"),
            };
            let lead = match s.newest() {
                Some(when) => format!("{count}, the newest from {when}."),
                None => format!("{count}."),
            };
            row(SysKind::Snapshots, format!("{lead} {base}"), SysSize::NotReported)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{CommandOutput, ReplayError, ReplayRunner};
    use std::fs;

    const LISTING: &str = "Snapshots for disk /:\ncom.apple.TimeMachine.2026-10-25-081502.local\ncom.apple.TimeMachine.2026-10-25-101112.local\n";

    fn put(root: &Path, rel: &str, len: usize) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; len]).unwrap();
    }

    fn env_with(runner: ReplayRunner) -> (tempfile::TempDir, Env) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let env = Env::for_tests(&home, &tmp.path().join("tidy")).with_runner(Arc::new(runner));
        (tmp, env)
    }

    #[test]
    fn snapshot_listings_parse_and_name_the_newest() {
        let s = Snapshots::parse(LISTING);
        assert_eq!(s.names.len(), 2);
        assert_eq!(s.newest().as_deref(), Some("Oct 25 at 10:11"));
        assert_eq!(Snapshots::parse("Snapshots for disk /:\n").names.len(), 0);
        assert_eq!(
            snapshot_date("com.apple.TimeMachine.2026-13-01-000000.local"),
            None
        );
        assert_eq!(snapshot_date("com.apple.os.update-ABC"), None);
    }

    #[test]
    fn the_snapshot_row_never_claims_a_size() {
        let listed = snapshot_row(&Some(Snapshots::parse(LISTING)));
        assert_eq!(listed.size, SysSize::NotReported);
        assert!(listed.report_only);
        assert!(
            listed
                .note
                .starts_with("2 snapshots, the newest from Oct 25 at 10:11.")
        );
        assert_eq!(listed.commands[0].command, "tmutil listlocalsnapshots /");
        assert_eq!(
            snapshot_row(&Some(Snapshots::default())).size,
            SysSize::None
        );
        let failed = snapshot_row(&None);
        assert_eq!(failed.size, SysSize::NotListed);
        assert!(failed.note.starts_with("Tidy couldn't list them"));
        assert_eq!(failed.size.label(), Some("Not listed"));
    }

    #[test]
    fn measure_adds_up_known_folders_and_lists_snapshots_through_the_seam() {
        let runner = ReplayRunner::new().with(
            "/usr/bin/tmutil listlocalsnapshots /",
            CommandOutput::ok(LISTING),
        );
        let (_tmp, env) = env_with(runner);
        put(&env.root, "Library/Caches/com.example.app/blob", 64 * 1024);
        put(&env.root, "Library/Logs/App/old.log", 8 * 1024);
        put(
            &env.root,
            "Library/Developer/Xcode/DerivedData/App-x/out.o",
            32 * 1024,
        );
        put(
            &env.root,
            "Library/Developer/CoreSimulator/Devices/A/data",
            16 * 1024,
        );
        put(&env.root, ".colima/default/disk", 16 * 1024);
        fs::create_dir_all(env.root.join(".docker")).unwrap();
        let r = measure(&env);
        let kinds: Vec<SysKind> = r.rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                SysKind::Caches,
                SysKind::Logs,
                SysKind::Xcode,
                SysKind::Simulators,
                SysKind::VirtualMachines,
                SysKind::Docker,
                SysKind::Snapshots
            ]
        );
        assert!(r.row(SysKind::Caches).unwrap().size.bytes() >= 64 * 1024);
        let docker = r.row(SysKind::Docker).unwrap();
        assert_eq!(docker.size, SysSize::NotMeasured);
        assert!(docker.report_only);
        let cmds: Vec<&str> = docker.commands.iter().map(|c| c.command.as_str()).collect();
        assert_eq!(cmds, ["docker system df", "docker system prune"]);
        assert_eq!(
            r.row(SysKind::Snapshots).unwrap().size,
            SysSize::NotReported
        );
        assert_eq!(
            r.measured_bytes(),
            r.rows.iter().map(|x| x.size.bytes()).sum::<u64>()
        );
        assert_eq!(r.at, env.now());
        // Docker and the snapshots add nothing to the measured total.
        assert_eq!(docker.size.bytes(), 0);
    }

    #[test]
    fn empty_rows_are_left_out_and_a_failed_listing_says_so() {
        let runner =
            ReplayRunner::new().with_error("tmutil listlocalsnapshots /", ReplayError::Refused);
        let (_tmp, env) = env_with(runner);
        let r = measure(&env);
        assert_eq!(r.rows.len(), 1, "only the snapshot row: {:?}", r.rows);
        assert_eq!(r.rows[0].size, SysSize::NotListed);
        assert_eq!(r.measured_bytes(), 0);
    }

    #[test]
    fn only_the_listing_runs_and_nothing_else() {
        let runner = Arc::new(ReplayRunner::new());
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir_all(home.join(".docker")).unwrap();
        let env = Env::for_tests(&home, &tmp.path().join("tidy")).with_runner(runner.clone());
        let _ = measure(&env);
        assert_eq!(runner.calls(), ["/usr/bin/tmutil listlocalsnapshots /"]);
        assert!(!runner.ran("docker"));
    }

    #[test]
    fn an_unreadable_backup_folder_needs_full_disk_access() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, env) = env_with(ReplayRunner::new());
        let backup = env.at(SysKind::IosBackups.folders()[0]);
        put(
            &env.root,
            "Library/Application Support/MobileSync/Backup/X/f",
            4096,
        );
        let r = measure(&env);
        assert!(r.row(SysKind::IosBackups).unwrap().size.bytes() >= 4096);
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o000)).unwrap();
        let r = measure(&env);
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o755)).unwrap();
        let row = r.row(SysKind::IosBackups).unwrap();
        assert_eq!(row.size, SysSize::NeedsFullDiskAccess);
        assert!(row.note.contains("Full Disk Access"));
    }

    #[test]
    fn copy_is_plain_and_never_promises_a_smaller_system_data_figure() {
        let mut texts = vec![REST_TITLE, REST_NOTE, FOOTER, LEAD];
        for k in SysKind::ALL {
            texts.push(k.title());
            texts.push(note(k));
        }
        for t in texts {
            assert!(!t.contains('\u{2014}') && !t.contains('\u{2013}'), "{t}");
            let lower = t.to_lowercase();
            for bad in [
                "will shrink",
                "reduce system data",
                "free up system data",
                "!",
            ] {
                assert!(!lower.contains(bad), "{t}");
            }
        }
        assert!(FOOTER.contains("may not drop"));
    }
}
