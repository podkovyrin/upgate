use std::str::FromStr;

use pep440_rs::{PrereleaseKind as Pep440PrereleaseKind, Version as Pep440Version};
use semver::Version as SemverVersion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseClass {
    Dev,
    Alpha,
    Beta,
    Rc,
    Final,
    UnknownPrerelease,
    Unknown,
}

impl ReleaseClass {
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Final)
    }

    pub const fn stability_rank(self) -> Option<u8> {
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

pub fn parse_semver(raw: &str) -> Result<SemverVersion, String> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    SemverVersion::parse(trimmed)
        .or_else(|err| {
            let mut parts = trimmed.split('.').collect::<Vec<_>>();
            if parts.len() > 3 || !is_numeric_dot_core(trimmed) {
                return Err(err);
            }
            while parts.len() < 3 {
                parts.push("0");
            }
            SemverVersion::parse(&parts.join("."))
        })
        .map_err(|err| err.to_string())
}

pub fn parse_pep440(raw: &str) -> Result<Pep440Version, String> {
    Pep440Version::from_str(raw).map_err(|err| err.to_string())
}

pub fn classify_semver_release(raw: &str) -> ReleaseClass {
    let Ok(parsed) = parse_semver(raw) else {
        return classify_semver_like_fallback(raw);
    };
    if parsed.pre.is_empty() {
        return ReleaseClass::Final;
    }
    classify_prerelease_text(parsed.pre.as_str()).unwrap_or(ReleaseClass::UnknownPrerelease)
}

pub fn classify_pep440_release(raw: &str) -> ReleaseClass {
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
    let raw = raw.trim();
    let raw = raw.strip_prefix(['v', 'V']).unwrap_or(raw);
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

pub fn classify_manager_native_release(raw: &str) -> ReleaseClass {
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

fn normalize_brew_version(raw: &str) -> &str {
    let trimmed = raw.trim();
    let without_cask_build = trimmed
        .split_once(',')
        .map_or(trimmed, |(head, _)| head.trim());
    strip_brew_revision_suffix(without_cask_build)
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
    let marker = normalized.trim_start_matches(|ch: char| ch.is_ascii_digit());
    match leading_alpha_prefix(marker) {
        "canary" | "nightly" | "snapshot" | "dev" | "devel" | "development" | "next" | "edge"
        | "preview" | "experimental" | "exp" => Some(ReleaseClass::Dev),
        "alpha" => Some(ReleaseClass::Alpha),
        "beta" => Some(ReleaseClass::Beta),
        "prerelease" | "pre" | "rc" => Some(ReleaseClass::Rc),
        "a" if has_short_brew_prerelease_context(version, start, token, marker) => {
            Some(ReleaseClass::Alpha)
        }
        "b" if has_short_brew_prerelease_context(version, start, token, marker) => {
            Some(ReleaseClass::Beta)
        }
        _ => None,
    }
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

const fn select_less_stable(
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
        .find(|ch: char| !ch.is_ascii_alphabetic())
        .unwrap_or(token.len());
    &token[..end]
}

fn is_numeric_dot_core(raw: &str) -> bool {
    raw.split('.')
        .all(|segment| !segment.is_empty() && segment.chars().all(|ch| ch.is_ascii_digit()))
}
