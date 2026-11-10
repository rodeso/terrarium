//! Calendar dates without time zones ("2026-11-14"), shared by the server
//! and the browser. Events are stored as plain local dates and times, like a
//! paper diary, so they never shift when the time zone changes.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Date {
    pub year: i32,
    pub month: u32, // 1..=12
    pub day: u32,   // 1..=31
}

pub const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October",
    "November", "December",
];
/// Monday first.
pub const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

impl Date {
    pub fn new(year: i32, month: u32, day: u32) -> Option<Self> {
        ((1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month)).then_some(Self { year, month, day })
    }

    /// Parses "YYYY-MM-DD".
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.trim().splitn(3, '-');
        let year = parts.next()?.parse().ok()?;
        let month = parts.next()?.parse().ok()?;
        let day = parts.next()?.parse().ok()?;
        Self::new(year, month, day)
    }

    /// Days since 1970-01-01 (negative before).
    pub fn to_days(self) -> i64 {
        // Howard Hinnant's days_from_civil.
        let y = self.year as i64 - (self.month <= 2) as i64;
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let m = self.month as i64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + self.day as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    pub fn from_days(days: i64) -> Self {
        // Howard Hinnant's civil_from_days.
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = (yoe + era * 400 + (month <= 2) as i64) as i32;
        Self { year, month, day }
    }

    pub fn add_days(self, n: i64) -> Self {
        Self::from_days(self.to_days() + n)
    }

    /// 0 = Monday … 6 = Sunday.
    pub fn weekday(self) -> usize {
        // 1970-01-01 was a Thursday (index 3).
        (self.to_days() + 3).rem_euclid(7) as usize
    }

    pub fn first_of_month(self) -> Self {
        Self { day: 1, ..self }
    }

    /// Same day in another month, clamped (31 Jan + 1 month = 28/29 Feb).
    pub fn add_months(self, n: i32) -> Self {
        let total = self.year * 12 + self.month as i32 - 1 + n;
        let (year, month) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
        Self { year, month, day: self.day.min(days_in_month(year, month)) }
    }

    /// The 42 days (6 weeks, Monday first) shown in a month grid.
    pub fn month_grid(self) -> Vec<Date> {
        let first = self.first_of_month();
        let start = first.add_days(-(first.weekday() as i64));
        (0..42).map(|i| start.add_days(i)).collect()
    }

    /// "Thursday 14 November 2026"
    pub fn long(self) -> String {
        format!("{} {} {} {}", WEEKDAYS[self.weekday()], self.day, MONTHS[self.month as usize - 1], self.year)
    }

    /// "November 2026"
    pub fn month_title(self) -> String {
        format!("{} {}", MONTHS[self.month as usize - 1], self.year)
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Normalizes "9:5", "09:05", "9h05", "9.05" or "9" to "09:05" / "09:00".
pub fn parse_time(s: &str) -> Option<String> {
    let s = s.trim();
    let (h, m) = match s.find([':', 'h', 'H', '.']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, "0"),
    };
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = if m.trim().is_empty() { 0 } else { m.trim().parse().ok()? };
    (h < 24 && m < 60).then(|| format!("{h:02}:{m:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_weekdays() {
        let d = Date::parse("2026-11-14").unwrap();
        assert_eq!(d.to_string(), "2026-11-14");
        assert_eq!(WEEKDAYS[d.weekday()], "Saturday");
        assert_eq!(Date::parse("1970-01-01").unwrap().to_days(), 0);
        assert_eq!(WEEKDAYS[Date::parse("2000-02-29").unwrap().weekday()], "Tuesday");
        for days in [-800_000, -1, 0, 1, 19_000, 20_402, 800_000] {
            assert_eq!(Date::from_days(days).to_days(), days);
        }
        assert_eq!(Date::parse("2026-12-31").unwrap().add_days(1).to_string(), "2027-01-01");
    }

    #[test]
    fn validation() {
        assert!(Date::parse("2026-02-29").is_none());
        assert!(Date::parse("2024-02-29").is_some());
        assert!(Date::parse("2026-13-01").is_none());
        assert!(Date::parse("14/11/2026").is_none());
    }

    #[test]
    fn months_and_grid() {
        let jan31 = Date::parse("2026-01-31").unwrap();
        assert_eq!(jan31.add_months(1).to_string(), "2026-02-28");
        assert_eq!(jan31.add_months(-1).to_string(), "2025-12-31");
        let grid = Date::parse("2026-11-10").unwrap().month_grid();
        assert_eq!(grid.len(), 42);
        assert_eq!(grid[0].to_string(), "2026-10-26"); // the Monday before 1 Nov (a Sunday)
        assert_eq!(grid[6].to_string(), "2026-11-01");
        assert_eq!(Date::parse("2026-11-14").unwrap().long(), "Saturday 14 November 2026");
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("9:5").as_deref(), Some("09:05"));
        assert_eq!(parse_time("15h30").as_deref(), Some("15:30"));
        assert_eq!(parse_time("9").as_deref(), Some("09:00"));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("noon"), None);
    }
}
