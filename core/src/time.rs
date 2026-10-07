//! Clocks, day attribution and display formatting.

use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, Local, NaiveDate, TimeDelta};

/// Source of time for the engine. Elapsed time comes only from `mono`; `now` is used for records.
pub trait Clock {
    /// Monotonic time since an arbitrary origin. On Linux `Instant` is `CLOCK_MONOTONIC`,
    /// which does not advance during suspend and ignores wall-clock changes.
    fn mono(&self) -> Duration;
    fn now(&self) -> DateTime<FixedOffset>;
}

pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        SystemClock { origin: Instant::now() }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn mono(&self) -> Duration {
        self.origin.elapsed()
    }

    fn now(&self) -> DateTime<FixedOffset> {
        Local::now().fixed_offset()
    }
}

/// The calendar day a moment belongs to, given the configured day-start hour.
pub fn local_date(t: DateTime<FixedOffset>, day_start_hour: u32) -> NaiveDate {
    (t.naive_local() - TimeDelta::hours(day_start_hour as i64)).date()
}

pub fn parse_ts(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s).ok()
}

pub fn fmt_ts(t: DateTime<FixedOffset>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, false)
}

/// Rounds to one decimal place, half up, tolerating binary floating-point error.
pub fn round1(x: f64) -> f64 {
    (x * 10.0 + 0.5 + 1e-9).floor() / 10.0
}

/// `1.1`, `4`, `0.3`: one decimal, trailing `.0` dropped.
pub fn fmt_decimal(x: f64) -> String {
    let r = round1(x);
    if (r - r.round()).abs() < 1e-9 {
        format!("{}", r.round() as i64)
    } else {
        format!("{r:.1}")
    }
}

/// `3h 28m`, `25m`, `0m`.
pub fn fmt_focus(sec: f64) -> String {
    let mins = (sec / 60.0).floor() as u64;
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// `18:42` (or `1:05:00` for phases over an hour).
pub fn fmt_clock(sec: f64) -> String {
    let s = sec.max(0.0).ceil() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// `2.5 min`, `12 min`.
pub fn fmt_minutes(sec: f64) -> String {
    format!("{} min", fmt_decimal(sec / 60.0))
}

/// Human hint for how long ago a session started, or `None` if it was today-ish.
pub fn started_ago(started: DateTime<FixedOffset>, now: DateTime<FixedOffset>) -> Option<String> {
    let hours = (now - started).num_hours();
    match hours {
        h if h < 12 => None,
        h if h < 24 => Some(format!("Started {h} hours ago")),
        h if h < 48 => Some("Started yesterday".to_owned()),
        h => Some(format!("Started {} days ago", h / 24)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_half_up() {
        assert_eq!(fmt_decimal(0.275), "0.3");
        assert_eq!(fmt_decimal(0.875), "0.9");
        assert_eq!(fmt_decimal(0.15), "0.2");
        assert_eq!(fmt_decimal(1.1), "1.1");
        assert_eq!(fmt_decimal(4.0), "4");
        assert_eq!(fmt_decimal(0.04), "0");
    }

    #[test]
    fn day_start_hour_shifts_date() {
        let t = DateTime::parse_from_rfc3339("2026-10-07T03:30:00+02:00").unwrap();
        assert_eq!(local_date(t, 0), NaiveDate::from_ymd_opt(2026, 10, 7).unwrap());
        assert_eq!(local_date(t, 4), NaiveDate::from_ymd_opt(2026, 10, 6).unwrap());
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_focus(3.0 * 3600.0 + 28.0 * 60.0 + 30.0), "3h 28m");
        assert_eq!(fmt_clock(18.0 * 60.0 + 41.2), "18:42");
        assert_eq!(fmt_minutes(150.0), "2.5 min");
    }
}
