use anyhow::{Context, Result};

pub(crate) fn parse_rfc3339_unix(raw: &str) -> Result<u64> {
    let dt = chrono::DateTime::parse_from_rfc3339(raw)
        .with_context(|| format!("invalid RFC3339 timestamp: {raw}"))?;

    u64::try_from(dt.timestamp()).context("timestamp before UNIX_EPOCH")
}

#[cfg(test)]
mod tests {
    use super::parse_rfc3339_unix;

    #[test]
    fn parses_valid_rfc3339() {
        let ts = parse_rfc3339_unix("1970-01-01T00:00:01Z").expect("should parse");
        assert_eq!(ts, 1);
    }

    #[test]
    fn rejects_invalid_rfc3339() {
        let err = parse_rfc3339_unix("not-a-date").expect_err("should fail");
        assert!(err.to_string().contains("invalid RFC3339 timestamp"));
    }
}
