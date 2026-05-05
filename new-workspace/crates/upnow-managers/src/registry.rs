use upnow_domain::ManagerId;

use crate::adapter::{ManagerAdapter, ManagerAdapterError};
use crate::bun::BunManager;
use crate::cargo::CargoManager;
use crate::npm::NpmManager;
use crate::pipx::PipxManager;
use crate::pnpm::PnpmManager;
use crate::yarn::YarnManager;

static BUN_MANAGER: BunManager = BunManager;
static CARGO_MANAGER: CargoManager = CargoManager;
static NPM_MANAGER: NpmManager = NpmManager;
static PIPX_MANAGER: PipxManager = PipxManager;
static PNPM_MANAGER: PnpmManager = PnpmManager;
static YARN_MANAGER: YarnManager = YarnManager;

pub fn manager_by_id(
    manager_id: &ManagerId,
) -> Result<&'static dyn ManagerAdapter, ManagerAdapterError> {
    match manager_id.as_str() {
        crate::bun::MANAGER_ID => Ok(&BUN_MANAGER),
        crate::cargo::MANAGER_ID => Ok(&CARGO_MANAGER),
        crate::npm::MANAGER_ID => Ok(&NPM_MANAGER),
        crate::pipx::MANAGER_ID => Ok(&PIPX_MANAGER),
        crate::pnpm::MANAGER_ID => Ok(&PNPM_MANAGER),
        crate::yarn::MANAGER_ID => Ok(&YARN_MANAGER),
        other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
    }
}

#[must_use]
pub fn available_managers() -> [&'static dyn ManagerAdapter; 6] {
    [
        &PNPM_MANAGER,
        &NPM_MANAGER,
        &YARN_MANAGER,
        &BUN_MANAGER,
        &CARGO_MANAGER,
        &PIPX_MANAGER,
    ]
}
