use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use upgate_audit::AuditService;
use upgate_domain::{
    AuditLookupResult, AuditQuery, InstalledTool, ManagerConfig, ManagerId,
    ManagerScanEvidenceInput, ManagerScanInput, PlanSelection, ScanIssue, ScanItem, ScanReport,
    UpdatePlan,
};
use upgate_execution::{ResolvedExecutionPlan, execute_commands, resolve_selection_for_execution};
use upgate_infra::{Env, HttpClient, ProcessRunner, run_ordered_parallel_stoppable};
use upgate_managers::adapter::ManagerAdapter;
use upgate_planning::default_batch_selection;
use upgate_presentation::{
    BatchRenderOptions, OutcomeTable, OutputTheme, apply_execution_report_table,
    manager_error_table, render_batch_table, scan_report_table, terminal::BatchTerminal,
    update_plan_table,
};

use crate::config::ConfigFile;
use crate::registry::{configured_manager, ensure_known_manager};
use crate::snapshot::write_apply_snapshot_for_selections;
use crate::{
    AppError, CliCommand, build_manager_plan, execution_report_has_failures,
    manager_executable_is_available, manager_mode_allows_run, map_manager_error,
    selected_manager_ids,
};

/// Runs a batch command for the managers selected by config and args.
///
/// # Errors
///
/// Returns an error for config, discovery, planning, or execution failures.
#[expect(clippy::too_many_arguments)]
pub fn run_batch(
    command: CliCommand,
    mut config: ConfigFile,
    process: &ProcessRunner,
    http: &HttpClient,
    env: &Env,
    theme: OutputTheme,
    terminal: BatchTerminal,
    max_parallel_checks_per_manager: usize,
    selected_managers: &[String],
    overrides: &[String],
    manager_concurrency_override: Option<usize>,
    snapshot_log_dir: Option<&Path>,
) -> Result<String, AppError> {
    if !selected_managers.is_empty() {
        config.apply_selected_managers_cli_override(selected_managers)?;
    }
    for override_value in overrides {
        config.apply_cli_override(override_value)?;
    }
    if let Some(manager_concurrency) = manager_concurrency_override {
        config.set_manager_concurrency(manager_concurrency)?;
    }
    let manager_ids = selected_manager_ids(selected_managers)?;
    if command == CliCommand::Apply && !any_runnable_manager(&config, env, &manager_ids)? {
        return Ok(String::new());
    }
    let manager_concurrency = config.manager_concurrency()?;
    let audit_service = AuditService::new(http.clone(), env, config.audit_concurrency()?);
    run_batch_for_managers(
        command,
        &config,
        process,
        http,
        &audit_service,
        env,
        theme,
        terminal,
        max_parallel_checks_per_manager,
        manager_concurrency,
        manager_ids,
        snapshot_log_dir,
    )
}

fn any_runnable_manager(
    config: &ConfigFile,
    env: &Env,
    manager_ids: &[ManagerId],
) -> Result<bool, AppError> {
    let mut any_runnable = false;
    // No early break: every manager is still resolved and probed so that a
    // later manager's config/availability error propagates exactly as before.
    for manager_id in manager_ids {
        let manager_config = config.resolve_manager(manager_id.as_str())?;
        if manager_mode_allows_run(manager_config.mode, true)
            && manager_executable_is_available(manager_id, env)?
        {
            any_runnable = true;
        }
    }
    Ok(any_runnable)
}

