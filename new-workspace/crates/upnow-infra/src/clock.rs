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

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::Clock;
    use crate::InfraError;

    #[test]
    fn fixed_clock_returns_deterministic_unix_seconds() {
        let clock = Clock::fixed(SystemTime::UNIX_EPOCH + Duration::from_secs(42));

        assert_eq!(
            clock.now(),
            SystemTime::UNIX_EPOCH + Duration::from_secs(42)
        );
        assert_eq!(clock.unix_secs().expect("valid timestamp"), 42);
    }

    #[test]
    fn unix_seconds_rejects_times_before_epoch() {
        let clock = Clock::fixed(SystemTime::UNIX_EPOCH - Duration::from_secs(1));

        assert!(matches!(
            clock.unix_secs(),
            Err(InfraError::ClockBeforeUnixEpoch)
        ));
    }
}
