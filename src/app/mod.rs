mod cli;

use anyhow::{Error, Result};
use clap::Parser;

use crate::config::UpnowConfig;
use crate::interactive;
use crate::manager::{
    ManagerPlugin, RunMode, all_plugins, build_ctx_for_plugin, resolve_selected_plugins,
};
use crate::ui::{finish_manager_spinner, init_output_theme, start_manager_spinner};
use crate::util::logging::{LoggingOptions, init_logging, session_dir, set_current_manager};

pub fn run() -> i32 {
    let cli = cli::Cli::parse();
    init_output_theme(cli.plain, cli.no_color, cli.verbose);

    if let Err(err) = init_command_logging(&cli) {
        return exit_with_error(format!("failed to initialize logging: {err}"));
    }

    match run_with_cli(&cli) {
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
        && let Some(path) = session_dir()
    {
        eprintln!("debug logs: {}", path.display());
    }

    Ok(())
}

fn run_with_cli(cli: &cli::Cli) -> Result<i32> {
    let run_mode = cli.run_mode();
    validate_interactive_mode(cli.interactive, run_mode)?;

    let mut config = load_config_with_overrides(&cli.set)?;
    let selected_plugins = resolve_selected_plugins(&cli.managers)?;

    Ok(run_selected_plugins(
        cli,
        run_mode,
        &mut config,
        selected_plugins,
    ))
}

fn validate_interactive_mode(interactive: bool, run_mode: RunMode) -> Result<()> {
    if !interactive {
        return Ok(());
    }

    if !matches!(run_mode, RunMode::Apply) {
        anyhow::bail!("--interactive is only supported with 'apply'");
    }

    interactive::ensure_tty_available()
}

fn load_config_with_overrides<S: AsRef<str>>(overrides: &[S]) -> Result<UpnowConfig> {
    let mut config = UpnowConfig::load()?;
    let known_manager_ids: Vec<&str> = all_plugins().iter().map(|p| p.id()).collect();
    for override_arg in overrides {
        config.apply_cli_override(override_arg.as_ref(), &known_manager_ids)?;
    }

    Ok(config)
}

fn run_selected_plugins(
    cli: &cli::Cli,
    run_mode: RunMode,
    config: &mut UpnowConfig,
    selected_plugins: Vec<&'static dyn ManagerPlugin>,
) -> i32 {
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

        if !manager_ctx
            .policy
            .mode
            .allows_run(matches!(run_mode, RunMode::Apply))
        {
            continue;
        }

        let spinner = start_manager_spinner(plugin.id(), run_mode);
        let run_result = plugin.run(&manager_ctx);
        finish_manager_spinner(spinner);

        if cli.interactive
            && matches!(run_mode, RunMode::Apply)
            && let Some(new_pins) = manager_ctx.take_pending_pins()
        {
            config.set_manager_pins(plugin.id(), new_pins);
            if let Err(err) = config.persist_manager_pins(plugin.id()) {
                eprintln!(
                    "error: failed to persist interactive pin updates after manager '{}': {err}",
                    plugin.id()
                );
                had_manager_failure = true;
            }
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

fn is_signal_termination(err: &Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<crate::util::process::CommandFailedError>()
            .is_some_and(crate::util::process::CommandFailedError::was_signaled)
            || cause
                .downcast_ref::<interactive::InteractiveCancelled>()
                .is_some()
    })
}

fn exit_with_error(msg: impl std::fmt::Display) -> i32 {
    eprintln!("error: {msg}");
    1
}
