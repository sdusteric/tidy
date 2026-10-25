//! Item details (design 17.1): why a Cleanup item is offered, what brings it back, which checks
//! guard its move, and which rule and path pattern matched it.
//!
//! Everything here comes from the rule table in [`crate::rules`]: the item's own reason, the
//! rule's `rebuild` sentence and command, and its targets, age, owner and keep policies. Nothing
//! is written per item, so the details can never say more than the rule that offered it.

use serde::{Deserialize, Serialize};

use crate::model::{Class, Item};
use crate::rules::{self, Age, Keep, KeepBy, Markers, Owner, Target};

/// The details of one Cleanup item, ready to draw.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Explanation {
    /// One or two sentences: the class verdict and the item's reason.
    pub verdict: String,
    /// What brings the target back, or what is lost. Empty for an unknown rule.
    pub rebuild: String,
    /// The checks that guard the move, one short line each.
    pub checks: Vec<String>,
    /// The rule id without a folded row's `.small` suffix.
    pub rule_id: String,
    /// The path patterns of the rule, relative to the home folder.
    pub patterns: Vec<String>,
    /// The item's path relative to the home folder.
    pub rel_path: String,
    /// The command a Report only row suggests. Tidy never runs it.
    pub command: Option<String>,
}

/// The first words of the verdict for each class.
pub fn verdict_lead(class: Class) -> &'static str {
    match class {
        Class::Safe => "Safe to clean.",
        Class::Review => "Worth a look before you clean it.",
        Class::ReportOnly => "Tidy won't remove this.",
    }
}

/// The last check of every item Tidy may move.
pub const RECHECK: &str = "Checked again right before the move";
/// The line about the Trash on every item Tidy may move.
pub const TRASH_LINE: &str = "Goes to the Trash, so you can put it back";
/// The only check line of a Report only item.
pub const NEVER_MOVED: &str = "Tidy never moves it";

/// The details of `item`.
pub fn explain(item: &Item) -> Explanation {
    let rule_id = item
        .rule_id()
        .strip_suffix(rules::SMALL_SUFFIX)
        .unwrap_or(item.rule_id())
        .to_string();
    let mut e = Explanation {
        verdict: verdict(item.class, &item.reason),
        rule_id: rule_id.clone(),
        rel_path: item.rel_path().to_string(),
        ..Explanation::default()
    };
    let mut checks = Vec::new();
    if let Some(r) = rules::rule(&rule_id) {
        e.rebuild = r.rebuild.to_string();
        e.command = r.command.map(str::to_string);
        e.patterns = r.targets.iter().map(target_pattern).collect();
        if item.class != Class::ReportOnly {
            checks.extend(owner_check(r.owner));
            checks.push(age_check(r.age));
            checks.extend(keep_check(r.keep));
            if r.in_use {
                checks.push("Never a version that a running program uses".to_string());
            }
        }
    } else if let Some(r) = rules::project_rule(&rule_id) {
        e.rebuild = r.rebuild.to_string();
        e.patterns = r.names.iter().map(|n| format!("<project>/{n}")).collect();
        if item.class != Class::ReportOnly {
            checks.push(marker_check(r.markers));
            checks.extend(owner_check(r.owner));
            checks.push(age_check(r.age));
        }
    }
    if item.class == Class::ReportOnly {
        checks.push(NEVER_MOVED.to_string());
    } else {
        checks.push(RECHECK.to_string());
        checks.push(TRASH_LINE.to_string());
    }
    e.checks = checks;
    e
}

/// "Safe to clean. Cache, rebuilt by the app." The reason gets a full stop when it has none.
pub fn verdict(class: Class, reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        return verdict_lead(class).to_string();
    }
    let stop = if reason.ends_with(['.', '?', '!']) {
        ""
    } else {
        "."
    };
    format!("{} {reason}{stop}", verdict_lead(class))
}

/// A rule target as a path pattern: alternatives in braces, the owner's folder as `<app>`.
pub fn target_pattern(t: &Target) -> String {
    match t {
        Target::Glob(p) => braces(p),
        Target::Files(p) => format!("{}/<file>", braces(p)),
        Target::CacheFolders(p) => format!("{p}/<app>"),
        Target::Chromium(names) => format!(
            "Library/Application Support/<app>/{}",
            if names.len() == 1 {
                names[0].to_string()
            } else {
                format!("{{{}}}", names.join(", "))
            }
        ),
    }
}

