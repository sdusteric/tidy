//! macOS glue: the free-space figure for the sidebar and the App Nap activity token.
//!
//! Everything here is inert in unit tests, so tests never touch Foundation or the process
//! activity table. Call these only from the view's setup or from explicit user actions.

use std::path::Path;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builds_are_inert() {
        assert_eq!(disk_info(Path::new("/")), None);
        drop(ActivityToken::begin("test"));
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
