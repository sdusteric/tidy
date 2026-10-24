//! Last-use dates for Large files: Spotlight's `kMDItemLastUsedDate` when it has one, else
//! the file times.
//!
//! [`Spotlight`] is the production [`LastUsed`] seam. It asks the Spotlight store through
//! CoreServices (`MDItemCreate`, `MDItemCopyAttribute`) and never runs a query. Tests and
//! `TIDY_HOME` runs use [`crate::env::FileTimesOnly`] instead (C22), so fixture dates decide
//! and the real Spotlight store is never consulted for fake paths.
//!
//! [`resolve`] combines the seam with the fallback and says which source the date came from,
//! so the row can read "Opened 8 months ago" or "Modified 2 years ago".

use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::env::{LastUsed, file_time};
use crate::model::{UseSource, unix_secs};

/// Spotlight's last-used date, through the CoreServices metadata API.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spotlight;

impl Spotlight {
    /// The production last-use source, ready for `SystemSeams::last_used`.
    pub fn shared() -> Arc<dyn LastUsed> {
        Arc::new(Spotlight)
    }
}

impl LastUsed for Spotlight {
    fn last_used(&self, path: &Path) -> Option<SystemTime> {
        spotlight_last_used(path)
    }
}

/// The last-use date of `path` and where it came from: `source` when it has one
/// ([`UseSource::Spotlight`]), otherwise `fallback`, the later of mtime and atime that the
/// caller already read ([`UseSource::FileTimes`]). Both as Unix seconds.
pub fn resolve(
    source: &dyn LastUsed,
    path: &Path,
    fallback: Option<u64>,
) -> (Option<u64>, UseSource) {
    match source.last_used(path) {
        Some(t) => (Some(unix_secs(t)), UseSource::Spotlight),
        None => (fallback, UseSource::FileTimes),
    }
}

/// The later of mtime and atime of `path` itself (not following a symlink), as Unix seconds.
pub fn file_time_secs(path: &Path) -> Option<u64> {
    let meta = path.symlink_metadata().ok()?;
    file_time(&meta).map(unix_secs)
}

// ---------- CoreServices FFI ----------

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFAllocatorRef = *const c_void;
type MDItemRef = *const c_void;
type CFTypeID = usize;
type CFIndex = isize;
type CFAbsoluteTime = f64;

/// `kCFStringEncodingUTF8`.
const UTF8: u32 = 0x0800_0100;
/// Seconds from 1970-01-01 to 2001-01-01, the CFAbsoluteTime epoch.
const CF_EPOCH_FROM_UNIX: f64 = 978_307_200.0;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        alloc: CFAllocatorRef,
        bytes: *const u8,
        num_bytes: CFIndex,
        encoding: u32,
        is_external_representation: u8,
    ) -> CFStringRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    fn CFDateGetTypeID() -> CFTypeID;
    fn CFDateGetAbsoluteTime(date: CFTypeRef) -> CFAbsoluteTime;
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn MDItemCreate(alloc: CFAllocatorRef, path: CFStringRef) -> MDItemRef;
    fn MDItemCopyAttribute(item: MDItemRef, name: CFStringRef) -> CFTypeRef;
    static kMDItemLastUsedDate: CFStringRef;
}

/// Releases a CF object when dropped.
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Create or Copy call, so this code owns one
            // reference to it.
            unsafe { CFRelease(self.0) }
        }
    }
}

/// `kMDItemLastUsedDate` for `path`, or None when Spotlight has no item or no date for it.
fn spotlight_last_used(path: &Path) -> Option<SystemTime> {
    let bytes = path.as_os_str().as_bytes();
    std::str::from_utf8(bytes).ok()?;
    let len = CFIndex::try_from(bytes.len()).ok()?;
    // SAFETY: `bytes` is valid for `len` bytes of UTF-8; a null allocator means the default.
    let cf_path =
        Owned(unsafe { CFStringCreateWithBytes(std::ptr::null(), bytes.as_ptr(), len, UTF8, 0) });
    if cf_path.0.is_null() {
        return None;
    }
    // SAFETY: `cf_path` is a valid CFString. MDItemCreate returns null for a path that does
    // not exist.
    let item = Owned(unsafe { MDItemCreate(std::ptr::null(), cf_path.0) });
    if item.0.is_null() {
        return None;
    }
    // SAFETY: `item` is a valid MDItem and kMDItemLastUsedDate a constant CFString.
    let value = Owned(unsafe { MDItemCopyAttribute(item.0, kMDItemLastUsedDate) });
    if value.0.is_null() {
        return None;
    }
    // SAFETY: `value` is a valid CF object; its type is checked before reading it as a date.
    let secs = unsafe {
        if CFGetTypeID(value.0) != CFDateGetTypeID() {
            return None;
        }
        CFDateGetAbsoluteTime(value.0) + CF_EPOCH_FROM_UNIX
    };
    cf_secs_to_time(secs)
}

/// Unix seconds (fractional) to a SystemTime. None before 1970 or when not finite.
fn cf_secs_to_time(secs: f64) -> Option<SystemTime> {
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::from_secs_f64(secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::FileTimesOnly;

    /// A seam that always answers, to check that [`resolve`] prefers it.
    struct Always(u64);

    impl LastUsed for Always {
        fn last_used(&self, _path: &Path) -> Option<SystemTime> {
            Some(UNIX_EPOCH + Duration::from_secs(self.0))
        }
    }

    #[test]
    fn resolve_prefers_the_seam_and_labels_the_source() {
        let p = Path::new("/nowhere/x.mov");
        assert_eq!(
            resolve(&Always(1_000), p, Some(5)),
            (Some(1_000), UseSource::Spotlight)
        );
        assert_eq!(
            resolve(&FileTimesOnly, p, Some(5)),
            (Some(5), UseSource::FileTimes)
        );
        assert_eq!(
            resolve(&FileTimesOnly, p, None),
            (None, UseSource::FileTimes)
        );
    }

    #[test]
    fn file_times_take_the_later_of_mtime_and_atime() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.bin");
        std::fs::write(&f, b"x").unwrap();
        let old = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let newer = UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        let file = std::fs::File::options().write(true).open(&f).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(old)
                .set_accessed(newer),
        )
        .unwrap();
        assert_eq!(file_time_secs(&f), Some(1_500_000_000));
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(newer)
                .set_accessed(old),
        )
        .unwrap();
        assert_eq!(file_time_secs(&f), Some(1_500_000_000));
        assert_eq!(file_time_secs(&tmp.path().join("missing")), None);
    }

    #[test]
    fn cf_dates_convert_from_their_epoch() {
        assert_eq!(cf_secs_to_time(0.0), Some(UNIX_EPOCH));
        assert_eq!(
            cf_secs_to_time(CF_EPOCH_FROM_UNIX).map(unix_secs),
            Some(978_307_200)
        );
        assert_eq!(cf_secs_to_time(-1.0), None);
        assert_eq!(cf_secs_to_time(f64::NAN), None);
    }

    #[test]
    fn spotlight_has_nothing_for_a_path_that_does_not_exist() {
        // Only a missing path inside a fresh temp folder: tests never ask Spotlight about real
        // user files.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("never-created.bin");
        assert_eq!(Spotlight.last_used(&missing), None);
    }
}
