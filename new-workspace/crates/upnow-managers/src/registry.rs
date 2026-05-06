use upnow_domain::ManagerId;

use crate::adapter::{ManagerAdapter, ManagerAdapterError};
use crate::bun::BunManager;
use crate::cargo::CargoManager;
use crate::dotnet::DotnetManager;
use crate::gem::GemManager;
use crate::go::GoManager;
use crate::mise::MiseManager;
use crate::npm::NpmManager;
use crate::pipx::PipxManager;
use crate::pnpm::PnpmManager;
use crate::uv::UvManager;
use crate::yarn::YarnManager;

static BUN_MANAGER: BunManager = BunManager;
static CARGO_MANAGER: CargoManager = CargoManager;
static DOTNET_MANAGER: DotnetManager = DotnetManager;
static GEM_MANAGER: GemManager = GemManager;
static GO_MANAGER: GoManager = GoManager;
static MISE_MANAGER: MiseManager = MiseManager;
static NPM_MANAGER: NpmManager = NpmManager;
static PIPX_MANAGER: PipxManager = PipxManager;
static PNPM_MANAGER: PnpmManager = PnpmManager;
static UV_MANAGER: UvManager = UvManager;
static YARN_MANAGER: YarnManager = YarnManager;

pub fn manager_by_id(
    manager_id: &ManagerId,
) -> Result<&'static dyn ManagerAdapter, ManagerAdapterError> {
    match manager_id.as_str() {
        crate::bun::MANAGER_ID => Ok(&BUN_MANAGER),
        crate::cargo::MANAGER_ID => Ok(&CARGO_MANAGER),
        crate::dotnet::MANAGER_ID => Ok(&DOTNET_MANAGER),
        crate::gem::MANAGER_ID => Ok(&GEM_MANAGER),
        crate::go::MANAGER_ID => Ok(&GO_MANAGER),
        crate::mise::MANAGER_ID => Ok(&MISE_MANAGER),
        crate::npm::MANAGER_ID => Ok(&NPM_MANAGER),
        crate::pipx::MANAGER_ID => Ok(&PIPX_MANAGER),
        crate::pnpm::MANAGER_ID => Ok(&PNPM_MANAGER),
        crate::uv::MANAGER_ID => Ok(&UV_MANAGER),
        crate::yarn::MANAGER_ID => Ok(&YARN_MANAGER),
        other => Err(ManagerAdapterError::UnknownManager(other.to_owned())),
    }
}

#[must_use]
pub fn available_managers() -> [&'static dyn ManagerAdapter; 11] {
    [
        &PNPM_MANAGER,
        &NPM_MANAGER,
        &YARN_MANAGER,
        &BUN_MANAGER,
        &CARGO_MANAGER,
        &PIPX_MANAGER,
        &GO_MANAGER,
        &MISE_MANAGER,
        &GEM_MANAGER,
        &DOTNET_MANAGER,
        &UV_MANAGER,
    ]
}
