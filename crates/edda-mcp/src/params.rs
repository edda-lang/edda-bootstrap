//! `params` shapes per `mcp-protocol.md` §§5-10.
//!
//! Fully-typed params are shipped for operations that route into
//! the daemon end-to-end (`build.typecheck`, `client.open_project`,
//! the document-overlay lifecycle, the position-bearing
//! `inspect.parsed_ast` / `inspect.diagnostics` queries). Every other
//! operation reaches its handler as a `serde_json::Value`; the
//! handler validates the shape it cares about and the rest is
//! preserved for the spec's "unrecognised fields in `params` are
//! rejected with `arg_shape_invalid`" rule.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Shared shape for `build.*` operations per `mcp-protocol.md` §5.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BuildCommonParams {
    /// Absolute path to the project root.
    pub project_root: PathBuf,
    /// `--target <triple>` override. `None` falls through to the
    /// manifest default.
    #[serde(default)]
    pub target: Option<String>,
    /// `--features <list>` overlay on top of the manifest defaults.
    #[serde(default)]
    pub features: Vec<String>,
    /// `--profile <name>` override.
    #[serde(default)]
    pub profile: Option<String>,
    /// `--manifest-path <path>` override.
    #[serde(default)]
    pub manifest_path: Option<PathBuf>,
    /// `--warn-as-error <classes>` overlay applied after the manifest.
    /// Class strings use `edda_diag::DiagnosticClass::name()` form.
    #[serde(default)]
    pub warn_as_error: Vec<String>,
    /// `--quiet` / `--verbose` tristate. `default` | `quiet` | `verbose`.
    #[serde(default = "default_verbosity")]
    pub verbosity: String,
    /// `--jobs <N>`. `0` means "host backend chooses".
    #[serde(default)]
    pub jobs: u32,
}

fn default_verbosity() -> String {
    "default".to_string()
}

/// `params` for `client.open_document`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenDocumentParams {
    /// On-disk path the document maps to.
    pub path: PathBuf,
    /// Editor-supplied monotonic version stamp (mirrors LSP's
    /// `textDocument.version`).
    pub version: u64,
    /// Full document text. The daemon takes ownership.
    pub text: String,
}

/// `params` for `client.apply_change`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplyChangeParams {
    /// On-disk path of the open overlay.
    pub path: PathBuf,
    /// New monotonic version stamp.
    pub version: u64,
    /// Full post-edit text. The daemon takes the whole document,
    /// not LSP-style ranges.
    pub new_text: String,
}

/// `params` for `client.close_document`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClosePathParams {
    /// On-disk path of the overlay to close.
    pub path: PathBuf,
}

/// `params` for `client.open_project`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenProjectParams {
    /// Absolute project root. The daemon looks for `package.toml`
    /// here (or under `manifest_path` if given).
    pub project_root: PathBuf,
    /// Optional `--manifest-path` override.
    #[serde(default)]
    pub manifest_path: Option<PathBuf>,
    /// Optional `--target <triple>` override.
    #[serde(default)]
    pub target: Option<String>,
    /// Optional `--profile <name>` override.
    #[serde(default)]
    pub profile: Option<String>,
    /// Feature overlay on top of the manifest defaults.
    #[serde(default)]
    pub features: Vec<String>,
}

/// `position` block embedded in many `inspect.*` and `typecheck.*`
/// requests per `mcp-protocol.md` §7.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PositionParam {
    /// 1-based line.
    pub line: u32,
    /// 1-based column (bytes from line start).
    pub col: u32,
}

/// Common shape for position-bearing `inspect.*` and `typecheck.*`
/// requests.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilePositionParams {
    /// Absolute project root the daemon was opened against.
    pub project_root: PathBuf,
    /// File path, absolute or relative to `project_root`.
    pub file: PathBuf,
    /// Optional 1-based position inside the file.
    #[serde(default)]
    pub position: Option<PositionParam>,
}

/// `params` for `inspect.parsed_ast` / `inspect.diagnostics`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileQueryParams {
    /// Absolute project root.
    pub project_root: PathBuf,
    /// File path, absolute or relative to `project_root`.
    pub file: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_common_params_round_trips() {
        let p = BuildCommonParams {
            project_root: PathBuf::from("/tmp/proj"),
            target: Some("x86_64-linux-gnu".to_string()),
            features: vec!["avx2".to_string()],
            profile: Some("release".to_string()),
            manifest_path: None,
            warn_as_error: vec!["unused_import".to_string()],
            verbosity: "default".to_string(),
            jobs: 0,
        };
        let v = serde_json::to_value(&p).unwrap();
        let back: BuildCommonParams = serde_json::from_value(v).unwrap();
        assert_eq!(back.project_root, p.project_root);
        assert_eq!(back.features, p.features);
    }

    #[test]
    fn open_document_params_round_trips() {
        let p = OpenDocumentParams {
            path: PathBuf::from("/x/main.ea"),
            version: 1,
            text: "let x = 1".to_string(),
        };
        let v = serde_json::to_value(&p).unwrap();
        let back: OpenDocumentParams = serde_json::from_value(v).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.text, "let x = 1");
    }
}
