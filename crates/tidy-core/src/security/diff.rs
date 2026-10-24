//! Week over week: marks what changed since the previous saved report (design section 5.4).
//!
//! - A login item or Chrome extension whose key was not in the previous report is New.
//! - A login item whose program or signer differs is Changed.
//! - A check whose status differs is Changed. The macOS updates row is compared only when
//!   both runs checked for updates, since "not checked" is not a change.
//!
//! The first run (no previous report) marks nothing and sets `first_run`. The marks feed the
//! Login items and Chrome extensions rows, so [`mark_items`] runs before those rows are built
//! and [`mark_checks`] after.

use std::collections::{HashMap, HashSet};

use crate::model::{CheckId, Mark, SecurityReport};

/// Marks New and Changed login items and New extensions against `previous`, and sets
/// `first_run` and `compared_to`.
pub fn mark_items(report: &mut SecurityReport, previous: Option<&SecurityReport>) {
    for item in &mut report.login_items {
        item.mark = None;
    }
    for ext in &mut report.extensions {
        ext.mark = None;
    }
    let Some(prev) = previous else {
        report.first_run = true;
        report.compared_to = None;
        return;
    };
    report.first_run = false;
    report.compared_to = Some(prev.week.clone());
    let before: HashMap<String, _> = prev.login_items.iter().map(|i| (i.key(), i)).collect();
    for item in &mut report.login_items {
        item.mark = match before.get(&item.key()) {
            None => Some(Mark::New),
            Some(old) if old.program != item.program || old.signer != item.signer => {
                Some(Mark::Changed)
            }
            Some(_) => None,
        };
    }
    let seen: HashSet<String> = prev.extensions.iter().map(|e| e.key()).collect();
    for ext in &mut report.extensions {
        if !seen.contains(&ext.key()) {
            ext.mark = Some(Mark::New);
        }
    }
}

/// Marks checks whose status changed since `previous`. Nothing is marked on a first run.
pub fn mark_checks(report: &mut SecurityReport, previous: Option<&SecurityReport>) {
    for check in &mut report.checks {
        check.mark = None;
    }
    let Some(prev) = previous else { return };
    let updates_comparable = report.updates_checked && prev.updates_checked;
    for check in &mut report.checks {
        if check.id == CheckId::MacosUpdates && !updates_comparable {
            continue;
        }
        if let Some(old) = prev.check(check.id)
            && old.status != check.status
        {
            check.mark = Some(Mark::Changed);
        }
    }
}

/// Both steps, for a report whose rows are already final.
pub fn apply(report: &mut SecurityReport, previous: Option<&SecurityReport>) {
    mark_items(report, previous);
    mark_checks(report, previous);
}
