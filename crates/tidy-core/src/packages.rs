//! Package detection: folders that Finder shows as one file (`.app`, `.framework` and so on).
//!
//! Large files counts a package as one item with the total size of its contents and never
//! lists or drills into what is inside (design section 5.2). A folder is a package when its
//! extension is in [`PACKAGE_EXTENSIONS`], or, for any other extension, when the system type
//! for that extension conforms to `com.apple.package` (asked through UTType and cached per
//! extension).
//!
//! The UTType answer depends on the apps installed on this Mac, so the large scan asks it only
//! outside `TIDY_HOME`. Under `TIDY_HOME` the extension list alone decides, the same way
//! fixture dates decide last-use dates there (C22).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use objc2_foundation::NSString;
use objc2_uniform_type_identifiers::{UTType, UTTypeDirectory, UTTypePackage};

/// Extensions that always mark a folder as a package, compared ignoring ASCII case.
pub const PACKAGE_EXTENSIONS: [&str; 8] = [
    "app",
    "bundle",
    "framework",
    "xcodeproj",
    "xcworkspace",
    "plugin",
    "kext",
    "photoslibrary",
];

/// The extension of a folder name as Path sees it: the part after the last dot, never for a
/// name that only starts with a dot (`.git`).
fn extension(name: &OsStr) -> Option<&str> {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty())
}

/// Whether a folder called `name` has one of [`PACKAGE_EXTENSIONS`].
pub fn has_package_extension(name: &OsStr) -> bool {
    extension(name).is_some_and(|ext| {
        PACKAGE_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext))
    })
}

/// Whether the system type for the file name extension `ext` conforms to `com.apple.package`.
/// Answers are cached for the life of the process.
pub fn extension_conforms_to_package(ext: &str) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let key = ext.to_ascii_lowercase();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(&known) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return known;
    }
    let answer = objc2::rc::autoreleasepool(|_| {
        let ns = NSString::from_str(&key);
        // SAFETY: UTTypeDirectory and UTTypePackage are immutable constants exported by the
        // framework.
        let (directory, package): (&UTType, &UTType) = unsafe { (UTTypeDirectory, UTTypePackage) };
        // Ask for the folder type with this extension: a plain lookup prefers the flat-file
        // type ("app" gives com.apple.application-file, which is not a package).
        UTType::typeWithFilenameExtension_conformingToType(&ns, directory)
            .is_some_and(|t| t.conformsToType(package))
    });
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, answer);
    answer
}

/// Whether a folder called `name` is a package: a known extension, or, when `ask_system` is
/// true and the extension is not known, a system type that conforms to `com.apple.package`.
/// The caller has already checked that the entry is a folder.
pub fn is_package_name(name: &OsStr, ask_system: bool) -> bool {
    let Some(ext) = extension(name) else {
        return false;
    };
    if PACKAGE_EXTENSIONS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(ext))
    {
        return true;
    }
    // Purely numeric extensions are version folders ("1.2.3"), never packages.
    if !ask_system || ext.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    extension_conforms_to_package(ext)
}

/// Whether `path` is a folder (not through a symlink) that [`is_package_name`] accepts.
pub fn is_package_dir(path: &Path, ask_system: bool) -> bool {
    path.symlink_metadata().is_ok_and(|m| m.is_dir())
        && path
            .file_name()
            .is_some_and(|n| is_package_name(n, ask_system))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_extensions_ignore_case() {
        for name in [
            "Big.app",
            "X.BUNDLE",
            "Foo.framework",
            "P.xcodeproj",
            "W.xcworkspace",
            "a.plugin",
            "d.kext",
            "Old.photoslibrary",
        ] {
            assert!(has_package_extension(OsStr::new(name)), "{name}");
            assert!(is_package_name(OsStr::new(name), false), "{name}");
        }
        for name in ["app", ".app", "notes.txt", "Movies", "v1.2.3", "a."] {
            assert!(!is_package_name(OsStr::new(name), false), "{name}");
        }
    }

    #[test]
    fn the_system_type_decides_unknown_extensions() {
        // Rich text bundles and installer packages are declared by macOS itself.
        assert!(extension_conforms_to_package("rtfd"));
        assert!(extension_conforms_to_package("RTFD"));
        assert!(extension_conforms_to_package("pkg"));
        assert!(extension_conforms_to_package("app"));
        assert!(!extension_conforms_to_package("txt"));
        assert!(!extension_conforms_to_package("mov"));
        assert!(!extension_conforms_to_package("zz-not-a-type-zz"));
        assert!(is_package_name(OsStr::new("Notes.rtfd"), true));
        assert!(!is_package_name(OsStr::new("Notes.rtfd"), false));
        assert!(!is_package_name(OsStr::new("lib-1.2"), true));
    }

    #[test]
    fn only_real_folders_are_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("Big.app");
        std::fs::create_dir(&app).unwrap();
        std::fs::write(tmp.path().join("file.app"), b"x").unwrap();
        std::os::unix::fs::symlink(&app, tmp.path().join("Link.app")).unwrap();
        assert!(is_package_dir(&app, false));
        assert!(!is_package_dir(&tmp.path().join("file.app"), false));
        assert!(!is_package_dir(&tmp.path().join("Link.app"), false));
        assert!(!is_package_dir(&tmp.path().join("missing.app"), false));
    }
}
