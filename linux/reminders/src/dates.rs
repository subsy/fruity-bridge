//! Due-date parsing, time-zone conversion and the short labels Apple's Reminders uses
//! ("Today 19:30", "Tomorrow").

use fruity_bridge_client::reminders::Due;
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use jiff::tz::TimeZone;

pub fn now() -> NaiveDateTime {
    Local::now().naive_local()
}

pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

pub fn parse_time(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M").ok()
}

/// The local calendar day of an RFC 3339 timestamp.
pub fn local_day(ts: &str) -> Option<NaiveDate> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.with_timezone(&Local).date_naive())
}

/// A due date in this computer's time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalDue {
    pub day: NaiveDate,
    /// `None` for an all-day due date.
    pub time: Option<NaiveTime>,
}

/// `due` in the local time zone. A timed due date is wall-clock time in its `timeZone`; one
/// without a zone (or with a zone this computer doesn't know) is taken as local.
pub fn local_due(due: &Due) -> Option<LocalDue> {
    local_due_in(due, &TimeZone::system())
}

fn local_due_in(due: &Due, local: &TimeZone) -> Option<LocalDue> {
    let day = parse_date(&due.date)?;
    let Some(time) = due.time.as_deref().and_then(parse_time) else {
        return Some(LocalDue { day, time: None });
    };
    let unconverted = LocalDue {
        day,
        time: Some(time),
    };
    let Some(zone) = due.time_zone.as_deref().and_then(|z| TimeZone::get(z).ok()) else {
        return Some(unconverted);
    };
    let convert = || -> Option<LocalDue> {
        let civil = jiff::civil::DateTime::new(
            i16::try_from(day.year()).ok()?,
            day.month() as i8,
            day.day() as i8,
            time.hour() as i8,
            time.minute() as i8,
            0,
            0,
        )
        .ok()?;
        let here = civil.to_zoned(zone).ok()?.with_time_zone(local.clone());
        let dt = here.datetime();
        Some(LocalDue {
            day: NaiveDate::from_ymd_opt(dt.year().into(), dt.month() as u32, dt.day() as u32)?,
            time: NaiveTime::from_hms_opt(dt.hour() as u32, dt.minute() as u32, 0),
        })
    };
    Some(convert().unwrap_or(unconverted))
}

/// This computer's IANA time zone (e.g. "Europe/London"), sent with timed due dates.
pub fn local_zone_name() -> Option<String> {
    TimeZone::system().iana_name().map(str::to_string)
}

/// The local day a reminder is due.
pub fn due_date(due: &Due) -> Option<NaiveDate> {
    local_due(due).map(|d| d.day)
}

/// Past its date, or past its time when it has one.
pub fn is_overdue(due: &Due, now: NaiveDateTime) -> bool {
    local_due(due).is_some_and(|d| is_overdue_local(d, now))
}

fn is_overdue_local(due: LocalDue, now: NaiveDateTime) -> bool {
    match due.time {
        Some(t) => due.day.and_time(t) < now,
        None => due.day < now.date(),
    }
}

/// Short day name: "Today", "Tomorrow", "Yesterday", a weekday within the coming week, else a date.
pub fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    match (day - today).num_days() {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        2..=6 => day.format("%A").to_string(),
        _ if day.year() == today.year() => day.format("%-d %b").to_string(),
        _ => day.format("%-d %b %Y").to_string(),
    }
}

/// Label for a due chip in local time, e.g. "Today 19:30" or "Fri 14 Nov".
pub fn due_label(due: &Due, today: NaiveDate) -> String {
    match local_due(due) {
        Some(d) => local_label(d, today),
        None => due.date.clone(),
    }
}

fn local_label(due: LocalDue, today: NaiveDate) -> String {
    let day = day_label(due.day, today);
    match due.time {
        Some(t) => format!("{day} {}", t.format("%H:%M")),
        None => day,
    }
}