#[expect(clippy::too_many_arguments)]
fn run_batch_for_managers(
    command: CliCommand,
    config: &ConfigFile,
    process: &ProcessRunner,
    http: &HttpClient,
    audit_service: &AuditService,
    env: &Env,
    theme: OutputTheme,
    terminal: BatchTerminal,
    max_parallel_checks_per_manager: usize,
    manager_concurrency: usize,
    manager_ids: Vec<ManagerId>,
    snapshot_log_dir: Option<&Path>,
) -> Result<String, AppError> {
    let _spinner = terminal.start_action_spinner(command.terminal_action());
    if command == CliCommand::Apply {
        return run_batch_apply_for_managers(
            config,
            process,
            http,
            audit_service,
            env,
            theme,
            max_parallel_checks_per_manager,
            manager_concurrency,
            manager_ids,
            snapshot_log_dir,
        );
    }

    let stop_requested = AtomicBool::new(false);
    let manager_outputs = run_ordered_parallel_stoppable(
        manager_ids,
        manager_concurrency,
        &format!("{command} managers"),
        &stop_requested,
        |manager_id| {
            let result = run_manager_scan_or_plan_batch(
                command,
                config,
                process,
                http,
                audit_service,
                env,
                theme,
                &manager_id,
                max_parallel_checks_per_manager,
            );
            (manager_id, result)
        },
        |(_, result)| result.as_ref().is_err_and(AppError::is_interruption),
    )
    .map_err(|err| AppError::Execution(err.to_string()))?;

    let mut table = OutcomeTable::default();
    let mut had_error = false;
    let command_label = command.to_string();
    for (manager_id, manager_output) in manager_outputs {
        match manager_output {
            Ok(manager_output) => {
                had_error |= manager_output.failed;
                table.rows.extend(manager_output.table.rows);
            }
            Err(err) if err.is_interruption() => return Err(err),
            Err(err) => {
                had_error = true;
                table.rows.extend(
                    manager_error_table(&manager_id, &command_label, &err.to_string()).rows,
                );
            }
        }
    }
    let output = render_batch_table(&table, theme);
    if had_error {
        Err(AppError::Manager(output))
    } else {
        Ok(with_success_suggestion(output, command))
    }
}

fn with_success_suggestion(mut output: String, command: CliCommand) -> String {
    let suggestion = match command {
        CliCommand::Scan => {
            "To preview available updates, run:\nupgate plan\n\nTo choose updates interactively, run:\nupgate apply\n"
        }
        CliCommand::Plan => "To choose and apply updates interactively, run:\nupgate apply\n",
        CliCommand::Apply => return output,
    };

    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(suggestion);
    output
}

struct ManagerBatchOutput {
    table: OutcomeTable,
    failed: bool,
}

struct PreparedBatchApply {
    plan: UpdatePlan,
    manager_config: ManagerConfig,
    selection: PlanSelection,
    execution_plan: ResolvedExecutionPlan,
}

#[expect(clippy::too_many_arguments)]
fn run_manager_scan_or_plan_batch(
    command: CliCommand,
    config: &ConfigFile,
    process: &ProcessRunner,
    http: &HttpClient,
    audit_service: &AuditService,
    env: &Env,
    theme: OutputTheme,
    manager_id: &ManagerId,
    max_parallel_checks_per_manager: usize,
) -> Result<ManagerBatchOutput, AppError> {
    ensure_known_manager(manager_id.as_str()).map_err(map_manager_error)?;
    let manager_config = config.resolve_manager(manager_id.as_str())?;
    if !manager_mode_allows_run(manager_config.mode, command == CliCommand::Apply)
        || !manager_executable_is_available(manager_id, env)?
    {
        return Ok(ManagerBatchOutput {
            table: OutcomeTable::default(),
            failed: false,
        });
    }

    match command {
        CliCommand::Scan if theme.verbose => {
            let manager = configured_manager(manager_config).map_err(map_manager_error)?;
            Ok(ManagerBatchOutput {
                table: scan_report_table(&build_verbose_scan_report(
                    manager_id.clone(),
                    manager.as_ref(),
                    process,
                    http,
                    audit_service,
                    env,
                    SystemTime::now(),
                    max_parallel_checks_per_manager,
                )?),
                failed: false,
            })
        }
        CliCommand::Scan => {
            let manager = configured_manager(manager_config).map_err(map_manager_error)?;
            Ok(ManagerBatchOutput {
                table: scan_report_table(&build_scan_report(
                    manager_id.clone(),
                    manager.as_ref(),
                    process,
                    env,
                )?),
                failed: false,
            })
        }
        CliCommand::Plan => {
            let manager = configured_manager(manager_config.clone()).map_err(map_manager_error)?;
            let plan = build_manager_plan(
                manager.as_ref(),
                process,
                http,
                audit_service,
                env,
                &manager_config,
                max_parallel_checks_per_manager,
            )?;
            Ok(ManagerBatchOutput {
                table: update_plan_table(
                    &plan,
                    BatchRenderOptions::new(theme)
                        .with_version_policy(manager_config.version_policy),
                ),
                failed: false,
            })
        }
        CliCommand::Apply => Err(AppError::Execution(
            "batch apply must be prepared before serial execution".to_owned(),
        )),
    }
}

