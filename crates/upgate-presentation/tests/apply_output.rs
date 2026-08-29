use upgate_domain::{
    ExecutionSupport, ManagerId, PackageName, PlanItem, PlanItemId, PlanSelection, SelectedItem,
    ToolId, UpdateCandidate, UpdatePlan, UpdateSelectionPolicy, VersionScheme, VersionText,
};
use upgate_execution::{
    ExecutionItemResult, ExecutionReport, ExecutionStatus, ResolvedExecutionTarget,
};
use upgate_presentation::{
    OutputTheme, ThemeOptions, apply_execution_report_table, render_batch_table,
    theme::TerminalCapabilities,
};

fn version(value: &str) -> VersionText {
    VersionText::new(value).expect("valid version")
}

fn manager_id() -> ManagerId {
    ManagerId::new("npm").expect("valid manager id")
}

fn update_item(package: &str) -> PlanItem {
    PlanItem::Update {
        id: PlanItemId::new(format!("npm:{package}")).expect("valid plan item id"),
        candidate: UpdateCandidate::new(
            ToolId::new(package).expect("valid tool id"),
            PackageName::new(package).expect("valid package name"),
            version("1.0.0"),
            version("2.0.0"),
            VersionScheme::SemVer,
            ExecutionSupport::exact_only(),
        ),
    }
}

fn theme(verbose: bool) -> OutputTheme {
    OutputTheme::from_terminal(
        ThemeOptions {
            plain: true,
            verbose,
            ..ThemeOptions::default()
        },
        TerminalCapabilities {
            stdout_is_tty: false,
            no_color_env: false,
            term_is_dumb: false,
        },
    )
}

#[test]
fn apply_output_only_shows_selected_results_unless_verbose() {
    let plan = UpdatePlan::new(
        manager_id(),
        vec![
            update_item("applied"),
            update_item("failed"),
            update_item("not-selected"),
        ],
    )
    .expect("valid plan");
    let selection = PlanSelection::new(
        &plan,
        vec![
            SelectedItem::recommended(PlanItemId::new("npm:applied").expect("valid plan item id")),
            SelectedItem::recommended(PlanItemId::new("npm:failed").expect("valid plan item id")),
        ],
        UpdateSelectionPolicy::include_all(),
    )
    .expect("valid selection");
    let report = ExecutionReport {
        manager_id: manager_id(),
        items: vec![
            ExecutionItemResult {
                plan_item_id: PlanItemId::new("npm:applied").expect("valid plan item id"),
                package_name: PackageName::new("applied").expect("valid package name"),
                installed_version: version("1.0.0"),
                target: ResolvedExecutionTarget::Known(version("2.0.0")),
                status: ExecutionStatus::Succeeded {
                    command: "npm update applied".to_owned(),
                    skipped_mutation: false,
                },
            },
            ExecutionItemResult {
                plan_item_id: PlanItemId::new("npm:failed").expect("valid plan item id"),
                package_name: PackageName::new("failed").expect("valid package name"),
                installed_version: version("1.0.0"),
                target: ResolvedExecutionTarget::Known(version("2.0.0")),
                status: ExecutionStatus::Failed {
                    command: "npm update failed".to_owned(),
                    detail: "command failed".to_owned(),
                },
            },
        ],
    };
    let table = apply_execution_report_table(&report, &plan, &selection);

    let normal = render_batch_table(&table, theme(false));
    assert!(normal.contains("applied"));
    assert!(normal.contains("failed"));
    assert!(!normal.contains("not-selected"));

    let verbose = render_batch_table(&table, theme(true));
    assert!(verbose.contains("applied"));
    assert!(verbose.contains("failed"));
    assert!(verbose.contains("not-selected"));
    assert!(verbose.contains("Skipped"));
}

#[test]
fn apply_output_only_reports_an_empty_selection_when_verbose() {
    let plan = UpdatePlan::new(manager_id(), Vec::new()).expect("valid plan");
    let selection = PlanSelection::new(&plan, Vec::new(), UpdateSelectionPolicy::include_all())
        .expect("valid selection");
    let report = ExecutionReport {
        manager_id: manager_id(),
        items: Vec::new(),
    };
    let table = apply_execution_report_table(&report, &plan, &selection);

    assert!(render_batch_table(&table, theme(false)).is_empty());

    let verbose = render_batch_table(&table, theme(true));
    assert!(verbose.contains("Current"));
    assert!(verbose.contains("no selected updates"));
}
