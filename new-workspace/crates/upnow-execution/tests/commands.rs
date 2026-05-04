use upnow_domain::{ManagerId, PackageName, PlanItemId, VersionText};
use upnow_execution::{ExecutionCommand, ExecutionStatus, execute_commands};
use upnow_infra::{CommandOutput, CommandSpec, ProcessRunner};

#[test]
fn executes_manager_supplied_commands() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(success_status(), "", ""))]);
    let command = ExecutionCommand {
        plan_item_id: PlanItemId::new("pnpm:alpha-ready").expect("valid id"),
        package_name: PackageName::new("alpha-ready").expect("valid package"),
        installed_version: VersionText::new("1.0.0").expect("valid version"),
        target_version: VersionText::new("1.2.0").expect("valid version"),
        command: CommandSpec::new("tool", ["install", "alpha-ready@1.2.0"]).mutating(),
    };

    let report = execute_commands(
        ManagerId::new("pnpm").expect("valid manager"),
        vec![command],
        &process,
    )
    .expect("execution should report success");

    assert_eq!(report.items.len(), 1);
    assert!(matches!(
        report.items[0].status,
        ExecutionStatus::Succeeded { .. }
    ));
    let calls = match &process {
        ProcessRunner::Fake(fake) => fake.calls(),
        ProcessRunner::Real { .. } => panic!("expected fake process"),
    };
    assert_eq!(calls[0].display(), "tool install alpha-ready@1.2.0");
}

#[test]
fn command_failures_are_item_scoped() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(
        failure_status(),
        "",
        "install failed",
    ))]);

    let report = execute_commands(
        ManagerId::new("pnpm").expect("valid manager"),
        vec![command()],
        &process,
    )
    .expect("ordinary command failure should stay item-scoped");

    assert!(matches!(
        report.items[0].status,
        ExecutionStatus::Failed { .. }
    ));
}

#[cfg(unix)]
#[test]
fn interrupted_commands_return_execution_error() {
    let process = ProcessRunner::fake([Ok(CommandOutput::from_parts(signal_status(), "", ""))]);

    let err = execute_commands(
        ManagerId::new("pnpm").expect("valid manager"),
        vec![command()],
        &process,
    )
    .expect_err("signal should interrupt execution");

    assert!(err.is_interruption());
}

fn command() -> ExecutionCommand {
    ExecutionCommand {
        plan_item_id: PlanItemId::new("pnpm:alpha-ready").expect("valid id"),
        package_name: PackageName::new("alpha-ready").expect("valid package"),
        installed_version: VersionText::new("1.0.0").expect("valid version"),
        target_version: VersionText::new("1.2.0").expect("valid version"),
        command: CommandSpec::new("tool", ["install", "alpha-ready@1.2.0"]).mutating(),
    }
}

#[cfg(unix)]
fn success_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(unix)]
fn failure_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(1 << 8)
}

#[cfg(unix)]
fn signal_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(2)
}

#[cfg(windows)]
fn success_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn failure_status() -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;

    std::process::ExitStatus::from_raw(1)
}
