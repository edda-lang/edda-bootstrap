//! Version-string parsing, SemVer range satisfaction, and registry
//! version selection for `edda add`.

use crate::command::AddCommand;

/// Split `"name"` or `"name@version"` into `(name, Option<version_req>)`.
pub(super) fn split_name_at_version(raw: &str) -> (String, Option<String>) {
    match raw.split_once('@') {
        Some((name, ver)) => (name.to_owned(), Some(ver.to_owned())),
        None => (raw.to_owned(), None),
    }
}

/// Select the highest satisfying version from registry entries.
pub(super) fn select_version<'a>(
    entries: &'a [edda_mimir_registry::IndexEntry],
    version_req: &Option<String>,
    accept_unstable: bool,
) -> Option<&'a edda_mimir_registry::IndexEntry> {
    entries.iter().rev().find(|e| {
        if !accept_unstable && e.version.contains('-') {
            return false;
        }
        match version_req {
            None => true,
            Some(req) => semver_satisfies(&e.version, req),
        }
    })
}

/// Minimal SemVer satisfaction check: caret (`^`) and tilde (`~`) ranges.
///
/// For slice H, this is intentionally simple. A follow-up wave will replace
/// this with the full `semver` crate implementation once it is a workspace dep.
pub(crate) fn semver_satisfies_pub(version: &str, req: &str) -> bool {
    semver_satisfies(version, req)
}

fn semver_satisfies(version: &str, req: &str) -> bool {
    if req.starts_with('^') {
        let base = req.trim_start_matches('^');
        semver_caret_satisfies(version, base)
    } else if req.starts_with('~') {
        let base = req.trim_start_matches('~');
        semver_tilde_satisfies(version, base)
    } else {
        // Exact match or bare `*`.
        req == "*" || version == req
    }
}

/// Caret range: `^1.2.3` ≙ `>=1.2.3, <2.0.0`.
fn semver_caret_satisfies(version: &str, base: &str) -> bool {
    let (va, vb, vc) = parse_triple(version).unwrap_or((0, 0, 0));
    let (ba, bb, bc) = parse_triple(base).unwrap_or((0, 0, 0));
    if ba > 0 {
        (va, vb, vc) >= (ba, bb, bc) && va == ba
    } else if bb > 0 {
        (va, vb, vc) >= (ba, bb, bc) && va == 0 && vb == bb
    } else {
        (va, vb, vc) >= (ba, bb, bc) && va == 0 && vb == 0 && vc == bc
    }
}

/// Tilde range: `~1.2.3` ≙ `>=1.2.3, <1.3.0`.
fn semver_tilde_satisfies(version: &str, base: &str) -> bool {
    let (va, vb, vc) = parse_triple(version).unwrap_or((0, 0, 0));
    let (ba, bb, bc) = parse_triple(base).unwrap_or((0, 0, 0));
    (va, vb, vc) >= (ba, bb, bc) && va == ba && vb == bb
}

/// Parse `"major.minor.patch"` into `(u64, u64, u64)`.
fn parse_triple(s: &str) -> Option<(u64, u64, u64)> {
    // Strip pre-release suffix.
    let base = s.split('-').next().unwrap_or(s);
    let mut parts = base.splitn(3, '.');
    let a = parts.next()?.parse::<u64>().ok()?;
    let b = parts.next().and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
    let c = parts.next().and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
    Some((a, b, c))
}

/// Resolve the effective `max_effects` list for the new dependency from
/// the command's `--max-effects` flags.
pub(super) fn resolve_max_effects(cmd: &AddCommand) -> Vec<Box<str>> {
    if cmd.max_effects.is_empty() {
        vec![]
    } else {
        cmd.max_effects.iter().map(|s| s.as_str().into()).collect()
    }
}
