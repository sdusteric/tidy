//! Chrome extensions from each profile's `Secure Preferences`.
//!
//! Chrome may be writing the file while Tidy reads it, so each one is copied to a temp file
//! first and the copy is parsed (then removed). Extensions Chrome ships itself are left out:
//! component extensions (location 5 and 10) and ones installed by default or by the Mac's
//! maker. Names come from the manifest; a `__MSG_name__` placeholder is looked up in the
//! extension's `_locales` folder, falling back to the id.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::model::ChromeExtension;

/// The file in each profile folder that lists the extensions.
pub const SECURE_PREFERENCES: &str = "Secure Preferences";
/// The largest `Secure Preferences` read. Real ones are well under 1 MB.
const MAX_PREFS_BYTES: u64 = 32 * 1024 * 1024;
/// Install locations Chrome uses for its own extensions: `COMPONENT` and
/// `EXTERNAL_COMPONENT`.
const COMPONENT_LOCATIONS: [i64; 2] = [5, 10];

/// Chrome's data folder under the home folder `home`.
pub fn chrome_root(home: &Path) -> PathBuf {
    home.join("Library/Application Support/Google/Chrome")
}

/// Profile folders under `root` that have a `Secure Preferences` file, by folder name.
/// `Ok(None)` when Chrome's folder does not exist.
pub fn profiles(root: &Path) -> io::Result<Option<Vec<(String, PathBuf)>>> {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let skip = name.starts_with('.') || name == "System Profile" || name == "Guest Profile";
            (!skip && e.path().join(SECURE_PREFERENCES).is_file()).then(|| (name, e.path()))
        })
        .collect();
    out.sort();
    Ok(Some(out))
}

/// A temp file path that no other read uses.
fn temp_copy_path() -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("tidy-chrome-prefs-{}-{n}.json", std::process::id()))
}

/// Copies `file` to a temp file, reads the copy and removes it.
fn read_copy(file: &Path) -> io::Result<Vec<u8>> {
    if std::fs::metadata(file)?.len() > MAX_PREFS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Secure Preferences is too large",
        ));
    }
    let tmp = temp_copy_path();
    let result = std::fs::copy(file, &tmp).and_then(|_| std::fs::read(&tmp));
    let _ = std::fs::remove_file(&tmp);
    result
}

/// The extensions of one profile folder (named `profile`).
pub fn read_profile(dir: &Path, profile: &str) -> io::Result<Vec<ChromeExtension>> {
    let bytes = read_copy(&dir.join(SECURE_PREFERENCES))?;
    parse_secure_preferences(&bytes, profile, Some(&dir.join("Extensions")))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// What reading every profile gave.
#[derive(Debug, Default)]
pub struct ChromeRead {
    /// False when Chrome's folder does not exist.
    pub installed: bool,
    /// Profiles read without an error.
    pub profiles: usize,
    /// Sorted by profile, then name.
    pub extensions: Vec<ChromeExtension>,
    /// Profiles that could not be read, with why.
    pub errors: Vec<(String, io::Error)>,
}

/// Reads every profile under `root` (see [`chrome_root`]).
pub fn read_all(root: &Path) -> ChromeRead {
    let mut out = ChromeRead::default();
    let list = match profiles(root) {
        Ok(Some(list)) => list,
        Ok(None) => return out,
        Err(e) => {
            out.installed = true;
            out.errors.push((String::new(), e));
            return out;
        }
    };
    out.installed = true;
    for (name, dir) in list {
        match read_profile(&dir, &name) {
            Ok(exts) => {
                out.profiles += 1;
                out.extensions.extend(exts);
            }
            Err(e) => {
                tracing::debug!("chrome: cannot read profile {name}: {e}");
                out.errors.push((name, e));
            }
        }
    }
    out.extensions.sort_by(|a, b| {
        (a.profile.as_str(), a.name.to_lowercase(), a.id.as_str()).cmp(&(
            b.profile.as_str(),
            b.name.to_lowercase(),
            b.id.as_str(),
        ))
    });
    out
}

/// Parses the `extensions.settings` object of a `Secure Preferences` file. `extensions_dir`
/// is the profile's `Extensions` folder, used to resolve `__MSG_name__` names.
pub fn parse_secure_preferences(
    json: &[u8],
    profile: &str,
    extensions_dir: Option<&Path>,
) -> Result<Vec<ChromeExtension>, String> {
    let root: Value = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    let Some(settings) = root
        .get("extensions")
        .and_then(|e| e.get("settings"))
        .and_then(Value::as_object)
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (id, s) in settings {
        let Some(manifest) = s.get("manifest").filter(|m| m.is_object()) else {
            // A settings stub without a manifest is not an installed extension.
            continue;
        };
        let location = s.get("location").and_then(Value::as_i64).unwrap_or(0);
        let flag = |k: &str| s.get(k).and_then(Value::as_bool).unwrap_or(false);
        if COMPONENT_LOCATIONS.contains(&location)
            || flag("was_installed_by_default")
            || flag("was_installed_by_oem")
        {
            continue;
        }
        let version = manifest
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string);
        let raw_name = manifest.get("name").and_then(Value::as_str).unwrap_or("");
        let name = resolve_name(raw_name, manifest, id, version.as_deref(), extensions_dir)
            .unwrap_or_else(|| id.clone());
        out.push(ChromeExtension {
            id: id.clone(),
            name,
            version,
            profile: profile.to_string(),
            enabled: is_enabled(s),
            mark: None,
        });
    }
    out.sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
    Ok(out)
}

