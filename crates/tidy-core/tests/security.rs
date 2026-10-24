//! The Security checks on captured outputs and fixture folders (design section 5.4).
//!
//! Command outputs come from `tests/fixtures/security/*.json`: `commands-captured.json` was
//! captured on the development Mac (macOS 27.0.1, 2026-10-24); the off and disabled variants
//! and the `softwareupdate --list` samples are written from the tools' documented formats and
//! exit codes (`spctl --status` exits 1 with `assessments disabled`, read from its binary).
//! Nothing here runs a status command for real or `softwareupdate` at all: every run uses a
//! [`ReplayRunner`]. Launchd and Chrome folders are fixtures or temp copies, never the real
//! ones. The only programs whose signatures are read are `/bin/ls`, `/usr/bin/true`, `/bin/sh`
//! and a binary the test compiles into a temp folder.

use std::fs::{File, FileTimes};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use tidy_core::env::{
    CommandOutput, FixedClock, ReplayError, ReplayRunner, TEST_NOW, command_line,
};
use tidy_core::model::{
    CheckId, CheckResult, LaunchKind, Mark, Scope, SecurityReport, SettingsLink, Signer, Status,
    iso_week_id,
};
use tidy_core::security::codesign::{self, Signatures};
use tidy_core::security::launchd::{self, LaunchdRoots};
use tidy_core::security::{self, Sources, chrome, diff, system};
use tidy_core::{Clock, Env};

const DAY: u64 = 86_400;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/security")
}

fn replay(name: &str) -> ReplayRunner {
    ReplayRunner::from_file(&fixtures().join(name)).unwrap()
}

/// A named sample from a JSON file of `{name: CommandOutput}`.
fn sample(file: &str, name: &str) -> CommandOutput {
    let bytes = std::fs::read(fixtures().join(file)).unwrap();
    let map: std::collections::HashMap<String, CommandOutput> =
        serde_json::from_slice(&bytes).unwrap();
    map.get(name).cloned().unwrap()
}

/// The words the copy rules of design section 5.4 leave out.
fn assert_plain(row: &CheckResult) {
    let s = row.sentence.to_lowercase();
    for word in ["threat", "infected", "danger", "risk score"] {
        assert!(!s.contains(word), "{word:?} in {:?}", row.sentence);
    }
    assert!(
        !row.sentence.contains('\u{2014}'),
        "em dash in {:?}",
        row.sentence
    );
    assert!(!row.sentence.is_empty());
}

/// Copies a folder tree.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), &dest).unwrap();
        }
    }
}

fn set_mtime(path: &Path, secs: u64) {
    let t = UNIX_EPOCH + Duration::from_secs(secs);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(t).set_accessed(t))
        .unwrap();
}

fn xml_plist(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n{body}\n</dict>\n</plist>\n"
    )
}

fn agent_plist(label: &str, program: &Path, run_at_load: bool) -> String {
    xml_plist(&format!(
        "<key>Label</key><string>{label}</string>\n<key>ProgramArguments</key><array><string>{}</string></array>\n<key>RunAtLoad</key><{}/>",
        program.display(),
        run_at_load
    ))
}

/// Compiles a tiny C program into `dir` with the system compiler: linker-signed ad hoc on
/// Apple silicon, unsigned on Intel. Never notarized.
fn compile_unsigned(dir: &Path) -> PathBuf {
    let src = dir.join("hello.c");
    std::fs::write(&src, "int main(void) { return 0; }\n").unwrap();
    let out = dir.join("hello-helper");
    let status = std::process::Command::new("/usr/bin/cc")
        .arg("-o")
        .arg(&out)
        .arg(&src)
        .stdin(std::process::Stdio::null())
        .status()
        .expect("cc runs");
    assert!(status.success(), "cc failed");
    out
}

/// A temp sandbox: a fake home with copies of the launchd and Chrome fixtures, an XProtect
/// plist, and Tidy's folders.
struct Sandbox {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    home: PathBuf,
    roots: LaunchdRoots,
    chrome: PathBuf,
    xprotect: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("home");
        let roots = LaunchdRoots {
            user_agents: home.join("Library/LaunchAgents"),
            system_agents: base.join("system/Library/LaunchAgents"),
            system_daemons: base.join("system/Library/LaunchDaemons"),
        };
        copy_tree(&fixtures().join("launchd/user"), &roots.user_agents);
        copy_tree(&fixtures().join("launchd/agents"), &roots.system_agents);
        copy_tree(&fixtures().join("launchd/daemons"), &roots.system_daemons);
        let chrome = chrome::chrome_root(&home);
        copy_tree(&fixtures().join("chrome"), &chrome);
        let xprotect = base.join("xprotect/Info.plist");
        std::fs::create_dir_all(xprotect.parent().unwrap()).unwrap();
        std::fs::copy(fixtures().join("xprotect/library-Info.plist"), &xprotect).unwrap();
        // Updated 10 days before the test clock.
        set_mtime(&xprotect, TEST_NOW - 10 * DAY);
        Sandbox {
            _tmp: tmp,
            base,
            home,
            roots,
            chrome,
            xprotect,
        }
    }

    fn sources(&self) -> Sources {
        let mut s = Sources::for_home(&self.home);
        s.launchd = self.roots.clone();
        s.chrome = self.chrome.clone();
        s.xprotect = vec![self.xprotect.clone()];
        s.uid = 501;
        s
    }

    fn env(&self, clock: Arc<FixedClock>, runner: Arc<ReplayRunner>) -> Env {
        Env::for_tests(&self.home, &self.base.join("tidy"))
            .with_clock(clock)
            .with_runner(runner)
    }
}

