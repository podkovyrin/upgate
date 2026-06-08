use std::time::{SystemTime, UNIX_EPOCH};

use crate::InfraError;

/// Clock source used by release-age and command tests.
#[derive(Debug, Clone, Copy)]
pub enum Clock {
    System,
    Fixed(SystemTime),
}

impl Clock {
    pub const fn system() -> Self {
        Self::System
    }
    pub const fn fixed(time: SystemTime) -> Self {
        Self::Fixed(time)
    }
    pub fn now(self) -> SystemTime {
        match self {
            Self::System => SystemTime::now(),
            Self::Fixed(time) => time,
        }
    }

    /// Returns the current time as UNIX seconds.
    ///
    /// # Errors
    ///
    /// Returns an error when the clock value is before the UNIX epoch.
    pub fn unix_secs(self) -> Result<u64, InfraError> {
        Ok(self
            .now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| InfraError::ClockBeforeUnixEpoch)?
            .as_secs())
    }
}
