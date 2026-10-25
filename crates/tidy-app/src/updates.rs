//! Auto-update with Sparkle 2 (the shared contract in the app-updates repo, CONTRACT.md).
//!
//! The app creates one `SPUStandardUpdaterController` on the main thread after it finished
//! launching and keeps it in the [`Updates`] global for the rest of its life. Sparkle reads the
//! feed URL, the public key and the check schedule from Info.plist, and stores the user's
//! choices (automatic checks, automatic downloads) in its own defaults, so Tidy saves nothing.
//!
//! The updater never starts in background or screenshot launches (`TIDY_BACKGROUND`,
//! `TIDY_DEMO`, `TIDY_DEMO_SHOT`, `SHOTS_RENDER`), in test runs (`TIDY_HOME`, unit tests), when
//! `TIDY_NO_UPDATES=1`, or outside a Tidy.app bundle with a feed URL. [`decide`] is that rule;
//! tidyctl never links Sparkle at all.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{App, Global};

/// Why the updater does not start in this run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Off {
    /// `TIDY_NO_UPDATES` is set.
    NoUpdates,
    /// `TIDY_BACKGROUND` or `TIDY_DEMO`: an acceptance or screenshot launch.
    Background,
    /// An offscreen screenshot (`TIDY_DEMO_SHOT` or `SHOTS_RENDER`).
    Shot,
    /// `TIDY_HOME` is set: a test run on fake folders.
    TestHome,
    /// Not started from a bundle whose Info.plist has `SUFeedURL` (`cargo run`, a test binary).
    NotBundled,
}

/// True for a set flag: not empty, not "0".
fn flag(v: Option<String>) -> bool {
    v.is_some_and(|v| {
        let v = v.trim();
        !v.is_empty() && v != "0"
    })
}

/// Whether this run may start the updater. `var` reads an environment variable; `bundled` is
/// true when the main bundle's Info.plist has a feed URL.
pub fn decide(var: impl Fn(&str) -> Option<String>, bundled: bool) -> Result<(), Off> {
    if flag(var("TIDY_NO_UPDATES")) {
        return Err(Off::NoUpdates);
    }
    if flag(var("SHOTS_RENDER")) || var("TIDY_DEMO_SHOT").is_some_and(|v| !v.is_empty()) {
        return Err(Off::Shot);
    }
    if crate::app::background_from(
        var("TIDY_BACKGROUND").as_deref(),
        var("TIDY_DEMO").as_deref(),
    ) {
        return Err(Off::Background);
    }
    if var("TIDY_HOME").is_some_and(|v| !v.is_empty()) {
        return Err(Off::TestHome);
    }
    if !bundled {
        return Err(Off::NotBundled);
    }
    Ok(())
}

/// What the app asks of the updater. [`Sparkle`] is the real one; [`FakeUpdates`] stands in for
/// it in tests and demo screenshots.
pub trait UpdateControl {
    /// Checks now and shows Sparkle's own window with the result (the menu item).
    fn check_for_updates(&self);
    fn automatically_checks(&self) -> bool;
    fn set_automatically_checks(&self, on: bool);
    fn automatically_downloads(&self) -> bool;
    fn set_automatically_downloads(&self, on: bool);
}

/// The running updater. Absent when [`decide`] said no, and then "Check for Updates…" is
/// greyed out and the Check up screen says updates are off.
pub struct Updates(pub Rc<dyn UpdateControl>);
impl Global for Updates {}

/// "Automatically install updates": Sparkle checks on its schedule and downloads and installs
/// by itself.
pub fn auto_install(c: &dyn UpdateControl) -> bool {
    c.automatically_checks() && c.automatically_downloads()
}

/// Turning it on also turns the scheduled check on. Turning it off keeps the check, so Sparkle
/// still says when a new version is out and asks before installing it.
pub fn set_auto_install(c: &dyn UpdateControl, on: bool) {
    if on && !c.automatically_checks() {
        c.set_automatically_checks(true);
    }
    c.set_automatically_downloads(on);
}

/// The updater of this run, if any.
pub fn control(cx: &App) -> Option<Rc<dyn UpdateControl>> {
    cx.try_global::<Updates>().map(|u| u.0.clone())
}