fn row(report: &SecurityReport, id: CheckId) -> &CheckResult {
    report.check(id).unwrap()
}

fn no_progress() -> impl FnMut(usize, usize) {
    |_, _| {}
}

// ---------- System checks on captured outputs ----------

#[test]
fn captured_outputs_from_this_mac_parse() {
    let r = replay("commands-captured.json");
    let fv = system::filevault(&r);
    assert_eq!(fv.status, Status::Green, "{fv:?}");
    assert_eq!(fv.action, None);
    let fw = system::firewall(&r);
    assert_eq!(fw.status, Status::Amber, "{fw:?}");
    assert_eq!(fw.action, Some(SettingsLink::Network));
    assert!(fw.sentence.starts_with("Firewall is off."), "{fw:?}");
    let gk = system::gatekeeper(&r);
    assert_eq!(gk.status, Status::Green, "{gk:?}");
    let sip = system::sip(&r);
    assert_eq!(sip.status, Status::Green, "{sip:?}");
    for row in [&fv, &fw, &gk, &sip] {
        assert_plain(row);
    }
    // The checks call the tools by full path.
    assert_eq!(
        r.calls(),
        vec![
            command_line(system::FDESETUP, &["status"]),
            command_line(system::SOCKETFILTERFW, &["--getglobalstate"]),
            command_line(system::SPCTL, &["--status"]),
            command_line(system::CSRUTIL, &["status"]),
        ]
    );
}

#[test]
fn off_and_disabled_variants_are_amber_with_their_button() {
    let r = replay("commands-variants.json");
    let fv = system::filevault(&r);
    assert_eq!(fv.status, Status::Amber, "{fv:?}");
    assert_eq!(fv.action, Some(SettingsLink::PrivacySecurity));
    let fw = system::firewall(&r);
    assert_eq!(fw.status, Status::Green, "{fw:?}");
    assert_eq!(fw.action, None);
    let gk = system::gatekeeper(&r);
    assert_eq!(gk.status, Status::Amber, "{gk:?}");
    assert_eq!(gk.action, Some(SettingsLink::PrivacySecurity));
    let sip = system::sip(&r);
    assert_eq!(sip.status, Status::Amber, "{sip:?}");
    // SIP has no settings page: the sentence says how to turn it back on.
    assert_eq!(sip.action, None);
    assert!(sip.sentence.contains("Recovery"), "{sip:?}");
    for row in [&fv, &fw, &gk, &sip] {
        assert_plain(row);
    }
}

/// `spctl --status` prints `assessments disabled` and then calls `exit(1)` (read from the
/// spctl binary with `otool -tV`), so turned-off Gatekeeper must come out amber with that
/// exit code. Other exit-1 output from spctl, and exit 1 from the other tools, stay gray.
#[test]
fn gatekeeper_off_answers_with_exit_1_and_is_amber() {
    let off = system::parse_output_allowing(
        CheckId::Gatekeeper,
        system::SPCTL,
        &CommandOutput::exit(1, "assessments disabled\n", ""),
        system::SPCTL_OK_EXITS,
        system::parse_gatekeeper,
    );
    assert_eq!(off.status, Status::Amber, "{off:?}");
    assert_eq!(off.action, Some(SettingsLink::PrivacySecurity));
    assert!(off.sentence.starts_with("Gatekeeper is off."), "{off:?}");

    // The real runner path through the replay of the variant fixture (status 1).
    let gk = system::gatekeeper(&replay("commands-variants.json"));
    assert_eq!(gk.status, Status::Amber, "{gk:?}");

    // spctl's other exit-1 lines are not an answer Tidy knows: gray, never green.
    let other = system::parse_output_allowing(
        CheckId::Gatekeeper,
        system::SPCTL,
        &CommandOutput::exit(1, "notarized apps disabled\n", ""),
        system::SPCTL_OK_EXITS,
        system::parse_gatekeeper,
    );
    assert_eq!(other.status, Status::Gray, "{other:?}");
    // An exit code spctl does not use for an answer is the error row.
    let crashed = system::parse_output_allowing(
        CheckId::Gatekeeper,
        system::SPCTL,
        &CommandOutput::exit(2, "assessments disabled\n", ""),
        system::SPCTL_OK_EXITS,
        system::parse_gatekeeper,
    );
    assert_eq!(crashed.status, Status::Gray, "{crashed:?}");
    assert!(
        crashed.sentence.contains("ended with an error"),
        "{crashed:?}"
    );
    // fdesetup answers only with exit 0.
    let fv = system::parse_output(
        CheckId::FileVault,
        system::FDESETUP,
        &CommandOutput::exit(1, "FileVault is Off.\n", ""),
        system::parse_filevault,
    );
    assert_eq!(fv.status, Status::Gray, "{fv:?}");
}

