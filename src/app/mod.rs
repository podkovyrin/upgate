pub mod cli;

use anyhow::{Error, Result};
use clap::Parser;

use crate::config::UpnowConfig;
use crate::interactive;
use crate::managers::{
    ManagerCtx, ManagerPlugin, RunMode, all_plugins, build_ctx_for_plugin, resolve_selected_plugins,
};
use crate::outcome::{
    ItemOutcome, ReasonCode, drain_text_outcomes, emit_text_outcome, flush_text_outcomes,
};
use crate::ui::{
    TerminalOutputSuppression, finish_manager_spinner, init_output_theme, start_manager_spinner,
    terminal_output_suppressed, with_spinner_suspended,
};
use crate::util::logging::{
    LoggingOptions, init_logging, log_warning, session_dir, set_current_manager,
};
use crate::util::process::{
    self, CommandFailedError, MUTATION_ENABLE_NOTICE, MUTATION_SKIP_NOTICE,
};

pub fn run() -> i32 {
    let cli = cli::Cli::parse();
    init_output_theme(cli.plain, cli.no_color, cli.verbose);

    if let Err(err) = init_command_logging(&cli) {
        return exit_with_error(format!("failed to initialize logging: {err}"));
    }

    let interactive_apply = cli.interactive && cli.run_mode().is_apply();
    let result = if interactive_apply {
        let _terminal_output_suppression = TerminalOutputSuppression::enter();
        let result = run_with_cli(&cli);
        let _ = drain_text_outcomes();
        result
    } else {
        let result = run_with_cli(&cli);
        flush_text_outcomes();
        result
    };

    match result {
        Ok(exit_code) => exit_code,
        Err(err) => exit_with_error(err),
    }
}

fn init_command_logging(cli: &cli::Cli) -> Result<()> {
    init_logging(LoggingOptions {
        debug_commands: cli.debug_commands,
        show_commands: cli.show_commands,
    })?;

    if cli.debug_commands
        && !(cli.interactive && cli.run_mode().is_apply())
        && let Some(path) = session_dir()
    {
        eprintln!("debug logs: {}", path.display());
    }

    Ok(())
}

fn run_with_cli(cli: &cli::Cli) -> Result<i32> {
    process::set_debug_force_skip_mutating_commands(cli.debug_no_mutate());

    let run_mode = cli.run_mode();
    validate_interactive_mode(cli.interactive, run_mode)?;
    maybe_emit_apply_mutation_mode_notice(run_mode)?;

    let mut config = UpnowConfig::load()?;
    let selected_plugins = resolve_selected_plugins(&cli.managers)?;
    config.apply_selected_managers_cli_override(&cli.managers);
    apply_config_overrides(&mut config, &cli.set)?;

    Ok(run_selected_plugins(
        cli,
        run_mode,
        &mut config,
        selected_plugins,
    ))
}

fn maybe_emit_apply_mutation_mode_notice(run_mode: RunMode) -> Result<()> {
    if !run_mode.is_apply() {
        return Ok(());
    }

    process::validate_required_mutation_mode()?;

    if !process::mutation_mode_notice_enabled() || terminal_output_suppressed() {
        return Ok(());
    }

    with_spinner_suspended(|| {
        if process::mutating_commands_are_skipped() {
            eprintln!("note: apply runs in safe mode: {MUTATION_SKIP_NOTICE}");
        } else {
            eprintln!("warning: apply runs with {MUTATION_ENABLE_NOTICE}");
        }
    });

    Ok(())
}

fn validate_interactive_mode(interactive: bool, run_mode: RunMode) -> Result<()> {
    if !interactive {
        return Ok(());
    }

    if !run_mode.is_apply() {
        anyhow::bail!("--interactive is only supported with 'apply'");
    }

    interactive::ensure_tty_available()
}

fn apply_config_overrides<S: AsRef<str>>(config: &mut UpnowConfig, overrides: &[S]) -> Result<()> {
    let known_manager_ids: Vec<&str> = all_plugins().iter().map(|p| p.id()).collect();
    for override_arg in overrides {
        config.apply_cli_override(override_arg.as_ref(), &known_manager_ids)?;
    }

    Ok(())
}

