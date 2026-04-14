use crate::config::ManagerPolicy;
use std::collections::BTreeSet;
use std::sync::Mutex;

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
        self.interactive && matches!(self.run_mode, RunMode::Apply)
    }

    pub fn record_pending_pins_if_changed(&self, pins: BTreeSet<String>) {
        if pins == self.policy.pinned {
            return;
        }

        let mut slot = self
            .pending_pins
            .lock()
            .expect("pending_pins mutex poisoned");
        *slot = Some(pins);
    }

    pub fn take_pending_pins(&self) -> Option<BTreeSet<String>> {
        self.pending_pins
            .lock()
            .expect("pending_pins mutex poisoned")
            .take()
    }
}
