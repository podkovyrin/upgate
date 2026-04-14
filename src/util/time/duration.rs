use anyhow::{Context, Result, bail};
use std::time::Duration;

pub fn parse_duration(raw: &str) -> Result<Duration> {
    let trimmed = raw.trim();
    if trimmed.len() < 2 {
        bail!("invalid duration '{raw}', expected values like 12h or 7d");
    }

    let (number, unit) = trimmed.split_at(trimmed.len() - 1);
    let value = number
        .parse::<u64>()
        .with_context(|| format!("invalid duration number in '{raw}'"))?;

    let secs = match unit {
        "s" => value,
        "m" => value.saturating_mul(60),
        "h" => value.saturating_mul(60 * 60),
        "d" => value.saturating_mul(24 * 60 * 60),
        _ => bail!("invalid duration unit '{unit}', expected one of: s, m, h, d"),
    };

    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::parse_duration;

    #[test]
    fn parse_duration_hours_and_days() {
        assert_eq!(
            parse_duration("12h").expect("should parse").as_secs(),
            12 * 3600
        );
        assert_eq!(
            parse_duration("7d").expect("should parse").as_secs(),
            7 * 24 * 3600
        );
    }

    #[test]
    fn parse_duration_supports_seconds_minutes_hours_days() {
        assert_eq!(parse_duration("5s").expect("should parse").as_secs(), 5);
        assert_eq!(parse_duration("2m").expect("should parse").as_secs(), 120);
        assert_eq!(
            parse_duration("3h").expect("should parse").as_secs(),
            10_800
        );
        assert_eq!(
            parse_duration("4d").expect("should parse").as_secs(),
            345_600
        );
    }
}