/// Section heading in the Scheduled view: "Today", "Tomorrow", "Friday", "Monday 24 November".
pub fn section_label(day: NaiveDate, today: NaiveDate) -> String {
    match (day - today).num_days() {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        2..=6 => day.format("%A").to_string(),
        _ if day.year() == today.year() => day.format("%A %-d %B").to_string(),
        _ => day.format("%A %-d %B %Y").to_string(),
    }
}

/// Long form for the editor: "Friday 14 November 2026".
pub fn long_date(day: NaiveDate) -> String {
    day.format("%A %-d %B %Y").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn due(date: &str, time: Option<&str>) -> Due {
        Due {
            date: date.into(),
            time: time.map(Into::into),
            time_zone: None,
        }
    }

    #[test]
    fn labels() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        assert_eq!(
            due_label(&due("2026-11-13", Some("19:30")), today),
            "Today 19:30"
        );
        assert_eq!(due_label(&due("2026-11-14", None), today), "Tomorrow");
        assert_eq!(due_label(&due("2026-11-12", None), today), "Yesterday");
        assert_eq!(due_label(&due("2026-11-17", None), today), "Tuesday");
        assert_eq!(due_label(&due("2026-12-25", None), today), "25 Dec");
        assert_eq!(due_label(&due("2027-01-02", None), today), "2 Jan 2027");
    }

    fn zoned(date: &str, time: &str, zone: &str) -> Due {
        Due {
            date: date.into(),
            time: Some(time.into()),
            time_zone: Some(zone.into()),
        }
    }

    fn at(day: (i32, u32, u32), time: (u32, u32)) -> LocalDue {
        LocalDue {
            day: NaiveDate::from_ymd_opt(day.0, day.1, day.2).unwrap(),
            time: NaiveTime::from_hms_opt(time.0, time.1, 0),
        }
    }

    #[test]
    fn converts_to_local_zone() {
        let london = TimeZone::get("Europe/London").unwrap();
        let la = TimeZone::get("America/Los_Angeles").unwrap();
        let ny = zoned("2026-11-14", "09:00", "America/New_York");
        assert_eq!(local_due_in(&ny, &london), Some(at((2026, 11, 14), (14, 0))));
        // Crossing midnight moves the day.
        let early = zoned("2026-11-14", "01:00", "Europe/London");
        assert_eq!(local_due_in(&early, &la), Some(at((2026, 11, 13), (17, 0))));
        // No zone, or an unknown one: taken as local wall-clock time.
        assert_eq!(
            local_due_in(&due("2026-11-14", Some("09:00")), &la),
            Some(at((2026, 11, 14), (9, 0)))
        );
        let odd = zoned("2026-11-14", "09:00", "Mars/Olympus");
        assert_eq!(local_due_in(&odd, &la), Some(at((2026, 11, 14), (9, 0))));
        // All-day dates aren't shifted.
        assert_eq!(
            local_due_in(&due("2026-11-14", None), &la).map(|d| d.time),
            Some(None)
        );
    }

    #[test]
    fn overdue_in_local_time() {
        let london = TimeZone::get("Europe/London").unwrap();
        // 09:00 in New York is 14:00 in London: not yet overdue at 12:00 London time.
        let ny = local_due_in(&zoned("2026-11-13", "09:00", "America/New_York"), &london).unwrap();
        let noon = NaiveDate::from_ymd_opt(2026, 11, 13)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        assert!(!is_overdue_local(ny, noon));
        assert!(is_overdue_local(ny, noon + chrono::Duration::hours(3)));
    }

    #[test]
    fn overdue() {
        let now = NaiveDate::from_ymd_opt(2026, 11, 13)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        assert!(is_overdue(&due("2026-11-12", None), now));
        assert!(!is_overdue(&due("2026-11-13", None), now));
        assert!(is_overdue(&due("2026-11-13", Some("11:59")), now));
        assert!(!is_overdue(&due("2026-11-13", Some("19:30")), now));
    }
}
