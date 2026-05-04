//! Execution behavior for the `upnow` rebuild.

use upnow_domain::{ManagerId, PackageName, PlanItemId, VersionText};
use upnow_infra::{CommandCheck, CommandSpec, InfraError, ProcessRunner};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub manager_id: ManagerId,
    pub items: Vec<ExecutionItemResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionItemResult {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: VersionText,
    pub status: ExecutionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionStatus {
    Succeeded {
        command: String,
        skipped_mutation: bool,
    },
    Failed {
        command: String,
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionCommand {
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: VersionText,
    pub command: CommandSpec,
}

/// Executes concrete commands produced by a manager.
#[must_use]
pub fn execute_commands(
    manager_id: ManagerId,
    commands: Vec<ExecutionCommand>,
    process: &ProcessRunner,
) -> Result<ExecutionReport, InfraError> {
    let items = commands
        .into_iter()
        .map(|item| {
            let command = item.command.display();
            let status = match process.run(&item.command, &CommandCheck::Success) {
                Ok(output) => ExecutionStatus::Succeeded {
                    command,
                    skipped_mutation: output.skipped_mutation(),
                },
                Err(err) if err.is_interruption() => return Err(err),
                Err(err) => ExecutionStatus::Failed {
                    command,
                    detail: err.to_string(),
                },
            };
            Ok(ExecutionItemResult {
                plan_item_id: item.plan_item_id,
                package_name: item.package_name,
                installed_version: item.installed_version,
                target_version: item.target_version,
                status,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ExecutionReport { manager_id, items })
}
