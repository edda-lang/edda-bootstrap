//! `result` shapes per `mcp-protocol.md` §§5-10.
//!
//! This module wires the result shapes for the operations that route
//! end-to-end into the daemon. Operations that respond with
//! `method_not_implemented` never construct a result.

use serde::{Deserialize, Serialize};

use crate::diagnostic::WireDiagnostic;

/// `result` of `build.typecheck`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TypecheckResult {
    /// `build-system.md` §10 one-line summary string.
    pub summary: String,
    /// Total modules in the resolved source graph.
    pub modules_typechecked: usize,
    /// Wall-clock duration of the typecheck pass, in milliseconds.
    pub wall_clock_ms: u64,
    /// Diagnostics produced during the pass, in push order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<WireDiagnostic>,
}

/// `result` of `build.compile`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompileResult {
    /// §10 one-line summary.
    pub summary: String,
    /// Modules compiled by the pass.
    pub modules_compiled: usize,
    /// Total reachable artifacts.
    pub artifacts_total: usize,
    /// Artifacts served from cache.
    pub artifacts_cached: usize,
    /// Artifacts freshly generated.
    pub artifacts_generated: usize,
    /// Wall-clock duration in milliseconds.
    pub wall_clock_ms: u64,
    /// Output file paths produced by the link pass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_paths: Vec<String>,
    /// Diagnostics produced during the pass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<WireDiagnostic>,
}

/// `result` of `client.open_project` and `client.close_project`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectLifecycleResult {
    /// Whether the operation took effect (`open_project` returns `true`
    /// on success; `close_project` returns `true` always since it's
    /// idempotent).
    pub applied: bool,
    /// Absolute project root after the operation. `None` when no
    /// project is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    /// Diagnostics accumulated during project open (parse + import-
    /// resolve, per the daemon). Empty for `close_project`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<WireDiagnostic>,
}

/// `result` of `client.open_document` and `client.apply_change`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentParseResult {
    /// Echo of the caller's version stamp.
    pub version: u64,
    /// Parse diagnostics for the overlay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<WireDiagnostic>,
}

/// `result` of `client.close_document`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloseDocumentResult {
    /// Always `true` (idempotent per the daemon's `close_document`).
    pub applied: bool,
}

/// `result` of `inspect.parsed_ast`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParsedAstResult {
    /// `true` if a parsed AST is available for this path (either an
    /// overlay or a resolved-source-graph entry).
    pub available: bool,
    /// Number of top-level items in the AST. The full structural
    /// surface is reserved for later; this currently returns a coarse
    /// reachability signal so the client can confirm the daemon is
    /// tracking the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_level_items: Option<usize>,
}

/// `result` of `inspect.diagnostics`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiagnosticsResult {
    /// Diagnostics whose primary span points at the queried path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<WireDiagnostic>,
}
