//! macOS glue: the free-space figure for the sidebar, the App Nap activity token, and (M2) the
//! two things the core cannot do for the app: asking an app to quit and scheduling the weekly
//! reminder notification.
//!
//! * [`Terminator`] wraps `NSRunningApplication` (a polite `terminate`, `forceTerminate`,
//!   `isTerminated`) and app icons. The Memory screen only uses it through the trait, and tests
//!   use [`fakes::FakeTerminator`], so no test ever signals a process.
//! * [`Notifier`] wraps `UNUserNotificationCenter`: authorization status, the one request that
//!   can show the system prompt, and the single `UNCalendarNotificationTrigger` request. Tests
//!   use [`fakes::FakeNotifier`].
//!
//! Everything here is inert in unit tests, so tests never touch Foundation, AppKit, the process
//! table or the notification center. Call these only from the view's setup or from explicit user
//! actions.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidy_core::reminder::ReminderDate;
use tokio::sync::oneshot;

/// The size of a volume, for the sidebar footer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskInfo {
    /// The volume's name, for example "Macintosh HD".
    pub name: String,
    pub total: u64,
    /// Space available for important data, which is the figure Finder shows
    /// (`NSURLVolumeAvailableCapacityForImportantUsageKey`, C25).
    pub available: u64,
}

impl DiskInfo {
    /// The fraction of the volume in use, 0 to 1.
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.;
        }
        let used = self.total.saturating_sub(self.available);
        (used as f64 / self.total as f64).clamp(0., 1.) as f32
    }
}

/// The volume that holds `path`. None when macOS gives no answer. It reads only volume
/// attributes, never folder contents. Compiled out of tests, which return None.
pub fn disk_info(path: &Path) -> Option<DiskInfo> {
    #[cfg(not(test))]
    {
        imp::disk_info(path)
    }
    #[cfg(test)]
    {
        let _ = path;
        None
    }
}

