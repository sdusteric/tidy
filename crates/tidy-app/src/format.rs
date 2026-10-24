//! Text for the screens: sizes, counts, relative dates, paths and button labels. Pure
//! functions, so every rule is unit-tested.
//!
//! Copy rules (design section 11): sentence case, plain words, sizes with one decimal in GB
//! ("6.3 GB", "640 MB"), dates as relative phrases ("8 months ago"), no exclamation marks.

use std::path::Path;

use tidy_core::model::{LargeKind, RunningSet, UseSource};

/// Finder-style size in powers of ten: "6.3 GB", "640 MB", "12 KB". One decimal below 100 GB
/// and below 10 MB, with a trailing ".0" left out; whole numbers elsewhere.
pub fn size(bytes: u64) -> String {
    const UNITS: [(&str, f64); 4] = [("TB", 1e12), ("GB", 1e9), ("MB", 1e6), ("KB", 1e3)];
    if bytes == 0 {
        return "0 KB".to_string();
    }
    if bytes < 1_000 {
        return if bytes == 1 {
            "1 byte".to_string()
        } else {
            format!("{bytes} bytes")
        };
    }
    let b = bytes as f64;
    let mut i = UNITS.iter().position(|(_, f)| b >= *f).unwrap_or(3);
    loop {
        let (name, factor) = UNITS[i];
        let v = b / factor;
        let places = match name {
            "TB" | "GB" if v < 100. => 1,
            "MB" if v < 10. => 1,
            _ => 0,
        };
        let text = format!("{v:.places$}");
        // A value that rounds up to 1000 belongs to the next unit ("999.9 MB" is "1 GB").
        if text.parse::<f64>().is_ok_and(|r| r >= 1000.) && i > 0 {
            i -= 1;
            continue;
        }
        let text = if places == 1 {
            text.strip_suffix(".0").unwrap_or(&text).to_string()
        } else {
            text
        };
        return format!("{text} {name}");
    }
}

/// 41200 becomes "41,200".
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "1 item", "14 items".
pub fn items(n: usize) -> String {
    if n == 1 {
        "1 item".to_string()
    } else {
        format!("{n} items")
    }
}

/// "Scanning, 41,200 files".
pub fn scanning(files: u64) -> String {
    format!("Scanning, {} files", thousands(files))
}

/// "2 min ago", "3 hours ago", "yesterday", "8 months ago". `then` after `now` reads as just
/// now.
pub fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    if secs < 45 {
        return "just now".to_string();
    }
    if secs < 3_600 {
        return format!("{} min ago", (secs / 60).max(1));
    }
    if secs < 86_400 {
        return plural(secs / 3_600, "hour");
    }
    let days = secs / 86_400;
    if days < 2 {
        "yesterday".to_string()
    } else if days < 30 {
        plural(days, "day")
    } else if days < 365 {
        plural(days / 30, "month")
    } else {
        plural(days / 365, "year")
    }
}

/// The date phrase on a Large files row: "Opened 8 months ago" (Spotlight) or "Modified 2 years
/// ago" (file times).
pub fn use_phrase(last_used: Option<u64>, source: UseSource, now: u64) -> String {
    match last_used {
        None => "No date".to_string(),
        Some(t) => {
            let verb = match source {
                UseSource::Spotlight => "Opened",
                UseSource::FileTimes => "Modified",
            };
            format!("{verb} {}", ago(now, t))
        }
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days since 1970-01-01 to (month 1 to 12, day of month), proleptic Gregorian calendar.
fn month_day(days: i64) -> (usize, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as usize;
    let _ = era;
    (m, d)
}

/// "Oct 23, 2:02 PM" for Unix seconds shifted by `offset_secs` (the zone's offset from UTC).
pub fn date_time_at(secs: u64, offset_secs: i64) -> String {
    let local = secs as i64 + offset_secs;
    let (m, d) = month_day(local.div_euclid(86_400));
    let rem = local.rem_euclid(86_400);
    let (h, min) = (rem / 3_600, (rem % 3_600) / 60);
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    let ampm = if h < 12 { "AM" } else { "PM" };
    format!("{} {d}, {h12}:{min:02} {ampm}", MONTHS[m - 1])
}

/// [`date_time_at`] in the Mac's time zone.
pub fn date_time(secs: u64) -> String {
    let t = secs as libc::time_t;
    // SAFETY: `tm` is a plain C struct that localtime_r fills in; `t` is a valid time_t.
    let offset = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff
        }
    };
    date_time_at(secs, offset)
}

/// "Scanned 2 min ago".
pub fn scanned(now: u64, at: u64) -> String {
    format!("Scanned {}", ago(now, at))
}

