use std::str::FromStr;
use std::time::{Duration, SystemTime};

use pep440_rs::{PrereleaseKind as Pep440PrereleaseKind, Version as Pep440Version};
use semver::Version as SemverVersion;
use upnow_domain::{
    BlockReason, DelayReason, ExecutionEligibility, ManagerSelectedTarget, PlanItem, PlanItemId,
    PolicyBlockReason, PolicyWarning, ReleaseEntry, ReleaseLookupResult, ReleaseTimeline,
    TargetAgeEvidence, TargetAgeLookupResult, TargetSelection, UpdateCandidate, UpdateSeed,
    VersionPolicy, VersionScheme, VersionText,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseClass {
    Dev,
    Alpha,
    Beta,
    Rc,
    Final,
    UnknownPrerelease,
    Unknown,
}

impl ReleaseClass {
    const fn is_final(self) -> bool {
        matches!(self, Self::Final)
    }

    const fn stability_rank(self) -> Option<u8> {
        match self {
            Self::Dev => Some(0),
            Self::Alpha => Some(1),
            Self::Beta => Some(2),
            Self::Rc => Some(3),
            Self::Final => Some(4),
            Self::UnknownPrerelease | Self::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PolicyDecision {
    allowed: bool,
    block_reason: Option<PolicyBlockReason>,
    warning: Option<PolicyWarning>,
}

/// Evaluate one manager-discovered update seed into a typed plan item.
#[must_use]
pub fn evaluate_seed(
    id: PlanItemId,
    seed: UpdateSeed,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    execution_eligibility: ExecutionEligibility,
) -> PlanItem {
    match seed.target_selection.clone() {
        TargetSelection::PlannerSelectable {
            discovered_target,
            release_lookup,
        } => evaluate_planner_selectable_seed(
            id,
            seed,
            &discovered_target,
            release_lookup,
            policy,
            now,
            min_release_age,
            execution_eligibility,
        ),
        TargetSelection::ManagerSelected(target) => evaluate_manager_selected_seed(
            id,
            seed,
            target,
            policy,
            now,
            min_release_age,
            execution_eligibility,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_planner_selectable_seed(
    id: PlanItemId,
    seed: UpdateSeed,
    discovered_target: &VersionText,
    release_lookup: ReleaseLookupResult,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    execution_eligibility: ExecutionEligibility,
) -> PlanItem {
    match release_lookup {
        ReleaseLookupResult::MissingMetadata => PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::MissingReleaseMetadata,
            policy_warnings: Vec::new(),
        },
        ReleaseLookupResult::LookupFailed(_) => PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::ReleaseLookupFailed,
            policy_warnings: Vec::new(),
        },
        ReleaseLookupResult::Known(timeline) => match seed.version_scheme {
            VersionScheme::SemVer => evaluate_semver_seed(
                id,
                seed,
                discovered_target,
                &timeline,
                policy,
                now,
                min_release_age,
                execution_eligibility,
            ),
            VersionScheme::Pep440 => evaluate_pep440_seed(
                id,
                seed,
                discovered_target,
                &timeline,
                policy,
                now,
                min_release_age,
                execution_eligibility,
            ),
            VersionScheme::ManagerNative => PlanItem::ResolverError {
                id,
                installed: seed.installed.clone(),
                message: "manager-native evaluation requires manager-specific planner".to_owned(),
            },
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_manager_selected_seed(
    id: PlanItemId,
    seed: UpdateSeed,
    target: ManagerSelectedTarget,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    execution_eligibility: ExecutionEligibility,
) -> PlanItem {
    let selected_target = target.target_version.clone();
    match selected_target_is_update(&seed, &selected_target) {
        Ok(false) => {
            return PlanItem::Current {
                id,
                installed: seed.installed,
            };
        }
        Ok(true) => {}
        Err(message) => {
            return PlanItem::ResolverError {
                id,
                installed: seed.installed,
                message,
            };
        }
    }

    let installed_class = classify_release(
        seed.version_scheme,
        seed.installed.installed_version.as_str(),
    );
    let target_class = classify_release(seed.version_scheme, selected_target.as_str());
    let policy_decision = evaluate_policy(policy, installed_class, target_class);
    if !policy_decision.allowed {
        return PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::VersionPolicy(
                policy_decision
                    .block_reason
                    .unwrap_or(PolicyBlockReason::UnsupportedPolicy),
            ),
            policy_warnings: policy_decision.warning.into_iter().collect(),
        };
    }

    let target_age = match target.target_age {
        TargetAgeLookupResult::Known(evidence) => evidence,
        TargetAgeLookupResult::MissingMetadata => {
            return PlanItem::Blocked {
                id,
                seed,
                reason: BlockReason::MissingReleaseMetadata,
                policy_warnings: Vec::new(),
            };
        }
        TargetAgeLookupResult::LookupFailed(_) => {
            return PlanItem::Blocked {
                id,
                seed,
                reason: BlockReason::ReleaseLookupFailed,
                policy_warnings: Vec::new(),
            };
        }
    };

    let candidate = candidate_from_seed(
        &seed,
        selected_target,
        execution_eligibility,
        policy_decision.warning.into_iter().collect(),
    );

    if is_evidence_old_enough(&target_age, now, min_release_age) {
        PlanItem::Update { id, candidate }
    } else {
        PlanItem::Delayed {
            id,
            candidate,
            reason: DelayReason::ReleaseTooFresh,
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn evaluate_semver_seed(
    id: PlanItemId,
    seed: UpdateSeed,
    discovered_target: &VersionText,
    timeline: &ReleaseTimeline,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    execution_eligibility: ExecutionEligibility,
) -> PlanItem {
    let Ok(installed_version) = parse_semver(seed.installed.installed_version.as_str()) else {
        return PlanItem::ResolverError {
            id,
            installed: seed.installed.clone(),
            message: "failed to parse installed version".to_owned(),
        };
    };
    let Ok(parsed_discovered_target) = parse_semver(discovered_target.as_str()) else {
        return PlanItem::ResolverError {
            id,
            installed: seed.installed.clone(),
            message: "failed to parse discovered target version".to_owned(),
        };
    };
    let mut target_metadata_found = false;
    for entry in &timeline.versions {
        let Ok(parsed) = parse_semver(entry.version.as_str()) else {
            let bad_version = entry.version.as_str().to_owned();
            return PlanItem::ResolverError {
                id,
                installed: seed.installed.clone(),
                message: format!("failed to parse release version `{bad_version}`"),
            };
        };
        if parsed == parsed_discovered_target {
            target_metadata_found = true;
        }
    }
    if !target_metadata_found {
        return PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::MissingReleaseMetadata,
            policy_warnings: Vec::new(),
        };
    }

    let mut newest_overall = None::<(SemverVersion, CandidateFact)>;
    let mut newest_policy_eligible = None::<(SemverVersion, CandidateFact)>;
    let mut newest_age_eligible = None::<(SemverVersion, CandidateFact)>;
    let installed_class = classify_semver_release(seed.installed.installed_version.as_str());

    for entry in &timeline.versions {
        let Ok(parsed) = parse_semver(entry.version.as_str()) else {
            let bad_version = entry.version.as_str().to_owned();
            return PlanItem::ResolverError {
                id,
                installed: seed.installed.clone(),
                message: format!("failed to parse release version `{bad_version}`"),
            };
        };
        if parsed <= installed_version {
            continue;
        }

        let candidate_class = classify_semver_release(entry.version.as_str());
        let policy_decision = evaluate_policy(policy, installed_class, candidate_class);
        let fact = CandidateFact::new(entry, policy_decision.warning);
        if newest_overall
            .as_ref()
            .is_none_or(|(current, _)| parsed > *current)
        {
            newest_overall = Some((parsed.clone(), fact.clone()));
        }

        if policy_decision.allowed {
            if newest_policy_eligible
                .as_ref()
                .is_none_or(|(current, _)| parsed > *current)
            {
                newest_policy_eligible = Some((parsed.clone(), fact.clone()));
            }
            if is_old_enough(entry, now, min_release_age)
                && newest_age_eligible
                    .as_ref()
                    .is_none_or(|(current, _)| parsed > *current)
            {
                newest_age_eligible = Some((parsed, fact));
            }
        }
    }

    let Some((_, newest_overall)) = newest_overall else {
        return PlanItem::Current {
            id,
            installed: seed.installed,
        };
    };

    let Some((_, policy_candidate)) = newest_policy_eligible else {
        let target_class = classify_semver_release(newest_overall.version.as_str());
        let reason = evaluate_policy(policy, installed_class, target_class)
            .block_reason
            .unwrap_or(PolicyBlockReason::UnsupportedPolicy);
        return PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::VersionPolicy(reason),
            policy_warnings: newest_overall.warnings,
        };
    };

    let candidate = candidate_from_seed(
        &seed,
        policy_candidate.version,
        execution_eligibility,
        policy_candidate.warnings,
    );

    let Some((_, age_candidate)) = newest_age_eligible else {
        return PlanItem::Delayed {
            id,
            candidate,
            reason: DelayReason::ReleaseTooFresh,
        };
    };

    PlanItem::Update {
        id,
        candidate: candidate_from_seed(
            &seed,
            age_candidate.version,
            execution_eligibility,
            age_candidate.warnings,
        ),
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn evaluate_pep440_seed(
    id: PlanItemId,
    seed: UpdateSeed,
    discovered_target: &VersionText,
    timeline: &ReleaseTimeline,
    policy: VersionPolicy,
    now: SystemTime,
    min_release_age: Duration,
    execution_eligibility: ExecutionEligibility,
) -> PlanItem {
    let Ok(installed_version) = parse_pep440(seed.installed.installed_version.as_str()) else {
        return PlanItem::ResolverError {
            id,
            installed: seed.installed.clone(),
            message: "failed to parse installed version".to_owned(),
        };
    };
    let Ok(parsed_discovered_target) = parse_pep440(discovered_target.as_str()) else {
        return PlanItem::ResolverError {
            id,
            installed: seed.installed.clone(),
            message: "failed to parse discovered target version".to_owned(),
        };
    };
    let mut target_metadata_found = false;
    for entry in &timeline.versions {
        let Ok(parsed) = parse_pep440(entry.version.as_str()) else {
            let bad_version = entry.version.as_str().to_owned();
            return PlanItem::ResolverError {
                id,
                installed: seed.installed.clone(),
                message: format!("failed to parse release version `{bad_version}`"),
            };
        };
        if parsed == parsed_discovered_target {
            target_metadata_found = true;
        }
    }
    if !target_metadata_found {
        return PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::MissingReleaseMetadata,
            policy_warnings: Vec::new(),
        };
    }

    let mut newest_overall = None::<(Pep440Version, CandidateFact)>;
    let mut newest_policy_eligible = None::<(Pep440Version, CandidateFact)>;
    let mut newest_age_eligible = None::<(Pep440Version, CandidateFact)>;
    let installed_class = classify_pep440_release(seed.installed.installed_version.as_str());

    for entry in &timeline.versions {
        let Ok(parsed) = parse_pep440(entry.version.as_str()) else {
            let bad_version = entry.version.as_str().to_owned();
            return PlanItem::ResolverError {
                id,
                installed: seed.installed.clone(),
                message: format!("failed to parse release version `{bad_version}`"),
            };
        };
        if parsed <= installed_version {
            continue;
        }

        let candidate_class = classify_pep440_release(entry.version.as_str());
        let policy_decision = evaluate_policy(policy, installed_class, candidate_class);
        let fact = CandidateFact::new(entry, policy_decision.warning);
        if newest_overall
            .as_ref()
            .is_none_or(|(current, _)| parsed > *current)
        {
            newest_overall = Some((parsed.clone(), fact.clone()));
        }

        if policy_decision.allowed {
            if newest_policy_eligible
                .as_ref()
                .is_none_or(|(current, _)| parsed > *current)
            {
                newest_policy_eligible = Some((parsed.clone(), fact.clone()));
            }
            if is_old_enough(entry, now, min_release_age)
                && newest_age_eligible
                    .as_ref()
                    .is_none_or(|(current, _)| parsed > *current)
            {
                newest_age_eligible = Some((parsed, fact));
            }
        }
    }

    let Some((_, newest_overall)) = newest_overall else {
        return PlanItem::Current {
            id,
            installed: seed.installed,
        };
    };

    let Some((_, policy_candidate)) = newest_policy_eligible else {
        let target_class = classify_pep440_release(newest_overall.version.as_str());
        let reason = evaluate_policy(policy, installed_class, target_class)
            .block_reason
            .unwrap_or(PolicyBlockReason::UnsupportedPolicy);
        return PlanItem::Blocked {
            id,
            seed,
            reason: BlockReason::VersionPolicy(reason),
            policy_warnings: newest_overall.warnings,
        };
    };

    let candidate = candidate_from_seed(
        &seed,
        policy_candidate.version,
        execution_eligibility,
        policy_candidate.warnings,
    );

    let Some((_, age_candidate)) = newest_age_eligible else {
        return PlanItem::Delayed {
            id,
            candidate,
            reason: DelayReason::ReleaseTooFresh,
        };
    };

    PlanItem::Update {
        id,
        candidate: candidate_from_seed(
            &seed,
            age_candidate.version,
            execution_eligibility,
            age_candidate.warnings,
        ),
    }
}

fn candidate_from_seed(
    seed: &UpdateSeed,
    target_version: VersionText,
    execution_eligibility: ExecutionEligibility,
    policy_warnings: Vec<PolicyWarning>,
) -> UpdateCandidate {
    UpdateCandidate::new(
        seed.installed.tool_id.clone(),
        seed.installed.package_name.clone(),
        seed.installed.installed_version.clone(),
        target_version,
        seed.version_scheme,
        execution_eligibility,
    )
    .with_policy_warnings(policy_warnings)
}

fn evaluate_policy(
    policy: VersionPolicy,
    installed_class: ReleaseClass,
    candidate_class: ReleaseClass,
) -> PolicyDecision {
    match policy {
        VersionPolicy::None => PolicyDecision {
            allowed: true,
            block_reason: None,
            warning: None,
        },
        VersionPolicy::Stable => evaluate_stable_policy(candidate_class, None),
        VersionPolicy::SameTrack => evaluate_same_track_policy(installed_class, candidate_class),
    }
}

fn classify_release(version_scheme: VersionScheme, raw: &str) -> ReleaseClass {
    match version_scheme {
        VersionScheme::SemVer => classify_semver_release(raw),
        VersionScheme::Pep440 => classify_pep440_release(raw),
        VersionScheme::ManagerNative => classify_manager_native_release(raw),
    }
}

fn selected_target_is_update(seed: &UpdateSeed, target: &VersionText) -> Result<bool, String> {
    if target == &seed.installed.installed_version {
        return Ok(false);
    }

    match seed.version_scheme {
        VersionScheme::SemVer => {
            let installed = parse_semver(seed.installed.installed_version.as_str())
                .map_err(|_| "failed to parse installed version".to_owned())?;
            let target = parse_semver(target.as_str())
                .map_err(|_| "failed to parse selected target version".to_owned())?;
            Ok(target > installed)
        }
        VersionScheme::Pep440 => {
            let installed = parse_pep440(seed.installed.installed_version.as_str())
                .map_err(|_| "failed to parse installed version".to_owned())?;
            let target = parse_pep440(target.as_str())
                .map_err(|_| "failed to parse selected target version".to_owned())?;
            Ok(target > installed)
        }
        VersionScheme::ManagerNative => Ok(target != &seed.installed.installed_version),
    }
}

fn evaluate_same_track_policy(
    installed_class: ReleaseClass,
    candidate_class: ReleaseClass,
) -> PolicyDecision {
    let Some(installed_rank) = installed_class.stability_rank() else {
        return evaluate_stable_policy(
            candidate_class,
            Some(PolicyWarning::InstalledTrackUnknownFallbackStable),
        );
    };

    if candidate_class.is_final() {
        return PolicyDecision {
            allowed: true,
            block_reason: None,
            warning: None,
        };
    }

    let Some(candidate_rank) = candidate_class.stability_rank() else {
        return PolicyDecision {
            allowed: false,
            block_reason: Some(PolicyBlockReason::UnknownStability),
            warning: None,
        };
    };

    if candidate_rank >= installed_rank {
        PolicyDecision {
            allowed: true,
            block_reason: None,
            warning: None,
        }
    } else {
        PolicyDecision {
            allowed: false,
            block_reason: Some(PolicyBlockReason::TrackRegression),
            warning: None,
        }
    }
}

fn evaluate_stable_policy(
    candidate_class: ReleaseClass,
    warning: Option<PolicyWarning>,
) -> PolicyDecision {
    if candidate_class.is_final() {
        PolicyDecision {
            allowed: true,
            block_reason: None,
            warning,
        }
    } else {
        PolicyDecision {
            allowed: false,
            block_reason: Some(PolicyBlockReason::PreReleaseBlocked),
            warning,
        }
    }
}

fn is_old_enough(entry: &ReleaseEntry, now: SystemTime, min_release_age: Duration) -> bool {
    release_age(entry, now) >= min_release_age
}

fn is_evidence_old_enough(
    evidence: &TargetAgeEvidence,
    now: SystemTime,
    min_release_age: Duration,
) -> bool {
    now.duration_since(*evidence.timestamp().as_system_time())
        .unwrap_or_default()
        >= min_release_age
}

fn release_age(entry: &ReleaseEntry, now: SystemTime) -> Duration {
    now.duration_since(*entry.published_at.as_system_time())
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidateFact {
    version: VersionText,
    warnings: Vec<PolicyWarning>,
}

impl CandidateFact {
    fn new(entry: &ReleaseEntry, warning: Option<PolicyWarning>) -> Self {
        Self {
            version: entry.version.clone(),
            warnings: warning.into_iter().collect(),
        }
    }
}

fn parse_semver(raw: &str) -> Result<SemverVersion, String> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    SemverVersion::parse(trimmed)
        .or_else(|_| {
            let parts = trimmed.split('.').collect::<Vec<_>>();
            if parts.is_empty()
                || parts.len() > 3
                || parts.iter().any(|part| part.is_empty())
                || !parts
                    .iter()
                    .all(|part| part.chars().all(|ch| ch.is_ascii_digit()))
            {
                return SemverVersion::parse(trimmed);
            }
            let mut padded = parts;
            while padded.len() < 3 {
                padded.push("0");
            }
            SemverVersion::parse(&padded.join("."))
        })
        .map_err(|err| err.to_string())
}

fn parse_pep440(raw: &str) -> Result<Pep440Version, String> {
    Pep440Version::from_str(raw).map_err(|err| err.to_string())
}

fn classify_semver_release(raw: &str) -> ReleaseClass {
    let raw = raw.trim();
    let Ok(parsed) = parse_semver(raw) else {
        return classify_semver_like_fallback(raw);
    };
    if parsed.pre.is_empty() {
        return ReleaseClass::Final;
    }
    classify_prerelease_text(parsed.pre.as_str()).unwrap_or(ReleaseClass::UnknownPrerelease)
}

fn classify_pep440_release(raw: &str) -> ReleaseClass {
    let Ok(parsed) = Pep440Version::from_str(raw) else {
        return ReleaseClass::Unknown;
    };
    if parsed.is_dev() {
        return ReleaseClass::Dev;
    }
    if let Some(pre) = parsed.pre() {
        return match pre.kind {
            Pep440PrereleaseKind::Alpha => ReleaseClass::Alpha,
            Pep440PrereleaseKind::Beta => ReleaseClass::Beta,
            Pep440PrereleaseKind::Rc => ReleaseClass::Rc,
        };
    }
    ReleaseClass::Final
}

fn classify_semver_like_fallback(raw: &str) -> ReleaseClass {
    let raw = raw.trim().strip_prefix(['v', 'V']).unwrap_or(raw.trim());
    if raw.is_empty() {
        return ReleaseClass::Unknown;
    }
    if let Some((core, prerelease)) = raw.split_once('-')
        && is_numeric_dot_core(core)
    {
        return classify_prerelease_text(prerelease).unwrap_or(ReleaseClass::UnknownPrerelease);
    }
    if is_numeric_dot_core(raw) {
        return ReleaseClass::Final;
    }
    ReleaseClass::Unknown
}

fn classify_manager_native_release(raw: &str) -> ReleaseClass {
    let normalized = normalize_brew_version(raw);
    let version = normalized.trim();
    if version.is_empty()
        || version.eq_ignore_ascii_case("latest")
        || !version.chars().any(|ch| ch.is_ascii_alphanumeric())
    {
        return ReleaseClass::Unknown;
    }
    classify_brew_prerelease(version).unwrap_or(ReleaseClass::Final)
}

fn normalize_brew_version(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_cask_build = trimmed
        .split_once(',')
        .map_or(trimmed, |(head, _)| head.trim());
    strip_brew_revision_suffix(without_cask_build).to_owned()
}

fn strip_brew_revision_suffix(raw: &str) -> &str {
    let Some((head, revision)) = raw.rsplit_once('_') else {
        return raw;
    };
    if !head.is_empty() && revision.chars().all(|ch| ch.is_ascii_digit()) {
        head
    } else {
        raw
    }
}

fn classify_brew_prerelease(version: &str) -> Option<ReleaseClass> {
    let mut best_match = None;
    let mut token_start = None;
    for (idx, ch) in version.char_indices() {
        if ch.is_ascii_alphanumeric() {
            token_start.get_or_insert(idx);
        } else if let Some(start) = token_start.take() {
            best_match = select_less_stable(best_match, classify_brew_token(version, start, idx));
        }
    }
    if let Some(start) = token_start {
        best_match = select_less_stable(
            best_match,
            classify_brew_token(version, start, version.len()),
        );
    }
    best_match
}

fn classify_brew_token(version: &str, start: usize, end: usize) -> Option<ReleaseClass> {
    let token = &version[start..end];
    let normalized = token.to_ascii_lowercase();
    let marker = normalized
        .trim_start_matches(|ch: char| ch.is_ascii_digit())
        .trim();
    let label = leading_alpha_prefix(marker);
    if label.is_empty() {
        return None;
    }
    if matches!(
        label,
        "canary"
            | "nightly"
            | "snapshot"
            | "dev"
            | "devel"
            | "development"
            | "next"
            | "edge"
            | "preview"
            | "experimental"
            | "exp"
    ) {
        return Some(ReleaseClass::Dev);
    }
    if label == "alpha" {
        return Some(ReleaseClass::Alpha);
    }
    if label == "beta" {
        return Some(ReleaseClass::Beta);
    }
    if matches!(label, "prerelease" | "pre" | "rc") {
        return Some(ReleaseClass::Rc);
    }
    if matches!(label, "a") && has_short_brew_prerelease_context(version, start, token, marker) {
        return Some(ReleaseClass::Alpha);
    }
    if matches!(label, "b") && has_short_brew_prerelease_context(version, start, token, marker) {
        return Some(ReleaseClass::Beta);
    }
    None
}

fn has_short_brew_prerelease_context(
    version: &str,
    token_start: usize,
    token: &str,
    marker: &str,
) -> bool {
    if marker.len() < token.len() {
        return true;
    }
    let prefix = version[..token_start]
        .trim_start_matches(['v', 'V'])
        .trim_matches(|ch: char| matches!(ch, '.' | '-' | '_' | '+'));
    prefix.is_empty()
        || (prefix.chars().any(|ch| ch.is_ascii_digit())
            && prefix
                .chars()
                .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '_' | '+')))
}

fn classify_prerelease_text(raw: &str) -> Option<ReleaseClass> {
    let mut best = None;
    for token in raw.split(['.', '-', '_']) {
        let normalized = token.to_ascii_lowercase();
        let label = leading_alpha_prefix(&normalized);
        let next = match label {
            "canary" | "nightly" | "snapshot" | "dev" | "devel" | "development" | "next"
            | "edge" | "preview" | "experimental" | "exp" => Some(ReleaseClass::Dev),
            "alpha" | "a" => Some(ReleaseClass::Alpha),
            "beta" | "b" => Some(ReleaseClass::Beta),
            "prerelease" | "pre" | "rc" => Some(ReleaseClass::Rc),
            _ => None,
        };
        best = select_less_stable(best, next);
    }
    best
}

fn select_less_stable(
    current: Option<ReleaseClass>,
    next: Option<ReleaseClass>,
) -> Option<ReleaseClass> {
    let Some(next) = next else {
        return current;
    };
    let Some(current) = current else {
        return Some(next);
    };
    match (current.stability_rank(), next.stability_rank()) {
        (Some(current_rank), Some(next_rank)) if next_rank < current_rank => Some(next),
        _ => Some(current),
    }
}

fn leading_alpha_prefix(token: &str) -> &str {
    let end = token
        .char_indices()
        .find_map(|(idx, ch)| (!ch.is_ascii_alphabetic()).then_some(idx))
        .unwrap_or(token.len());
    &token[..end]
}

fn is_numeric_dot_core(raw: &str) -> bool {
    raw.split('.')
        .all(|segment| !segment.is_empty() && segment.chars().all(|ch| ch.is_ascii_digit()))
}
