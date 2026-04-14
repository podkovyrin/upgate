use crate::config::{ManagerMode, UpnowConfig};
use crate::managers;
use anyhow::{Result, bail};
use std::collections::BTreeSet;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunMode {
    Plan,
    Apply,
    Scan,
}

impl RunMode {
    pub(crate) fn is_dry_run(self) -> bool {
        matches!(self, Self::Plan | Self::Scan)
    }

    pub(crate) fn is_scan(self) -> bool {
        matches!(self, Self::Scan)
    }
}

pub(crate) struct ManagerCtx {
    pub(crate) run_mode: RunMode,
    pub(crate) max_parallel_checks: usize,
    pub(crate) policy: crate::config::ManagerPolicy,
    pub(crate) scan_old_age_threshold: std::time::Duration,
    interactive: bool,
    pending_pins: Mutex<Option<BTreeSet<String>>>,
}

impl ManagerCtx {
    pub(crate) fn is_dry_run(&self) -> bool {
        self.run_mode.is_dry_run()
    }

    pub(crate) fn is_scan(&self) -> bool {
        self.run_mode.is_scan()
    }

    pub(crate) fn is_interactive_apply(&self) -> bool {
        self.interactive && matches!(self.run_mode, RunMode::Apply)
    }

    pub(crate) fn record_pending_pins_if_changed(&self, pins: BTreeSet<String>) {
        if pins == self.policy.pinned {
            return;
        }

        let mut slot = self
            .pending_pins
            .lock()
            .expect("pending_pins mutex poisoned");
        *slot = Some(pins);
    }

    pub(crate) fn take_pending_pins(&self) -> Option<BTreeSet<String>> {
        self.pending_pins
            .lock()
            .expect("pending_pins mutex poisoned")
            .take()
    }
}

pub(crate) trait ManagerPlugin: Sync {
    fn id(&self) -> &'static str;
    fn default_min_release_age(&self) -> &'static str;
    fn default_mode(&self) -> ManagerMode {
        ManagerMode::Apply
    }
    fn supports_no_update(&self) -> bool {
        false
    }
    fn run(&self, ctx: &ManagerCtx) -> Result<()>;
}

pub(crate) fn all_plugins() -> &'static [&'static dyn ManagerPlugin] {
    static ALL: [&dyn ManagerPlugin; 12] = [
        &managers::brew::PLUGIN,
        &managers::bun::PLUGIN,
        &managers::cargo::PLUGIN,
        &managers::npm::PLUGIN,
        &managers::yarn::PLUGIN,
        &managers::mise::PLUGIN,
        &managers::pipx::PLUGIN,
        &managers::pnpm::PLUGIN,
        &managers::uv::PLUGIN,
        &managers::go::PLUGIN,
        &managers::gem::PLUGIN,
        &managers::dotnet::PLUGIN,
    ];

    &ALL
}

pub(crate) fn build_ctx_for_plugin(
    plugin: &'static dyn ManagerPlugin,
    run_mode: RunMode,
    max_parallel_checks: usize,
    config: &UpnowConfig,
    interactive: bool,
) -> Result<ManagerCtx> {
    let policy = config.resolve_manager_policy(
        plugin.id(),
        plugin.default_min_release_age(),
        plugin.default_mode(),
        plugin.supports_no_update(),
    )?;

    Ok(ManagerCtx {
        run_mode,
        max_parallel_checks,
        policy,
        scan_old_age_threshold: config.scan_old_age_threshold()?,
        interactive,
        pending_pins: Mutex::new(None),
    })
}

pub(crate) fn resolve_selected_plugins(
    selected_ids: &[String],
) -> Result<Vec<&'static dyn ManagerPlugin>> {
    let all = all_plugins();

    if selected_ids.is_empty() {
        return Ok(all.to_vec());
    }

    let known_ids: BTreeSet<&str> = all.iter().map(|p| p.id()).collect();
    for id in selected_ids {
        if !known_ids.contains(id.as_str()) {
            bail!("unknown manager '{id}'");
        }
    }

    let selected: BTreeSet<&str> = selected_ids.iter().map(String::as_str).collect();
    Ok(all
        .iter()
        .copied()
        .filter(|plugin| selected.contains(plugin.id()))
        .collect())
}