fn run_selected_plugins(
    cli: &cli::Cli,
    run_mode: RunMode,
    config: &mut UpnowConfig,
    selected_plugins: Vec<&'static dyn ManagerPlugin>,
) -> i32 {
    if cli.interactive && run_mode.is_apply() {
        return run_interactive_apply_selected_plugins(cli, run_mode, config, selected_plugins);
    }

    let mut had_manager_failure = false;

    for plugin in selected_plugins {
        set_current_manager(Some(plugin.id()));

        let manager_ctx = match build_ctx_for_plugin(
            plugin,
            run_mode,
            cli.max_parallel_checks,
            config,
            cli.interactive,
        ) {
            Ok(ctx) => ctx,
            Err(err) => {
                eprintln!("error: manager '{}' setup failed: {err}", plugin.id());
                had_manager_failure = true;
                continue;
            }
        };

        if !manager_ctx.policy.mode.allows_run(run_mode.is_apply()) {
            continue;
        }

        if !plugin.supports_current_platform() {
            emit_manager_preflight_skip(
                plugin.id(),
                ReasonCode::UnsupportedPlatform,
                plugin.unsupported_platform_reason(),
            );
            continue;
        }

        if let Some(command) = plugin.probe_command()
            && !process::command_exists(&command)
        {
            emit_manager_preflight_skip(
                plugin.id(),
                ReasonCode::MissingCommand,
                format!("required command '{command}' is not available"),
            );
            continue;
        }

        let spinner = start_manager_spinner(plugin.id(), run_mode);
        let run_result = plugin.run(&manager_ctx);
        finish_manager_spinner(spinner);

        if cli.interactive && run_mode.is_apply() {
            had_manager_failure |= persist_interactive_pins(config, plugin.id(), &manager_ctx);
        }

        if let Err(err) = run_result {
            eprintln!("error: manager '{}' failed: {err}", plugin.id());
            if is_signal_termination(&err) {
                set_current_manager(None);
                return 130;
            }
            had_manager_failure = true;
        }
    }

    set_current_manager(None);
    i32::from(had_manager_failure)
}

fn run_interactive_apply_selected_plugins(
    cli: &cli::Cli,
    run_mode: RunMode,
    config: &mut UpnowConfig,
    selected_plugins: Vec<&'static dyn ManagerPlugin>,
) -> i32 {
    let planning_tasks = interactive_apply_planning_tasks(cli, run_mode, config, selected_plugins);

    let selection_output = match interactive::tui::run_lazy_selection(planning_tasks) {
        Ok(output) => output,
        Err(err) => {
            if is_signal_termination(&err) {
                set_current_manager(None);
                return 130;
            }
            log_suppressed_terminal_error(format!("interactive selection failed: {err}"));
            return 1;
        }
    };

    if selection_output.interrupted {
        set_current_manager(None);
        return 130;
    }

    set_current_manager(None);

    let mut had_manager_failure = selection_output.had_manager_failure;
    let mut apply_tasks = Vec::new();
    for (mut planned, result) in std::iter::zip(selection_output.planned, selection_output.results)
    {
        let manager_id = planned.plan.manager_id;
        debug_assert_eq!(manager_id, result.manager_id);
        let selection = interactive::apply::apply_chosen_candidates_with_meta(
            &planned.ctx,
            manager_id,
            planned.plan.take_candidates(),
            result.chosen_versions,
            planned.ctx.policy.pinned.clone(),
        );

        had_manager_failure |= persist_interactive_pins(config, manager_id, &planned.ctx);

        if selection.selected.is_empty() || planned.ctx.is_dry_run() {
            continue;
        }

        let selected = selection.selected.clone();
        apply_tasks.push(interactive::tui::ApplyProgressTask::new(
            manager_id,
            selected,
            Box::new(move || {
                set_current_manager(Some(manager_id));
                planned.plan.apply(&planned.ctx, selection)
            }),
        ));
    }

    let _ = drain_text_outcomes();

    if !apply_tasks.is_empty() {
        match interactive::tui::run_apply_progress(apply_tasks) {
            Ok(summary) => {
                had_manager_failure |= summary.had_failure;
                if summary.interrupted {
                    set_current_manager(None);
                    return 130;
                }
            }
            Err(err) => {
                if is_signal_termination(&err) {
                    set_current_manager(None);
                    return 130;
                }
                log_suppressed_terminal_error(format!("interactive apply failed: {err}"));
                return 1;
            }
        }
    }

    set_current_manager(None);
    i32::from(had_manager_failure)
}