/// Free space in bytes on the volume of `path` by `statvfs`, the plain fallback and the figure
/// the self-test prints.
pub fn statvfs_available(path: &Path) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` is a valid out-parameter.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frag = st.f_frsize as u64;
    Some((st.f_blocks as u64 * frag, st.f_bavail as u64 * frag))
}

#[cfg(not(test))]
mod imp {
    use std::path::Path;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{
        NSArray, NSNumber, NSString, NSURL, NSURLVolumeAvailableCapacityForImportantUsageKey,
        NSURLVolumeLocalizedNameKey, NSURLVolumeTotalCapacityKey,
    };

    use super::{DiskInfo, statvfs_available};

    pub fn disk_info(path: &Path) -> Option<DiskInfo> {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        // SAFETY: the three statics are NSString constants provided by Foundation.
        let keys = unsafe {
            NSArray::from_slice(&[
                NSURLVolumeAvailableCapacityForImportantUsageKey,
                NSURLVolumeTotalCapacityKey,
                NSURLVolumeLocalizedNameKey,
            ])
        };
        let values = url.resourceValuesForKeys_error(&keys).ok()?;
        let number = |key: &objc2_foundation::NSURLResourceKey| -> Option<u64> {
            let obj: Retained<AnyObject> = values.objectForKey(key)?;
            let n = obj.downcast_ref::<NSNumber>()?;
            u64::try_from(n.longLongValue()).ok()
        };
        // SAFETY: reading Foundation's NSString constants.
        let (available, total, name) = unsafe {
            (
                number(NSURLVolumeAvailableCapacityForImportantUsageKey),
                number(NSURLVolumeTotalCapacityKey),
                values
                    .objectForKey(NSURLVolumeLocalizedNameKey)
                    .and_then(|o| o.downcast_ref::<NSString>().map(|s| s.to_string())),
            )
        };
        let (fb_total, fb_avail) = statvfs_available(path).unwrap_or((0, 0));
        let total = total.filter(|t| *t > 0).unwrap_or(fb_total);
        if total == 0 {
            return None;
        }
        Some(DiskInfo {
            name: name.unwrap_or_else(|| "Macintosh HD".to_string()),
            total,
            available: available.unwrap_or(fb_avail),
        })
    }
}

/// Keeps App Nap from throttling Tidy while a scan or clean runs. The system may still sleep
/// when idle. Dropping the token ends the activity.
pub struct ActivityToken {
    #[cfg(not(test))]
    token:
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>,
}

impl ActivityToken {
    pub fn begin(reason: &str) -> ActivityToken {
        #[cfg(not(test))]
        {
            use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
            let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                &NSString::from_str(reason),
            );
            ActivityToken { token }
        }
        #[cfg(test)]
        {
            let _ = reason;
            ActivityToken {}
        }
    }
}

impl Drop for ActivityToken {
    fn drop(&mut self) {
        #[cfg(not(test))]
        {
            use objc2_foundation::NSProcessInfo;
            // SAFETY: the token came from beginActivityWithOptions_reason on the same process
            // info object and is ended exactly once.
            unsafe { NSProcessInfo::processInfo().endActivity(&self.token) };
        }
    }
}

impl std::fmt::Debug for ActivityToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActivityToken")
    }
}

// ---------- Quitting apps (Memory screen) ----------

/// How the Memory screen asks an app to quit, and where it gets app icons. The real one is
/// [`SystemTerminator`]; tests use [`fakes::FakeTerminator`]. A polite quit is always a request:
/// `terminate` asks the app, and the app may ask the user to save first. Force Quit is only
/// called after the user confirms.
pub trait Terminator {
    /// `NSRunningApplication.terminate` for the app with this pid. True when the request was
    /// delivered; false when there is no such app (a plain process has no
    /// `NSRunningApplication`).
    fn terminate(&self, pid: i32) -> bool;
    /// `NSRunningApplication.forceTerminate`.
    fn force_terminate(&self, pid: i32) -> bool;
    /// True when the app has exited, or when no app with this pid is running any more.
    fn is_terminated(&self, pid: i32) -> bool;
    /// `NSRunningApplication.bundleURL` of the app with this pid, as a path: what Relaunch
    /// opens again once the app has quit. None for a plain process or Tidy itself.
    fn bundle_path(&self, pid: i32) -> Option<PathBuf>;
    /// Opens the app bundle at `bundle` (`NSWorkspace openApplicationAtURL`), without bringing
    /// it to the front. True when the request was made.
    fn open_app(&self, bundle: &Path) -> bool;
    /// The app's icon as a PNG image for the row, or None.
    fn icon(&self, app_path: &Path) -> Option<Arc<gpui_kit::Image>>;
}

/// The real [`Terminator`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTerminator;

impl Terminator for SystemTerminator {
    fn terminate(&self, pid: i32) -> bool {
        #[cfg(not(test))]
        {
            imp_apps::running(pid).is_some_and(|app| app.terminate())
        }
        #[cfg(test)]
        {
            let _ = pid;
            false
        }
    }

    fn force_terminate(&self, pid: i32) -> bool {
        #[cfg(not(test))]
        {
            imp_apps::running(pid).is_some_and(|app| app.forceTerminate())
        }
        #[cfg(test)]
        {
            let _ = pid;
            false
        }
    }

    fn is_terminated(&self, pid: i32) -> bool {
        #[cfg(not(test))]
        {
            imp_apps::running(pid).is_none_or(|app| app.isTerminated())
        }
        #[cfg(test)]
        {
            let _ = pid;
            true
        }
    }

    fn bundle_path(&self, pid: i32) -> Option<PathBuf> {
        #[cfg(not(test))]
        {
            let url = imp_apps::running(pid)?.bundleURL()?;
            let path = url.path()?.to_string();
            (!path.is_empty()).then(|| PathBuf::from(path))
        }
        #[cfg(test)]
        {
            let _ = pid;
            None
        }
    }

    fn open_app(&self, bundle: &Path) -> bool {
        #[cfg(not(test))]
        {
            imp_apps::open(bundle)
        }
        #[cfg(test)]
        {
            let _ = bundle;
            false
        }
    }

    fn icon(&self, app_path: &Path) -> Option<Arc<gpui_kit::Image>> {
        #[cfg(not(test))]
        {
            let png = icon_png(app_path, ICON_PX)?;
            Some(Arc::new(gpui_kit::Image::from_bytes(
                gpui_kit::ImageFormat::Png,
                png,
            )))
        }
        #[cfg(test)]
        {
            let _ = app_path;
            None
        }
    }
}

/// The pixel size icons are drawn at: 28 pt tiles at 2x.
pub const ICON_PX: usize = 64;

#[cfg(not(test))]
mod imp_apps {
    use objc2::rc::Retained;
    use objc2_app_kit::NSRunningApplication;

    /// The running app with this pid. None for a pid that is not an app (a plain process, or
    /// one that has exited). Never returns Tidy itself: the model refuses that pid first, and
    /// this is a second guard.
    pub fn running(pid: i32) -> Option<Retained<NSRunningApplication>> {
        if pid <= 0 || pid as u32 == std::process::id() {
            return None;
        }
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
    }

    /// Opens an app bundle again after Relaunch saw it quit. It asks Launch Services, the way
    /// a Dock click does, and leaves Tidy in front. Only a folder that ends in `.app` is
    /// opened.
    pub fn open(bundle: &std::path::Path) -> bool {
        use objc2_app_kit::{NSWorkspace, NSWorkspaceOpenConfiguration};
        use objc2_foundation::{NSString, NSURL};
        let is_app = bundle
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
            && bundle.is_dir();
        if !is_app {
            return false;
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(&bundle.to_string_lossy()));
        let config = NSWorkspaceOpenConfiguration::configuration();
        config.setActivates(false);
        NSWorkspace::sharedWorkspace()
            .openApplicationAtURL_configuration_completionHandler(&url, &config, None);
        true
    }
}

/// The icon of the file at `path` (an `.app` bundle) drawn into a `px` x `px` bitmap and
/// encoded as PNG. It reads the file's icon through `NSWorkspace` and draws into an offscreen
/// bitmap; no window is created. None when AppKit gives no icon.
pub fn icon_png(path: &Path, px: usize) -> Option<Vec<u8>> {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSData, NSDictionary, NSPoint, NSRect, NSSize, NSString};

    let side = px as f64;
    // SAFETY: plain Objective-C calls on AppKit classes with their documented signatures. Every
    // object is checked for nil before it is used, and the graphics state is restored.
    unsafe {
        let workspace: Option<Retained<AnyObject>> =
            msg_send![class!(NSWorkspace), sharedWorkspace];
        let image: Option<Retained<AnyObject>> = msg_send![
            &*workspace?,
            iconForFile: &*NSString::from_str(&path.to_string_lossy())
        ];
        let image = image?;
        let alloc: Allocated<AnyObject> = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: Option<Retained<AnyObject>> = msg_send![
            alloc,
            initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>(),
            pixelsWide: px as isize,
            pixelsHigh: px as isize,
            bitsPerSample: 8isize,
            samplesPerPixel: 4isize,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: &*NSString::from_str("NSDeviceRGBColorSpace"),
            bytesPerRow: 0isize,
            bitsPerPixel: 0isize
        ];
        let rep = rep?;
        let context: Option<Retained<AnyObject>> = msg_send![
            class!(NSGraphicsContext),
            graphicsContextWithBitmapImageRep: &*rep
        ];
        let context = context?;
        let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
        let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: &*context];
        let target = NSRect::new(NSPoint::new(0., 0.), NSSize::new(side, side));
        let whole = NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.));
        // NSCompositingOperationSourceOver is 2.
        let _: () = msg_send![
            &*image,
            drawInRect: target,
            fromRect: whole,
            operation: 2usize,
            fraction: 1.0f64
        ];
        let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];
        let props = NSDictionary::<NSString, AnyObject>::new();
        // NSBitmapImageFileTypePNG is 4.
        let data: Option<Retained<NSData>> = msg_send![
            &*rep,
            representationUsingType: 4usize,
            properties: &*props
        ];
        data.map(|d| d.to_vec())
    }
}

// ---------- The weekly reminder (Check up screen) ----------

/// Where notification permission stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    /// Never asked. Only switching the reminder on asks.
    NotDetermined,
    Granted,
    /// The user said no (or turned Tidy's notifications off in System Settings).
    Denied,
    /// There is no notification center to ask: the app is not running from a bundle.
    Unavailable,
}

/// The weekly reminder's notification, behind a trait so tests never touch the notification
/// center. The receivers are tokio oneshots because the system answers on another thread.
pub trait Notifier {
    /// The current permission. Never shows a prompt.
    fn status(&self) -> oneshot::Receiver<Permission>;
    /// Asks for permission. This is the only call that can show the system prompt; the app
    /// makes it only when the user switches the reminder on.
    fn request(&self) -> oneshot::Receiver<Permission>;
    /// Makes `when` the single pending reminder, replacing any earlier one.
    fn schedule(&self, when: &ReminderDate);
    /// Removes the pending reminder.
    fn cancel(&self);
}

/// The real [`Notifier`], on `UNUserNotificationCenter`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn status(&self) -> oneshot::Receiver<Permission> {
        let (tx, rx) = oneshot::channel();
        #[cfg(not(test))]
        imp_notify::status(tx);
        #[cfg(test)]
        let _ = tx.send(Permission::Unavailable);
        rx
    }

    fn request(&self) -> oneshot::Receiver<Permission> {
        let (tx, rx) = oneshot::channel();
        #[cfg(not(test))]
        imp_notify::request(tx);
        #[cfg(test)]
        let _ = tx.send(Permission::Unavailable);
        rx
    }

    fn schedule(&self, when: &ReminderDate) {
        #[cfg(not(test))]
        imp_notify::schedule(when);
        #[cfg(test)]
        let _ = when;
    }

    fn cancel(&self) {
        #[cfg(not(test))]
        imp_notify::cancel();
    }
}

/// A sender the notification delegate uses to tell the app that the user clicked the reminder.
/// The app takes the receiving end with [`take_reminder_clicks`].
static REMINDER_CLICKS: std::sync::OnceLock<
    std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<()>>>,
> = std::sync::OnceLock::new();

/// A channel that gets one message each time the user opens Tidy from the reminder. Call once;
/// a second call replaces the first receiver. Installs the notification delegate (which does
/// not prompt for anything) when the app runs from a bundle.
pub fn take_reminder_clicks() -> tokio::sync::mpsc::UnboundedReceiver<()> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let slot = REMINDER_CLICKS.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(mut slot) = slot.lock() {
        *slot = Some(tx);
    }
    #[cfg(not(test))]
    imp_notify::install_delegate();
    rx
}

#[cfg(not(test))]
fn reminder_clicked() {
    if let Some(slot) = REMINDER_CLICKS.get()
        && let Ok(slot) = slot.lock()
        && let Some(tx) = slot.as_ref()
    {
        let _ = tx.send(());
    }
}

#[cfg(not(test))]
mod imp_notify {
    use std::ptr::NonNull;
    use std::sync::Mutex;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{MainThreadOnly, define_class, msg_send};
    use objc2_foundation::{NSArray, NSBundle, NSDateComponents, NSError, NSInteger, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNAuthorizationStatus, UNCalendarNotificationTrigger,
        UNMutableNotificationContent, UNNotificationRequest, UNNotificationResponse,
        UNNotificationSettings, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    use tidy_core::reminder::{REMINDER_BODY, REMINDER_ID, ReminderDate};
    use tokio::sync::oneshot;

    use super::Permission;

    /// The notification center needs an app bundle; a bare executable would raise an
    /// Objective-C exception on first use.
    fn bundled() -> bool {
        NSBundle::mainBundle()
            .bundlePath()
            .to_string()
            .ends_with(".app")
    }

    fn map(status: UNAuthorizationStatus) -> Permission {
        if status == UNAuthorizationStatus::Denied {
            Permission::Denied
        } else if status == UNAuthorizationStatus::NotDetermined {
            Permission::NotDetermined
        } else {
            // Authorized, Provisional and Ephemeral all deliver.
            Permission::Granted
        }
    }

    pub fn status(tx: oneshot::Sender<Permission>) {
        if !bundled() {
            let _ = tx.send(Permission::Unavailable);
            return;
        }
        let tx = Mutex::new(Some(tx));
        let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: the system passes a valid settings object for the duration of the call.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            if let Ok(mut tx) = tx.lock()
                && let Some(tx) = tx.take()
            {
                let _ = tx.send(map(status));
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&block);
    }

    pub fn request(tx: oneshot::Sender<Permission>) {
        if !bundled() {
            let _ = tx.send(Permission::Unavailable);
            return;
        }
        let tx = Mutex::new(Some(tx));
        let block = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            if let Ok(mut tx) = tx.lock()
                && let Some(tx) = tx.take()
            {
                let _ = tx.send(if granted.as_bool() {
                    Permission::Granted
                } else {
                    Permission::Denied
                });
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &block,
            );
    }

    pub fn schedule(when: &ReminderDate) {
        if !bundled() {
            return;
        }
        let parts = NSDateComponents::new();
        parts.setYear(NSInteger::from(when.year));
        parts.setMonth(NSInteger::from(when.month));
        parts.setDay(NSInteger::from(when.day));
        parts.setHour(NSInteger::from(when.hour));
        parts.setMinute(NSInteger::from(when.minute));
        let trigger =
            UNCalendarNotificationTrigger::triggerWithDateMatchingComponents_repeats(&parts, false);
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str("Tidy"));
        content.setBody(&NSString::from_str(REMINDER_BODY));
        // The same identifier replaces the earlier request, so there is only ever one.
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(REMINDER_ID),
            &content,
            Some(&trigger),
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, None);
    }

    pub fn cancel() {
        if !bundled() {
            return;
        }
        let id = NSString::from_str(REMINDER_ID);
        UNUserNotificationCenter::currentNotificationCenter()
            .removePendingNotificationRequestsWithIdentifiers(&NSArray::from_slice(&[&*id]));
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and the class has no ivars.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "TidyNotificationDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                _response: &UNNotificationResponse,
                done: &block2::DynBlock<dyn Fn()>,
            ) {
                super::reminder_clicked();
                done.call(());
            }
        }
    );

    /// Sets the delegate that reports a click on the reminder. Setting a delegate never prompts.
    pub fn install_delegate() {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return;
        };
        if !bundled() {
            return;
        }
        let delegate: Retained<Delegate> = {
            let this = mtm.alloc::<Delegate>();
            // SAFETY: NSObject's init.
            unsafe { msg_send![super(this.set_ivars(())), init] }
        };
        let proto = ProtocolObject::from_ref(&*delegate);
        UNUserNotificationCenter::currentNotificationCenter().setDelegate(Some(proto));
        // The center holds its delegate weakly; this one lives as long as the app.
        std::mem::forget(delegate);
    }
}

/// Fakes for tests: nothing here signals a process or touches the notification center.
#[cfg(test)]
pub mod fakes {
    use std::cell::{Cell, RefCell};
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use tidy_core::reminder::ReminderDate;
    use tokio::sync::oneshot;

    use super::{Notifier, Permission, Terminator};

    /// Records the quit requests it gets. A pid counts as exited once a test puts it in
    /// `exited`.
    #[derive(Default)]
    pub struct FakeTerminator {
        pub polite: RefCell<Vec<i32>>,
        pub forced: RefCell<Vec<i32>>,
        pub exited: RefCell<HashSet<i32>>,
        /// The bundle each pid reports, for Relaunch.
        pub bundles: RefCell<HashMap<i32, PathBuf>>,
        /// The bundles Relaunch opened again.
        pub opened: RefCell<Vec<PathBuf>>,
        /// What `open_app` answers (true unless a test says otherwise).
        pub open_fails: Cell<bool>,
    }

    impl Terminator for FakeTerminator {
        fn terminate(&self, pid: i32) -> bool {
            self.polite.borrow_mut().push(pid);
            true
        }

        fn force_terminate(&self, pid: i32) -> bool {
            self.forced.borrow_mut().push(pid);
            true
        }

        fn is_terminated(&self, pid: i32) -> bool {
            self.exited.borrow().contains(&pid)
        }

        fn bundle_path(&self, pid: i32) -> Option<PathBuf> {
            self.bundles.borrow().get(&pid).cloned()
        }

        fn open_app(&self, bundle: &Path) -> bool {
            if self.open_fails.get() {
                return false;
            }
            self.opened.borrow_mut().push(bundle.to_path_buf());
            true
        }

        fn icon(&self, _app_path: &Path) -> Option<Arc<gpui_kit::Image>> {
            None
        }
    }

    /// Records every call. `permission` is what `status` answers; `on_request` is what the
    /// system prompt would answer, and becomes the new `permission`.
    pub struct FakeNotifier {
        pub permission: Cell<Permission>,
        pub on_request: Cell<Permission>,
        pub status_calls: Cell<usize>,
        pub requests: Cell<usize>,
        pub scheduled: RefCell<Vec<ReminderDate>>,
        pub cancels: Cell<usize>,
    }

    impl Default for FakeNotifier {
        fn default() -> Self {
            FakeNotifier {
                permission: Cell::new(Permission::NotDetermined),
                on_request: Cell::new(Permission::Granted),
                status_calls: Cell::new(0),
                requests: Cell::new(0),
                scheduled: RefCell::new(Vec::new()),
                cancels: Cell::new(0),
            }
        }
    }

    impl Notifier for FakeNotifier {
        fn status(&self) -> oneshot::Receiver<Permission> {
            self.status_calls.set(self.status_calls.get() + 1);
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(self.permission.get());
            rx
        }

        fn request(&self) -> oneshot::Receiver<Permission> {
            self.requests.set(self.requests.get() + 1);
            self.permission.set(self.on_request.get());
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(self.permission.get());
            rx
        }

        fn schedule(&self, when: &ReminderDate) {
            self.scheduled.borrow_mut().push(*when);
        }

        fn cancel(&self) {
            self.cancels.set(self.cancels.get() + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builds_are_inert() {
        assert_eq!(disk_info(Path::new("/")), None);
        drop(ActivityToken::begin("test"));
    }

    #[test]
    fn test_builds_never_quit_or_notify() {
        // The real terminator and notifier do nothing in a test build.
        let t = SystemTerminator;
        assert!(!t.terminate(1));
        assert!(!t.force_terminate(1));
        assert!(t.bundle_path(1).is_none());
        assert!(!t.open_app(Path::new("/System/Applications/Calculator.app")));
        assert!(t.icon(Path::new("/Applications/Nope.app")).is_none());
        let n = SystemNotifier;
        n.cancel();
        let when = ReminderDate {
            at: 1,
            year: 2026,
            month: 10,
            day: 30,
            hour: 10,
            minute: 0,
        };
        n.schedule(&when);
        let status = n.status().blocking_recv().unwrap();
        assert_eq!(status, Permission::Unavailable);
        assert_eq!(
            n.request().blocking_recv().unwrap(),
            Permission::Unavailable
        );
    }

    /// Draws a system app's icon into a bitmap with AppKit. It reads one icon file and opens no
    /// window, but it is real AppKit work, so it only runs on request:
    /// `cargo test -p tidy-app app_icons_encode_as_png -- --ignored`.
    #[test]
    #[ignore = "uses AppKit"]
    fn app_icons_encode_as_png() {
        let png = icon_png(Path::new("/System/Applications/Calculator.app"), ICON_PX)
            .expect("an icon for Calculator");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(png.len() > 500);
    }

    #[test]
    fn used_fraction_is_clamped() {
        let d = |total, available| DiskInfo {
            name: "x".into(),
            total,
            available,
        };
        assert_eq!(d(0, 0).used_fraction(), 0.);
        assert!((d(1000, 250).used_fraction() - 0.75).abs() < 1e-6);
        assert_eq!(d(1000, 2000).used_fraction(), 0.);
    }

    #[test]
    fn statvfs_reads_a_temp_dir() {
        let (total, avail) = statvfs_available(&std::env::temp_dir()).expect("statvfs");
        assert!(total > 0 && avail <= total);
    }
}
