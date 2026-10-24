//! The weekly check up reminder (design section 5.5, C16) and the check up log it counts
//! from.
//!
//! The app keeps one non-repeating `UNCalendarNotificationTrigger` with the id
//! [`REMINDER_ID`]. It asks the core for [`next_reminder`] on each launch and after each
//! check up, and replaces the trigger with the date it gets back. So if the app is never
//! opened again, at most one reminder arrives. The date is 10:00 local time on the seventh
//! day after the last check up, counted in calendar days in the given time zone, so a
//! daylight saving change in between still lands on 10:00.
//!
//! The math is a pure function of the last check up, the current time and a time zone. Tests
//! pass a fixed zone; nothing here reads the system time zone except
//! [`next_reminder_system`].
//!
//! `checkups.jsonl` holds one [`Checkup`] per line, appended with `O_APPEND` and fsync like
//! the history file. A line that does not parse is skipped.

use std::io;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use crate::model::Checkup;
use crate::paths::AppPaths;
use crate::store::{append_json_line, read_json_lines};

/// The hour (local time) the reminder arrives.
pub const REMINDER_HOUR: i8 = 10;
/// Days from the last check up to the reminder.
pub const REMINDER_DAYS: i32 = 7;
/// The identifier of the single pending notification request. Scheduling again with the
/// same id replaces it.
pub const REMINDER_ID: &str = "dev.sdusteric.tidy.weekly-checkup";
/// The notification's text.
pub const REMINDER_BODY: &str = "Time for your weekly check up.";

/// When the next reminder fires: the instant, and the same instant as local date parts for
/// `NSDateComponents` (the calendar trigger matches on these).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderDate {
    /// Unix seconds.
    pub at: u64,
    pub year: i16,
    /// 1 to 12.
    pub month: i8,
    /// 1 to 31.
    pub day: i8,
    /// 0 to 23, local time. Always [`REMINDER_HOUR`] unless the zone skips that hour.
    pub hour: i8,
    pub minute: i8,
}

impl ReminderDate {
    fn from_zoned(z: &Zoned) -> ReminderDate {
        ReminderDate {
            at: u64::try_from(z.timestamp().as_second()).unwrap_or(0),
            year: z.year(),
            month: z.month(),
            day: z.day(),
            hour: z.hour(),
            minute: z.minute(),
        }
    }
}

/// The next reminder: 10:00 local time in `tz`, seven calendar days after the local date of
/// `last_checkup` (Unix seconds), or seven days after `now` when there has been no check up.
///
/// Never in the past: when that moment is not after `now` (the user skipped a week), the
/// reminder is the next 10:00 after `now` (today when it is still before 10:00, else
/// tomorrow).
pub fn next_reminder(last_checkup: Option<u64>, now: u64, tz: &TimeZone) -> ReminderDate {
    let base = last_checkup.unwrap_or(now);
    let due = local_date(base, tz)
        .checked_add(Span::new().days(REMINDER_DAYS))
        .ok()
        .and_then(|d| at_reminder_hour(d, tz));
    match due {
        Some(z) if secs(&z) > now => ReminderDate::from_zoned(&z),
        _ => next_reminder_hour_after(now, tz),
    }
}

/// [`next_reminder`] in the system's time zone.
pub fn next_reminder_system(last_checkup: Option<u64>, now: u64) -> ReminderDate {
    next_reminder(last_checkup, now, &TimeZone::system())
}

/// [`next_reminder`] for the last line of `checkups.jsonl`.
pub fn next_reminder_for(paths: &AppPaths, now: u64, tz: &TimeZone) -> ReminderDate {
    next_reminder(last_checkup(paths).map(|c| c.at), now, tz)
}

/// The first 10:00 local time strictly after `now`.
fn next_reminder_hour_after(now: u64, tz: &TimeZone) -> ReminderDate {
    let today = local_date(now, tz);
    let candidates = [Some(today), today.tomorrow().ok()];
    for day in candidates.into_iter().flatten() {
        if let Some(z) = at_reminder_hour(day, tz)
            && secs(&z) > now
        {
            return ReminderDate::from_zoned(&z);
        }
    }
    // Only reached at the end of the calendar: a day later is as good as it gets.
    let z = Timestamp::from_second(i64::try_from(now).unwrap_or(i64::MAX - 86_400) + 86_400)
        .unwrap_or(Timestamp::MAX)
        .to_zoned(tz.clone());
    ReminderDate::from_zoned(&z)
}

/// The local calendar date of Unix seconds `t` in `tz`.
fn local_date(t: u64, tz: &TimeZone) -> Date {
    let ts = Timestamp::from_second(i64::try_from(t).unwrap_or(i64::MAX)).unwrap_or(Timestamp::MAX);
    ts.to_zoned(tz.clone()).date()
}

