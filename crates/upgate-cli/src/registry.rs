use std::time::Duration;

use upgate_domain::{ManagerConfig, ManagerId, VersionPolicy};
use upgate_managers::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerConfigDefaults, ManagerConfigRuleError,
};
use upgate_managers::brew::BrewManager;
use upgate_managers::bun::BunManager;
use upgate_managers::cargo::CargoManager;
use upgate_managers::dotnet::DotnetManager;
use upgate_managers::gem::GemManager;
use upgate_managers::go::GoManager;
use upgate_managers::mise::MiseManager;
use upgate_managers::npm::NpmManager;
use upgate_managers::pipx::PipxManager;
use upgate_managers::pnpm::PnpmManager;
use upgate_managers::uv::UvManager;

macro_rules! with_known_managers {
    ($macro:ident) => {
        $macro!(
            BrewManager,
            PnpmManager,
            NpmManager,
            BunManager,
            CargoManager,
            PipxManager,
            GoManager,
            MiseManager,
            GemManager,
            DotnetManager,
            UvManager
        )
    };
}

/// Returns migrated manager ids in the CLI's default processing order.
pub fn available_manager_ids() -> impl Iterator<Item = ManagerId> {
    macro_rules! available_ids {
        ($($manager:ty),+ $(,)?) => {
            [$(<$manager>::id()),+].into_iter()
        };
    }
    with_known_managers!(available_ids)
}

/// Returns the default config values for a manager.
///
/// # Errors
///
/// Returns an error when `manager_id` is not a known migrated manager.
pub fn manager_defaults(manager_id: &str) -> Result<ManagerConfigDefaults, ManagerAdapterError> {
    macro_rules! defaults {
        ($($manager:ty),+ $(,)?) => {
            match manager_id {
                $(id if id == <$manager>::id().as_str() => {
                    Ok(<$manager as ManagerAdapter>::default_config())
                })+
                other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
            }
        };
    }
    with_known_managers!(defaults)
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
    macro_rules! policy_support {
        ($($manager:ty),+ $(,)?) => {
            match manager_id {
                $(id if id == <$manager>::id().as_str() => {
                    Ok(<$manager as ManagerAdapter>::supports_version_policy(policy))
                })+
                other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
            }
        };
    }
    with_known_managers!(policy_support)
}

/// Checks whether the concrete manager accepts `no_update`.
///
/// # Errors
///
/// Returns an error when `manager_id` is not a known migrated manager.
pub fn accepts_no_update(manager_id: &str) -> Result<bool, ManagerAdapterError> {
    macro_rules! no_update_support {
        ($($manager:ty),+ $(,)?) => {
            match manager_id {
                $(id if id == <$manager>::id().as_str() => {
                    Ok(<$manager as ManagerAdapter>::accepts_no_update())
                })+
                other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
            }
        };
    }
    with_known_managers!(no_update_support)
}

/// Returns the primary executable required for a manager to be present.
///
/// # Errors
///
/// Returns an error when `manager_id` is not a known migrated manager.
pub fn required_executable(manager_id: &str) -> Result<&'static str, ManagerAdapterError> {
    macro_rules! executable {
        ($($manager:ty),+ $(,)?) => {
            match manager_id {
                $(id if id == <$manager>::id().as_str() => {
                    Ok(<$manager as ManagerAdapter>::required_executable())
                })+
                other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
            }
        };
    }
    with_known_managers!(executable)
}

/// Checks manager-owned min-release-age rules.
///
/// # Errors
///
/// Returns an error when `manager_id` is unknown.
pub fn min_release_age_rule_error(
    manager_id: &str,
    min_release_age: Duration,
) -> Result<Option<ManagerConfigRuleError>, ManagerAdapterError> {
    macro_rules! min_release_age_rule {
        ($($manager:ty),+ $(,)?) => {
            match manager_id {
                $(id if id == <$manager>::id().as_str() => {
                    Ok(<$manager as ManagerAdapter>::validate_min_release_age_rule(min_release_age).err())
                })+
                other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
            }
        };
    }
    with_known_managers!(min_release_age_rule)
}

/// Validates that a manager id is known by the CLI registry.
///
/// # Errors
///
/// Returns an error when `manager_id` is not a known migrated manager.
pub fn ensure_known_manager(manager_id: &str) -> Result<(), ManagerAdapterError> {
    macro_rules! known {
        ($($manager:ty),+ $(,)?) => {
            match manager_id {
                $(id if id == <$manager>::id().as_str() => Ok(()),)+
                other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
            }
        };
    }
    with_known_managers!(known)
}

/// Builds the concrete manager adapter for resolved manager config.
///
/// # Errors
///
/// Returns an error when the resolved config references an unknown manager.
pub fn configured_manager(
    config: ManagerConfig,
) -> Result<Box<dyn ManagerAdapter>, ManagerAdapterError> {
    let manager_id = config.manager_id.as_str();
    macro_rules! construct {
        ($($manager:ty),+ $(,)?) => {
            match manager_id {
                $(id if id == <$manager>::id().as_str() => Ok(Box::new(<$manager>::new(config))),)+
                other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
            }
        };
    }
    with_known_managers!(construct)
}
