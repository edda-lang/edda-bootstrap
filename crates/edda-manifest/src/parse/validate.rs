//! Pure validation helpers for the `package.toml` parser (§3 / §4).

use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_span::FileId;
use edda_target::{Arch, known_features};

use crate::schema::{BuildConfig, SemVer};

use super::emit;

pub(crate) fn parse_semver_text(s: &str) -> Option<SemVer> {
    let (core, build) = match s.split_once('+') {
        Some((c, b)) if !b.is_empty() => (c, Some(b.to_owned().into_boxed_str())),
        Some(_) => return None,
        None => (s, None),
    };
    let (core, pre_release) = match core.split_once('-') {
        Some((c, p)) if !p.is_empty() => (c, Some(p.to_owned().into_boxed_str())),
        Some(_) => return None,
        None => (core, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(SemVer { major, minor, patch, pre_release, build })
}

pub(crate) fn validate_default_features(
    build: &BuildConfig,
    _file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let Some(triple) = build.default_target else {
        return;
    };
    let arch = triple.arch();
    for feature in &build.default_features {
        if !is_known_feature(arch, &feature.name) {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::UnknownTargetFeature,
                feature.span,
                format!(
                    "default feature {:?} is not in the {} catalogue",
                    feature.name.as_ref(),
                    arch
                ),
            );
        }
    }
}

fn is_known_feature(arch: Arch, name: &str) -> bool {
    known_features(arch).iter().any(|&f| f == name)
}

/// Package name: lowercase, may contain hyphens and underscores (Cargo convention).
pub(crate) fn is_package_name(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Identifier: lowercase snake_case only (used for root_namespace and profile names).
pub(crate) fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// Validate a `[workspace].members` entry as a POSIX-style relative path
/// under `lib/`. Returns `Some(reason)` describing why the entry is
/// invalid, or `None` when the path is admitted. Components must each
/// be a [`is_identifier`]-shaped directory name; `/` separates them; no
/// `..`, no backslashes, no absolute paths.
pub(crate) fn invalid_member_path_reason(s: &str) -> Option<&'static str> {
    if s.is_empty() {
        return Some("must not be empty");
    }
    if s.contains('\\') {
        return Some("must use forward slashes (POSIX-style); backslashes are not admitted");
    }
    if s.starts_with('/') {
        return Some("must be a relative path; absolute paths are not admitted");
    }
    for component in s.split('/') {
        if component.is_empty() {
            return Some("must not contain empty path components (consecutive or trailing slashes)");
        }
        if component == "." || component == ".." {
            return Some("must not contain `.` or `..` components");
        }
        if !is_identifier(component) {
            return Some("must consist of snake_case directory components separated by `/`");
        }
    }
    None
}
