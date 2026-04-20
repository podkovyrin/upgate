use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::blocking::Client;

pub const HTTP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
pub const HTTP_TIMEOUT_SECS: u64 = 8;

pub fn default_blocking_client() -> Result<Client> {
    Client::builder()
        .user_agent(HTTP_USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .context("failed to build HTTP client")
}

pub fn env_base_url(var_name: &str, default: &str) -> String {
    std::env::var(var_name)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}
