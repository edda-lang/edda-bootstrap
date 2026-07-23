//! `build-system.md` §10 one-line summary rendering.

use std::fmt::Write as _;

use crate::outcome::Summary;

/// Render the §10 summary line:
///
/// ```text
/// build: 142 modules, 23 artifacts (5 cached, 18 generated), 0.42s
/// ```
///
/// `verb` is the originating command name
/// ([`crate::command::Command::name`]). The artifact clause is elided
/// when no artifacts are in scope (every plain build, every
/// `edda check`).
pub fn render(verb: &str, summary: &Summary) -> String {
    let mut out = String::with_capacity(80);
    let _ = write!(&mut out, "{verb}: {} modules", summary.modules_total);
    let total_artifacts = summary.artifacts_cached + summary.artifacts_generated;
    if total_artifacts > 0 {
        let _ = write!(
            &mut out,
            ", {total_artifacts} artifacts ({} cached, {} generated)",
            summary.artifacts_cached, summary.artifacts_generated,
        );
    }
    let _ = write!(&mut out, ", {:.2}s", summary.elapsed.as_secs_f64());
    out
}