fn interactive_apply_planning_tasks(
    cli: &cli::Cli,
    run_mode: RunMode,
    config: &UpnowConfig,
    selected_plugins: Vec<&'static dyn ManagerPlugin>,
) -> Vec<interactive::tui::SelectionPlanningTask> {
    let max_parallel_checks = cli.max_parallel_checks;
    let interactive = cli.interactive;

    selected_plugins
        .into_iter()
        .map(|plugin| {
            let config = config.clone();
            interactive::tui::SelectionPlanningTask::new(
                plugin.id(),
                Box::new(move || {
                    set_current_manager(Some(plugin.id()));

                    let manager_ctx = build_ctx_for_plugin(
                        plugin,
                        run_mode,
                        max_parallel_checks,
                        &config,
                        interactive,
                    )?;

                    if !manager_ctx.policy.mode.allows_run(run_mode.is_apply()) {
                        return Ok(None);
                    }

                    if !interactive_manager_preflight(plugin) {
                        return Ok(None);
                    }

                    let spinner = start_manager_spinner(plugin.id(), RunMode::Plan);
                    let plan_result = plugin.interactive_apply(&manager_ctx);
                    finish_manager_spinner(spinner);
                    let _ = drain_text_outcomes();

                    plan_result.map(|plan| {
                        plan.map(|plan| interactive::tui::SelectionPlan {
                            ctx: manager_ctx,
                            plan,
                        })
                    })
                }),
            )
        })
        .collect()
}

fn interactive_manager_preflight(plugin: &'static dyn ManagerPlugin) -> bool {
    if !plugin.supports_current_platform() {
        emit_manager_preflight_skip(
            plugin.id(),
            ReasonCode::UnsupportedPlatform,
            plugin.unsupported_platform_reason(),
        );
        let _ = drain_text_outcomes();
        return false;
    }

    if let Some(command) = plugin.probe_command()
        && !process::command_exists(&command)
    {
        emit_manager_preflight_skip(
            plugin.id(),
            ReasonCode::MissingCommand,
            format!("required command '{command}' is not available"),
        );
        let _ = drain_text_outcomes();
        return false;
    }

    true
}

fn is_signal_termination(err: &Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<CommandFailedError>()
            .is_some_and(CommandFailedError::was_signaled)
            || cause
                .downcast_ref::<interactive::InteractiveCancelled>()
                .is_some()
    })
}

fn exit_with_error(msg: impl std::fmt::Display) -> i32 {
    eprintln!("error: {msg}");
    1
}

fn emit_manager_preflight_skip(
    manager: &'static str,
    reason_code: ReasonCode,
    reason_detail: impl Into<String>,
) {
    let outcome = ItemOutcome::skipped(manager, "*", "*", "*", reason_code, reason_detail);
    emit_text_outcome(&outcome);
}

fn persist_interactive_pins(
    config: &mut UpnowConfig,
    manager_id: &'static str,
    manager_ctx: &ManagerCtx,
) -> bool {
    let Some(new_pins) = manager_ctx.take_pending_pins() else {
        return false;
    };

    config.set_manager_pins(manager_id, new_pins);
    if let Err(err) = config.persist_manager_pins(manager_id) {
        let message = format!(
            "failed to persist interactive pin updates after manager '{manager_id}': {err}"
        );
        if terminal_output_suppressed() {
            log_suppressed_terminal_error(message);
        } else {
            eprintln!("error: {message}");
        }
        return true;
    }

    false
}

fn log_suppressed_terminal_error(message: impl AsRef<str>) {
    log_warning(format!("error: {}", message.as_ref()));
}
