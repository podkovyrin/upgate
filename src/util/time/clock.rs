use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

pub fn now_unix_secs() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock before UNIX_EPOCH")?
        .as_secs())
}