#[expect(clippy::too_many_arguments)]
fn run_batch_apply_for_managers(
    config: &ConfigFile,
    process: &ProcessRunner,
    http: &HttpClient,
    audit_service: &AuditService,
    env: &Env,
    theme: OutputTheme,
    max_parallel_checks_per_manager: usize,
    manager_concurrency: usize,
    manager_ids: Vec<ManagerId>,
    snapshot_log_dir: Option<&Path>,
) -> Result<String, AppError> {
    let stop_requested = AtomicBool::new(false);
    let prepared = run_ordered_parallel_stoppable(
        manager_ids,
        manager_concurrency,
        "apply planning managers",
        &stop_requested,
        |manager_id| {
            let result = prepare_manager_batch_apply(
                config,
                process,
                http,
                audit_service,
                env,
                &manager_id,
                max_parallel_checks_per_manager,
            );
            (manager_id, result)
        },
        |(_, result)| result.as_ref().is_err_and(AppError::is_interruption),
    )
    .map_err(|err| AppError::Execution(err.to_string()))?;

    if let Some(log_dir) = snapshot_log_dir {
        write_apply_snapshot_for_selections(
            prepared.iter().filter_map(|(_, prepared_manager)| {
                if let Ok(Some(prepared_manager)) = prepared_manager {
                    Some((&prepared_manager.plan, &prepared_manager.selection))
                } else {
                    None
                }
            }),
            log_dir,
        )?;
    }

    let mut table = OutcomeTable::default();
    let mut had_error = false;
    for (manager_id, prepared_manager) in prepared {
        let outcome = match prepared_manager {
            Ok(None) => continue,
            Ok(Some(prepared_manager)) => {
                execute_prepared_batch_apply(process, env, prepared_manager)
            }
            Err(err) => Err(err),
        };
        match outcome {
            Ok(manager_output) => {
                had_error |= manager_output.failed;
                table.rows.extend(manager_output.table.rows);
            }
            Err(err) if err.is_interruption() => return Err(err),
            Err(err) => {
                had_error = true;
                table
                    .rows
                    .extend(manager_error_table(&manager_id, "apply", &err.to_string()).rows);
            }
        }
    }

    let output = render_batch_table(&table, theme);
    if had_error {
        Err(AppError::Manager(output))
    } else {
        Ok(output)
    }
}

fn prepare_manager_batch_apply(
    config: &ConfigFile,
    process: &ProcessRunner,
    http: &HttpClient,
    audit_service: &AuditService,
    env: &Env,
    manager_id: &ManagerId,
    max_parallel_checks_per_manager: usize,
) -> Result<Option<PreparedBatchApply>, AppError> {
    ensure_known_manager(manager_id.as_str()).map_err(map_manager_error)?;
    let manager_config = config.resolve_manager(manager_id.as_str())?;
    if !manager_mode_allows_run(manager_config.mode, true)
        || !manager_executable_is_available(manager_id, env)?
    {
        return Ok(None);
    }

    let manager = configured_manager(manager_config.clone()).map_err(map_manager_error)?;
    let plan = build_manager_plan(
        manager.as_ref(),
        process,
        http,
        audit_service,
        env,
        &manager_config,
        max_parallel_checks_per_manager,
    )?;
    let selection = default_batch_selection(&plan, &manager_config.selection)
        .map_err(|err| AppError::Planning(err.to_string()))?;
    let execution_plan = resolve_selection_for_execution(
        &plan,
        &selection,
        manager.capabilities(),
        manager_config.version_policy,
    )
    .map_err(|err| AppError::Manager(err.to_string()))?;

    Ok(Some(PreparedBatchApply {
        plan,
        manager_config,
        selection,
        execution_plan,
    }))
}

fn execute_prepared_batch_apply(
    process: &ProcessRunner,
    env: &Env,
    prepared: PreparedBatchApply,
) -> Result<ManagerBatchOutput, AppError> {
    let manager = configured_manager(prepared.manager_config).map_err(map_manager_error)?;
    let commands = manager
        .commands_for_execution_plan(process, env, &prepared.execution_plan)
        .map_err(map_manager_error)?;
    let report = execute_commands(prepared.plan.manager_id.clone(), commands, process, None)
        .map_err(|err| {
            if err.is_interruption() {
                AppError::Interrupted(err.to_string())
            } else {
                AppError::Execution(err.to_string())
            }
        })?;
    Ok(ManagerBatchOutput {
        table: apply_execution_report_table(&report, &prepared.plan, &prepared.selection),
        failed: execution_report_has_failures(&report),
    })
}