#[test]
fn progress_and_partial_states_parse() {
    let parse = |name: &str, id: CheckId, program: &str, f: fn(&str) -> Option<CheckResult>| {
        system::parse_output(id, program, &sample("commands-more.json", name), f)
    };
    let enc = parse(
        "fdesetup-encrypting",
        CheckId::FileVault,
        system::FDESETUP,
        system::parse_filevault,
    );
    assert_eq!(enc.status, Status::Green, "{enc:?}");
    let dec = parse(
        "fdesetup-decrypting",
        CheckId::FileVault,
        system::FDESETUP,
        system::parse_filevault,
    );
    assert_eq!(dec.status, Status::Amber, "{dec:?}");
    let deferred = parse(
        "fdesetup-deferred",
        CheckId::FileVault,
        system::FDESETUP,
        system::parse_filevault,
    );
    assert_eq!(deferred.status, Status::Amber, "{deferred:?}");
    let all = parse(
        "socketfilterfw-block-all",
        CheckId::Firewall,
        system::SOCKETFILTERFW,
        system::parse_firewall,
    );
    assert_eq!(all.status, Status::Green, "{all:?}");
    let custom = parse(
        "csrutil-custom",
        CheckId::Sip,
        system::CSRUTIL,
        system::parse_sip,
    );
    assert_eq!(custom.status, Status::Amber, "{custom:?}");
    assert!(custom.sentence.contains("partly off"), "{custom:?}");
    for row in [&enc, &dec, &deferred, &all, &custom] {
        assert_plain(row);
    }
}

#[test]
fn failures_timeouts_and_unknown_output_are_gray_never_green() {
    let cases: Vec<(&str, ReplayRunner)> = vec![
        (
            "timed out",
            ReplayRunner::new().with_error("fdesetup status", ReplayError::TimedOut),
        ),
        ("not found", ReplayRunner::new()),
        (
            "refused",
            ReplayRunner::new().with_error("fdesetup status", ReplayError::Refused),
        ),
        (
            "exit 1",
            ReplayRunner::new().with(
                "fdesetup status",
                CommandOutput::exit(1, "FileVault is On.\n", ""),
            ),
        ),
        (
            "killed",
            ReplayRunner::new().with(
                "fdesetup status",
                CommandOutput {
                    status: None,
                    stdout: "FileVault is On.\n".into(),
                    stderr: String::new(),
                },
            ),
        ),
        (
            "garbage",
            ReplayRunner::new().with(
                "fdesetup status",
                CommandOutput::ok("FileVault est activ\u{e9}.\n"),
            ),
        ),
        (
            "empty",
            ReplayRunner::new().with("fdesetup status", CommandOutput::ok("")),
        ),
    ];
    for (what, r) in cases {
        let row = system::filevault(&r);
        assert_eq!(row.status, Status::Gray, "{what}: {row:?}");
        assert_eq!(row.action, None, "{what}: a gray row has no button");
        assert!(
            row.sentence.starts_with("Tidy couldn't check this"),
            "{what}: {row:?}"
        );
        assert_plain(&row);
    }
    // The same rule for every status check.
    let empty = ReplayRunner::new()
        .with(
            "socketfilterfw --getglobalstate",
            CommandOutput::ok("Firewall is maybe.\n"),
        )
        .with("spctl --status", CommandOutput::ok("assessments unknown\n"))
        .with(
            "csrutil status",
            CommandOutput::ok("System Integrity Protection status: who knows.\n"),
        );
    assert_eq!(system::firewall(&empty).status, Status::Gray);
    assert_eq!(system::gatekeeper(&empty).status, Status::Gray);
    assert_eq!(system::sip(&empty).status, Status::Gray);
}

// ---------- XProtect ----------

