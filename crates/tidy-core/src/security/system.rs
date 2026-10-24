//! The system checks of design section 5.4: FileVault (`fdesetup status`), the firewall
//! (`socketfilterfw --getglobalstate`), Gatekeeper (`spctl --status`), System Integrity
//! Protection (`csrutil status`), XProtect (its Info.plist) and macOS updates
//! (`softwareupdate --list`, only when asked for).
//!
//! Every command goes through the [`CommandRunner`] seam with a timeout. A runner error, a
//! timeout, an exit code the command does not use for an answer, or output Tidy does not
//! recognize gives a gray row that says why, never a green one. Exit 0 is an answer for every
//! command; `spctl --status` also answers `assessments disabled` with exit 1. The parsers are
//! pure, so tests feed them captured outputs.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::cf::{Plist, read_plist};
use crate::env::{CommandOutput, CommandRunner};
use crate::model::{CheckId, CheckResult, Status, unix_secs};

pub const FDESETUP: &str = "/usr/bin/fdesetup";
pub const SOCKETFILTERFW: &str = "/usr/libexec/ApplicationFirewall/socketfilterfw";
pub const SPCTL: &str = "/usr/sbin/spctl";
pub const CSRUTIL: &str = "/usr/bin/csrutil";
pub const SOFTWAREUPDATE: &str = "/usr/sbin/softwareupdate";

/// How long a status command may take. They answer in well under a second.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
/// How long `softwareupdate --list` may take (design section 5.4).
pub const UPDATES_TIMEOUT: Duration = Duration::from_secs(60);

/// XProtect counts as out of date after this many days without an update.
pub const XPROTECT_MAX_DAYS: u64 = 60;

/// Where XProtect keeps its Info.plist. macOS 15 and later update the copy under
/// `/var/protected`; the check takes the newer of the two.
pub const XPROTECT_PLISTS: [&str; 2] = [
    "/Library/Apple/System/Library/CoreServices/XProtect.bundle/Contents/Info.plist",
    "/var/protected/xprotect/XProtect.bundle/Contents/Info.plist",
];

/// What the macOS updates row says when the check did not run.
pub const UPDATES_NOT_CHECKED: &str = "Not checked. Tidy looks for macOS updates only when you ask, because it can take up to a minute.";

// ---------- Running a status command ----------

/// The tool name in a gray sentence: `fdesetup`.
fn tool(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// The gray row for a command that could not run.
pub fn gray_for_error(id: CheckId, program: &str, e: &io::Error) -> CheckResult {
    let tool = tool(program);
    let sentence = match e.kind() {
        io::ErrorKind::TimedOut => {
            format!("Tidy couldn't check this: {tool} did not answer in time.")
        }
        io::ErrorKind::NotFound => {
            format!("Tidy couldn't check this: {tool} was not found on this Mac.")
        }
        io::ErrorKind::PermissionDenied => {
            format!("Tidy couldn't check this: macOS did not let Tidy run {tool}.")
        }
        _ => format!("Tidy couldn't check this: {tool} did not run."),
    };
    CheckResult::new(id, Status::Gray, sentence)
}

/// Runs `program args` and parses what it printed with `parse`, which sees stdout and stderr
/// joined. `ok_exits` lists the exit codes the command uses for a real answer (always 0, and
/// 1 for `spctl --status`, which reports `assessments disabled` with exit 1). Gray on any
/// other exit code, a failure to run, or unknown output.
pub fn run_check(
    runner: &dyn CommandRunner,
    id: CheckId,
    program: &str,
    args: &[&str],
    timeout: Duration,
    ok_exits: &[i32],
    parse: fn(&str) -> Option<CheckResult>,
) -> CheckResult {
    match runner.run(program, args, timeout) {
        Err(e) => gray_for_error(id, program, &e),
        Ok(out) => parse_output_allowing(id, program, &out, ok_exits, parse),
    }
}

/// The row for a finished command that answers with exit 0: gray for any other exit code or
/// unknown output.
pub fn parse_output(
    id: CheckId,
    program: &str,
    out: &CommandOutput,
    parse: fn(&str) -> Option<CheckResult>,
) -> CheckResult {
    parse_output_allowing(id, program, out, &[0], parse)
}

/// [`parse_output`] for a command whose real answers use any of the exit codes in
/// `ok_exits`. A signal, or an exit code outside the list, gives the gray error row.
pub fn parse_output_allowing(
    id: CheckId,
    program: &str,
    out: &CommandOutput,
    ok_exits: &[i32],
    parse: fn(&str) -> Option<CheckResult>,
) -> CheckResult {
    let tool = tool(program);
    let answered = out.success() || out.status.is_some_and(|code| ok_exits.contains(&code));
    if !answered {
        return CheckResult::new(
            id,
            Status::Gray,
            format!("Tidy couldn't check this: {tool} ended with an error."),
        );
    }
    let text = format!("{}\n{}", out.stdout, out.stderr);
    parse(&text).unwrap_or_else(|| {
        CheckResult::new(
            id,
            Status::Gray,
            format!("Tidy couldn't check this: it didn't recognize what {tool} reported."),
        )
    })
}

// ---------- FileVault ----------

pub fn filevault(runner: &dyn CommandRunner) -> CheckResult {
    run_check(
        runner,
        CheckId::FileVault,
        FDESETUP,
        &["status"],
        STATUS_TIMEOUT,
        &[0],
        parse_filevault,
    )
}

/// `FileVault is On.`, `FileVault is Off.`, and the progress lines
/// (`Encryption in progress: Percent completed = 34.5`, `Decryption in progress: ...`).
pub fn parse_filevault(text: &str) -> Option<CheckResult> {
    let id = CheckId::FileVault;
    if text.contains("Decryption in progress") {
        return Some(CheckResult::new(
            id,
            Status::Amber,
            "FileVault is turning off. Once macOS finishes, the files on this Mac can be read without your password.",
        ));
    }
    if text.contains("Encryption in progress") {
        return Some(CheckResult::new(
            id,
            Status::Green,
            "FileVault is turning on. macOS is encrypting the disk in the background.",
        ));
    }
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("FileVault is "))?;
    if line.starts_with("FileVault is On") {
        Some(CheckResult::new(
            id,
            Status::Green,
            "FileVault is on. Your disk is encrypted, so your files stay private if your Mac is lost.",
        ))
    } else if line.starts_with("FileVault is Off") {
        Some(CheckResult::new(
            id,
            Status::Amber,
            "FileVault is off. Someone with your Mac in hand could read the files on its disk. Turning it on encrypts the disk.",
        ))
    } else {
        None
    }
}