/// 10:00 on `day` in `tz`. A 10:00 that a zone skips moves forward, one that repeats takes
/// the earlier one (jiff's "compatible" rule).
fn at_reminder_hour(day: Date, tz: &TimeZone) -> Option<Zoned> {
    day.at(REMINDER_HOUR, 0, 0, 0).to_zoned(tz.clone()).ok()
}

fn secs(z: &Zoned) -> u64 {
    u64::try_from(z.timestamp().as_second()).unwrap_or(0)
}

// ---------- checkups.jsonl ----------

/// Every check up, oldest first. A missing or unreadable file is empty (logged); lines that
/// do not parse are skipped.
pub fn read_checkups(paths: &AppPaths) -> Vec<Checkup> {
    let file = paths.checkups_file();
    read_json_lines(&file).unwrap_or_else(|e| {
        tracing::warn!("cannot read {}: {e}", file.display());
        Vec::new()
    })
}

/// The latest check up (the one with the largest `at`), if any.
pub fn last_checkup(paths: &AppPaths) -> Option<Checkup> {
    read_checkups(paths).into_iter().max_by_key(|c| c.at)
}

/// Appends one check up to `checkups.jsonl`.
pub fn append_checkup(paths: &AppPaths, checkup: &Checkup) -> io::Result<()> {
    append_json_line(&paths.checkups_file(), checkup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::TEST_NOW;
    use crate::model::{CheckupMemory, Pressure};

    /// America/Los_Angeles rules as a POSIX string: independent of the system's zone and of
    /// its zoneinfo files.
    fn la() -> TimeZone {
        TimeZone::posix("PST8PDT,M3.2.0,M11.1.0").unwrap()
    }

    /// Unix seconds of a UTC date and time.
    fn utc(y: i16, mo: i8, d: i8, h: i8, mi: i8) -> u64 {
        let z = jiff::civil::date(y, mo, d)
            .at(h, mi, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap();
        secs(&z)
    }

    /// Unix seconds of a local date and time in `tz`.
    fn local(tz: &TimeZone, y: i16, mo: i8, d: i8, h: i8, mi: i8) -> u64 {
        let z = jiff::civil::date(y, mo, d)
            .at(h, mi, 0, 0)
            .to_zoned(tz.clone())
            .unwrap();
        secs(&z)
    }

    fn parts(r: &ReminderDate) -> (i16, i8, i8, i8, i8) {
        (r.year, r.month, r.day, r.hour, r.minute)
    }

    #[test]
    fn seven_days_after_the_last_check_up_at_ten() {
        // TEST_NOW is 2026-10-23T14:02:11Z, 07:02 on Friday Oct 23 in Los Angeles (PDT).
        let r = next_reminder(Some(TEST_NOW), TEST_NOW, &la());
        assert_eq!(parts(&r), (2026, 10, 30, 10, 0));
        assert_eq!(r.at, utc(2026, 10, 30, 17, 0), "10:00 PDT is 17:00 UTC");
    }

    #[test]
    fn no_check_up_counts_from_now() {
        let r = next_reminder(None, TEST_NOW, &la());
        assert_eq!(parts(&r), (2026, 10, 30, 10, 0));
    }

    #[test]
    fn the_local_date_counts_not_the_utc_date() {
        let tz = la();
        // 23:30 on Oct 23 in Los Angeles is already Oct 24 in UTC.
        let last = local(&tz, 2026, 10, 23, 23, 30);
        assert_eq!(last, utc(2026, 10, 24, 6, 30));
        let r = next_reminder(Some(last), last, &tz);
        assert_eq!(parts(&r), (2026, 10, 30, 10, 0));
    }

    #[test]
    fn holds_ten_o_clock_across_the_autumn_change() {
        let tz = la();
        // Daylight time ends on Sunday 2026-11-01. A check up on the Wednesday before at
        // 20:00 PDT gives 10:00 PST the next Wednesday, which is 18:00 UTC, not 17:00.
        let last = local(&tz, 2026, 10, 28, 20, 0);
        let r = next_reminder(Some(last), last + 60, &tz);
        assert_eq!(parts(&r), (2026, 11, 4, 10, 0));
        assert_eq!(r.at, utc(2026, 11, 4, 18, 0));
        // 6 days and 14 hours of wall-clock time is one hour more in real seconds.
        assert_eq!(r.at - last, 6 * 86_400 + 14 * 3600 + 3600);
    }

    #[test]
    fn holds_ten_o_clock_across_the_spring_change() {
        let tz = la();
        // Daylight time starts on Sunday 2026-03-08.
        let last = local(&tz, 2026, 3, 5, 9, 0);
        assert_eq!(last, utc(2026, 3, 5, 17, 0), "09:00 PST");
        let r = next_reminder(Some(last), last, &tz);
        assert_eq!(parts(&r), (2026, 3, 12, 10, 0));
        assert_eq!(r.at, utc(2026, 3, 12, 17, 0), "10:00 PDT");
    }

    #[test]
    fn the_zone_database_agrees_with_the_posix_rules() {
        // The system's zoneinfo copy of America/Los_Angeles (a fixed zone, not the
        // system's own setting) gives the same dates.
        let Ok(db) = TimeZone::get("America/Los_Angeles") else {
            panic!("zoneinfo has no America/Los_Angeles");
        };
        for last in [
            TEST_NOW,
            local(&la(), 2026, 10, 28, 20, 0),
            local(&la(), 2026, 3, 5, 9, 0),
        ] {
            assert_eq!(
                next_reminder(Some(last), last, &db),
                next_reminder(Some(last), last, &la())
            );
        }
        // Another zone gives another instant: the zone passed in is the one used.
        let tokyo = TimeZone::fixed(jiff::tz::offset(9));
        let r = next_reminder(Some(TEST_NOW), TEST_NOW, &tokyo);
        // 23:02 on Oct 23 in Tokyo; Oct 30 at 10:00 there is 01:00 UTC.
        assert_eq!(parts(&r), (2026, 10, 30, 10, 0));
        assert_eq!(r.at, utc(2026, 10, 30, 1, 0));
    }

    #[test]
    fn a_new_check_up_and_a_launch_reschedule() {
        let tz = la();
        let first = next_reminder(Some(TEST_NOW), TEST_NOW, &tz);
        // A launch two days later, with no new check up, keeps the same date.
        let launch = TEST_NOW + 2 * 86_400;
        assert_eq!(next_reminder(Some(TEST_NOW), launch, &tz), first);
        // A check up on the Monday moves it to the next Monday.
        let monday = local(&tz, 2026, 10, 26, 15, 0);
        let r = next_reminder(Some(monday), monday, &tz);
        assert_eq!(parts(&r), (2026, 11, 2, 10, 0));
        assert_eq!(r.at, utc(2026, 11, 2, 18, 0), "PST by then");
        assert_ne!(r, first);
    }

    #[test]
    fn never_in_the_past() {
        let tz = la();
        let last = local(&tz, 2026, 9, 1, 12, 0);
        // A month later, before 10:00: today at 10:00.
        let now = local(&tz, 2026, 10, 23, 8, 0);
        let r = next_reminder(Some(last), now, &tz);
        assert_eq!(parts(&r), (2026, 10, 23, 10, 0));
        // After 10:00: tomorrow at 10:00.
        let now = local(&tz, 2026, 10, 23, 11, 0);
        assert_eq!(
            parts(&next_reminder(Some(last), now, &tz)),
            (2026, 10, 24, 10, 0)
        );
        // Exactly at 10:00 counts as past.
        let now = local(&tz, 2026, 10, 23, 10, 0);
        let r = next_reminder(Some(last), now, &tz);
        assert_eq!(parts(&r), (2026, 10, 24, 10, 0));
        assert!(r.at > now);
        // Exactly when the weekly reminder would fire: the next day.
        let last = local(&tz, 2026, 10, 16, 9, 0);
        let now = local(&tz, 2026, 10, 23, 10, 0);
        assert_eq!(
            parts(&next_reminder(Some(last), now, &tz)),
            (2026, 10, 24, 10, 0)
        );
        // Every result is after now, over a few months of hourly steps.
        let start = local(&tz, 2026, 10, 1, 0, 0);
        for h in 0..(24 * 120) {
            let now = start + h * 3600;
            let r = next_reminder(Some(start), now, &tz);
            assert!(r.at > now, "{h}");
            assert_eq!((r.hour, r.minute), (10, 0), "{h}");
        }
    }

    #[test]
    fn checkup_log_appends_and_reads_the_latest() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(tmp.path());
        paths.create_all().unwrap();
        assert!(read_checkups(&paths).is_empty());
        assert_eq!(last_checkup(&paths), None);
        let first = Checkup {
            at: TEST_NOW,
            ..Checkup::default()
        };
        let second = Checkup {
            at: TEST_NOW + 86_400,
            include_updates: true,
            memory: Some(CheckupMemory {
                pressure: Pressure::Warning,
                swap_used: 1,
                compressed_bytes: 2,
            }),
            ..Checkup::default()
        };
        append_checkup(&paths, &first).unwrap();
        append_checkup(&paths, &second).unwrap();
        // A corrupt line is skipped.
        crate::store::append_line(&paths.checkups_file(), "{ not json").unwrap();
        assert_eq!(read_checkups(&paths), [first, second.clone()]);
        assert_eq!(last_checkup(&paths), Some(second));
        let r = next_reminder_for(&paths, TEST_NOW + 86_400, &la());
        assert_eq!(parts(&r), (2026, 10, 31, 10, 0));
    }
}
