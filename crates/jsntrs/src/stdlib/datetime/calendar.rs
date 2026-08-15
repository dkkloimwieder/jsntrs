//! Proleptic-Gregorian calendar math — pure arithmetic, no external crate.
//!
//! Date↔epoch-day conversions use Howard Hinnant's `civil_from_days` /
//! `days_from_civil` algorithms (<https://howardhinnant.github.io/date_algorithms.html>),
//! which shift the year to start on March 1 so leap days land at the end of
//! the internal year. Their magic constants:
//!
//! - `719_468` — days between 0000-03-01 (the algorithm's epoch) and
//!   1970-01-01 (the Unix epoch).
//! - `146_097` — days per 400-year Gregorian era (400·365 + 97 leap days).
//! - `146_096` — last day index within an era, used to round negative day
//!   counts toward the correct era.
//! - `1460` / `36_524` — days per 4-year and per 100-year sub-cycle.
//! - `(153 * mp + 2) / 5` — day-of-year of March-based month `mp`: month
//!   lengths from March repeat the 5-month pattern 31,30,31,30,31 (153 days).

/// Convert calendar components to epoch milliseconds (UTC), checked.
///
/// Components may exceed their calendar ranges: excess months roll into
/// years and excess days/hours/minutes/seconds roll into larger units,
/// matching JS `Date.UTC` (and Go `time.Date`) normalization. Returns
/// `None` when the result cannot be represented in i64 milliseconds —
/// callers treat that as an unparseable timestamp.
pub(super) fn datetime_to_epoch_ms(
    y: i32,
    m: i64,
    d: i64,
    h: i64,
    mi: i64,
    s: i64,
    ms: i64,
) -> Option<i64> {
    let years_extra = (m - 1).div_euclid(12);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "rem_euclid(12) + 1 is always in 1..=12"
    )]
    let m_norm = ((m - 1).rem_euclid(12) + 1) as u8;
    let y = i32::try_from(i64::from(y).checked_add(years_extra)?).ok()?;
    let days = ymd_to_epoch_days(y, m_norm, 1).checked_add(d.checked_sub(1)?)?;
    days.checked_mul(86_400_000)?
        .checked_add(h.checked_mul(3_600_000)?)?
        .checked_add(mi.checked_mul(60_000)?)?
        .checked_add(s.checked_mul(1000)?)?
        .checked_add(ms)
}

/// Convert Unix seconds to (year, month, day, hour, minute, second).
pub(super) fn secs_to_ymd_hms(secs: i64) -> (i32, u8, u8, u8, u8, u8) {
    let (date_days, time_secs) = if secs >= 0 {
        (secs / 86400, secs % 86400)
    } else {
        let d = (secs + 1) / 86400 - 1;
        let t = secs - d * 86400;
        (d, t)
    };

    let h = (time_secs / 3600) as u8;
    let mi = ((time_secs % 3600) / 60) as u8;
    let s = (time_secs % 60) as u8;

    let (y, mo, d) = days_to_ymd(date_days);
    (y, mo, d, h, mi, s)
}

/// Convert days since Unix epoch to (year, month, day).
///
/// Hinnant's `civil_from_days` — see the module docs for the constants.
pub(super) fn days_to_ymd(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u8, d as u8)
}