#[test]
fn xprotect_is_amber_after_60_days() {
    let tmp = tempfile::tempdir().unwrap();
    let plist = tmp.path().join("Info.plist");
    std::fs::copy(fixtures().join("xprotect/library-Info.plist"), &plist).unwrap();
    let updated = TEST_NOW - 100 * DAY;
    set_mtime(&plist, updated);
    let clock = FixedClock::new(updated + 59 * DAY);
    let fresh = system::xprotect(std::slice::from_ref(&plist), clock.unix_now());
    assert_eq!(fresh.status, Status::Green, "{fresh:?}");
    assert_eq!(fresh.detail.as_deref(), Some("5367"));
    assert!(fresh.sentence.contains("59 days ago"), "{fresh:?}");
    clock.set(updated + 60 * DAY);
    assert_eq!(
        system::xprotect(std::slice::from_ref(&plist), clock.unix_now()).status,
        Status::Green
    );
    clock.set(updated + 61 * DAY);
    let stale = system::xprotect(std::slice::from_ref(&plist), clock.unix_now());
    assert_eq!(stale.status, Status::Amber, "{stale:?}");
    assert_eq!(stale.action, Some(SettingsLink::SoftwareUpdate));
    assert!(stale.sentence.contains("61 days ago"), "{stale:?}");
    assert_plain(&fresh);
    assert_plain(&stale);
}

#[test]
fn xprotect_takes_the_newest_plist_and_its_installation_time() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("library-Info.plist");
    let newer = tmp.path().join("protected-Info.plist");
    std::fs::copy(fixtures().join("xprotect/library-Info.plist"), &old).unwrap();
    std::fs::copy(fixtures().join("xprotect/protected-Info.plist"), &newer).unwrap();
    // The library copy is a year old; the protected copy has an old mtime too but says it was
    // installed on 2026-10-14 12:14:03 UTC.
    let installed = 1_791_980_043;
    set_mtime(&old, installed - 365 * DAY);
    set_mtime(&newer, installed - 365 * DAY);
    let info = system::read_xprotect(&newer).unwrap();
    assert_eq!(info.updated, installed);
    assert_eq!(info.version.as_deref(), Some("5367"));
    let row = system::xprotect(&[old.clone(), newer.clone()], installed + 5 * DAY);
    assert_eq!(row.status, Status::Green, "{row:?}");
    assert!(row.sentence.contains("5 days ago"), "{row:?}");
    // Only the old copy: amber.
    let row = system::xprotect(&[old], installed + 5 * DAY);
    assert_eq!(row.status, Status::Amber, "{row:?}");
    // Nothing readable: gray.
    let row = system::xprotect(&[tmp.path().join("missing.plist")], installed);
    assert_eq!(row.status, Status::Gray, "{row:?}");
}

// ---------- macOS updates ----------

#[test]
fn softwareupdate_samples_parse() {
    let none = system::parse_output(
        CheckId::MacosUpdates,
        system::SOFTWAREUPDATE,
        &sample("softwareupdate.json", "none"),
        system::parse_updates,
    );
    assert_eq!(none.status, Status::Green, "{none:?}");
    let one = system::parse_output(
        CheckId::MacosUpdates,
        system::SOFTWAREUPDATE,
        &sample("softwareupdate.json", "one"),
        system::parse_updates,
    );
    assert_eq!(one.status, Status::Amber, "{one:?}");
    assert_eq!(one.action, Some(SettingsLink::SoftwareUpdate));
    assert_eq!(one.detail.as_deref(), Some("macOS 27.0.2"));
    assert!(
        one.sentence
            .starts_with("1 update is available: macOS 27.0.2."),
        "{one:?}"
    );
    let two = system::update_names(&sample("softwareupdate.json", "two").stdout).unwrap();
    assert_eq!(two, vec!["macOS 27.0.2", "Command Line Tools for Xcode"]);
    let offline = system::parse_output(
        CheckId::MacosUpdates,
        system::SOFTWAREUPDATE,
        &sample("softwareupdate.json", "offline"),
        system::parse_updates,
    );
    assert_eq!(offline.status, Status::Gray, "{offline:?}");
    for row in [&none, &one, &offline] {
        assert_plain(row);
    }
}

#[test]
fn softwareupdate_runs_only_when_asked_with_a_60_second_timeout() {
    assert_eq!(system::UPDATES_TIMEOUT, Duration::from_secs(60));
    let r = ReplayRunner::new().with(
        "softwareupdate --list",
        sample("softwareupdate.json", "one"),
    );
    let skipped = system::updates(&r, false);
    assert_eq!(skipped.status, Status::Gray);
    assert!(!r.ran("softwareupdate"));
    let row = system::updates(&r, true);
    assert_eq!(row.status, Status::Amber);
    assert_eq!(
        r.calls(),
        vec!["/usr/sbin/softwareupdate --list".to_string()]
    );
    let timed_out = ReplayRunner::new().with_error("softwareupdate --list", ReplayError::TimedOut);
    assert_eq!(system::updates(&timed_out, true).status, Status::Gray);
}

// ---------- Launch agents and daemons ----------

