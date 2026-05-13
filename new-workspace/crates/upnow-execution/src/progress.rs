use upnow_domain::{ManagerId, PackageName, PlanItemId, VersionText};

use crate::{
    ExecutionCommandIntent, ExecutionReport, ExecutionStatus, ResolvedExecutionItem,
    ResolvedExecutionPlan,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionProgressState {
    pub rows: Vec<ExecutionProgressRow>,
    pub manager_failures: Vec<ExecutionProgressManagerFailure>,
    pub finished: bool,
    pub stop_after_current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionProgressRow {
    pub manager_id: ManagerId,
    pub plan_item_id: PlanItemId,
    pub package_name: PackageName,
    pub installed_version: VersionText,
    pub target_version: VersionText,
    pub status: ExecutionProgressStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionProgressStatus {
    Pending,
    Running,
    Succeeded {
        command: String,
        skipped_mutation: bool,
    },
    Failed {
        detail: String,
    },
    Skipped {
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionProgressManagerFailure {
    pub manager_id: ManagerId,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionProgressEvent {
    ManagerStarted {
        manager_id: ManagerId,
    },
    ManagerFinished {
        report: ExecutionReport,
    },
    ManagerFailed {
        manager_id: ManagerId,
        detail: String,
    },
    Fatal {
        detail: String,
    },
    StopAfterCurrentRequested,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionProgressSummary {
    pub had_failure: bool,
    pub stopped_after_current: bool,
}

impl ExecutionProgressState {
    pub fn from_execution_plans(plans: Vec<(ManagerId, ResolvedExecutionPlan)>) -> Self {
        let rows = plans
            .into_iter()
            .flat_map(|(manager_id, plan)| {
                plan.intents.into_iter().flat_map(move |intent| {
                    execution_intent_items(intent).into_iter().map({
                        let manager_id = manager_id.clone();
                        move |item| row_from_item(manager_id.clone(), item)
                    })
                })
            })
            .collect();

        Self {
            rows,
            manager_failures: Vec::new(),
            finished: false,
            stop_after_current: false,
        }
    }

    pub fn apply_event(&mut self, event: ExecutionProgressEvent) {
        match event {
            ExecutionProgressEvent::ManagerStarted { manager_id } => {
                for row in self
                    .rows
                    .iter_mut()
                    .filter(|row| row.manager_id == manager_id)
                {
                    if row.status == ExecutionProgressStatus::Pending {
                        row.status = ExecutionProgressStatus::Running;
                    }
                }
            }
            ExecutionProgressEvent::ManagerFinished { report } => {
                for result in report.items {
                    if let Some(row) = self.rows.iter_mut().find(|row| {
                        row.manager_id == report.manager_id
                            && row.plan_item_id == result.plan_item_id
                    }) {
                        row.status = match result.status {
                            ExecutionStatus::Succeeded {
                                command,
                                skipped_mutation,
                            } => ExecutionProgressStatus::Succeeded {
                                command,
                                skipped_mutation,
                            },
                            ExecutionStatus::Failed { command, detail } => {
                                ExecutionProgressStatus::Failed {
                                    detail: format!("{command}: {detail}"),
                                }
                            }
                        };
                    }
                }
            }
            ExecutionProgressEvent::ManagerFailed { manager_id, detail } => {
                self.manager_failures.push(ExecutionProgressManagerFailure {
                    manager_id: manager_id.clone(),
                    detail: detail.clone(),
                });
                for row in self
                    .rows
                    .iter_mut()
                    .filter(|row| row.manager_id == manager_id)
                {
                    if matches!(
                        row.status,
                        ExecutionProgressStatus::Pending | ExecutionProgressStatus::Running
                    ) {
                        row.status = ExecutionProgressStatus::Failed {
                            detail: detail.clone(),
                        };
                    }
                }
            }
            ExecutionProgressEvent::Fatal { detail } => {
                for row in &mut self.rows {
                    if matches!(
                        row.status,
                        ExecutionProgressStatus::Pending | ExecutionProgressStatus::Running
                    ) {
                        row.status = ExecutionProgressStatus::Failed {
                            detail: detail.clone(),
                        };
                    }
                }
            }
            ExecutionProgressEvent::StopAfterCurrentRequested => {
                self.stop_after_current = true;
            }
            ExecutionProgressEvent::Finished => {
                self.finished = true;
                if self.stop_after_current {
                    for row in &mut self.rows {
                        if row.status == ExecutionProgressStatus::Pending {
                            row.status = ExecutionProgressStatus::Skipped {
                                detail: "stopped after current manager".to_owned(),
                            };
                        }
                    }
                }
            }
        }
    }
    pub fn summary(&self) -> ExecutionProgressSummary {
        ExecutionProgressSummary {
            had_failure: !self.manager_failures.is_empty()
                || self
                    .rows
                    .iter()
                    .any(|row| matches!(row.status, ExecutionProgressStatus::Failed { .. })),
            stopped_after_current: self.stop_after_current,
        }
    }
}

impl ExecutionProgressEvent {
    pub const fn manager_started(manager_id: ManagerId) -> Self {
        Self::ManagerStarted { manager_id }
    }
    pub const fn manager_finished(report: ExecutionReport) -> Self {
        Self::ManagerFinished { report }
    }
    pub fn manager_failed(manager_id: ManagerId, detail: impl Into<String>) -> Self {
        Self::ManagerFailed {
            manager_id,
            detail: detail.into(),
        }
    }
    pub fn fatal(detail: impl Into<String>) -> Self {
        Self::Fatal {
            detail: detail.into(),
        }
    }
}

fn execution_intent_items(intent: ExecutionCommandIntent) -> Vec<ResolvedExecutionItem> {
    match intent {
        ExecutionCommandIntent::Exact(item)
        | ExecutionCommandIntent::NativeSelected(item)
        | ExecutionCommandIntent::ResolverNative(item) => vec![item],
        ExecutionCommandIntent::NativeGlobal(items)
        | ExecutionCommandIntent::ResolverNativeGlobal(items) => items,
    }
}

fn row_from_item(manager_id: ManagerId, item: ResolvedExecutionItem) -> ExecutionProgressRow {
    ExecutionProgressRow {
        manager_id,
        plan_item_id: item.plan_item_id,
        package_name: item.package_name,
        installed_version: item.installed_version,
        target_version: item.target_version,
        status: ExecutionProgressStatus::Pending,
    }
}
