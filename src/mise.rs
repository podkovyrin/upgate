use crate::Cli;
use anyhow::{Context, Result, bail};
use std::process::Command;

const MISE_DELAY: &str = "7d";

pub(crate) fn run(cli: &Cli) -> Result<()> {
    let planned = mise_upgrade_dry_run_with_before(MISE_DELAY)?;
    let plan_lines = build_plan_lines(&planned);

    for line in plan_lines {
        println!("{line}");
    }

    if cli.dry_run {
        return Ok(());
    }

    run_mise(&["upgrade", "--before", MISE_DELAY])?;
    Ok(())
}

fn build_plan_lines(lines: &[String]) -> Vec<String> {
    let mut old_versions: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut result = Vec::new();

    for line in lines {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("Would uninstall ") {
            if let Some((tool, from_ver)) = split_tool_and_version(rest) {
                old_versions.insert(tool.to_string(), from_ver.to_string());
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Would install ")
            && let Some((tool, to_ver)) = split_tool_and_version(rest)
        {
            let from = old_versions
                .get(tool)
                .map_or_else(|| "v?".to_string(), |v| version_label(v));
            let to = version_label(to_ver);
            result.push(format!(
                "mise: {tool} {from} -> {to} (source: mise, delay: {MISE_DELAY})"
            ));
        }
    }

    result
}

fn split_tool_and_version(input: &str) -> Option<(&str, &str)> {
    let idx = input.rfind('@')?;
    let (tool, ver) = input.split_at(idx);
    Some((tool, ver.strip_prefix('@')?))
}

fn mise_upgrade_dry_run_with_before(before: &str) -> Result<Vec<String>> {
    let output = Command::new("mise")
        .args(["upgrade", "--dry-run", "--before", before])
        .output()
        .with_context(|| format!("failed to run mise upgrade --dry-run --before {before}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "mise upgrade --dry-run --before {before} failed: {}",
            stderr.trim()
        );
    }

    let stdout = String::from_utf8(output.stdout).context("mise dry-run output not UTF-8")?;
    Ok(stdout.lines().map(str::to_string).collect())
}

fn run_mise(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("mise")
        .args(args)
        .output()
        .with_context(|| format!("failed to run mise {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("mise {} failed: {}", args.join(" "), stderr.trim());
    }

    Ok(output.stdout)
}

fn version_label(version: &str) -> String {
    if version.starts_with('v') {
        return version.to_string();
    }

    match version.chars().next() {
        Some(c) if c.is_ascii_digit() => format!("v{version}"),
        _ => version.to_string(),
    }
}