fn build_scan_report(
    manager_id: ManagerId,
    manager: &dyn ManagerAdapter,
    process: &ProcessRunner,
    env: &Env,
) -> Result<ScanReport, AppError> {
    match manager.scan_inputs(process, env) {
        Ok(inputs) => Ok(ScanReport::new(
            manager_id,
            inputs.into_iter().map(scan_item_from_input).collect(),
            Vec::new(),
        )),
        Err(err) if err.is_interruption() => Err(map_manager_error(err)),
        Err(err) => Ok(ScanReport::new(
            manager_id,
            Vec::new(),
            vec![ScanIssue::DiscoveryFailed {
                detail: err.to_string(),
            }],
        )),
    }
}

#[expect(clippy::too_many_arguments)]
fn build_verbose_scan_report(
    manager_id: ManagerId,
    manager: &dyn ManagerAdapter,
    process: &ProcessRunner,
    http: &HttpClient,
    audit_service: &AuditService,
    env: &Env,
    now: SystemTime,
    max_parallel_checks_per_manager: usize,
) -> Result<ScanReport, AppError> {
    match manager.scan_inputs_with_release_evidence(
        process,
        http,
        env,
        max_parallel_checks_per_manager,
    ) {
        Ok(inputs) => {
            let audit_results = audit_service
                .query(scan_audit_queries(&inputs))
                .map_err(|err| AppError::Planning(err.to_string()))?;
            Ok(ScanReport::new(
                manager_id,
                inputs
                    .into_iter()
                    .map(|input| {
                        scan_item_from_evidence_input_with_audit(input, now, &audit_results)
                    })
                    .collect(),
                Vec::new(),
            ))
        }
        Err(err) if err.is_interruption() => Err(map_manager_error(err)),
        Err(err) => Ok(ScanReport::new(
            manager_id,
            Vec::new(),
            vec![ScanIssue::DiscoveryFailed {
                detail: err.to_string(),
            }],
        )),
    }
}

fn scan_item_from_evidence_input_with_audit(
    input: ManagerScanEvidenceInput,
    now: SystemTime,
    audit_results: &BTreeMap<AuditQuery, AuditLookupResult>,
) -> ScanItem {
    match input {
        ManagerScanEvidenceInput::Installed {
            tool,
            release_evidence,
        } => {
            let age = release_evidence.map(|evidence| {
                now.duration_since(evidence.published_at.as_system_time())
                    .unwrap_or(Duration::ZERO)
            });
            scan_item_with_audit(tool, age, audit_results)
        }
        ManagerScanEvidenceInput::Skipped { installed, reason } => ScanItem::Skipped {
            tool: installed,
            reason,
        },
    }
}

fn scan_audit_queries(
    inputs: &[ManagerScanEvidenceInput],
) -> impl Iterator<Item = AuditQuery> + '_ {
    inputs.iter().filter_map(|input| match input {
        ManagerScanEvidenceInput::Installed { tool, .. } => tool
            .audit_subject
            .as_ref()
            .map(|subject| AuditQuery::new(subject.clone(), tool.installed_version.clone())),
        ManagerScanEvidenceInput::Skipped { .. } => None,
    })
}

fn scan_item_with_audit(
    tool: InstalledTool,
    age: Option<Duration>,
    audit_results: &BTreeMap<AuditQuery, AuditLookupResult>,
) -> ScanItem {
    let Some(subject) = tool.audit_subject.as_ref() else {
        return match age {
            Some(age) => ScanItem::InstalledWithReleaseAge { tool, age },
            None => ScanItem::Installed(tool),
        };
    };
    let query = AuditQuery::new(subject.clone(), tool.installed_version.clone());
    let audit =
        audit_results
            .get(&query)
            .cloned()
            .unwrap_or_else(|| AuditLookupResult::LookupFailed {
                detail: "audit lookup result missing".to_owned(),
            });
    ScanItem::InstalledWithAudit { tool, age, audit }
}

fn scan_item_from_input(input: ManagerScanInput) -> ScanItem {
    match input {
        ManagerScanInput::Installed(tool) => ScanItem::Installed(tool),
        ManagerScanInput::Skipped { installed, reason } => ScanItem::Skipped {
            tool: installed,
            reason,
        },
    }
}
