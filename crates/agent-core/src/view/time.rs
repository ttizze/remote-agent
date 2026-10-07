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

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// "8/13", with the year once it differs from `now`'s: "8/13/2025".
fn numeric_date<Tz: TimeZone>(date: &DateTime<Tz>, now: &DateTime<Tz>) -> String {
    if date.year() == now.year() {
        format!("{}/{}", date.month(), date.day())
    } else {
        format!("{}/{}/{}", date.month(), date.day(), date.year())
    }
}

/// Calendar days from `from` to `to` in their zone.
fn calendar_days<Tz: TimeZone>(from: &DateTime<Tz>, to: &DateTime<Tz>) -> i64 {
    (to.date_naive() - from.date_naive()).num_days()
}

/// A chat time that adds the date once it is not from today: "12:34 PM",
/// "yesterday at 12:34 PM", "8/13 12:34 PM", "8/13/2025 12:34 PM".
pub fn day_aware_timestamp<Tz: TimeZone>(
    date: &DateTime<Tz>,
    now: &DateTime<Tz>,
    format: TimestampFormat,
) -> String {
    let time = time_of_day(date, format);
    match calendar_days(date, now) {
        ..=0 => time,
        1 => format!("yesterday at {time}"),
        _ => format!("{} {time}", numeric_date(date, now)),
    }
}

/// The forward-looking form for an instant still to come, such as a usage
/// limit reset: "12:34 PM", "tomorrow at 12:34 PM", "8/13 12:34 PM".
pub fn upcoming_timestamp<Tz: TimeZone>(
    date: &DateTime<Tz>,
    now: &DateTime<Tz>,
    format: TimestampFormat,
) -> String {
    let time = time_of_day(date, format);
    match calendar_days(now, date) {
        ..0 => day_aware_timestamp(date, now, format),
        0 => time,
        1 => format!("tomorrow at {time}"),
        _ => format!("{} {time}", numeric_date(date, now)),
    }
}

fn ordinal_suffix(day: u32) -> &'static str {
    match (day % 100, day % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    }
}

/// The tooltip of a chat time: "12:04 PM, 4th June 2026".
pub fn chat_timestamp_tooltip<Tz: TimeZone>(
    date: &DateTime<Tz>,
    format: TimestampFormat,
) -> String {
    let day = date.day();
    format!(
        "{}, {day}{} {} {}",
        time_of_day(date, format),
        ordinal_suffix(day),
        MONTH_NAMES[date.month0() as usize],
        date.year()
    )
}

/// A date and time as the default English locale writes it in full:
/// "10/7/2026, 3:04:05 PM".
pub fn locale_date_time<Tz: TimeZone>(date: &DateTime<Tz>) -> String {
    let hour = match date.hour() % 12 {
        0 => 12,
        hour => hour,
    };
    format!(
        "{}/{}/{}, {hour}:{:02}:{:02} {}",
        date.month(),
        date.day(),
        date.year(),
        date.minute(),
        date.second(),
        if date.hour() < 12 { "AM" } else { "PM" }
    )
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

/// "Working for …" time: whole seconds below a minute, then `format_duration`.
pub fn format_working_timer(started_at_ms: i64, now_ms: i64) -> String {
    let seconds = (now_ms - started_at_ms).max(0) / 1_000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    format_duration(seconds * 1_000)
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

    #[rstest]
    #[case(-1_000, "0s")]
    #[case(1_500, "1s")]
    #[case(59_999, "59s")]
    #[case(65_400, "1m 5s")]
    #[case(3_661_900, "1h 1m 1s")]
    fn working_timer_floors_to_whole_seconds(#[case] elapsed_ms: i64, #[case] expected: &str) {
        assert_eq!(format_working_timer(1_000_000, 1_000_000 + elapsed_ms), expected);
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

    #[test]
    fn chat_times_add_the_date_once_the_day_has_passed() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 14, 9, 0, 0).unwrap();
        let at = |m, d, h| chrono::Utc.with_ymd_and_hms(2026, m, d, h, 34, 0).unwrap();
        let format = TimestampFormat::TwelveHour;
        assert_eq!(
            day_aware_timestamp(&at(8, 14, 12), &now, format),
            "12:34 PM"
        );
        assert_eq!(
            day_aware_timestamp(&at(8, 13, 23), &now, format),
            "yesterday at 11:34 PM"
        );
        assert_eq!(
            day_aware_timestamp(&at(8, 12, 0), &now, format),
            "8/12 12:34 AM"
        );
        let last_year = chrono::Utc
            .with_ymd_and_hms(2025, 12, 31, 18, 5, 0)
            .unwrap();
        assert_eq!(
            day_aware_timestamp(&last_year, &now, TimestampFormat::TwentyFourHour),
            "12/31/2025 18:05"
        );
    }

    #[test]
    fn upcoming_times_name_tomorrow_and_fall_back_for_the_past() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 14, 9, 0, 0).unwrap();
        let at = |d, h| chrono::Utc.with_ymd_and_hms(2026, 8, d, h, 0, 0).unwrap();
        let format = TimestampFormat::TwelveHour;
        assert_eq!(upcoming_timestamp(&at(14, 17), &now, format), "5:00 PM");
        assert_eq!(
            upcoming_timestamp(&at(15, 1), &now, format),
            "tomorrow at 1:00 AM"
        );
        assert_eq!(upcoming_timestamp(&at(20, 1), &now, format), "8/20 1:00 AM");
        assert_eq!(
            upcoming_timestamp(&at(13, 1), &now, format),
            "yesterday at 1:00 AM"
        );
    }

    #[test]
    fn tooltips_spell_the_day_with_its_ordinal_and_month() {
        let at = |d| chrono::Utc.with_ymd_and_hms(2026, 6, d, 12, 4, 0).unwrap();
        let format = TimestampFormat::TwentyFourHour;
        assert_eq!(
            chat_timestamp_tooltip(&at(4), format),
            "12:04, 4th June 2026"
        );
        assert_eq!(
            chat_timestamp_tooltip(&at(1), format),
            "12:04, 1st June 2026"
        );
        assert_eq!(
            chat_timestamp_tooltip(&at(22), format),
            "12:04, 22nd June 2026"
        );
        assert_eq!(
            chat_timestamp_tooltip(&at(13), format),
            "12:04, 13th June 2026"
        );
        assert_eq!(
            chat_timestamp_tooltip(&at(23), format),
            "12:04, 23rd June 2026"
        );
    }

    #[test]
    fn locale_date_times_carry_seconds_and_the_meridiem() {
        let date = chrono::Utc.with_ymd_and_hms(2026, 10, 7, 15, 4, 5).unwrap();
        assert_eq!(locale_date_time(&date), "10/7/2026, 3:04:05 PM");
        let midnight = chrono::Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 9).unwrap();
        assert_eq!(locale_date_time(&midnight), "1/2/2026, 12:00:09 AM");
    }
}
