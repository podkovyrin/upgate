use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;

use upgate_audit::AuditService;
use upgate_domain::{ManagerConfig, ManagerId, PlanSelection, UpdatePlan};
use upgate_execution::{
    ExecutionCommand, ExecutionReport, ResolvedExecutionPlan, execute_commands,
    resolve_selection_for_execution,
};
use upgate_infra::{Env, HttpClient, ProcessRunner, run_ordered_parallel_stoppable};
use upgate_presentation::tui::{
    InteractiveManagerSelectionDraft, InteractiveSelectionOutcome,
    InteractiveSelectionPlanningEvent, run_interactive_selection_with_planning_events,
};
use upgate_presentation::{
    OutcomeTable, OutputTheme, apply_execution_report_table, manager_error_table,
    render_batch_table, selection_view,
};

use crate::config::ConfigFile;
use crate::registry::{configured_manager, ensure_known_manager};
use crate::snapshot::write_apply_snapshot_for_selections;
use crate::{
    AppError, build_manager_plan, manager_executable_is_available, manager_mode_allows_run,
    map_manager_error, selected_manager_ids,
};

#[derive(Clone)]
pub struct InteractiveCommandLog {
    enabled: bool,
}

impl InteractiveCommandLog {
    pub const fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    const fn enabled(&self) -> bool {
        self.enabled
    }

