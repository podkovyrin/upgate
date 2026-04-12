use anyhow::{Context, Result};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn now_unix_secs() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs())
}