// ---------- Firewall ----------

pub fn firewall(runner: &dyn CommandRunner) -> CheckResult {
    run_check(
        runner,
        CheckId::Firewall,
        SOCKETFILTERFW,
        &["--getglobalstate"],
        STATUS_TIMEOUT,
        &[0],
        parse_firewall,
    )
}

/// `Firewall is enabled. (State = 1)`, `Firewall is disabled. (State = 0)`, and state 2
/// (block all incoming connections).
pub fn parse_firewall(text: &str) -> Option<CheckResult> {
    let id = CheckId::Firewall;
    let state = text.split_once("State = ").and_then(|(_, rest)| {
        rest.trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|n| n.parse::<u32>().ok())
    });
    let state = match state {
        Some(s) => s,
        None if text.contains("Firewall is disabled") => 0,
        None if text.contains("Firewall is enabled") => 1,
        None => return None,
    };
    match state {
        0 => Some(CheckResult::new(
            id,
            Status::Amber,
            "Firewall is off. Your Mac accepts incoming connections from apps that ask for them. Most home networks are fine either way.",
        )),
        1 => Some(CheckResult::new(
            id,
            Status::Green,
            "Firewall is on. Apps need your OK before they accept incoming connections.",
        )),
        2 => Some(CheckResult::new(
            id,
            Status::Green,
            "Firewall is on and blocks every incoming connection except the ones macOS needs.",
        )),
        _ => None,
    }
}

// ---------- Gatekeeper ----------

pub fn gatekeeper(runner: &dyn CommandRunner) -> CheckResult {
    run_check(
        runner,
        CheckId::Gatekeeper,
        SPCTL,
        &["--status"],
        STATUS_TIMEOUT,
        SPCTL_OK_EXITS,
        parse_gatekeeper,
    )
}

/// The exit codes of `spctl --status` that carry an answer: 0 with `assessments enabled`,
/// 1 with `assessments disabled` (spctl calls `exit(1)` after printing that line).
pub const SPCTL_OK_EXITS: &[i32] = &[0, 1];

/// `assessments enabled` or `assessments disabled`.
pub fn parse_gatekeeper(text: &str) -> Option<CheckResult> {
    let id = CheckId::Gatekeeper;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("assessments "))?;
    match line {
        "assessments enabled" => Some(CheckResult::new(
            id,
            Status::Green,
            "Gatekeeper is on. macOS checks who made an app before it opens the first time.",
        )),
        "assessments disabled" => Some(CheckResult::new(
            id,
            Status::Amber,
            "Gatekeeper is off. Apps open without macOS checking who made them. Turning it back on is safer for apps from the internet.",
        )),
        _ => None,
    }
}

// ---------- System Integrity Protection ----------