/// The menu item's action.
pub fn check_for_updates(cx: &mut App) {
    match control(cx) {
        Some(c) => c.check_for_updates(),
        None => tracing::info!("updates: off in this run, nothing to check"),
    }
}

/// Puts `control` in the [`Updates`] global and enables "Check for Updates…".
pub fn install(control: Rc<dyn UpdateControl>, cx: &mut App) {
    let first = cx.try_global::<Updates>().is_none();
    cx.set_global(Updates(control));
    if first {
        cx.on_action(|_: &crate::app::CheckForUpdates, cx| check_for_updates(cx));
    }
}

/// Starts Sparkle when [`decide`] allows it. Call once, inside `run()` (the app has finished
/// launching), on the main thread. Unit tests never start it.
pub fn start(cx: &mut App) {
    let decision = decide(|k| std::env::var(k).ok(), bundle_has_feed());
    if let Err(off) = decision {
        tracing::info!("updates: off ({off:?})");
        return;
    }
    if cfg!(test) {
        return;
    }
    match Sparkle::create() {
        Some(sparkle) => {
            tracing::info!("updates: Sparkle started");
            install(Rc::new(sparkle), cx);
        }
        None => tracing::warn!("updates: Sparkle did not start"),
    }
}

/// True when the main bundle's Info.plist has `SUFeedURL`, so Sparkle has a feed to read.
fn bundle_has_feed() -> bool {
    use objc2_foundation::{NSBundle, NSString};
    NSBundle::mainBundle()
        .objectForInfoDictionaryKey(&NSString::from_str("SUFeedURL"))
        .is_some()
}

/// Sparkle's standard updater controller, with its standard user interface.
pub struct Sparkle {
    controller: objc2::rc::Retained<objc2::runtime::AnyObject>,
}

impl Sparkle {
    /// `[[SPUStandardUpdaterController alloc] initWithStartingUpdater:YES updaterDelegate:nil
    /// userDriverDelegate:nil]`. None off the main thread or when the class is missing.
    fn create() -> Option<Sparkle> {
        use objc2::rc::{Allocated, Retained};
        use objc2::runtime::{AnyClass, AnyObject};
        use objc2::{MainThreadMarker, msg_send};

        MainThreadMarker::new()?;
        let class = AnyClass::get(c"SPUStandardUpdaterController")?;
        let nil: Option<&AnyObject> = None;
        // SAFETY: the class is Sparkle's SPUStandardUpdaterController, linked into this binary.
        // alloc and the designated initializer take and return objects as declared in
        // SPUStandardUpdaterController.h; nil delegates are allowed. We are on the main thread.
        let controller: Option<Retained<AnyObject>> = unsafe {
            let obj: Allocated<AnyObject> = msg_send![class, alloc];
            msg_send![obj, initWithStartingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil]
        };
        controller.map(|controller| Sparkle { controller })
    }

    /// The controller's `SPUUpdater`.
    fn updater(&self) -> objc2::rc::Retained<objc2::runtime::AnyObject> {
        // SAFETY: `updater` is a nonnull readonly property of SPUStandardUpdaterController.
        unsafe { objc2::msg_send![&*self.controller, updater] }
    }
}

impl UpdateControl for Sparkle {
    fn check_for_updates(&self) {
        let nil: Option<&objc2::runtime::AnyObject> = None;
        // SAFETY: `checkForUpdates:` is an IBAction that takes a nullable sender.
        unsafe { objc2::msg_send![&*self.controller, checkForUpdates: nil] }
    }

    fn automatically_checks(&self) -> bool {
        // SAFETY: a BOOL property of SPUUpdater.
        unsafe { objc2::msg_send![&*self.updater(), automaticallyChecksForUpdates] }
    }

    fn set_automatically_checks(&self, on: bool) {
        // SAFETY: the setter of the same BOOL property.
        unsafe { objc2::msg_send![&*self.updater(), setAutomaticallyChecksForUpdates: on] }
    }

    fn automatically_downloads(&self) -> bool {
        // SAFETY: a BOOL property of SPUUpdater.
        unsafe { objc2::msg_send![&*self.updater(), automaticallyDownloadsUpdates] }
    }

