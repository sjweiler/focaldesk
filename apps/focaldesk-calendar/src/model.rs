use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use focaldesk_ipc::CalendarEventSnapshot;

pub fn week_start(date: NaiveDate) -> NaiveDate {
    date - Duration::days(i64::from(date.weekday().num_days_from_monday()))
}

pub fn midnight(date: NaiveDate) -> Result<i64, String> {
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|time| time.timestamp())
        .ok_or_else(|| "This date has no local midnight".into())
}

pub fn editable_time(value: &str, all_day: bool) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|time| {
            if all_day {
                time.format("%Y-%m-%d 00:00").to_string()
            } else {
                time.with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            }
        })
        .unwrap_or_else(|_| value.into())
}

pub fn display_time(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_else(|_| value.into())
}

pub fn validate_times(start: &str, end: &str, all_day: bool) -> Result<(String, String), String> {
    let parse = |value: &str| {
        let time = NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M")
            .map_err(|_| "Use YYYY-MM-DD HH:MM for event times".to_string())?;
        if all_day && time.time() != chrono::NaiveTime::MIN {
            return Err("All-day events must start and end at 00:00".into());
        }
        if all_day {
            return Ok(time.and_utc().with_timezone(&Local));
        }
        Local.from_local_datetime(&time).single().ok_or_else(|| {
            "Time is ambiguous or does not exist due to daylight saving; choose another time"
                .to_string()
        })
    };
    let start = parse(start)?;
    let end = parse(end)?;
    if end <= start {
        return Err("End time must follow start time".into());
    }
    if all_day {
        return Ok((
            start.with_timezone(&chrono::Utc).to_rfc3339(),
            end.with_timezone(&chrono::Utc).to_rfc3339(),
        ));
    }
    Ok((start.to_rfc3339(), end.to_rfc3339()))
}

pub fn day_segment(event: &CalendarEventSnapshot, date: NaiveDate) -> Option<(u32, u32)> {
    if event.all_day {
        let start = DateTime::parse_from_rfc3339(&event.start_time)
            .ok()?
            .date_naive();
        let end = DateTime::parse_from_rfc3339(&event.end_time)
            .ok()?
            .date_naive();
        return (start <= date && date < end).then_some((0, 1440));
    }
    let start = DateTime::parse_from_rfc3339(&event.start_time)
        .ok()?
        .with_timezone(&Local);
    let end = DateTime::parse_from_rfc3339(&event.end_time)
        .ok()?
        .with_timezone(&Local);
    if start.date_naive() > date || end.date_naive() < date {
        return None;
    }
    let from = if start.date_naive() < date {
        0
    } else {
        start.hour() * 60 + start.minute()
    };
    let until = if end.date_naive() > date {
        1440
    } else {
        end.hour() * 60 + end.minute()
    };
    (until > from).then_some((from, until))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_day_dates_are_not_shifted_by_local_timezone() {
        let (start_time, end_time) =
            validate_times("2026-11-09 00:00", "2026-11-10 00:00", true).unwrap();
        assert_eq!(start_time, "2026-11-09T00:00:00+00:00");
        assert_eq!(editable_time(&start_time, true), "2026-11-09 00:00");
        let event = CalendarEventSnapshot {
            account_id: "a".into(),
            provider: "google".into(),
            calendar_id: "primary".into(),
            event_id: "e".into(),
            title: "Day".into(),
            start_time,
            end_time,
            organizer: String::new(),
            all_day: true,
        };
        assert_eq!(
            day_segment(&event, NaiveDate::from_ymd_opt(2026, 11, 8).unwrap()),
            None
        );
        assert_eq!(
            day_segment(&event, NaiveDate::from_ymd_opt(2026, 11, 9).unwrap()),
            Some((0, 1440))
        );
        assert_eq!(
            day_segment(&event, NaiveDate::from_ymd_opt(2026, 11, 10).unwrap()),
            None
        );
    }

    #[test]
    fn weeks_cross_year_boundaries() {
        assert_eq!(
            week_start(NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()),
            NaiveDate::from_ymd_opt(2026, 12, 28).unwrap()
        );
    }
    #[test]
    fn rejects_invalid_event_intervals() {
        assert!(validate_times("2026-11-08 12:00", "2026-11-08 11:00", false).is_err());
        assert!(validate_times("2026-11-08 12:00", "2026-11-09 00:00", true).is_err());
        assert!(validate_times("invalid", "2026-11-09 00:00", false).is_err());
        assert!(validate_times("2026-11-08 10:00", "2026-11-08 11:00", false).is_ok());
    }
    #[test]
    fn splits_overnight_events_and_excludes_exclusive_end() {
        let (start_time, end_time) =
            validate_times("2026-11-08 23:00", "2026-11-10 00:00", false).unwrap();
        let event = CalendarEventSnapshot {
            account_id: "a".into(),
            provider: "google".into(),
            calendar_id: "primary".into(),
            event_id: "e".into(),
            title: "Overnight".into(),
            start_time,
            end_time,
            organizer: String::new(),
            all_day: false,
        };
        assert_eq!(
            day_segment(&event, NaiveDate::from_ymd_opt(2026, 11, 8).unwrap()),
            Some((1380, 1440))
        );
        assert_eq!(
            day_segment(&event, NaiveDate::from_ymd_opt(2026, 11, 9).unwrap()),
            Some((0, 1440))
        );
        assert_eq!(
            day_segment(&event, NaiveDate::from_ymd_opt(2026, 11, 10).unwrap()),
            None
        );
    }
}
