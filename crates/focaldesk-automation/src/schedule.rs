use chrono::{Local, NaiveTime};
use std::time::Duration;

/// A schedule is time-based only (no reactive/event triggers in v1 — see
/// crate docs). `Interval` fires every fixed duration starting one duration
/// from when the automation was loaded; `Daily` fires once at a wall-clock
/// time every day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    Interval(Duration),
    Daily { hour: u32, minute: u32 },
}

impl Schedule {
    /// Parses `"every <N><unit>"` (unit one of `s`, `m`, `h`) or
    /// `"daily <HH:MM>"`.
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        let lower = input.to_ascii_lowercase();

        if let Some(rest) = lower.strip_prefix("every ") {
            return parse_interval(rest.trim());
        }

        if let Some(rest) = lower.strip_prefix("daily ") {
            return parse_daily(rest.trim());
        }

        Err(format!(
            "unrecognized schedule '{input}' — expected 'every <N><s|m|h>' or 'daily <HH:MM>'"
        ))
    }

    /// Time to sleep before the next run. Callers re-invoke this after each
    /// run to get the next delay, so `Daily`'s "roll to tomorrow" behavior
    /// falls out naturally once today's occurrence is in the past.
    pub fn next_delay(&self) -> Duration {
        match self {
            Schedule::Interval(duration) => *duration,
            Schedule::Daily { hour, minute } => {
                let now = Local::now();
                let target_time = NaiveTime::from_hms_opt(*hour, *minute, 0)
                    .expect("hour/minute validated at parse time");

                let mut target = now.with_time(target_time).single().unwrap_or(now);
                if target <= now {
                    target += chrono::Duration::days(1);
                }

                (target - now).to_std().unwrap_or(Duration::from_secs(0))
            }
        }
    }
}

fn parse_interval(spec: &str) -> Result<Schedule, String> {
    let unit_index = spec
        .find(|ch: char| !ch.is_ascii_digit())
        .ok_or_else(|| format!("missing unit in interval '{spec}' — expected e.g. '15m'"))?;
    let (digits, unit) = spec.split_at(unit_index);

    let amount: u64 = digits
        .parse()
        .map_err(|_| format!("invalid interval amount '{digits}' in '{spec}'"))?;
    if amount == 0 {
        return Err(format!(
            "interval amount must be greater than zero: '{spec}'"
        ));
    }

    let duration = match unit {
        "s" => Duration::from_secs(amount),
        "m" => Duration::from_secs(amount * 60),
        "h" => Duration::from_secs(amount * 3600),
        other => {
            return Err(format!(
                "unknown interval unit '{other}' — expected s, m, or h"
            ));
        }
    };

    Ok(Schedule::Interval(duration))
}

fn parse_daily(spec: &str) -> Result<Schedule, String> {
    let (hour_str, minute_str) = spec
        .split_once(':')
        .ok_or_else(|| format!("expected 'HH:MM' in daily schedule '{spec}'"))?;

    let hour: u32 = hour_str
        .parse()
        .map_err(|_| format!("invalid hour '{hour_str}' in '{spec}'"))?;
    let minute: u32 = minute_str
        .parse()
        .map_err(|_| format!("invalid minute '{minute_str}' in '{spec}'"))?;

    if hour > 23 || minute > 59 {
        return Err(format!("time out of range in daily schedule '{spec}'"));
    }

    Ok(Schedule::Daily { hour, minute })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_second_interval() {
        assert_eq!(
            Schedule::parse("every 30s").unwrap(),
            Schedule::Interval(Duration::from_secs(30))
        );
    }

    #[test]
    fn parses_minute_and_hour_intervals() {
        assert_eq!(
            Schedule::parse("every 15m").unwrap(),
            Schedule::Interval(Duration::from_secs(15 * 60))
        );
        assert_eq!(
            Schedule::parse("every 2h").unwrap(),
            Schedule::Interval(Duration::from_secs(2 * 3600))
        );
    }

    #[test]
    fn parses_daily_schedule() {
        assert_eq!(
            Schedule::parse("daily 22:30").unwrap(),
            Schedule::Daily {
                hour: 22,
                minute: 30
            }
        );
    }

    #[test]
    fn is_case_and_whitespace_insensitive() {
        assert_eq!(
            Schedule::parse("  EVERY 5M  ").unwrap(),
            Schedule::Interval(Duration::from_secs(5 * 60))
        );
    }

    #[test]
    fn rejects_zero_interval() {
        assert!(Schedule::parse("every 0m").is_err());
    }

    #[test]
    fn rejects_unknown_unit() {
        assert!(Schedule::parse("every 5d").is_err());
    }

    #[test]
    fn rejects_out_of_range_daily_time() {
        assert!(Schedule::parse("daily 24:00").is_err());
        assert!(Schedule::parse("daily 10:60").is_err());
    }

    #[test]
    fn rejects_unrecognized_prefix() {
        assert!(Schedule::parse("weekly monday").is_err());
    }

    #[test]
    fn interval_next_delay_is_constant() {
        let schedule = Schedule::Interval(Duration::from_secs(42));
        assert_eq!(schedule.next_delay(), Duration::from_secs(42));
        assert_eq!(schedule.next_delay(), Duration::from_secs(42));
    }

    #[test]
    fn daily_next_delay_is_at_most_24_hours() {
        let schedule = Schedule::Daily {
            hour: 12,
            minute: 0,
        };
        let delay = schedule.next_delay();
        assert!(delay <= Duration::from_secs(24 * 3600));
    }
}
