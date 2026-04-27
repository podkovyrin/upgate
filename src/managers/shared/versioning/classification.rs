#![allow(dead_code)]

use std::str::FromStr;

use pep440_rs::{PrereleaseKind as Pep440PrereleaseKind, Version as Pep440Version};
use semver::Version as SemverVersion;

/// Normalized release stability classes used by version-policy checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseClass {
    Dev,
    Alpha,
    Beta,
    Rc,
    Final,
    /// Prerelease is known, but we cannot safely place it on the normalized ladder.
    UnknownPrerelease,
    /// Parsing/classification failed entirely.
    Unknown,
}

impl ReleaseClass {
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Final)
    }

    pub const fn is_prerelease(self) -> bool {
        matches!(
            self,
            Self::Dev | Self::Alpha | Self::Beta | Self::Rc | Self::UnknownPrerelease
        )
    }

    pub(crate) const fn stability_rank(self) -> Option<u8> {
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

/// Classify a `SemVer` version into the normalized release ladder.
pub fn classify_semver_release(version: &str) -> ReleaseClass {
    let normalized = version.trim();
    if normalized.is_empty() {
        return ReleaseClass::Unknown;
    }

    if let Some(class) = classify_strict_semver(normalized) {
        return class;
    }

    if let Some(stripped) = normalized.strip_prefix(['v', 'V'])
        && let Some(class) = classify_strict_semver(stripped)
    {
        return class;
    }

    classify_semver_like_fallback(normalized)
}

/// Classify a PEP 440 version into the normalized release ladder.
pub fn classify_pep440_release(version: &str) -> ReleaseClass {
    let Ok(parsed) = Pep440Version::from_str(version) else {
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

fn classify_semver_prerelease(raw: &str) -> ReleaseClass {
    let mut best_match = None;

    for token in raw.split('.') {
        for fragment in token.split(['-', '_']) {
            if let Some(class) = classify_semver_fragment(fragment) {
                best_match = Some(select_less_stable_class(best_match, class));
            }
        }
    }

    best_match.unwrap_or(ReleaseClass::UnknownPrerelease)
}

fn classify_strict_semver(raw: &str) -> Option<ReleaseClass> {
    let Ok(parsed) = SemverVersion::parse(raw) else {
        return None;
    };

    Some(if parsed.pre.is_empty() {
        ReleaseClass::Final
    } else {
        classify_semver_prerelease(parsed.pre.as_str())
    })
}

fn classify_semver_like_fallback(raw: &str) -> ReleaseClass {
    let raw = raw.trim();
    if raw.is_empty() {
        return ReleaseClass::Unknown;
    }

    let raw = raw.strip_prefix(['v', 'V']).unwrap_or(raw);
    if raw.is_empty() {
        return ReleaseClass::Unknown;
    }

    let Some(raw_without_build) = strip_semver_like_build_metadata(raw) else {
        return ReleaseClass::Unknown;
    };

    if let Some((core, prerelease)) = raw_without_build.split_once('-') {
        if !is_numeric_dot_core(core) {
            return ReleaseClass::Unknown;
        }
        if prerelease.trim().is_empty() {
            return ReleaseClass::UnknownPrerelease;
        }
        return classify_semver_prerelease(prerelease);
    }

    if is_numeric_dot_core(raw_without_build) {
        return ReleaseClass::Final;
    }

    if has_numeric_core_with_alpha_suffix(raw_without_build) {
        return ReleaseClass::UnknownPrerelease;
    }

    ReleaseClass::Unknown
}

fn strip_semver_like_build_metadata(raw: &str) -> Option<&str> {
    match raw.split_once('+') {
        Some((left, build)) if !left.is_empty() && is_valid_semver_like_build_metadata(build) => {
            Some(left)
        }
        Some(_) => None,
        None => Some(raw),
    }
}

fn is_valid_semver_like_build_metadata(raw: &str) -> bool {
    !raw.is_empty()
        && raw.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
        })
}

fn is_numeric_dot_core(raw: &str) -> bool {
    raw.split('.')
        .all(|segment| !segment.is_empty() && segment.chars().all(|ch| ch.is_ascii_digit()))
}

fn has_numeric_core_with_alpha_suffix(raw: &str) -> bool {
    let mut segments = raw.split('.').peekable();
    while let Some(segment) = segments.next() {
        if segment.is_empty() {
            return false;
        }
        if segments.peek().is_none() {
            return is_numeric_alpha_suffix_segment(segment);
        }
        if !segment.chars().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
    }

    false
}

fn is_numeric_alpha_suffix_segment(segment: &str) -> bool {
    let mut chars = segment.chars().peekable();
    let mut saw_numeric_prefix = false;
    while let Some(ch) = chars.peek() {
        if ch.is_ascii_digit() {
            saw_numeric_prefix = true;
            chars.next();
            continue;
        }
        break;
    }

    if !saw_numeric_prefix {
        return false;
    }

    let mut saw_alpha_suffix = false;
    for ch in chars {
        if ch.is_ascii_alphabetic() {
            saw_alpha_suffix = true;
            continue;
        }
        if saw_alpha_suffix && ch.is_ascii_digit() {
            continue;
        }
        return false;
    }

    saw_alpha_suffix
}

pub(crate) const fn select_less_stable_class(
    current: Option<ReleaseClass>,
    next: ReleaseClass,
) -> ReleaseClass {
    let Some(current) = current else {
        return next;
    };

    match (current.stability_rank(), next.stability_rank()) {
        (Some(current_rank), Some(next_rank)) if next_rank < current_rank => next,
        _ => current,
    }
}

fn classify_semver_fragment(fragment: &str) -> Option<ReleaseClass> {
    let fragment = fragment.trim();
    if fragment.is_empty() {
        return None;
    }

    let normalized = fragment.to_ascii_lowercase();
    let label = leading_alpha_prefix(&normalized);
    if label.is_empty() {
        return None;
    }

    if matches_any_label(label, DEV_LABELS) {
        return Some(ReleaseClass::Dev);
    }
    if matches_any_label(label, ALPHA_LABELS) {
        return Some(ReleaseClass::Alpha);
    }
    if matches_any_label(label, BETA_LABELS) {
        return Some(ReleaseClass::Beta);
    }
    if matches_any_label(label, RC_LABELS) {
        return Some(ReleaseClass::Rc);
    }

    None
}

pub(crate) fn matches_any_label(label: &str, labels: &[&str]) -> bool {
    labels.contains(&label)
}

pub(crate) fn leading_alpha_prefix(token: &str) -> &str {
    let end = token
        .char_indices()
        .find_map(|(idx, ch)| (!ch.is_ascii_alphabetic()).then_some(idx))
        .unwrap_or(token.len());
    &token[..end]
}

pub(crate) const DEV_LABELS: &[&str] = &[
    "canary",
    "nightly",
    "snapshot",
    "dev",
    "devel",
    "development",
    "next",
    "edge",
    "preview",
    "experimental",
    "exp",
];
const ALPHA_LABELS: &[&str] = &["alpha", "a"];
const BETA_LABELS: &[&str] = &["beta", "b"];
pub(crate) const RC_LABELS: &[&str] = &["prerelease", "pre", "rc"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_semver_prerelease_known_and_unknown_labels() {
        assert_eq!(
            classify_semver_release("1.2.3-alpha.1"),
            ReleaseClass::Alpha
        );
        assert_eq!(classify_semver_release("1.2.3-rc1"), ReleaseClass::Rc);
        assert_eq!(
            classify_semver_release("1.2.3-canary.123"),
            ReleaseClass::Dev
        );
        assert_eq!(classify_semver_release("1.2.3-next"), ReleaseClass::Dev);
        assert_eq!(classify_semver_release("1.2.3-edge.4"), ReleaseClass::Dev);
        assert_eq!(
            classify_semver_release("1.2.3-preview.5"),
            ReleaseClass::Dev
        );
        assert_eq!(classify_semver_release("1.2.3-exp1"), ReleaseClass::Dev);
        assert_eq!(classify_semver_release("1.2.3-pre.1"), ReleaseClass::Rc);
        assert_eq!(
            classify_semver_release("1.2.3-prerelease"),
            ReleaseClass::Rc
        );
        assert_eq!(classify_semver_release("1.2.3-dev.rc1"), ReleaseClass::Dev);
        assert_eq!(classify_semver_release("1.2.3-rc.dev1"), ReleaseClass::Dev);
        assert_eq!(
            classify_semver_release("1.2.3-beta.alpha2"),
            ReleaseClass::Alpha
        );
        assert_eq!(
            classify_semver_release("1.2.3-foo.1"),
            ReleaseClass::UnknownPrerelease
        );
        assert_eq!(classify_semver_release("not-semver"), ReleaseClass::Unknown);
    }

    #[test]
    fn classify_semver_like_versions_with_v_prefix_and_build_metadata() {
        assert_eq!(classify_semver_release("v1.2.3"), ReleaseClass::Final);
        assert_eq!(classify_semver_release("V1.2.3"), ReleaseClass::Final);
        assert_eq!(
            classify_semver_release("1.2.3+build.7"),
            ReleaseClass::Final
        );
        assert_eq!(classify_semver_release("  v1.2.3-rc.1  "), ReleaseClass::Rc);
    }

    #[test]
    fn does_not_misclassify_clear_nonfinal_semver_like_strings_as_final() {
        assert_eq!(
            classify_semver_release("1.2.3-foo.1"),
            ReleaseClass::UnknownPrerelease
        );
        assert_eq!(
            classify_semver_release("1.2.3-"),
            ReleaseClass::UnknownPrerelease
        );
        assert_eq!(
            classify_semver_release("1.2.3rc1"),
            ReleaseClass::UnknownPrerelease
        );
    }

    #[test]
    fn keeps_invalid_or_non_version_text_as_unknown() {
        assert_eq!(classify_semver_release(""), ReleaseClass::Unknown);
        assert_eq!(classify_semver_release("v"), ReleaseClass::Unknown);
        assert_eq!(classify_semver_release("1..2"), ReleaseClass::Unknown);
        assert_eq!(classify_semver_release("1.2.3+"), ReleaseClass::Unknown);
        assert_eq!(
            classify_semver_release("1.2.3+build..7"),
            ReleaseClass::Unknown
        );
        assert_eq!(
            classify_semver_release("1.2.3+build+meta"),
            ReleaseClass::Unknown
        );
        assert_eq!(classify_semver_release("not-semver"), ReleaseClass::Unknown);
    }

    #[test]
    fn classify_pep440_release_classes() {
        assert_eq!(classify_pep440_release("1.0.0"), ReleaseClass::Final);
        assert_eq!(classify_pep440_release("1.0.0a1"), ReleaseClass::Alpha);
        assert_eq!(classify_pep440_release("1.0.0rc1"), ReleaseClass::Rc);
        assert_eq!(classify_pep440_release("1.0.0.dev1"), ReleaseClass::Dev);
        assert_eq!(classify_pep440_release("not-pep440"), ReleaseClass::Unknown);
    }
}