    fn process_for_selection(
        &self,
        process: &ProcessRunner,
        event_tx: mpsc::Sender<InteractiveSelectionPlanningEvent>,
    ) -> ProcessRunner {
        if !self.enabled {
            return process.clone();
        }

        process.clone().with_command_start_listener(move |command| {
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::CommandStarted { command });
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedInteractiveManagerApply {
    pub plan: UpdatePlan,
    pub manager_config: ManagerConfig,
    pub selection: PlanSelection,
}

#[derive(Debug, Clone)]
struct PreparedInteractiveApply {
    config: ConfigFile,
    managers: Vec<PreparedInteractiveManagerApply>,
    planning_failures: Vec<(ManagerId, String)>,
}

#[derive(Debug, Clone)]
struct PreparedInteractiveManagerApply {
    plan: UpdatePlan,
    manager_config: ManagerConfig,
}

/// Runs interactive apply through selection, config persistence, and execution.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, terminal, selection, persistence, or
/// interrupted execution failures.
#[expect(clippy::too_many_arguments)]
pub fn run_interactive_apply(
    config: ConfigFile,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    selected_managers: &[String],
    overrides: &[String],
    max_parallel_checks_per_manager: usize,
    manager_concurrency_override: Option<usize>,
    command_log: &InteractiveCommandLog,
    snapshot_log_dir: Option<&Path>,
    theme: OutputTheme,
) -> Result<Option<String>, AppError> {
    let (mut config, manager_configs) =
        prepare_interactive_manager_configs(config, selected_managers, overrides)?;
    let manager_configs = available_manager_configs(manager_configs, env)?;
    if manager_configs.is_empty() {
        return Ok(Some(String::new()));
    }
    if let Some(manager_concurrency) = manager_concurrency_override {
        config.set_manager_concurrency(manager_concurrency)?;
    }
    let manager_concurrency = config.manager_concurrency()?;
    let audit_service = AuditService::new(http.clone(), env, config.audit_concurrency()?);
    match run_live_confirmed_selection(
        config,
        manager_configs,
        process,
        http,
        &audit_service,
        env,
        max_parallel_checks_per_manager,
        manager_concurrency,
        command_log,
    )? {
        Some((config, confirmed)) => {
            if let Some(log_dir) = snapshot_log_dir {
                write_apply_snapshot_for_selections(
                    confirmed
                        .iter()
                        .map(|manager| (&manager.plan, &manager.selection)),
                    log_dir,
                )?;
            }
            let output = execute_confirmed_interactive_apply_streaming(
                config, process, env, confirmed, theme,
            )?;
            Ok(Some(output))
        }
        None => Ok(None),
    }
}

fn prepare_interactive_manager_configs(
    mut config: ConfigFile,
    selected_managers: &[String],
    overrides: &[String],
) -> Result<(ConfigFile, Vec<ManagerConfig>), AppError> {
    if !selected_managers.is_empty() {
        config.apply_selected_managers_cli_override(selected_managers)?;
    }
    for override_value in overrides {
        config.apply_cli_override(override_value)?;
    }
    let manager_ids = selected_manager_ids(selected_managers)?;
    let mut manager_configs = Vec::new();
    for manager_id in manager_ids {
        ensure_known_manager(manager_id.as_str()).map_err(map_manager_error)?;
        let manager_config = config.resolve_manager(manager_id.as_str())?;
        if !manager_mode_allows_run(manager_config.mode, true) {
            continue;
        }
        manager_configs.push(manager_config);
    }
    Ok((config, manager_configs))
}

fn available_manager_configs(
    manager_configs: Vec<ManagerConfig>,
    env: &Env,
) -> Result<Vec<ManagerConfig>, AppError> {
    manager_configs
        .into_iter()
        .filter_map(|manager_config| {
            match manager_executable_is_available(&manager_config.manager_id, env) {
                Ok(true) => Some(Ok(manager_config)),
                Ok(false) => None,
                Err(err) => Some(Err(err)),
            }
        })
        .collect()
}

#[expect(clippy::too_many_arguments)]
fn run_live_confirmed_selection(
    config: ConfigFile,
    manager_configs: Vec<ManagerConfig>,
    process: &ProcessRunner,
    http: &HttpClient,
    audit_service: &AuditService,
    env: &Env,
    max_parallel_checks_per_manager: usize,
    manager_concurrency: usize,
    command_log: &InteractiveCommandLog,
) -> Result<Option<(ConfigFile, Vec<ConfirmedInteractiveManagerApply>)>, AppError> {
    let manager_ids = manager_configs
        .iter()
        .map(|manager_config| manager_config.manager_id.clone())
        .collect::<Vec<_>>();
    let (event_tx, event_rx) = mpsc::channel();
    let (prepared_tx, prepared_rx) = mpsc::channel();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop_requested);
    let process = command_log.process_for_selection(process, event_tx.clone());
    let http = http.clone();
    let audit_service = audit_service.clone();
    let env = env.clone();
    let worker = thread::spawn(move || {
        let prepared = prepare_interactive_apply_with_events(
            config,
            manager_configs,
            &process,
            &http,
            &audit_service,
            &env,
            max_parallel_checks_per_manager,
            manager_concurrency,
            &event_tx,
            &worker_stop,
        );
        if let Err(err) = &prepared {
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::PlanningFailed {
                detail: err.to_string(),
            });
        }
        let _ = prepared_tx.send(prepared);
    });

    let outcome = match run_interactive_selection_with_planning_events(
        manager_ids,
        event_rx,
        command_log.enabled(),
    ) {
        Ok(outcome) => outcome,
        Err(err) => {
            stop_requested.store(true, Ordering::Relaxed);
            worker.join().map_err(|_| {
                AppError::Planning("interactive planning worker panicked".to_owned())
            })?;
            return Err(AppError::Planning(err.to_string()));
        }
    };

    match outcome {
        InteractiveSelectionOutcome::Cancelled => {
            stop_requested.store(true, Ordering::Relaxed);
            Ok(None)
        }
        InteractiveSelectionOutcome::Interrupted => {
            stop_requested.store(true, Ordering::Relaxed);
            Err(AppError::Interrupted(
                "interactive selection interrupted".to_owned(),
            ))
        }
        InteractiveSelectionOutcome::Confirmed(drafts) => {
            let prepared = prepared_rx
                .recv()
                .map_err(|err| AppError::Planning(err.to_string()))??;
            worker.join().map_err(|_| {
                AppError::Planning("interactive planning worker panicked".to_owned())
            })?;
            confirmed_from_drafts(prepared, &drafts).map(Some)
        }
    }
}

#[expect(clippy::too_many_arguments)]
fn prepare_interactive_apply_with_events(
    config: ConfigFile,
    manager_configs: Vec<ManagerConfig>,
    process: &ProcessRunner,
    http: &HttpClient,
    audit_service: &AuditService,
    env: &Env,
    max_parallel_checks_per_manager: usize,
    manager_concurrency: usize,
    event_tx: &mpsc::Sender<InteractiveSelectionPlanningEvent>,
    stop_requested: &AtomicBool,
) -> Result<PreparedInteractiveApply, AppError> {
    let worker_results = run_interactive_planning_workers(
        manager_configs,
        process,
        http,
        audit_service,
        env,
        max_parallel_checks_per_manager,
        manager_concurrency,
        event_tx,
        stop_requested,
    )?;

    let mut managers = Vec::new();
    let mut planning_failures = Vec::new();
    for result in worker_results {
        match result {
            InteractivePlanningWorkerResult::Ready { manager } => managers.push(manager),
            InteractivePlanningWorkerResult::Failed { manager_id, detail } => {
                planning_failures.push((manager_id, detail));
            }
        }
    }

    if !stop_requested.load(Ordering::Relaxed) {
        let _ = event_tx.send(InteractiveSelectionPlanningEvent::Finished);
    }
    Ok(PreparedInteractiveApply {
        config,
        managers,
        planning_failures,
    })
}

enum InteractivePlanningWorkerResult {
    Ready {
        manager: PreparedInteractiveManagerApply,
    },
    Failed {
        manager_id: ManagerId,
        detail: String,
    },
}

#[expect(clippy::too_many_arguments)]
fn run_interactive_planning_workers(
    manager_configs: Vec<ManagerConfig>,
    process: &ProcessRunner,
    http: &HttpClient,
    audit_service: &AuditService,
    env: &Env,
    max_parallel_checks_per_manager: usize,
    manager_concurrency: usize,
    event_tx: &mpsc::Sender<InteractiveSelectionPlanningEvent>,
    stop_requested: &AtomicBool,
) -> Result<Vec<InteractivePlanningWorkerResult>, AppError> {
    run_ordered_parallel_stoppable(
        manager_configs,
        manager_concurrency,
        "interactive planning managers",
        stop_requested,
        |manager_config| {
            prepare_one_interactive_manager_with_events(
                manager_config,
                process,
                http,
                audit_service,
                env,
                max_parallel_checks_per_manager,
                event_tx,
            )
        },
        |result| result.as_ref().is_err_and(AppError::is_interruption),
    )
    .map_err(|err| AppError::Execution(err.to_string()))?
    .into_iter()
    .collect::<Result<Vec<_>, AppError>>()
}

fn prepare_one_interactive_manager_with_events(
    manager_config: ManagerConfig,
    process: &ProcessRunner,
    http: &HttpClient,
    audit_service: &AuditService,
    env: &Env,
    max_parallel_checks_per_manager: usize,
    event_tx: &mpsc::Sender<InteractiveSelectionPlanningEvent>,
) -> Result<InteractivePlanningWorkerResult, AppError> {
    let manager_id = manager_config.manager_id.clone();
    let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerStarted {
        manager_id: manager_id.clone(),
    });