#[test]
fn launchd_fixture_folders_parse_xml_and_binary_plists() {
    let roots = LaunchdRoots {
        user_agents: fixtures().join("launchd/user"),
        system_agents: fixtures().join("launchd/agents"),
        system_daemons: fixtures().join("launchd/daemons"),
    };
    let read = launchd::read_roots(&roots);
    assert!(read.errors.is_empty());
    let labels: Vec<&str> = read.jobs.iter().map(|j| j.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "com.example.binary",
            "com.example.sync",
            "org.example.updater",
            "com.example.disabled",
            "com.example.helperd",
        ]
    );
    let job = |l: &str| read.jobs.iter().find(|j| j.label == l).unwrap();

    let binary = job("com.example.binary");
    assert_eq!(binary.program.as_deref(), Some(Path::new("/usr/bin/true")));
    assert!(binary.keep_alive && !binary.runs_at_load);
    assert_eq!(
        (binary.scope, binary.kind),
        (Scope::User, LaunchKind::Agent)
    );

    let sync = job("com.example.sync");
    assert_eq!(sync.program.as_deref(), Some(Path::new("/bin/ls")));
    assert!(sync.runs_at_load && !sync.keep_alive);

    let updater = job("org.example.updater");
    assert_eq!(
        (updater.scope, updater.kind),
        (Scope::System, LaunchKind::Agent)
    );
    assert!(updater.keep_alive);

    let helperd = job("com.example.helperd");
    assert_eq!(
        helperd.program, None,
        "BundleProgram is not a path Tidy can check"
    );
    assert!(helperd.runs_at_load);
    assert_eq!(
        (helperd.scope, helperd.kind),
        (Scope::System, LaunchKind::Daemon)
    );

    assert!(job("com.example.disabled").disabled);

    // The broken plist and the text file are skipped, not fatal.
    let user = launchd::read_folder(&roots.user_agents, Scope::User, LaunchKind::Agent);
    assert_eq!(user.skipped.len(), 1);
    assert!(user.skipped[0].ends_with("com.example.broken.plist"));
}

#[test]
fn launchctl_print_lists_loaded_and_disabled_services() {
    let text = std::fs::read_to_string(fixtures().join("launchctl-print-gui.txt")).unwrap();
    let domain = launchd::parse_print(&text);
    assert_eq!(domain.services.len(), 10);
    let labels = domain.labels();
    assert!(labels.contains("com.example.sync"));
    assert!(labels.contains("com.example.registered-helper"));
    // Nested blocks (unmanaged processes, endpoints) are not services.
    assert!(!labels.contains("com.apple.coredrag"));
    assert!(!labels.iter().any(|l| l.contains("0x2d203")));
    let binary = domain
        .services
        .iter()
        .find(|s| s.label == "com.example.binary")
        .unwrap();
    assert_eq!(binary.pid, Some(4242));
    let sync = domain
        .services
        .iter()
        .find(|s| s.label == "com.example.sync")
        .unwrap();
    assert_eq!(sync.pid, None);
    assert_eq!(domain.disabled.get("com.example.turned-off"), Some(&true));
    assert_eq!(
        domain.disabled.get("com.example.registered-helper"),
        Some(&false)
    );
}

#[test]
fn apple_and_unsigned_programs_are_told_apart() {
    let ls = codesign::check(Path::new("/bin/ls"));
    assert_eq!(ls.signer, Signer::Apple, "{ls:?}");
    assert_eq!(ls.notarized, None);

    let tmp = tempfile::tempdir().unwrap();
    let bin = compile_unsigned(tmp.path());
    let sig = codesign::check(&bin);
    assert_eq!(sig.signer, Signer::Unsigned, "{sig:?}");
    assert_eq!(sig.notarized, Some(false));
}

