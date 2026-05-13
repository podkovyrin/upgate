use std::time::Duration;

use upnow_domain::{ManagerConfig, ManagerId, ManagerMode, UpdateSelectionPolicy, VersionPolicy};
use upnow_managers::adapter::{ManagerAdapter, ManagerAdapterError};
use upnow_managers::brew::BrewManager;
use upnow_managers::bun::BunManager;
use upnow_managers::cargo::CargoManager;
use upnow_managers::dotnet::DotnetManager;
use upnow_managers::gem::GemManager;
use upnow_managers::go::GoManager;
use upnow_managers::mise::MiseManager;
use upnow_managers::npm::NpmManager;
use upnow_managers::pipx::PipxManager;
use upnow_managers::pnpm::PnpmManager;
use upnow_managers::uv::UvManager;
use upnow_managers::yarn::YarnManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerDefaultMode {
    Off,
    Apply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagerDefaults {
    pub min_release_age: Duration,
    pub mode: ManagerDefaultMode,
}
pub const fn available_manager_ids() -> [&'static str; 12] {
    [
        "brew", "pnpm", "npm", "yarn", "bun", "cargo", "pipx", "go", "mise", "gem", "dotnet", "uv",
    ]
}

/// Returns the default config values for a manager.
///
/// # Errors
///
/// Returns an error when `manager_id` is not a known migrated manager.
pub fn manager_defaults(manager_id: &str) -> Result<ManagerDefaults, ManagerAdapterError> {
    match manager_id {
        "brew" => Ok(ManagerDefaults {
            min_release_age: Duration::from_secs(12 * 60 * 60),
            mode: ManagerDefaultMode::Apply,
        }),
        "gem" | "dotnet" => Ok(ManagerDefaults {
            min_release_age: Duration::from_secs(7 * 24 * 60 * 60),
            mode: ManagerDefaultMode::Off,
        }),
        "pnpm" | "npm" | "yarn" | "bun" | "cargo" | "pipx" | "go" | "mise" | "uv" => {
            Ok(ManagerDefaults {
                min_release_age: Duration::from_secs(7 * 24 * 60 * 60),
                mode: ManagerDefaultMode::Apply,
            })
        }
        other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
    }
}
/// Checks whether the concrete manager adapter supports a version policy.
///
/// # Errors
///
/// Returns an error when `manager_id` is not a known migrated manager.
pub fn supports_version_policy(
    manager_id: &str,
    policy: VersionPolicy,
) -> Result<bool, ManagerAdapterError> {
    let defaults = manager_defaults(manager_id)?;
    let config = ManagerConfig {
        manager_id: ManagerId::new(manager_id.to_owned())
            .map_err(|_| ManagerAdapterError::UnknownManager(manager_id.to_owned()))?,
        mode: match defaults.mode {
            ManagerDefaultMode::Off => ManagerMode::Off,
            ManagerDefaultMode::Apply => ManagerMode::Apply,
        },
        min_release_age: defaults.min_release_age,
        version_policy: policy,
        no_update: false,
        selection: UpdateSelectionPolicy::default(),
    };
    configured_manager(config).map(|manager| manager.supports_version_policy(policy))
}

/// Validates that a manager id is known by the CLI registry.
///
/// # Errors
///
/// Returns an error when `manager_id` is not a known migrated manager.
pub fn ensure_known_manager(manager_id: &str) -> Result<(), ManagerAdapterError> {
    if available_manager_ids().contains(&manager_id) {
        Ok(())
    } else {
        Err(ManagerAdapterError::UnknownManager(manager_id.to_owned()))
    }
}

/// Builds the concrete manager adapter for resolved manager config.
///
/// # Errors
///
/// Returns an error when the resolved config references an unknown manager.
pub fn configured_manager(
    config: ManagerConfig,
) -> Result<Box<dyn ManagerAdapter>, ManagerAdapterError> {
    match config.manager_id.as_str() {
        "brew" => Ok(Box::new(BrewManager::new(config))),
        "bun" => Ok(Box::new(BunManager::new(config))),
        "cargo" => Ok(Box::new(CargoManager::new(config))),
        "dotnet" => Ok(Box::new(DotnetManager::new(config))),
        "gem" => Ok(Box::new(GemManager::new(config))),
        "go" => Ok(Box::new(GoManager::new(config))),
        "mise" => Ok(Box::new(MiseManager::new(config))),
        "npm" => Ok(Box::new(NpmManager::new(config))),
        "pipx" => Ok(Box::new(PipxManager::new(config))),
        "pnpm" => Ok(Box::new(PnpmManager::new(config))),
        "uv" => Ok(Box::new(UvManager::new(config))),
        "yarn" => Ok(Box::new(YarnManager::new(config))),
        other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
    }
}