    let manager = match configured_manager(manager_config.clone()).map_err(map_manager_error) {
        Ok(manager) => manager,
        Err(err) if err.is_interruption() => return Err(err),
        Err(err) => {
            let detail = err.to_string();
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerError {
                manager_id: manager_id.clone(),
                detail: detail.clone(),
            });
            return Ok(InteractivePlanningWorkerResult::Failed { manager_id, detail });
        }
    };

    match build_manager_plan(
        manager.as_ref(),
        process,
        http,
        audit_service,
        env,
        &manager_config,
        max_parallel_checks_per_manager,
    ) {
        Ok(plan) => {
            let selection_policy = manager_config.selection.clone();
            let view = selection_view(&plan, &selection_policy);
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerReady {
                view,
                selection_policy,
                version_policy: manager_config.version_policy,
            });
            Ok(InteractivePlanningWorkerResult::Ready {
                manager: PreparedInteractiveManagerApply {
                    plan,
                    manager_config,
                },
            })
        }
        Err(err) if err.is_interruption() => Err(err),
        Err(err) => {
            let detail = err.to_string();
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::ManagerError {
                manager_id: manager_id.clone(),
                detail: detail.clone(),
            });
            Ok(InteractivePlanningWorkerResult::Failed { manager_id, detail })
        }
    }
}