/// `a/b|c/d` as `a/{b, c}/d`.
fn braces(pattern: &str) -> String {
    pattern
        .split('/')
        .map(|seg| {
            if seg.contains('|') {
                format!("{{{}}}", seg.split('|').collect::<Vec<_>>().join(", "))
            } else {
                seg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn owner_check(owner: Owner) -> Option<String> {
    match owner {
        Owner::None => None,
        Owner::Fixed(info) => Some(format!("Skipped while {} is running", info.name)),
        Owner::Folder => Some("Skipped while its app is running".to_string()),
    }
}

fn age_check(age: Age) -> String {
    match age {
        Age::None => "Nothing inside changed in the last 24 hours".to_string(),
        Age::Target(d) => format!("Nothing inside changed in the last {d} days"),
        Age::Project(d) => format!("The project hasn't changed in {d} days"),
    }
}

fn keep_check(keep: Keep) -> Option<String> {
    match keep {
        Keep::None => None,
        Keep::Newest(KeepBy::Folder) => Some("The newest one is always kept".to_string()),
        Keep::Newest(KeepBy::BeforeLastDash) => {
            Some("The newest build of each browser is always kept".to_string())
        }
        Keep::Newest(KeepBy::DeviceModel) => {
            Some("The newest one for each device is always kept".to_string())
        }
    }
}

fn or_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

fn marker_check(m: Markers) -> String {
    match m {
        Markers::ParentHasAny(names) => format!("{} sits beside it", or_list(names)),
        Markers::CargoTarget => "Cargo.toml sits beside it, or Cargo's tag is inside".to_string(),
        Markers::Pods => "Podfile sits beside it (Safe only with Podfile.lock)".to_string(),
        Markers::Venv => {
            "It holds pyvenv.cfg, and pyproject.toml, requirements.txt or uv.lock sits beside it"
                .to_string()
        }
        Markers::PythonSource => "A .py file sits beside it".to_string(),
        Markers::XcodeProject => "An Xcode project sits beside it".to_string(),
        Markers::GitIgnored(names) => {
            format!("{} sits beside it and .gitignore names it", or_list(names))
        }
        Markers::WithLockfile(files, locks) => {
            format!("{} sits beside it with {}", or_list(files), or_list(locks))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Group;
    use std::path::PathBuf;

    fn item(id: &str, class: Class, reason: &str) -> Item {
        Item {
            id: id.to_string(),
            group: Group::Caches,
            class,
            ticked: class == Class::Safe,
            blocked: None,
            name: "x".into(),
            reason: reason.into(),
            path: PathBuf::from("/h").join(id.split_once(':').unwrap().1),
            bytes_alloc: 1,
            bytes_logical: 1,
            newest_mtime: None,
            members: vec![],
        }
    }

    #[test]
    fn a_safe_app_cache_says_why_and_what_rebuilds_it() {
        let e = explain(&item(
            "cache.app:Library/Caches/com.spotify.client",
            Class::Safe,
            "Cache, rebuilt by the app",
        ));
        assert_eq!(e.verdict, "Safe to clean. Cache, rebuilt by the app.");
        assert_eq!(
            e.rebuild,
            "The app makes a new cache the next time it needs one."
        );
        assert_eq!(e.rule_id, "cache.app");
        assert_eq!(
            e.patterns,
            [
                "Library/Caches/<app>",
                "Library/Application Support/Caches/<app>"
            ]
        );
        assert_eq!(e.rel_path, "Library/Caches/com.spotify.client");
        assert_eq!(
            e.checks,
            [
                "Skipped while its app is running",
                "Nothing inside changed in the last 24 hours",
                RECHECK,
                TRASH_LINE
            ]
        );
        assert_eq!(e.command, None);
    }

    #[test]
    fn fixed_owners_ages_and_keep_rules_become_checks() {
        let e = explain(&item(
            "dev.deriveddata:Library/Developer/Xcode/DerivedData/App-abc",
            Class::Safe,
            "Build data, Xcode makes it again",
        ));
        assert!(
            e.checks
                .contains(&"Skipped while Xcode is running".to_string())
        );
        assert!(
            e.checks
                .contains(&"Nothing inside changed in the last 7 days".to_string())
        );
        let e = explain(&item(
            "dev.devicesupport:Library/Developer/Xcode/iOS DeviceSupport/iPhone15,2 17.5",
            Class::Review,
            "Xcode copies it again when you connect the device, which takes a few minutes.",
        ));
        assert!(
            e.verdict
                .starts_with("Worth a look before you clean it. Xcode copies")
        );
        assert!(
            e.verdict.ends_with("minutes."),
            "no double stop: {}",
            e.verdict
        );
        assert!(
            e.checks
                .contains(&"The newest one for each device is always kept".to_string())
        );
        assert_eq!(
            e.patterns,
            [
                "Library/Developer/Xcode/{iOS DeviceSupport, watchOS DeviceSupport, tvOS DeviceSupport, visionOS DeviceSupport, xrOS DeviceSupport}/*"
            ]
        );
    }

    #[test]
    fn report_only_rows_carry_their_command_and_no_move_checks() {
        let e = explain(&item(
            "dev.simulators:Library/Developer/CoreSimulator/Devices",
            Class::ReportOnly,
            "Remove unused simulators with: xcrun simctl delete unavailable",
        ));
        assert!(e.verdict.starts_with("Tidy won't remove this."));
        assert_eq!(
            e.command.as_deref(),
            Some("xcrun simctl delete unavailable")
        );
        assert_eq!(e.checks, [NEVER_MOVED]);
        let vm = explain(&item(
            "ai.vm:.colima",
            Class::ReportOnly,
            "Virtual machine disk.",
        ));
        assert_eq!(vm.command, None);
        assert!(vm.rebuild.contains("Nothing rebuilds"));
    }

    #[test]
    fn project_rules_name_their_markers_and_folded_rows_find_their_rule() {
        let e = explain(&item(
            "proj.target:Documents/projects/tidy/target",
            Class::Safe,
            "Build output, project not touched for 41 days",
        ));
        assert_eq!(e.patterns, ["<project>/target"]);
        assert_eq!(e.rebuild, "cargo build makes it again.");
        assert_eq!(
            e.checks[0],
            "Cargo.toml sits beside it, or Cargo's tag is inside"
        );
        assert!(
            e.checks
                .contains(&"Skipped while Cargo is running".to_string())
        );
        assert!(
            e.checks
                .contains(&"The project hasn't changed in 30 days".to_string())
        );
        let folded = explain(&item(
            "logs.app.small:Library/Logs/Slack",
            Class::Safe,
            "Log from Slack, older than 14 days",
        ));
        assert_eq!(folded.rule_id, "logs.app");
        assert_eq!(folded.patterns, ["Library/Logs/*/<file>"]);
        let web = explain(&item(
            "proj.web-cache:src/site/.next",
            Class::Safe,
            "Build output",
        ));
        assert_eq!(web.checks[0], "package.json sits beside it");
    }

    #[test]
    fn chromium_and_alternation_patterns_read_as_braces() {
        let e = explain(&item(
            "cache.electron:Library/Application Support/Slack/Cache",
            Class::Safe,
            "Cache, rebuilt by the app",
        ));
        assert!(e.patterns[0].starts_with("Library/Application Support/<app>/{Cache, Code Cache"));
        let sw = explain(&item(
            "cache.electron.sw:Library/Application Support/Slack/Service Worker/CacheStorage",
            Class::Review,
            "Some websites may load slower once or lose offline data.",
        ));
        assert_eq!(
            sw.patterns,
            ["Library/Application Support/<app>/Service Worker/CacheStorage"]
        );
    }

    #[test]
    fn an_unknown_rule_still_explains_its_reason_and_path() {
        let e = explain(&item("demo:Library/Caches/x", Class::Safe, "Cache"));
        assert_eq!(e.verdict, "Safe to clean. Cache.");
        assert!(e.rebuild.is_empty() && e.patterns.is_empty());
        assert_eq!(e.checks, [RECHECK, TRASH_LINE]);
        assert_eq!(e.rel_path, "Library/Caches/x");
    }

    #[test]
    fn every_rule_can_be_explained_in_plain_words() {
        let all = rules::RULES
            .iter()
            .map(|r| (r.id, r.class, r.rebuild))
            .chain(
                rules::PROJECT_RULES
                    .iter()
                    .map(|r| (r.id, r.class, r.rebuild)),
            )
            .chain([(
                rules::UNKNOWN_CACHE.id,
                rules::UNKNOWN_CACHE.class,
                rules::UNKNOWN_CACHE.rebuild,
            )]);
        for (id, class, rebuild) in all {
            assert!(!rebuild.is_empty(), "{id} has no rebuild sentence");
            let e = explain(&item(&format!("{id}:a/b"), class, "Reason"));
            for text in std::iter::once(&e.verdict)
                .chain(std::iter::once(&e.rebuild))
                .chain(e.checks.iter())
            {
                assert!(
                    !text.contains('\u{2014}') && !text.contains('\u{2013}'),
                    "{text}"
                );
            }
            if class != Class::ReportOnly {
                assert_eq!(
                    e.checks.last().map(String::as_str),
                    Some(TRASH_LINE),
                    "{id}"
                );
            }
        }
        // Report only rows whose reason names a command carry that command.
        for r in rules::RULES.iter().filter(|r| r.reason.contains("with: ")) {
            let cmd = r
                .command
                .unwrap_or_else(|| panic!("{} has no command", r.id));
            assert!(r.reason.ends_with(cmd), "{}", r.id);
        }
    }
}