/// A path as the screens show it: under the scan root as `~/rel`, otherwise as it is.
pub fn display_path(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".to_string(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Shortens `s` to at most `max` characters by replacing the middle with "...", keeping the
/// start and the file name: `~/Movies/2025/.../trip.mov`.
pub fn middle_truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() >= 3 {
        let last = parts[parts.len() - 1];
        for keep in (1..parts.len() - 1).rev() {
            let candidate = format!("{}/.../{}", parts[..keep].join("/"), last);
            if candidate.chars().count() <= max {
                return candidate;
            }
        }
    }
    // One long name: keep both ends of it.
    let chars: Vec<char> = s.chars().collect();
    let room = max.saturating_sub(3);
    let head = room.div_ceil(2);
    let tail = room / 2;
    let mut out: String = chars[..head].iter().collect();
    out.push_str("...");
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// "212 GB available of 1 TB".
pub fn disk_line(available: u64, total: u64) -> String {
    format!("{} available of {}", size(available), size(total))
}

/// The label of the Cleanup primary button.
pub fn clean_label(
    selected_bytes: u64,
    any_review: bool,
    cleaning: Option<(usize, usize)>,
) -> String {
    match cleaning {
        Some((done, total)) => format!("Cleaning {done} of {total}"),
        None if any_review => "Clean selected".to_string(),
        None => format!("Clean {}", size(selected_bytes)),
    }
}

/// "12 selected, 6.3 GB".
pub fn selected_line(count: usize, bytes: u64) -> String {
    format!("{count} selected, {}", size(bytes))
}

/// The threshold menu label: "Over 500 MB".
pub fn over(min: u64) -> String {
    format!("Over {}", size(min))
}

/// "Quit Google Chrome first" gives "Google Chrome".
pub fn blocked_owner(reason: &str) -> Option<&str> {
    reason
        .strip_prefix("Quit ")
        .and_then(|r| r.strip_suffix(" first"))
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether an app called `name` ("Google Chrome") looks like one of the running bundle ids or
/// process names ("com.google.chrome"). An unknown process list counts as running. The core
/// re-checks right before every move, so a wrong "not running" here can only skip an item
/// later, never move one that is open.
pub fn owner_running(name: &str, running: &RunningSet) -> bool {
    if running.is_unknown() {
        return true;
    }
    let want = squash(name);
    if want.is_empty() {
        return false;
    }
    running.ids().any(|id| {
        let have = squash(id);
        have.len() >= 4 && (have.contains(&want) || want.contains(&have))
    })
}

/// The glyph a Large files row shows, by kind of item and name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Film,
    Archive,
    DiskImage,
    App,
    Folder,
    Document,
}

/// Picks the glyph from the item's kind and file name extension.
pub fn glyph_for(kind: LargeKind, name: &str) -> Glyph {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match kind {
        LargeKind::Folder => Glyph::Folder,
        LargeKind::Package if ext == "app" => Glyph::App,
        LargeKind::Package => Glyph::Folder,
        LargeKind::File => match ext.as_str() {
            "mov" | "mp4" | "m4v" | "mkv" | "avi" | "webm" | "mpg" | "mpeg" | "prores" => {
                Glyph::Film
            }
            "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" => Glyph::Archive,
            "dmg" | "iso" | "img" | "sparseimage" | "sparsebundle" | "pkg" => Glyph::DiskImage,
            "app" => Glyph::App,
            _ => Glyph::Document,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn sizes_follow_the_copy_rules() {
        assert_eq!(size(0), "0 KB");
        assert_eq!(size(1), "1 byte");
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(12_345), "12 KB");
        assert_eq!(size(4_194_304), "4.2 MB");
        assert_eq!(size(4_000_000), "4 MB");
        assert_eq!(size(640_000_000), "640 MB");
        assert_eq!(size(6_300_000_000), "6.3 GB");
        assert_eq!(size(34_000_000_000), "34 GB");
        assert_eq!(size(66_900_000_000), "66.9 GB");
        assert_eq!(size(214_000_000_000), "214 GB");
        assert_eq!(size(1_000_000_000_000), "1 TB");
        assert_eq!(size(2_100_000_000), "2.1 GB");
    }

    #[test]
    fn sizes_that_round_up_move_to_the_next_unit() {
        assert_eq!(size(999_700_000), "1 GB");
        assert_eq!(size(999_600), "1 MB");
        assert_eq!(size(999_999_999_999), "1 TB");
    }

    #[test]
    fn counts() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(41_200), "41,200");
        assert_eq!(thousands(1_204_000), "1,204,000");
        assert_eq!(items(1), "1 item");
        assert_eq!(items(14), "14 items");
        assert_eq!(scanning(41_200), "Scanning, 41,200 files");
    }

    #[test]
    fn relative_dates() {
        let now = 1_792_764_131;
        let d = 86_400;
        assert_eq!(ago(now, now), "just now");
        assert_eq!(ago(now, now + 500), "just now");
        assert_eq!(ago(now, now - 120), "2 min ago");
        assert_eq!(ago(now, now - 3_600), "1 hour ago");
        assert_eq!(ago(now, now - 2 * 3_600), "2 hours ago");
        assert_eq!(ago(now, now - d), "yesterday");
        assert_eq!(ago(now, now - 3 * d), "3 days ago");
        assert_eq!(ago(now, now - 200 * d), "6 months ago");
        assert_eq!(ago(now, now - 240 * d), "8 months ago");
        assert_eq!(ago(now, now - 800 * d), "2 years ago");
        assert_eq!(scanned(now, now - 120), "Scanned 2 min ago");
    }

    #[test]
    fn dates_with_a_zone_offset() {
        // 2026-10-23 14:02:11 UTC.
        let at = 1_792_764_131;
        assert_eq!(date_time_at(at, 0), "Oct 23, 2:02 PM");
        assert_eq!(date_time_at(at, -7 * 3_600), "Oct 23, 7:02 AM");
        assert_eq!(date_time_at(at, 10 * 3_600), "Oct 24, 12:02 AM");
        assert_eq!(date_time_at(0, 0), "Jan 1, 12:00 AM");
        assert_eq!(date_time_at(43_200, 0), "Jan 1, 12:00 PM");
        assert!(!date_time(at).is_empty());
    }

    #[test]
    fn use_phrases_name_their_source() {
        let now = 1_792_764_131;
        let t = now - 240 * 86_400;
        assert_eq!(
            use_phrase(Some(t), UseSource::Spotlight, now),
            "Opened 8 months ago"
        );
        assert_eq!(
            use_phrase(Some(t), UseSource::FileTimes, now),
            "Modified 8 months ago"
        );
        assert_eq!(use_phrase(None, UseSource::FileTimes, now), "No date");
    }

    #[test]
    fn paths_show_under_a_tilde() {
        let root = Path::new("/tmp/fake");
        assert_eq!(display_path(Path::new("/tmp/fake"), root), "~");
        assert_eq!(
            display_path(Path::new("/tmp/fake/Movies/a.mov"), root),
            "~/Movies/a.mov"
        );
        assert_eq!(
            display_path(Path::new("/elsewhere/x"), root),
            "/elsewhere/x"
        );
    }

    #[test]
    fn middle_truncation_keeps_the_start_and_the_name() {
        assert_eq!(middle_truncate("~/a/b.mov", 40), "~/a/b.mov");
        assert_eq!(
            middle_truncate("~/Movies/2025/trips/italy/trip.mov", 26),
            "~/Movies/2025/.../trip.mov"
        );
        let long = middle_truncate("~/Movies/2025/trips/italy/a-very-long-file-name.mov", 20);
        assert!(long.chars().count() <= 20, "{long}");
        assert!(long.contains("..."));
        let one = middle_truncate("abcdefghijklmnopqrstuvwxyz", 11);
        assert_eq!(one, "abcd...wxyz");
        assert!(one.chars().count() <= 11);
    }

    #[test]
    fn labels() {
        assert_eq!(
            disk_line(212_000_000_000, 1_000_000_000_000),
            "212 GB available of 1 TB"
        );
        assert_eq!(clean_label(6_300_000_000, false, None), "Clean 6.3 GB");
        assert_eq!(clean_label(6_300_000_000, true, None), "Clean selected");
        assert_eq!(clean_label(0, false, None), "Clean 0 KB");
        assert_eq!(clean_label(5, false, Some((4, 12))), "Cleaning 4 of 12");
        assert_eq!(selected_line(12, 6_300_000_000), "12 selected, 6.3 GB");
        assert_eq!(over(500_000_000), "Over 500 MB");
        assert_eq!(over(1_000_000_000), "Over 1 GB");
    }

    #[test]
    fn owner_names_match_running_ids() {
        assert_eq!(
            blocked_owner("Quit Google Chrome first"),
            Some("Google Chrome")
        );
        assert_eq!(blocked_owner("Cache"), None);
        let chrome = RunningSet::new(["com.google.Chrome"]);
        assert!(owner_running("Google Chrome", &chrome));
        assert!(!owner_running("Google Chrome", &RunningSet::default()));
        assert!(!owner_running("Slack", &chrome));
        assert!(owner_running("Slack", &RunningSet::unknown()));
        assert!(!owner_running("", &chrome));
        // A short process name must not match every app.
        assert!(!owner_running("Xcode", &RunningSet::new(["sh"])));
    }

    #[test]
    fn glyphs() {
        assert_eq!(glyph_for(LargeKind::File, "trip.MOV"), Glyph::Film);
        assert_eq!(glyph_for(LargeKind::File, "backup.zip"), Glyph::Archive);
        assert_eq!(
            glyph_for(LargeKind::File, "Installer.dmg"),
            Glyph::DiskImage
        );
        assert_eq!(glyph_for(LargeKind::File, "notes.txt"), Glyph::Document);
        assert_eq!(glyph_for(LargeKind::File, "noext"), Glyph::Document);
        assert_eq!(glyph_for(LargeKind::Package, "Big.app"), Glyph::App);
        assert_eq!(
            glyph_for(LargeKind::Package, "Lib.framework"),
            Glyph::Folder
        );
        assert_eq!(glyph_for(LargeKind::Folder, "Library"), Glyph::Folder);
    }

    #[test]
    fn copy_has_no_em_dashes() {
        for s in [
            scanning(5),
            clean_label(1, true, None),
            use_phrase(None, UseSource::FileTimes, 0),
        ] {
            assert!(!s.contains('\u{2014}'));
        }
    }
}