    fn set_automatically_downloads(&self, on: bool) {
        // SAFETY: the setter of the same BOOL property.
        unsafe { objc2::msg_send![&*self.updater(), setAutomaticallyDownloadsUpdates: on] }
    }
}

/// An in-memory updater for tests and demo screenshots. Nothing is saved and nothing is
/// fetched; `checks` counts the "Check for Updates…" calls.
pub struct FakeUpdates {
    pub automatic_checks: Cell<bool>,
    pub automatic_downloads: Cell<bool>,
    pub checks: Cell<u32>,
}

impl FakeUpdates {
    /// Sparkle's state with the contract's Info.plist: both on.
    pub fn new(checks: bool, downloads: bool) -> FakeUpdates {
        FakeUpdates {
            automatic_checks: Cell::new(checks),
            automatic_downloads: Cell::new(downloads),
            checks: Cell::new(0),
        }
    }
}

impl UpdateControl for FakeUpdates {
    fn check_for_updates(&self) {
        self.checks.set(self.checks.get() + 1);
    }
    fn automatically_checks(&self) -> bool {
        self.automatic_checks.get()
    }
    fn set_automatically_checks(&self, on: bool) {
        self.automatic_checks.set(on);
    }
    fn automatically_downloads(&self) -> bool {
        self.automatic_downloads.get()
    }
    fn set_automatically_downloads(&self, on: bool) {
        self.automatic_downloads.set(on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::collections::HashMap;

    fn with(vars: &[(&str, &str)], bundled: bool) -> Result<(), Off> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        decide(|k| map.get(k).cloned(), bundled)
    }

    #[test]
    fn a_plain_launch_of_the_bundle_starts_the_updater() {
        assert_eq!(with(&[], true), Ok(()));
        // Variables that are set but off do not stop it.
        assert_eq!(
            with(
                &[
                    ("TIDY_NO_UPDATES", "0"),
                    ("TIDY_BACKGROUND", "0"),
                    ("TIDY_DEMO", "off"),
                    ("TIDY_DEMO_SHOT", ""),
                    ("SHOTS_RENDER", "0"),
                    ("TIDY_HOME", ""),
                ],
                true
            ),
            Ok(())
        );
    }

    #[test]
    fn no_updates_stops_it() {
        assert_eq!(with(&[("TIDY_NO_UPDATES", "1")], true), Err(Off::NoUpdates));
    }

    #[test]
    fn background_and_shot_launches_never_start_it() {
        assert_eq!(
            with(&[("TIDY_BACKGROUND", "1")], true),
            Err(Off::Background)
        );
        assert_eq!(
            with(&[("TIDY_DEMO", "settings")], true),
            Err(Off::Background)
        );
        assert_eq!(with(&[("TIDY_DEMO", "typo")], true), Err(Off::Background));
        assert_eq!(with(&[("SHOTS_RENDER", "1")], true), Err(Off::Shot));
        assert_eq!(
            with(&[("TIDY_DEMO_SHOT", "/tmp/x.png")], true),
            Err(Off::Shot)
        );
    }

    #[test]
    fn test_runs_and_unbundled_binaries_never_start_it() {
        assert_eq!(with(&[("TIDY_HOME", "/tmp/h")], true), Err(Off::TestHome));
        assert_eq!(with(&[], false), Err(Off::NotBundled));
    }

    #[test]
    fn the_unit_test_binary_has_no_feed_and_starts_nothing() {
        // The test binary is no bundle, so even with a clean environment there is no updater.
        assert!(!bundle_has_feed());
    }

    #[test]
    fn auto_install_follows_both_sparkle_settings() {
        let f = FakeUpdates::new(true, true);
        assert!(auto_install(&f));
        set_auto_install(&f, false);
        assert!(!auto_install(&f));
        assert!(f.automatic_checks.get(), "the daily check stays on");
        let f = FakeUpdates::new(false, false);
        set_auto_install(&f, true);
        assert!(f.automatic_checks.get() && f.automatic_downloads.get());
        let f = FakeUpdates::new(false, true);
        assert!(!auto_install(&f), "no downloads without checks");
    }
}