fn confirmed_from_drafts(
    prepared: PreparedInteractiveApply,
    drafts: &[InteractiveManagerSelectionDraft],
) -> Result<(ConfigFile, Vec<ConfirmedInteractiveManagerApply>), AppError> {
    if !prepared.planning_failures.is_empty() {
        let details = prepared
            .planning_failures
            .iter()
            .map(|(manager_id, detail)| format!("{manager_id}: {detail}"))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(AppError::Planning(details));
    }

    let mut confirmed = Vec::new();
    for manager in prepared.managers {
        let Some(draft) = drafts
            .iter()
            .find(|draft| draft.manager_id == manager.plan.manager_id)
        else {
            return Err(AppError::Planning(format!(
                "missing interactive selection for {}",
                manager.plan.manager_id
            )));
        };
        let selection = PlanSelection::new(
            &manager.plan,
            draft.selected_items.clone(),
            draft.selection_policy.clone(),
        )
        .map_err(|err| AppError::Planning(err.to_string()))?;
        confirmed.push(ConfirmedInteractiveManagerApply {
            plan: manager.plan,
            manager_config: manager.manager_config,
            selection,
        });
    }

    Ok((prepared.config, confirmed))
}

/// Executes confirmed interactive selections, streaming each command to the
/// terminal as it runs and returning the rendered result table.
///
/// Commands run with their output captured to the command log. The terminal is
/// left in normal mode so a command that needs elevated privileges can prompt
/// for a password directly.
fn execute_confirmed_interactive_apply_streaming(
    mut config: ConfigFile,
    process: &ProcessRunner,
    env: &Env,
    confirmed: Vec<ConfirmedInteractiveManagerApply>,
    theme: OutputTheme,
) -> Result<String, AppError> {
    let resolved = resolve_confirmed_execution_plans(&confirmed)?;
    persist_confirmed_selection_policies(&mut config, &confirmed)?;

    let process = process.clone().with_command_start_listener(|command| {
        eprintln!("$ {command}");
    });

    let mut table = OutcomeTable::default();
    for manager in confirmed {
        let manager_id = manager.plan.manager_id.clone();
        let execution_plan = execution_plan_for(&resolved, &manager_id)?;
        let adapter =
            configured_manager(manager.manager_config.clone()).map_err(map_manager_error)?;
        match adapter.commands_for_execution_plan(&process, env, execution_plan) {
            Ok(commands) => {
                let report = run_manager_commands(manager_id, commands, &process)?;
                table.rows.extend(
                    apply_execution_report_table(&report, &manager.plan, &manager.selection).rows,
                );
            }
            Err(err) if err.is_interruption() => return Err(map_manager_error(err)),
            Err(err) => {
                table
                    .rows
                    .extend(manager_error_table(&manager_id, "apply", &err.to_string()).rows);
            }
        }
    }
    Ok(render_batch_table(&table, theme))
}

fn persist_confirmed_selection_policies(
    config: &mut ConfigFile,
    confirmed: &[ConfirmedInteractiveManagerApply],
) -> Result<(), AppError> {
    for manager in confirmed {
        config.set_manager_selection_policy(
            manager.plan.manager_id.as_str(),
            &manager.selection.selection_policy,
        )?;
        config.persist_manager_selection_policy(manager.plan.manager_id.as_str())?;
    }
    Ok(())
}

fn execution_plan_for<'a>(
    resolved: &'a [(ManagerId, ResolvedExecutionPlan)],
    manager_id: &ManagerId,
) -> Result<&'a ResolvedExecutionPlan, AppError> {
    resolved
        .iter()
        .find(|(id, _)| id == manager_id)
        .map(|(_, plan)| plan)
        .ok_or_else(|| AppError::Execution(format!("missing execution plan for {manager_id}")))
}

fn run_manager_commands(
    manager_id: ManagerId,
    commands: Vec<ExecutionCommand>,
    process: &ProcessRunner,
) -> Result<ExecutionReport, AppError> {
    execute_commands(manager_id, commands, process).map_err(|err| {
        if err.is_interruption() {
            AppError::Interrupted(err.to_string())
        } else {
            AppError::Execution(err.to_string())
        }
    })
}

fn resolve_confirmed_execution_plans(
    confirmed: &[ConfirmedInteractiveManagerApply],
) -> Result<Vec<(ManagerId, ResolvedExecutionPlan)>, AppError> {
    let mut resolved = Vec::new();
    for manager in confirmed {
        let execution_plan = resolve_selection_for_execution(
            &manager.plan,
            &manager.selection,
            configured_manager(manager.manager_config.clone())
                .map_err(map_manager_error)?
                .capabilities(),
            manager.manager_config.version_policy,
        )
        .map_err(|err| AppError::Manager(err.to_string()))?;
        resolved.push((manager.plan.manager_id.clone(), execution_plan));
    }
    Ok(resolved)
}
