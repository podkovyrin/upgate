use crate::config::UpnowConfig;
use crate::managers;
use anyhow::{Result, bail};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunMode {
    Plan,
    Apply,
}

impl RunMode {
    pub(crate) fn is_dry_run(self) -> bool {
        matches!(self, Self::Plan)
    }
}

pub(crate) struct ManagerCtx {
    pub(crate) run_mode: RunMode,
    pub(crate) max_parallel_checks: usize,
    pub(crate) policy: crate::config::ManagerPolicy,
}

impl ManagerCtx {
    pub(crate) fn is_dry_run(&self) -> bool {
        self.run_mode.is_dry_run()
    }
}

pub(crate) trait ManagerPlugin: Sync {
    fn id(&self) -> &'static str;
    fn default_min_release_age(&self) -> &'static str;
    fn supports_no_update(&self) -> bool {
        false
    }
    fn run(&self, ctx: &ManagerCtx) -> Result<()>;
}

pub(crate) fn all_plugins() -> &'static [&'static dyn ManagerPlugin] {
    static ALL: [&dyn ManagerPlugin; 10] = [
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
    ];

    &ALL
}

pub(crate) fn build_ctx_for_plugin(
    plugin: &'static dyn ManagerPlugin,
    run_mode: RunMode,
    max_parallel_checks: usize,
    config: &UpnowConfig,
) -> Result<ManagerCtx> {
    let policy = config.resolve_manager_policy(
        plugin.id(),
        plugin.default_min_release_age(),
        plugin.supports_no_update(),
    )?;

    Ok(ManagerCtx {
        run_mode,
        max_parallel_checks,
        policy,
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