pub fn sip(runner: &dyn CommandRunner) -> CheckResult {
    run_check(
        runner,
        CheckId::Sip,
        CSRUTIL,
        &["status"],
        STATUS_TIMEOUT,
        &[0],
        parse_sip,
    )
}

/// `System Integrity Protection status: enabled.`, `... disabled.`, or
/// `... unknown (Custom Configuration).` followed by the per-part list.
pub fn parse_sip(text: &str) -> Option<CheckResult> {
    let id = CheckId::Sip;
    let (_, rest) = text.split_once("System Integrity Protection status:")?;
    let value = rest.trim_start().to_ascii_lowercase();
    let how_to = "To turn it back on, start up in Recovery, open Terminal and run csrutil enable.";
    if value.starts_with("enabled") {
        Some(CheckResult::new(
            id,
            Status::Green,
            "System Integrity Protection is on. macOS system files are protected from changes, even by apps with admin rights.",
        ))
    } else if value.starts_with("disabled") {
        Some(CheckResult::new(
            id,
            Status::Amber,
            format!(
                "System Integrity Protection is off. Apps with admin rights can change macOS system files. {how_to}"
            ),
        ))
    } else if value.contains("custom configuration") {
        Some(CheckResult::new(
            id,
            Status::Amber,
            format!(
                "System Integrity Protection is partly off. Some macOS system files can be changed by apps with admin rights. {how_to}"
            ),
        ))
    } else {
        None
    }
}

// ---------- XProtect ----------

/// The version and update time of one XProtect Info.plist.
#[derive(Clone, Debug, PartialEq)]
pub struct XProtectInfo {
    pub plist: PathBuf,
    pub version: Option<String>,
    /// Unix seconds: the plist's `InstallationTime` when it has one, else its mtime.
    pub updated: u64,
}

/// Reads one XProtect Info.plist.
pub fn read_xprotect(plist: &Path) -> io::Result<XProtectInfo> {
    let meta = std::fs::metadata(plist)?;
    let value = read_plist(plist)?;
    let version = value
        .get("CFBundleShortVersionString")
        .and_then(Plist::as_str)
        .map(str::to_string);
    let installed = value
        .get("InstallationTime")
        .and_then(Plist::as_date)
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(|d| d as u64);
    let mtime = meta.modified().map(unix_secs).unwrap_or(0);
    Ok(XProtectInfo {
        plist: plist.to_path_buf(),
        version,
        updated: installed.map_or(mtime, |i| i.max(mtime)),
    })
}

/// "today", "1 day ago", "12 days ago".
fn days_ago(days: u64) -> String {
    match days {
        0 => "today".into(),
        1 => "1 day ago".into(),
        n => format!("{n} days ago"),
    }
}

/// The XProtect row from the newest of `plists`, judged at `now` (Unix seconds). Amber after
/// [`XPROTECT_MAX_DAYS`] days; gray when no plist can be read.
pub fn xprotect(plists: &[PathBuf], now: u64) -> CheckResult {
    let id = CheckId::XProtect;
    let newest = plists
        .iter()
        .filter_map(|p| match read_xprotect(p) {
            Ok(info) => Some(info),
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::debug!("XProtect: cannot read {}: {e}", p.display());
                }
                None
            }
        })
        .max_by_key(|i| i.updated);
    let Some(info) = newest else {
        return CheckResult::new(
            id,
            Status::Gray,
            "Tidy couldn't read the XProtect version on this Mac.",
        );
    };
    let days = now.saturating_sub(info.updated) / 86_400;
    let version = info
        .version
        .as_deref()
        .map(|v| format!(" (version {v})"))
        .unwrap_or_default();
    let mut row = if days > XPROTECT_MAX_DAYS {
        CheckResult::new(
            id,
            Status::Amber,
            format!(
                "XProtect was last updated {}{version}. macOS updates it in the background, so updates may be paused. Software Update can check now.",
                days_ago(days)
            ),
        )
    } else {
        CheckResult::new(
            id,
            Status::Green,
            format!(
                "XProtect, the malware scanner built into macOS, was updated {}{version}.",
                days_ago(days)
            ),
        )
    };
    row.detail = info.version;
    row
}

// ---------- macOS updates ----------

/// The macOS updates row. `include` false (the default) gives the gray "not checked" row
/// without running anything; only true runs `softwareupdate --list`.
pub fn updates(runner: &dyn CommandRunner, include: bool) -> CheckResult {
    if !include {
        return CheckResult::new(CheckId::MacosUpdates, Status::Gray, UPDATES_NOT_CHECKED);
    }
    run_check(
        runner,
        CheckId::MacosUpdates,
        SOFTWAREUPDATE,
        &["--list"],
        UPDATES_TIMEOUT,
        &[0],
        parse_updates,
    )
}