pub(super) fn is_leap_year(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

pub(super) fn day_of_year(y: i32, mo: u8, d: u8) -> u32 {
    let months: &[u8] = &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut doy = u32::from(d);
    for (m, &days) in months[..mo as usize - 1].iter().enumerate() {
        doy += u32::from(days);
        if m == 1 && is_leap_year(y) {
            doy += 1;
        }
    }
    doy
}

/// Day of week: 0=Sunday, 1=Monday, ..., 6=Saturday.
///
/// Derived from epoch days: 1970-01-01 was a Thursday, hence the `+ 4`.
/// (The previous Sakamoto form used Rust's truncating `/`, which diverges
/// from floor division for years <= 0 and gave wrong pre-1CE weekdays.)
pub(super) fn day_of_week(y: i32, m: u8, d: u8) -> u8 {
    ((ymd_to_epoch_days(y, m, d) + 4).rem_euclid(7)) as u8
}

/// ISO week number: returns (iso_year, iso_week).
pub(super) fn iso_week(y: i32, m: u8, d: u8) -> (i32, u32) {
    // ISO 8601 week date: weeks start on Monday, week 1 contains the year's first Thursday.
    // Formula: week = (ordinalDay - isoDow + 10) / 7
    //   where isoDow: Mon=1..Sun=7
    let doy = day_of_year(y, m, d) as i32;
    let dow = i32::from(day_of_week(y, m, d)); // 0=Sun..6=Sat
    let dow_iso1 = (dow + 6) % 7 + 1; // Mon=1..Sun=7
    let week = (doy - dow_iso1 + 10) / 7;
    if week < 1 {
        // Belongs to last week of previous year.
        let prev_y = y - 1;
        return (prev_y, iso_weeks_in_year(prev_y));
    }
    let max_week = iso_weeks_in_year(y);
    if week > max_week as i32 {
        return (y + 1, 1);
    }
    (y, week as u32)
}

/// Returns the number of ISO weeks in a given year (52 or 53).
pub(super) fn iso_weeks_in_year(y: i32) -> u32 {
    // A year has 53 weeks if and only if Dec 31 is a Thursday,
    // or Dec 30 is a Thursday (which happens in leap years).
    // Equivalently: Jan 1 is Thursday, or Dec 31 is Thursday.
    let jan1_dow = day_of_week(y, 1, 1); // 0=Sun..6=Sat
    let dec31_dow = day_of_week(y, 12, 31);
    // Thursday = 4 in our system (0=Sun)
    if jan1_dow == 4 || dec31_dow == 4 {
        53
    } else {
        52
    }
}

/// Returns the Thursday of the ISO week for a given date.
pub(super) fn iso_week_thursday(y: i32, m: u8, d: u8) -> (i32, u8, u8) {
    // Thursday is dow_iso = 3 (Mon=0..Sun=6).
    let dow_iso = (i32::from(day_of_week(y, m, d)) + 6) % 7;
    let offset = 3 - dow_iso; // days to add to reach Thursday
    add_days(y, m, d, offset)
}

pub(super) fn add_days(y: i32, m: u8, d: u8, delta: i32) -> (i32, u8, u8) {
    let total_days = ymd_to_epoch_days(y, m, d) + i64::from(delta);
    let (ny, nm, nd) = days_to_ymd(total_days);
    (ny, nm, nd)
}

/// Convert (year, month, day) to days since Unix epoch.
///
/// Hinnant's `days_from_civil`, the inverse of [`days_to_ymd`].
pub(super) fn ymd_to_epoch_days(y: i32, m: u8, d: u8) -> i64 {
    let m = i32::from(m);
    let d = i32::from(d);
    let y = if m <= 2 {
        i64::from(y) - 1
    } else {
        i64::from(y)
    };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as u64 + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// The `[w]` component: which week of the month the week is, given the
/// **Thursday** of that week (only its day-of-month is needed, which is why
/// this takes one argument and not a whole date).
///
/// XPath 3.1 F&O §9.8.4.8 fixes the convention, since the calendar it
/// otherwise defers to does not:
///
/// > ISO 8601 does not define a numbering for weeks within a month. When the
/// > `w` component is used, the convention to be adopted is that each
/// > Monday-to-Sunday week is considered to fall within a particular month if
/// > its Thursday occurs in that month; the weeks that fall in a particular
/// > month under this definition are numbered starting from 1.
///
/// "The *n*th Thursday of the month" is exactly `ceil(day / 7)`, so the
/// caller resolves the week's Thursday with `iso_week_thursday` and this
/// counts it. The spec's own example checks out: 29 January 2013 has its
/// Thursday on the 31st, and `31.div_ceil(7)` is 5 —
///
/// > Thus, for example, 29 January 2013 falls in week 5 because the Thursday
/// > of the week (31 January 2013) is the fifth Thursday in January, and
/// > 1 February 2013 is also in week 5 for the same reason.
pub(super) fn week_of_month(thursday_day: u8) -> u32 {
    u32::from(thursday_day).div_ceil(7)
}

#[cfg(test)]
mod tests {
    use super::{day_of_week, iso_week_thursday, week_of_month};

    /// The worked example XPath 3.1 F&O §9.8.4.8 gives for the `[w]`
    /// component: "29 January 2013 falls in week 5 because the Thursday of
    /// the week (31 January 2013) is the fifth Thursday in January, and
    /// 1 February 2013 is also in week 5 for the same reason." The second
    /// half is the load-bearing one — 1 February is in a *different* month
    /// from its own week's Thursday, so a naive `day_of_month / 7` would
    /// answer 1 there.
    #[test]
    fn week_of_month_follows_the_thursday() {
        let wom = |y, m, d| {
            let (_thy, _thm, thd) = iso_week_thursday(y, m, d);
            week_of_month(thd)
        };
        assert_eq!(wom(2013, 1, 29), 5);
        assert_eq!(wom(2013, 2, 1), 5);
        // Boundaries of the ceil: the Thursdays of these weeks are 3, 7, 10
        // and 31 January 2013 — the 1st, 1st, 2nd and 5th Thursdays.
        assert_eq!(wom(2013, 1, 3), 1);
        assert_eq!(wom(2013, 1, 7), 2); // Mon 7 Jan; its Thursday is the 10th
        assert_eq!(wom(2013, 1, 6), 1); // Sun 6 Jan; still the 3 Jan week
        assert_eq!(wom(2013, 1, 31), 5);
    }

    #[test]
    fn day_of_week_known_dates() {
        // 0=Sunday .. 6=Saturday, proleptic Gregorian.
        assert_eq!(day_of_week(1970, 1, 1), 4); // Thursday (Unix epoch)
        assert_eq!(day_of_week(2000, 1, 1), 6); // Saturday
        assert_eq!(day_of_week(2024, 2, 29), 4); // Thursday (leap day)
        assert_eq!(day_of_week(1600, 1, 1), 6); // Saturday
        assert_eq!(day_of_week(1, 1, 1), 1); // Monday (1 CE)
    }

    #[test]
    fn day_of_week_pre_1ce() {
        // Year 0 is a leap year (366 days); 0000-01-01 is 366 days before
        // 0001-01-01 (Monday), so it falls on a Saturday. The truncating-
        // division Sakamoto form said Sunday here.
        assert_eq!(day_of_week(0, 1, 1), 6);
        assert_eq!(day_of_week(0, 12, 31), 0); // Sunday, day before 1 CE
        assert_eq!(day_of_week(-1, 12, 31), 5); // Friday, day before year 0
    }
}
