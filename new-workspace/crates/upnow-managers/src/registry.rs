use upnow_domain::ManagerId;

use crate::adapter::{ManagerAdapter, ManagerAdapterError};
use crate::npm::NpmManager;
use crate::pnpm::PnpmManager;

static NPM_MANAGER: NpmManager = NpmManager;
static PNPM_MANAGER: PnpmManager = PnpmManager;

pub fn manager_by_id(
    manager_id: &ManagerId,
) -> Result<&'static dyn ManagerAdapter, ManagerAdapterError> {
    match manager_id.as_str() {
        crate::npm::MANAGER_ID => Ok(&NPM_MANAGER),
        crate::pnpm::MANAGER_ID => Ok(&PNPM_MANAGER),
        other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
    }
}

#[must_use]
pub fn available_managers() -> [&'static dyn ManagerAdapter; 2] {
    [&PNPM_MANAGER, &NPM_MANAGER]
}