#[test]
fn login_items_carry_label_program_run_at_load_and_signer() {
    let sb = Sandbox::new();
    let bin_dir = sb.base.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bin = compile_unsigned(&bin_dir);
    std::fs::write(
        sb.roots.user_agents.join("com.example.unsigned.plist"),
        agent_plist("com.example.unsigned", &bin, true),
    )
    .unwrap();
    let runner = Arc::new(replay("commands-captured.json"));
    runner.set(
        "launchctl print gui/501",
        tidy_core::env::Replay::Output(CommandOutput::ok(
            std::fs::read_to_string(fixtures().join("launchctl-print-gui.txt")).unwrap(),
        )),
    );
    let env = sb.env(Arc::new(FixedClock::new(TEST_NOW)), runner.clone());
    let mut steps = Vec::new();
    let report = security::run(&env, &sb.sources(), false, None, &mut |d, t| {
        steps.push((d, t))
    });

    let item = |l: &str| report.login_items.iter().find(|i| i.label == l).unwrap();
    let sync = item("com.example.sync");
    assert_eq!(sync.program.as_deref(), Some(Path::new("/bin/ls")));
    assert!(sync.runs_at_load);
    assert_eq!(sync.signer, Signer::Apple);
    assert_eq!(sync.notarized, None);
    assert!(sync.can_trash());
    assert!(!sync.needs_a_look());

    let unsigned = item("com.example.unsigned");
    assert_eq!(unsigned.program.as_deref(), Some(bin.as_path()));
    assert!(unsigned.runs_at_load);
    assert_eq!(unsigned.signer, Signer::Unsigned);
    assert_eq!(unsigned.notarized, Some(false));
    assert!(unsigned.needs_a_look());

    // `sh` resolves on the default PATH, so the disabled daemon is checked as /bin/sh.
    assert_eq!(item("com.example.disabled").signer, Signer::Apple);
    // A missing program and a bundled one cannot be checked.
    assert_eq!(item("org.example.updater").signer, Signer::Unknown);
    assert_eq!(item("com.example.helperd").signer, Signer::Unknown);
    assert!(
        !item("org.example.updater").can_trash(),
        "system agents never go to the Trash"
    );

    // A helper only launchctl lists joins the list; macOS services, running apps and a
    // disabled helper do not.
    let helper = item("com.example.registered-helper");
    assert_eq!(helper.plist, None);
    assert_eq!(helper.program, None);
    assert!(!helper.can_trash());
    for l in [
        "com.example.turned-off",
        "com.apple.assistantd",
        "com.openssh.ssh-agent",
    ] {
        assert!(!report.login_items.iter().any(|i| i.label == l), "{l}");
    }
    assert!(
        !report
            .login_items
            .iter()
            .any(|i| i.label.starts_with("application."))
    );

    // First run: nothing is new, the unsigned helper makes the row amber.
    assert!(report.first_run);
    let row = row(&report, CheckId::LoginItems);
    assert_eq!(row.status, Status::Amber, "{row:?}");
    assert_eq!(row.action, Some(SettingsLink::LoginItems));
    assert!(row.sentence.contains("1 not signed"), "{row:?}");
    assert!(!row.sentence.contains("new since"), "{row:?}");
    assert_plain(row);

    // Progress: one step per check and one per distinct program, ending at the total.
    let total = steps.last().unwrap().1;
    // /bin/ls, /usr/bin/true, /bin/sh, the updater, the compiled binary.
    assert_eq!(total, CheckId::ALL.len() + 5);
    assert_eq!(steps.first(), Some(&(0, total)));
    assert_eq!(steps.last(), Some(&(total, total)));
    assert!(steps.windows(2).all(|w| w[1].0 == w[0].0 + 1));
}

// ---------- Chrome ----------

#[test]
fn chrome_secure_preferences_list_the_extensions_people_installed() {
    let read = chrome::read_all(&fixtures().join("chrome"));
    assert!(read.installed);
    assert_eq!(read.profiles, 2, "System Profile is skipped");
    assert!(read.errors.is_empty());
    let got: Vec<(&str, &str, bool)> = read
        .extensions
        .iter()
        .map(|e| (e.profile.as_str(), e.name.as_str(), e.enabled))
        .collect();
    assert_eq!(
        got,
        vec![
            ("Default", "Example Notes", true),
            ("Default", "Old Helper", false),
            ("Default", "uBlock Origin Lite", true),
            ("Profile 1", "Example Writer", false),
            ("Profile 1", "uBlock Origin Lite", true),
        ]
    );
    let notes = &read.extensions[0];
    assert_eq!(notes.id, "abcdefghijklmnopabcdefghijklmnop");
    assert_eq!(notes.version.as_deref(), Some("1.4.0"));
    // Component, default-installed and manifest-less entries are left out.
    for id in [
        "mhjfbmdgcfjbbpaeojofohoefgiehjai",
        "nmmhkkegccagdldgiimedpiccmgmieda",
        "aapocclcgogkmnckokdopfmhonfmgoek",
        "ghbmnnjooekpmoecnnnilnnbdlolhkhi",
    ] {
        assert!(!read.extensions.iter().any(|e| e.id == id), "{id}");
    }
}

// ---------- Week over week ----------