/// Whether Chrome has the extension turned on: no `disable_reasons` (a list in current
/// Chrome, a bit field in older versions), and `state` not 0 in older versions.
fn is_enabled(s: &Value) -> bool {
    let reasons = match s.get("disable_reasons") {
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    };
    let state_off = s.get("state").and_then(Value::as_i64) == Some(0);
    !reasons && !state_off
}

/// The display name: the manifest name, or the localized message it points at.
fn resolve_name(
    raw: &str,
    manifest: &Value,
    id: &str,
    version: Option<&str>,
    extensions_dir: Option<&Path>,
) -> Option<String> {
    let raw = raw.trim();
    let Some(key) = raw
        .strip_prefix("__MSG_")
        .and_then(|k| k.strip_suffix("__"))
    else {
        return (!raw.is_empty()).then(|| raw.to_string());
    };
    let dir = extensions_dir?.join(id);
    let version_dir = match version {
        Some(v) if dir.join(format!("{v}_0")).is_dir() => dir.join(format!("{v}_0")),
        Some(v) if dir.join(v).is_dir() => dir.join(v),
        _ => first_subdir(&dir)?,
    };
    let default_locale = manifest
        .get("default_locale")
        .and_then(Value::as_str)
        .unwrap_or("en");
    for locale in [default_locale, "en", "en_US"] {
        let file = version_dir
            .join("_locales")
            .join(locale)
            .join("messages.json");
        if let Some(msg) = message(&file, key) {
            return Some(msg);
        }
    }
    None
}

fn first_subdir(dir: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.pop()
}

/// `key`'s `message` in a `messages.json`, matching the key ignoring case as Chrome does.
fn message(file: &Path, key: &str) -> Option<String> {
    let bytes = std::fs::read(file).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let obj = v.as_object()?;
    obj.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, m)| m.get("message"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_give_no_extensions() {
        assert_eq!(
            parse_secure_preferences(b"{}", "Default", None).unwrap(),
            vec![]
        );
        assert!(parse_secure_preferences(b"not json", "Default", None).is_err());
    }

    #[test]
    fn a_missing_chrome_folder_is_not_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let read = read_all(&tmp.path().join("Chrome"));
        assert!(!read.installed);
        assert!(read.errors.is_empty());
    }

    #[test]
    fn the_copy_is_parsed_and_then_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("Default");
        std::fs::create_dir_all(&profile).unwrap();
        let prefs = profile.join(SECURE_PREFERENCES);
        let json = br#"{"extensions":{"settings":{"cjpalhdlnbpafiamejdnhcphjbkeiagm":{"location":1,"manifest":{"name":"One","version":"1"}}}}}"#;
        std::fs::write(&prefs, json).unwrap();
        let exts = read_profile(&profile, "Default").unwrap();
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].name, "One");
        // The original is untouched and no copy is left behind.
        assert_eq!(std::fs::read(&prefs).unwrap(), json);
        let prefix = format!("tidy-chrome-prefs-{}-", std::process::id());
        let left: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn disable_reasons_in_both_forms() {
        let on: Value = serde_json::json!({"disable_reasons": []});
        let off_list: Value = serde_json::json!({"disable_reasons": [1]});
        let off_bits: Value = serde_json::json!({"disable_reasons": 1});
        let off_state: Value = serde_json::json!({"state": 0});
        assert!(is_enabled(&on));
        assert!(!is_enabled(&off_list));
        assert!(!is_enabled(&off_bits));
        assert!(!is_enabled(&off_state));
    }
}
