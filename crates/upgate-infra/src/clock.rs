use std::time::SystemTime;

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
}
