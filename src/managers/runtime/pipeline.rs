use anyhow::Result;

use super::ManagerCtx;

/// Shared manager pipeline entrypoint used by all manager plugins.
///
/// This provides a consistent run-mode dispatch (`scan` vs `plan/apply`).
pub fn run_manager_pipeline<Scan, PlanApply>(
    ctx: &ManagerCtx,
    scan: Scan,
    plan_apply: PlanApply,
) -> Result<()>
where
    Scan: FnOnce(&ManagerCtx) -> Result<()>,
    PlanApply: FnOnce(&ManagerCtx) -> Result<()>,
{
    if ctx.is_scan() {
        return scan(ctx);
    }

    plan_apply(ctx)
}