#[test]
fn week_over_week_marks_new_and_changed_and_the_first_run_says_so() {
    let sb = Sandbox::new();
    let clock = Arc::new(FixedClock::new(TEST_NOW));
    let runner = Arc::new(replay("commands-captured.json"));
    let env = sb.env(clock.clone(), runner.clone());
    let sources = sb.sources();

    // Week 1: the first check.
    let first = security::run_and_save_with(&env, &sources, false, &mut no_progress());
    assert!(first.first_run);
    assert_eq!(first.compared_to, None);
    assert_eq!(first.week, iso_week_id(TEST_NOW));
    assert!(first.checks.iter().all(|c| c.mark.is_none()));
    assert!(first.login_items.iter().all(|i| i.mark.is_none()));
    assert!(first.extensions.iter().all(|e| e.mark.is_none()));
    assert_eq!(
        first.checks.iter().map(|c| c.id).collect::<Vec<_>>(),
        CheckId::ALL.to_vec()
    );
    assert_eq!(row(&first, CheckId::ChromeExtensions).status, Status::Green);
    assert_eq!(row(&first, CheckId::LoginItems).status, Status::Green);
    assert_eq!(row(&first, CheckId::XProtect).status, Status::Green);
    assert_eq!(row(&first, CheckId::MacosUpdates).status, Status::Gray);
    assert!(env.paths.security_file(&first.week).is_file());
    assert_eq!(security::latest_report(&env.paths).as_ref(), Some(&first));

    // A week later: a new agent, a changed program, a new extension, and FileVault and the
    // firewall flipped.
    clock.advance(7 * DAY);
    set_mtime(&sb.xprotect, clock.unix_now() - DAY);
    std::fs::write(
        sb.roots.user_agents.join("com.example.added.plist"),
        agent_plist("com.example.added", Path::new("/bin/ls"), true),
    )
    .unwrap();
    std::fs::write(
        sb.roots.user_agents.join("com.example.sync.plist"),
        agent_plist("com.example.sync", Path::new("/bin/cat"), true),
    )
    .unwrap();
    let prefs = sb.chrome.join("Profile 1/Secure Preferences");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&prefs).unwrap()).unwrap();
    v["extensions"]["settings"]["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"] = serde_json::json!({
        "location": 1,
        "manifest": {"name": "Example Tabs", "version": "3.0"},
        "disable_reasons": []
    });
    std::fs::write(&prefs, serde_json::to_vec(&v).unwrap()).unwrap();
    let env = sb.env(clock.clone(), Arc::new(replay("commands-variants.json")));

    let second = security::run_and_save_with(&env, &sources, false, &mut no_progress());
    assert!(!second.first_run);
    assert_eq!(second.compared_to.as_deref(), Some(first.week.as_str()));
    assert_ne!(second.week, first.week);

    // New items first.
    assert_eq!(second.login_items[0].label, "com.example.added");
    assert_eq!(second.login_items[0].mark, Some(Mark::New));
    let sync = second
        .login_items
        .iter()
        .find(|i| i.label == "com.example.sync")
        .unwrap();
    assert_eq!(sync.mark, Some(Mark::Changed));
    let marked: Vec<&str> = second
        .login_items
        .iter()
        .filter(|i| i.mark.is_some())
        .map(|i| i.label.as_str())
        .collect();
    assert_eq!(marked, vec!["com.example.added", "com.example.sync"]);

    let new_ext: Vec<&str> = second
        .extensions
        .iter()
        .filter(|e| e.mark == Some(Mark::New))
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(new_ext, vec!["Example Tabs"]);

    let li = row(&second, CheckId::LoginItems);
    assert_eq!(li.status, Status::Amber, "{li:?}");
    assert!(li.sentence.contains("1 new since the last check"), "{li:?}");
    assert_eq!(li.mark, Some(Mark::Changed), "green last week, amber now");
    let ce = row(&second, CheckId::ChromeExtensions);
    assert_eq!(ce.status, Status::Amber, "{ce:?}");
    assert!(
        ce.sentence
            .starts_with("1 extension was added since the last check."),
        "{ce:?}"
    );
    assert_eq!(
        ce.action, None,
        "Chrome's page cannot be opened from outside Chrome"
    );

    assert_eq!(row(&second, CheckId::FileVault).mark, Some(Mark::Changed));
    assert_eq!(row(&second, CheckId::Firewall).mark, Some(Mark::Changed));
    assert_eq!(row(&second, CheckId::Gatekeeper).mark, Some(Mark::Changed));
    assert_eq!(row(&second, CheckId::Sip).mark, Some(Mark::Changed));
    assert_eq!(row(&second, CheckId::XProtect).mark, None);
    assert_eq!(row(&second, CheckId::MacosUpdates).mark, None);
    for c in &second.checks {
        assert_plain(c);
    }

    // A second run in the same week still compares with the week before, and replaces the
    // week's file.
    clock.advance(DAY);
    let again = security::run_and_save_with(&env, &sources, false, &mut no_progress());
    assert_eq!(again.week, second.week);
    assert_eq!(again.compared_to.as_deref(), Some(first.week.as_str()));
    assert_eq!(again.login_items[0].mark, Some(Mark::New));
    assert_eq!(
        security::saved_weeks(&env.paths),
        vec![first.week.clone(), second.week.clone()]
    );
    assert_eq!(
        security::latest_report(&env.paths).map(|r| r.at),
        Some(again.at)
    );
    assert_eq!(
        security::previous_report(&env.paths, &second.week).map(|r| r.week),
        Some(first.week.clone())
    );
}

#[test]
fn updates_not_checked_is_not_a_change() {
    let mut prev = SecurityReport::empty(TEST_NOW);
    prev.updates_checked = true;
    prev.checks = vec![CheckResult::new(
        CheckId::MacosUpdates,
        Status::Amber,
        "1 update is available: x.",
    )];
    let mut now = SecurityReport::empty(TEST_NOW + 7 * DAY);
    now.checks = vec![CheckResult::new(
        CheckId::MacosUpdates,
        Status::Gray,
        system::UPDATES_NOT_CHECKED,
    )];
    diff::apply(&mut now, Some(&prev));
    assert_eq!(now.checks[0].mark, None);
    now.updates_checked = true;
    now.checks[0] = CheckResult::new(CheckId::MacosUpdates, Status::Green, "macOS is up to date.");
    diff::apply(&mut now, Some(&prev));
    assert_eq!(now.checks[0].mark, Some(Mark::Changed));
}

