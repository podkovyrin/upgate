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
    plugin.validate_version_policy(policy.version_policy)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UpnowConfig;
    use crate::managers::RunMode;

    struct MockPlugin;

    impl ManagerPlugin for MockPlugin {
        fn id(&self) -> &'static str {
            "mock"
        }

        fn default_min_release_age(&self) -> &'static str {
            "7d"
        }

        fn run(&self, _ctx: &ManagerCtx) -> Result<()> {
            Ok(())
        }
    }

    struct StableOnlyPlugin;

    impl ManagerPlugin for StableOnlyPlugin {
        fn id(&self) -> &'static str {
            "stable-only"
        }

        fn default_min_release_age(&self) -> &'static str {
            "7d"
        }

        fn supports_version_policy(
            &self,
            policy: crate::managers::shared::versioning::policy::VersionPolicy,
        ) -> bool {
            matches!(
                policy,
                crate::managers::shared::versioning::policy::VersionPolicy::Disabled
                    | crate::managers::shared::versioning::policy::VersionPolicy::Stable
            )
        }

        fn run(&self, _ctx: &ManagerCtx) -> Result<()> {
            Ok(())
        }
    }

    static MOCK_PLUGIN: MockPlugin = MockPlugin;
    static STABLE_ONLY_PLUGIN: StableOnlyPlugin = StableOnlyPlugin;

    #[test]
    fn build_ctx_accepts_any_policy_when_plugin_supports_it() {
        let config: UpnowConfig =
            toml::from_str("[mock]\nversion_policy = \"any\"\n").expect("valid config");
        let ctx = build_ctx_for_plugin(&MOCK_PLUGIN, RunMode::Plan, 4, &config, false)
            .expect("context should build");

        assert_eq!(
            ctx.policy.version_policy.as_str(),
            crate::managers::shared::versioning::policy::VersionPolicy::Any.as_str()
        );
    }

    #[test]
    fn build_ctx_rejects_unsupported_policy_for_plugin() {
        let config: UpnowConfig =
            toml::from_str("[stable-only]\nversion_policy = \"same-track\"\n")
                .expect("valid config");
        let err = build_ctx_for_plugin(&STABLE_ONLY_PLUGIN, RunMode::Plan, 4, &config, false)
            .err()
            .expect("context build should fail");

        assert_eq!(
            err.to_string(),
            "version_policy \"same-track\" is not supported by this manager"
        );
    }
}
