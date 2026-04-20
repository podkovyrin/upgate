use std::collections::BTreeSet;

use anyhow::{Result, bail};

use super::{ManagerCtx, ManagerPlugin, RunMode};
use crate::config::UpnowConfig;
use crate::managers;

pub fn all_plugins() -> &'static [&'static dyn ManagerPlugin] {
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

pub fn build_ctx_for_plugin(
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

    Ok(ManagerCtx::new(
        run_mode,
        max_parallel_checks,
        policy,
        config.scan_old_age_threshold()?,
        interactive,
    ))
}

pub fn resolve_selected_plugins<S: AsRef<str>>(
    selected_ids: &[S],
) -> Result<Vec<&'static dyn ManagerPlugin>> {
    let all = all_plugins();

    if selected_ids.is_empty() {
        return Ok(all.to_vec());
    }

    let known_ids: BTreeSet<&str> = all.iter().map(|p| p.id()).collect();
    for id in selected_ids {
        let id = id.as_ref();
        if !known_ids.contains(id) {
            bail!("unknown manager '{id}'");
        }
    }

    let selected: BTreeSet<&str> = selected_ids.iter().map(AsRef::as_ref).collect();
    Ok(all
        .iter()
        .copied()
        .filter(|plugin| selected.contains(plugin.id()))
        .collect())
}