#[test]
fn a_run_without_include_updates_never_runs_softwareupdate() {
    let sb = Sandbox::new();
    let runner = Arc::new(replay("commands-captured.json"));
    let env = sb.env(Arc::new(FixedClock::new(TEST_NOW)), runner.clone());
    let report = security::run(&env, &sb.sources(), false, None, &mut no_progress());
    assert!(!report.updates_checked);
    assert!(!runner.ran("softwareupdate"));
    assert_eq!(
        row(&report, CheckId::MacosUpdates).sentence,
        system::UPDATES_NOT_CHECKED
    );

    let runner = Arc::new(replay("commands-captured.json"));
    runner.set(
        "softwareupdate --list",
        tidy_core::env::Replay::Output(sample("softwareupdate.json", "none")),
    );
    let env = sb.env(Arc::new(FixedClock::new(TEST_NOW)), runner.clone());
    let report = security::run(&env, &sb.sources(), true, None, &mut no_progress());
    assert!(report.updates_checked);
    assert!(runner.ran("softwareupdate"));
    assert_eq!(row(&report, CheckId::MacosUpdates).status, Status::Green);
}

#[test]
fn a_fake_signature_checker_can_stand_in() {
    struct AllTeam;
    impl Signatures for AllTeam {
        fn check(&self, _program: &Path) -> codesign::Signature {
            codesign::Signature {
                signer: Signer::Team("Example Corp".into()),
                notarized: Some(false),
            }
        }
    }
    let sb = Sandbox::new();
    let env = sb.env(
        Arc::new(FixedClock::new(TEST_NOW)),
        Arc::new(replay("commands-captured.json")),
    );
    let sources = sb.sources().with_signatures(Arc::new(AllTeam));
    let report = security::run(&env, &sources, false, None, &mut no_progress());
    let li = row(&report, CheckId::LoginItems);
    assert_eq!(li.status, Status::Amber, "{li:?}");
    assert!(li.sentence.contains("not notarized by Apple"), "{li:?}");
}

#[test]
fn nothing_readable_is_gray_with_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let env = Env::for_tests(&home, &tmp.path().join("tidy"));
    let mut sources = Sources::for_home(&home);
    // Files where folders should be: every folder read fails.
    let file = tmp.path().join("not-a-folder");
    std::fs::write(&file, "x").unwrap();
    sources.launchd = LaunchdRoots {
        user_agents: file.clone(),
        system_agents: file.clone(),
        system_daemons: file.clone(),
    };
    sources.xprotect = vec![tmp.path().join("missing.plist")];
    let report = security::run(&env, &sources, false, None, &mut no_progress());
    // The empty runner answers nothing: every command check is gray, none green.
    for c in &report.checks {
        assert_eq!(c.status, Status::Gray, "{c:?}");
        assert_eq!(c.action, None);
        assert_plain(c);
    }
    assert_eq!(report.amber(), 0);
    assert_eq!(report.gray(), CheckId::ALL.len());
}

#[test]
fn security_sources_use_no_scare_words() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/security");
    for e in std::fs::read_dir(&dir).unwrap() {
        let path = e.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap().to_lowercase();
        for word in ["threat", "infected", "danger", "risk score"] {
            assert!(!text.contains(word), "{word:?} in {}", path.display());
        }
        assert!(!text.contains('\u{2014}'), "em dash in {}", path.display());
    }
}

/// A read-only run against this Mac's real status commands, launchd folders and signatures
/// (with a fake home, so no user folder is read). Never runs `softwareupdate`. Run by hand:
/// `cargo test -p tidy-core --test security live -- --ignored`.
#[test]
#[ignore = "reads this Mac's system state; run by hand"]
fn live_run_on_this_mac() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let env = Env::for_tests(&home, &tmp.path().join("tidy"))
        .with_clock(Arc::new(tidy_core::env::SystemClock))
        .with_runner(Arc::new(tidy_core::env::SystemRunner));
    let start = std::time::Instant::now();
    let report = security::run(
        &env,
        &Sources::for_env(&env),
        false,
        None,
        &mut no_progress(),
    );
    let took = start.elapsed();
    eprintln!("took {took:?}");
    for c in &report.checks {
        eprintln!("{:?} {:?}: {}", c.id, c.status, c.sentence);
    }
    eprintln!("{} login items", report.login_items.len());
    for i in &report.login_items {
        eprintln!(
            "  {} {:?} {:?} {:?}",
            i.label, i.scope, i.signer, i.notarized
        );
    }
    assert!(took < Duration::from_secs(20));
    assert_eq!(report.checks.len(), CheckId::ALL.len());
    assert_eq!(
        row(&report, CheckId::MacosUpdates).sentence,
        system::UPDATES_NOT_CHECKED
    );
}