/// The update names in `softwareupdate --list` output. Some(empty) for "No new software
/// available.", None for output that is neither.
///
/// Current format (macOS 10.15 and later):
/// ```text
/// Software Update found the following new or updated software:
/// * Label: macOS Sequoia 15.1-24B83
///     Title: macOS Sequoia 15.1, Version: 15.1, Size: 3145728KiB, Recommended: YES, Action: restart,
/// ```
/// Older format: `   * Label-1.0` then a tab and `Title (1.0), 1234K [recommended]`.
pub fn update_names(text: &str) -> Option<Vec<String>> {
    if text.contains("No new software available") {
        return Some(Vec::new());
    }
    if !text.contains("Software Update found the following") {
        return None;
    }
    let mut names = Vec::new();
    let mut pending_label: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(label) = t.strip_prefix("* Label:") {
            if let Some(l) = pending_label.take() {
                names.push(l);
            }
            pending_label = Some(label.trim().to_string());
        } else if let Some(title) = t.strip_prefix("Title:") {
            let title = title.split(',').next().unwrap_or("").trim();
            pending_label = None;
            if !title.is_empty() {
                names.push(title.to_string());
            }
        } else if let Some(label) = t.strip_prefix("* ") {
            if let Some(l) = pending_label.take() {
                names.push(l);
            }
            pending_label = Some(label.trim().trim_end_matches('-').to_string());
        } else if pending_label.is_some() && line.starts_with('\t') && !t.is_empty() {
            // Older format: the line under the label is "Title (version), size [flags]".
            let title = t.split(" (").next().unwrap_or(t).trim();
            pending_label = None;
            if !title.is_empty() {
                names.push(title.to_string());
            }
        }
    }
    if let Some(l) = pending_label.take() {
        names.push(l);
    }
    names.retain(|n| !n.is_empty());
    if names.is_empty() { None } else { Some(names) }
}

/// The macOS updates row from `softwareupdate --list` output.
pub fn parse_updates(text: &str) -> Option<CheckResult> {
    let id = CheckId::MacosUpdates;
    let names = update_names(text)?;
    let mut row = match names.len() {
        0 => CheckResult::new(id, Status::Green, "macOS is up to date."),
        1 => CheckResult::new(
            id,
            Status::Amber,
            format!(
                "1 update is available: {}. Software Update can install it.",
                names[0]
            ),
        ),
        n => CheckResult::new(
            id,
            Status::Amber,
            format!(
                "{n} updates are available: {}. Software Update can install them.",
                names.join(", ")
            ),
        ),
    };
    if !names.is_empty() {
        row.detail = Some(names.join(", "));
    }
    Some(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{ReplayError, ReplayRunner};

    #[test]
    fn errors_and_unknown_output_are_gray() {
        let r = ReplayRunner::new()
            .with_error("fdesetup status", ReplayError::TimedOut)
            .with("spctl --status", CommandOutput::ok("something new\n"))
            .with(
                "csrutil status",
                CommandOutput::exit(1, "System Integrity Protection status: enabled.\n", ""),
            );
        let fv = filevault(&r);
        assert_eq!(fv.status, Status::Gray);
        assert!(fv.sentence.contains("did not answer in time"), "{fv:?}");
        assert_eq!(gatekeeper(&r).status, Status::Gray);
        // A failing exit code is gray even when the text looks fine.
        assert_eq!(sip(&r).status, Status::Gray);
        let fw = firewall(&r);
        assert_eq!(fw.status, Status::Gray);
        assert!(
            fw.sentence.contains("socketfilterfw was not found"),
            "{fw:?}"
        );
        assert_eq!(fw.action, None);
    }

    #[test]
    fn updates_do_not_run_unless_asked() {
        let r = ReplayRunner::new();
        let row = updates(&r, false);
        assert_eq!(row.status, Status::Gray);
        assert_eq!(row.sentence, UPDATES_NOT_CHECKED);
        assert!(r.calls().is_empty());
        assert!(!r.ran("softwareupdate"));
    }

    #[test]
    fn older_softwareupdate_format_parses() {
        let text = "Software Update Tool\n\nFinding available software\nSoftware Update found the following new or updated software:\n   * macOS Catalina 10.15.7 Supplemental Update-\n\tmacOS Catalina 10.15.7 Supplemental Update (10.15.7), 3195948K [recommended] [restart]\n";
        assert_eq!(
            update_names(text),
            Some(vec![
                "macOS Catalina 10.15.7 Supplemental Update".to_string()
            ])
        );
    }

    #[test]
    fn days_read_plainly() {
        assert_eq!(days_ago(0), "today");
        assert_eq!(days_ago(1), "1 day ago");
        assert_eq!(days_ago(61), "61 days ago");
    }
}
