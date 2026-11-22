//! Presentation that needs the platform: dates in the local time zone.
//! The same rules as the GPUI shell's `util.rs`.

use chrono::{Datelike, Local, TimeZone};
use tl_app::settings::DateFormat;

/// A date in the file list, written the way the settings ask for.
pub fn date(mtime: Option<i64>, format: DateFormat) -> String {
    if format == DateFormat::Relative {
        return date_long(mtime);
    }
    let Some(secs) = mtime else {
        return String::new();
    };
    let pattern = match format {
        DateFormat::DayFirst => "%d.%m.%Y %H:%M",
        DateFormat::MonthFirst => "%m/%d/%Y %-I:%M %p",
        _ => "%Y-%m-%d %H:%M",
    };
    match Local.timestamp_opt(secs, 0).single() {
        Some(dt) => dt.format(pattern).to_string(),
        None => String::new(),
    }
}

/// The long form, for status lines and dialogs: `today 14:32`,
/// `Oct 3 09:12`, `2024-03-01 11:40`.
pub fn date_long(mtime: Option<i64>) -> String {
    let Some(secs) = mtime else {
        return String::new();
    };
    let Some(dt) = Local.timestamp_opt(secs, 0).single() else {
        return String::new();
    };
    let now = Local::now();
    if dt.date_naive() == now.date_naive() {
        dt.format("today %H:%M").to_string()
    } else if dt.year() == now.year() {
        dt.format("%b %-d %H:%M").to_string()
    } else {
        dt.format("%Y-%m-%d %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_follow_the_setting() {
        let when = Some(1_792_247_520); // 2026-10-15 14:32 UTC
        let iso = date(when, DateFormat::Iso);
        assert_eq!(iso.len(), 16, "{iso}");
        assert_eq!(&iso[..7], "2026-10");
        let day_first = date(when, DateFormat::DayFirst);
        assert_eq!(&day_first[5..10], ".2026");
        let month_first = date(when, DateFormat::MonthFirst);
        assert!(month_first.ends_with("AM") || month_first.ends_with("PM"), "{month_first}");
        assert_eq!(date(when, DateFormat::Relative), date_long(when));
        for format in DateFormat::ALL {
            assert_eq!(date(None, format), "");
        }
    }
}
