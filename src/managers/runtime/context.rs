use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard};

use crate::config::ManagerPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Plan,
    Apply,
    Scan,
}

impl RunMode {
    pub const fn is_dry_run(self) -> bool {
        matches!(self, Self::Plan | Self::Scan)
    }

    pub const fn is_scan(self) -> bool {
        matches!(self, Self::Scan)
    }

    pub const fn is_apply(self) -> bool {
        matches!(self, Self::Apply)
    }

    pub const fn action_label(self) -> &'static str {
        match self {
            Self::Plan => "Planning",
            Self::Apply => "Applying",
            Self::Scan => "Scanning",
        }
    }
}

pub struct ManagerCtx {
    pub run_mode: RunMode,
    pub max_parallel_checks: usize,
    pub policy: ManagerPolicy,
    pub scan_old_age_threshold: std::time::Duration,
    interactive: bool,
    pending_pins: Mutex<Option<BTreeSet<String>>>,
}

impl ManagerCtx {
    pub const fn new(
        run_mode: RunMode,
        max_parallel_checks: usize,
        policy: ManagerPolicy,
        scan_old_age_threshold: std::time::Duration,
        interactive: bool,
    ) -> Self {
        Self {
            run_mode,
            max_parallel_checks,
            policy,
            scan_old_age_threshold,
            interactive,
            pending_pins: Mutex::new(None),
        }
    }

    pub const fn is_dry_run(&self) -> bool {
        self.run_mode.is_dry_run()
    }

    pub const fn is_scan(&self) -> bool {
        self.run_mode.is_scan()
    }

    pub const fn is_interactive_apply(&self) -> bool {
        self.interactive && self.run_mode.is_apply()
    }

    pub fn record_pending_pins_if_changed(&self, pins: &BTreeSet<String>) {
        if pins == &self.policy.pinned {
            return;
        }

        *self.lock_pending_pins() = Some(pins.clone());
    }

    pub fn take_pending_pins(&self) -> Option<BTreeSet<String>> {
        self.lock_pending_pins().take()
    }

    fn lock_pending_pins(&self) -> MutexGuard<'_, Option<BTreeSet<String>>> {
        self.pending_pins
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
