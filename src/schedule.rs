use std::fmt;

use chrono::{DateTime, Duration, Utc};
use croner::Cron;

use crate::primitives::ScheduleId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleType {
    Cron,
    Interval,
    Once,
}

impl ScheduleType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Cron => "cron",
            Self::Interval => "interval",
            Self::Once => "once",
        }
    }
}

impl fmt::Display for ScheduleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for ScheduleType {
    fn from(s: String) -> Self {
        match s.as_str() {
            "cron" => Self::Cron,
            "interval" => Self::Interval,
            "once" => Self::Once,
            other => {
                tracing::warn!(value = other, "unknown ScheduleType, defaulting to Once");
                Self::Once
            }
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct Schedule {
    pub id: ScheduleId,
    pub name: String,
    pub schedule_type: ScheduleType,
    pub schedule_expr: String,
    pub action: String,
    pub enabled: bool,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub run_count: i64,
    pub max_runs: Option<i64>,
}

/// Parse a human-friendly interval string like "5m", "1h", "30s", "2d" into a Duration.
pub fn parse_interval(s: &str) -> anyhow::Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty interval string");
    }

    let (num_str, suffix) = s.split_at(s.len() - 1);
    let value: i64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid interval '{s}': expected format like 5m, 1h, 30s"))?;

    let duration = match suffix {
        "s" => Duration::seconds(value),
        "m" => Duration::minutes(value),
        "h" => Duration::hours(value),
        "d" => Duration::days(value),
        _ => anyhow::bail!("unknown interval suffix '{suffix}' in '{s}' (use s, m, h, or d)"),
    };

    Ok(duration)
}

/// Compute the next run time for a schedule based on its type and expression.
pub fn compute_next_run(
    schedule_type: &ScheduleType,
    schedule_expr: &str,
    from: DateTime<Utc>,
) -> anyhow::Result<Option<DateTime<Utc>>> {
    match schedule_type {
        ScheduleType::Cron => {
            let cron = Cron::new(schedule_expr)
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid cron expression '{schedule_expr}': {e}"))?;
            let next = cron
                .find_next_occurrence(&from, false)
                .map_err(|e| anyhow::anyhow!("failed to compute next cron run: {e}"))?;
            Ok(Some(next))
        }
        ScheduleType::Interval => {
            let duration = parse_interval(schedule_expr)?;
            Ok(Some(from + duration))
        }
        ScheduleType::Once => {
            let dt = DateTime::parse_from_rfc3339(schedule_expr)
                .map_err(|e| anyhow::anyhow!("invalid timestamp '{schedule_expr}': {e}"))?
                .with_timezone(&Utc);
            if dt > from { Ok(Some(dt)) } else { Ok(None) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn parse_interval_seconds() {
        let d = parse_interval("30s").unwrap();
        assert_eq!(d, Duration::seconds(30));
    }

    #[test]
    fn parse_interval_minutes() {
        let d = parse_interval("5m").unwrap();
        assert_eq!(d, Duration::minutes(5));
    }

    #[test]
    fn parse_interval_hours() {
        let d = parse_interval("2h").unwrap();
        assert_eq!(d, Duration::hours(2));
    }

    #[test]
    fn parse_interval_days() {
        let d = parse_interval("1d").unwrap();
        assert_eq!(d, Duration::days(1));
    }

    #[test]
    fn parse_interval_invalid() {
        assert!(parse_interval("").is_err());
        assert!(parse_interval("abc").is_err());
        assert!(parse_interval("5x").is_err());
    }

    #[test]
    fn compute_next_run_cron() {
        let from = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = compute_next_run(&ScheduleType::Cron, "0 9 * * *", from)
            .unwrap()
            .unwrap();
        assert_eq!(next.hour(), 9);
    }

    #[test]
    fn compute_next_run_interval() {
        let from = Utc::now();
        let next = compute_next_run(&ScheduleType::Interval, "5m", from)
            .unwrap()
            .unwrap();
        assert!(next > from);
        let diff = next - from;
        assert_eq!(diff.num_minutes(), 5);
    }

    #[test]
    fn compute_next_run_once_future() {
        let future = (Utc::now() + Duration::hours(1)).to_rfc3339();
        let next = compute_next_run(&ScheduleType::Once, &future, Utc::now())
            .unwrap()
            .unwrap();
        assert!(next > Utc::now());
    }

    #[test]
    fn compute_next_run_once_past() {
        let past = (Utc::now() - Duration::hours(1)).to_rfc3339();
        let next = compute_next_run(&ScheduleType::Once, &past, Utc::now()).unwrap();
        assert!(next.is_none());
    }
}
