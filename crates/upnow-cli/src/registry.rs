use std::time::Duration;

use upnow_domain::{ManagerConfig, ManagerId, VersionPolicy};
use upnow_managers::adapter::{
    ManagerAdapter, ManagerAdapterError, ManagerConfigDefaults, ManagerConfigRuleError,
};
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

macro_rules! with_known_managers {
    ($macro:ident) => {
        $macro!(
            BrewManager,
            PnpmManager,
            NpmManager,
            YarnManager,
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
