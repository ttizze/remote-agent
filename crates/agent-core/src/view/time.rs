//! Clock, age and duration labels the views share.
use chrono::{DateTime, Datelike, NaiveDateTime, TimeZone, Timelike};

/// The clock preference for wall-clock labels. `Locale` follows the default
/// English locale (12-hour).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TimestampFormat {
    #[default]
    Locale,
    TwelveHour,
    TwentyFourHour,
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// "9:00 AM" or "18:00".
pub fn time_of_day<Tz: TimeZone>(date: &DateTime<Tz>, format: TimestampFormat) -> String {
    let (hour, minute) = (date.hour(), date.minute());
    match format {
        TimestampFormat::TwentyFourHour => format!("{hour:02}:{minute:02}"),
        TimestampFormat::Locale | TimestampFormat::TwelveHour => {
            let suffix = if hour < 12 { "AM" } else { "PM" };
            let hour = match hour % 12 {
                0 => 12,
                hour => hour,
            };
            format!("{hour}:{minute:02} {suffix}")
        }
    }
}

pub fn short_weekday<Tz: TimeZone>(date: &DateTime<Tz>) -> &'static str {
    WEEKDAYS[date.weekday().num_days_from_sunday() as usize]
}

/// "Apr 13".
pub fn short_month_day<Tz: TimeZone>(date: &DateTime<Tz>) -> String {
    format!("{} {}", MONTHS[date.month0() as usize], date.day())
}

/// A local wall-clock time, moved past a daylight-saving gap the way a
/// calendar does.
pub fn resolve_local<Tz: TimeZone>(zone: &Tz, local: NaiveDateTime) -> Option<DateTime<Tz>> {
    zone.from_local_datetime(&local).earliest().or_else(|| {
        zone.from_local_datetime(&(local + chrono::Duration::hours(1)))
            .earliest()
    })
}

/// Compact age of a timestamp for list rows: "<1m", "5m", "3h", "2d".
pub fn relative_time(timestamp_ms: i64, now_ms: i64) -> String {
    let seconds = (now_ms - timestamp_ms).max(0) / 1000;
    if seconds < 60 {
        return "<1m".into();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    format!("{}d", hours / 24)
}

/// Desktop age label: "just now", "5m ago", "3h ago", "2d ago".
pub fn relative_time_label(timestamp_ms: i64, now_ms: i64) -> String {
    let seconds = (now_ms - timestamp_ms) / 1000;
    if seconds < 60 {
        return "just now".into();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

/// The desktop row form of `relative_time_label`: "now", "5m", "3h", "2d".
pub fn compact_relative_time_label(timestamp_ms: i64, now_ms: i64) -> String {
    let label = relative_time_label(timestamp_ms, now_ms);
    match label.strip_suffix(" ago") {
        Some(age) => age.into(),
        None => "now".into(),
    }
}

/// Durations as `250ms`, `1.5s`, `22s`, `1m 5s` or `1h 1m 1s`.
pub fn format_duration(duration_ms: i64) -> String {
    if duration_ms < 0 {
        return "0ms".into();
    }
    if duration_ms < 1_000 {
        return format!("{}ms", duration_ms.max(1));
    }
    if duration_ms < 10_000 {
        let tenths = (duration_ms + 50) / 100;
        return if tenths >= 100 {
            "10s".into()
        } else {
            format!("{}.{}s", tenths / 10, tenths % 10)
        };
    }
    let total_seconds = (duration_ms + 500) / 1_000;
    if duration_ms < 60_000 {
        return format!("{total_seconds}s");
    }
    let hours = total_seconds / 3_600;
    let minutes = total_seconds % 3_600 / 60;
    let seconds = total_seconds % 60;
    [(hours, "h"), (minutes, "m"), (seconds, "s")]
        .iter()
        .filter(|(value, _)| *value > 0)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
pub(crate) mod fixtures {
    use chrono::{DateTime, Local, NaiveDate};

    /// A local wall-clock time on this machine, like `new Date(y, m - 1, d, h, min)`.
    pub fn local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        let naive = NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap();
        super::resolve_local(&Local, naive).unwrap()
    }

    /// Runs under the named zone; nextest gives each test its own process.
    pub fn with_zone(zone: &str) {
        // SAFETY: each test runs in its own process under nextest.
        unsafe { std::env::set_var("TZ", zone) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(-5, "0ms")]
    #[case(-1, "0ms")]
    #[case(0, "1ms")]
    #[case(250, "250ms")]
    #[case(850, "850ms")]
    #[case(1_050, "1.1s")]
    #[case(1_500, "1.5s")]
    #[case(9_950, "10s")]
    #[case(9_960, "10s")]
    #[case(12_400, "12s")]
    #[case(22_000, "22s")]
    #[case(60_000, "1m")]
    #[case(65_000, "1m 5s")]
    #[case(119_500, "2m")]
    #[case(3_599_499, "59m 59s")]
    #[case(3_599_500, "1h")]
    #[case(3_600_000, "1h")]
    #[case(3_601_000, "1h 1s")]
    #[case(3_660_000, "1h 1m")]
    #[case(3_661_000, "1h 1m 1s")]
    #[case(3_787_000, "1h 3m 7s")]
    #[case(7_199_500, "2h")]
    #[case(25_190_000, "6h 59m 50s")]
    #[case(90_061_000, "25h 1m 1s")]
    fn formats_durations(#[case] duration_ms: i64, #[case] expected: &str) {
        assert_eq!(format_duration(duration_ms), expected);
    }

    #[test]
    fn relative_time_never_counts_seconds() {
        let now = 10 * 86_400_000;
        assert_eq!(relative_time(now - 59_000, now), "<1m");
        assert_eq!(relative_time(now + 5_000, now), "<1m");
        assert_eq!(relative_time(now - 5 * 60_000, now), "5m");
        assert_eq!(relative_time(now - 3 * 3_600_000, now), "3h");
        assert_eq!(relative_time(now - 2 * 86_400_000, now), "2d");
    }

    #[test]
    fn desktop_age_labels_say_just_now_under_a_minute_and_drop_ago_in_rows() {
        let now = 10 * 86_400_000;
        assert_eq!(relative_time_label(now + 5_000, now), "just now");
        assert_eq!(relative_time_label(now - 59_000, now), "just now");
        assert_eq!(relative_time_label(now - 5 * 60_000, now), "5m ago");
        assert_eq!(relative_time_label(now - 3 * 3_600_000, now), "3h ago");
        assert_eq!(relative_time_label(now - 2 * 86_400_000, now), "2d ago");
        assert_eq!(compact_relative_time_label(now - 1_000, now), "now");
        assert_eq!(compact_relative_time_label(now - 5 * 60_000, now), "5m");
    }

    #[test]
    fn formats_times_with_the_clock_preference() {
        let date = chrono::Utc.with_ymd_and_hms(2026, 4, 8, 18, 5, 0).unwrap();
        assert_eq!(time_of_day(&date, TimestampFormat::TwelveHour), "6:05 PM");
        assert_eq!(time_of_day(&date, TimestampFormat::TwentyFourHour), "18:05");
        let midnight = chrono::Utc.with_ymd_and_hms(2026, 4, 8, 0, 0, 0).unwrap();
        assert_eq!(time_of_day(&midnight, TimestampFormat::Locale), "12:00 AM");
        assert_eq!(short_weekday(&date), "Wed");
        assert_eq!(short_month_day(&date), "Apr 8");
    }
}
