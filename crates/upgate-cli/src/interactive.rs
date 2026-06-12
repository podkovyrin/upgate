use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use upgate_audit::AuditService;
use upgate_domain::{ManagerConfig, ManagerId, PlanSelection, UpdatePlan};
use upgate_execution::progress::{
    ExecutionProgressEvent, ExecutionProgressState, ExecutionProgressSummary,
};
use upgate_execution::{
    ResolvedExecutionPlan, execute_commands, execute_commands_stoppable,
    resolve_selection_for_execution,
};
use upgate_infra::{Env, HttpClient, ProcessRunner, run_ordered_parallel_stoppable};
use upgate_presentation::selection_view;
use upgate_presentation::tui::{
    InteractiveManagerSelectionDraft, InteractiveProgressOutcome, InteractiveSelectionOutcome,
    InteractiveSelectionPlanningEvent, run_interactive_progress,
    run_interactive_selection_with_planning_events,
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
    entries: Arc<Mutex<Vec<String>>>,
}

impl InteractiveCommandLog {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    const fn enabled(&self) -> bool {
        self.enabled
    }

    fn snapshot(&self) -> Vec<String> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn record(&self, command: String) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(command);
    }

    fn process_for_selection(
        &self,
        process: &ProcessRunner,
        event_tx: mpsc::Sender<InteractiveSelectionPlanningEvent>,
    ) -> ProcessRunner {
        if !self.enabled {
            return process.clone();
        }

        let command_log = self.clone();
        process.clone().with_command_start_listener(move |command| {
            command_log.record(command.clone());
            let _ = event_tx.send(InteractiveSelectionPlanningEvent::CommandStarted { command });
        })
    }

    fn process_for_progress(
        &self,
        process: &ProcessRunner,
        tx: mpsc::Sender<ExecutionProgressEvent>,
    ) -> ProcessRunner {
        if !self.enabled {
            return process.clone();
        }

        let command_log = self.clone();
        process.clone().with_command_start_listener(move |command| {
            command_log.record(command.clone());
            let _ = tx.send(ExecutionProgressEvent::CommandStarted { command });
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedInteractiveManagerApply {
    pub plan: UpdatePlan,
    pub manager_config: ManagerConfig,
    pub selection: PlanSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractiveApplyReport {
    pub progress: ExecutionProgressState,
    pub summary: ExecutionProgressSummary,
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

/// Runs interactive apply through selection, config persistence, execution, and progress output.
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
            execute_confirmed_interactive_apply_live(config, process, env, confirmed, command_log)?;
            Ok(Some(String::new()))
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

/// Executes confirmed interactive selections and persists selection policy to a specific config.
///
/// # Errors
///
/// Returns an error for config persistence, selection resolution, or interrupted execution.
/// Manager command construction and execution failures are reported in the progress report.
pub fn execute_confirmed_interactive_apply_with_config_path(
    mut config: ConfigFile,
    process: &ProcessRunner,
    env: &Env,
    confirmed: Vec<ConfirmedInteractiveManagerApply>,
    config_path: &Path,
) -> Result<InteractiveApplyReport, AppError> {
    let resolved = resolve_confirmed_execution_plans(&confirmed)?;
    let mut progress = ExecutionProgressState::from_execution_plans(&resolved);
    execute_confirmed_interactive_apply_resolved(
        &mut config,
        process,
        env,
        confirmed,
        &resolved,
        Some(config_path),
        None,
        &mut |event| {
            progress.apply_event(event);
            Ok(())
        },
    )?;
    let summary = progress.summary();
    Ok(InteractiveApplyReport { progress, summary })
}

fn execute_confirmed_interactive_apply_live(
    config: ConfigFile,
    process: &ProcessRunner,
    env: &Env,
    confirmed: Vec<ConfirmedInteractiveManagerApply>,
    command_log: &InteractiveCommandLog,
) -> Result<ExecutionProgressSummary, AppError> {
    let resolved = resolve_confirmed_execution_plans(&confirmed)?;
    let initial_progress = ExecutionProgressState::from_execution_plans(&resolved)
        .with_command_log(command_log.snapshot());
    let (tx, rx) = mpsc::channel();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop_requested);
    let hard_interrupt_requested = Arc::new(AtomicBool::new(false));
    let worker_process = command_log
        .process_for_progress(process, tx.clone())
        .with_interrupt_flag(Arc::clone(&hard_interrupt_requested));

    thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let mut config = config;
            let result = execute_confirmed_interactive_apply_resolved(
                &mut config,
                &worker_process,
                env,
                confirmed,
                &resolved,
                None,
                Some(&worker_stop),
                &mut |event| {
                    tx.send(event).map_err(|err| {
                        AppError::Execution(format!("progress event stream closed: {err}"))
                    })
                },
            );
            if let Err(err) = &result {
                let _ = tx.send(ExecutionProgressEvent::Fatal {
                    detail: err.to_string(),
                });
                let _ = tx.send(ExecutionProgressEvent::Finished);
            }
            result
        });

        let tui_result = run_interactive_progress(
            initial_progress,
            &rx,
            &stop_requested,
            command_log.enabled(),
        )
        .map_err(|err| AppError::Planning(err.to_string()))
        .and_then(|outcome| match outcome {
            InteractiveProgressOutcome::Finished(summary) => Ok(summary),
            InteractiveProgressOutcome::Interrupted => {
                hard_interrupt_requested.store(true, Ordering::Relaxed);
                stop_requested.store(true, Ordering::Relaxed);
                Err(AppError::Interrupted(
                    "interactive apply interrupted".to_owned(),
                ))
            }
        });
        let worker_result = worker
            .join()
            .map_err(|_| AppError::Execution("interactive apply worker panicked".to_owned()))?;

        worker_result?;
        tui_result
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

#[expect(clippy::too_many_arguments)]
fn execute_confirmed_interactive_apply_resolved(
    config: &mut ConfigFile,
    process: &ProcessRunner,
    env: &Env,
    confirmed: Vec<ConfirmedInteractiveManagerApply>,
    resolved: &[(ManagerId, ResolvedExecutionPlan)],
    config_path: Option<&Path>,
    stop_requested: Option<&AtomicBool>,
    emit: &mut impl FnMut(ExecutionProgressEvent) -> Result<(), AppError>,
) -> Result<(), AppError> {
    for manager in &confirmed {
        config.set_manager_selection_policy(
            manager.plan.manager_id.as_str(),
            &manager.selection.selection_policy,
        )?;
        if let Some(path) = config_path {
            config
                .persist_manager_selection_policy_to_path(manager.plan.manager_id.as_str(), path)?;
        } else {
            config.persist_manager_selection_policy(manager.plan.manager_id.as_str())?;
        }
    }

    for manager in confirmed {
        if stop_requested.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            break;
        }

        emit(ExecutionProgressEvent::ManagerStarted {
            manager_id: manager.plan.manager_id.clone(),
        })?;

        let manager_adapter =
            configured_manager(manager.manager_config).map_err(map_manager_error)?;
        let Some((_, execution_plan)) = resolved
            .iter()
            .find(|(manager_id, _)| manager_id == &manager.plan.manager_id)
        else {
            return Err(AppError::Execution(format!(
                "missing execution plan for {}",
                manager.plan.manager_id
            )));
        };
        let commands =
            match manager_adapter.commands_for_execution_plan(process, env, execution_plan) {
                Ok(commands) => commands,
                Err(err) if err.is_interruption() => return Err(map_manager_error(err)),
                Err(err) => {
                    emit(ExecutionProgressEvent::ManagerFailed {
                        manager_id: manager.plan.manager_id,
                        detail: err.to_string(),
                    })?;
                    continue;
                }
            };
        let report = if let Some(stop_requested) = stop_requested {
            execute_commands_stoppable(manager.plan.manager_id, commands, process, stop_requested)
        } else {
            execute_commands(manager.plan.manager_id, commands, process)
        }
        .map_err(|err| {
            if err.is_interruption() {
                AppError::Interrupted(err.to_string())
            } else {
                AppError::Execution(err.to_string())
            }
        })?;
        emit(ExecutionProgressEvent::ManagerFinished { report })?;

        if stop_requested.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            break;
        }
    }

    emit(ExecutionProgressEvent::Finished)?;
    Ok(())
}
