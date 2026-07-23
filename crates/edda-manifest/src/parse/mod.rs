//! Validate a parsed TOML document against the `package.toml` schema (§3).

use std::path::Path;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_span::{BytePos, FileId, SourceMap, Span};

use crate::schema::{
    BuildConfig, CodegenConfig, Dependency, LoadedManifest,
    PackageManifest, ProfileSet, SemVer, StructmapConfig, WorkspaceManifest, WorkspaceTable,
};

const RESERVED_KEYS: &[&str] = &["target_overrides", "features", "build_script"];

/// Module-path prefixes reserved by build-system.md §4.
const RESERVED_ROOT_NAMESPACES: &[&str] =
    &["std", "codegen", "tests", "bench", "examples"];


mod build;
mod codegen;
mod dependencies;
mod lints;
mod package;
mod profiles;
mod structmap;
mod validate;
mod workspace;

pub(crate) use validate::{
    invalid_member_path_reason, is_identifier, is_package_name, parse_semver_text,
};

/// Read `path` from disk, register it with `source_map`, and parse it as
/// a `package.toml` manifest.
///
/// Errors when the file declares only `[workspace]` (no `[package]`);
/// callers that need to accept the workspace-only shape should use
/// [`load_any`] instead. The historical [`load`] keeps the
/// `PackageManifest` return type so existing single-package consumers
/// compile unchanged.
///
/// On I/O failure, emits a `parse_error` diagnostic at `Span::DUMMY`
/// (no file has been registered yet) and returns `None`. The driver
/// translates this into exit code 2 per `build-system.md` §10.
pub fn load(
    path: &Path,
    source_map: &SourceMap,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<PackageManifest> {
    match load_any(path, source_map, diags, lint_cfg)? {
        LoadedManifest::Package(p) => Some(p),
        LoadedManifest::WorkspaceOnly(_) => {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::ParseError,
                Span::DUMMY,
                format!(
                    "manifest at {} is workspace-only ([package] absent); \
                     this code path requires a package manifest",
                    path.display(),
                ),
            );
            None
        }
    }
}

/// Read `path` from disk and parse it as either a single-package or
/// workspace-only `package.toml`. Companion to [`load`] for callers
/// that need to handle the Cargo-style virtual manifest shape.
///
/// The workspace-only branch is taken when **both** of the following
/// hold: `[package]` is absent, and the manifest's directory has no
/// `src/` sibling. When `[package]` is absent but `src/` exists the
/// caller almost certainly forgot the section; emit the usual
/// `[package].name`-missing diagnostic instead of silently accepting
/// the file as a workspace manifest.
pub fn load_any(
    path: &Path,
    source_map: &SourceMap,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<LoadedManifest> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::ParseError,
                Span::DUMMY,
                format!("cannot read manifest at {}: {}", path.display(), e),
            );
            return None;
        }
    };
    let file = source_map.add_file(path.to_path_buf(), content);
    let src = source_map.file_content(file);
    let allow_workspace_only = !manifest_dir_has_src(path);
    finish_parse_any(src, file, diags, lint_cfg, allow_workspace_only)
}

/// Parse `src` as a single-package `package.toml` manifest. Returns
/// `None` if a hard parse error occurred or a required §3 field is
/// missing; warnings such as `unknown_manifest_key` are pushed to
/// `diags` and do not block the return value on their own.
pub fn parse(
    src: &str,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<PackageManifest> {
    match parse_any(src, file, diags, lint_cfg)? {
        LoadedManifest::Package(p) => Some(p),
        LoadedManifest::WorkspaceOnly(_) => {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::ParseError,
                fspan(file),
                "manifest is workspace-only ([package] absent); \
                 this code path requires a package manifest"
                    .to_owned(),
            );
            None
        }
    }
}

/// Parse `src` as either a single-package or workspace-only manifest.
///
/// Workspace-only is admitted whenever `[package]` is absent and
/// `[workspace]` is present. Callers that need the additional
/// "no `src/` sibling" guard (see `build-system.md` §3) should reach
/// for [`load_any`] instead, which has the manifest path on hand to
/// perform the filesystem check.
pub fn parse_any(
    src: &str,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<LoadedManifest> {
    finish_parse_any(src, file, diags, lint_cfg, true)
}

/// Core implementation behind [`parse`], [`parse_any`], and [`load_any`].
fn finish_parse_any(
    src: &str,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    allow_workspace_only: bool,
) -> Option<LoadedManifest> {
    let table = parse_toml_table(src, file, diags, lint_cfg)?;
    let fields = collect_top_level_fields(&table, file, diags, lint_cfg)?;
    validate::validate_default_features(&fields.build, file, diags, lint_cfg);
    finalize_loaded_manifest(fields, file, diags, lint_cfg, allow_workspace_only)
}

/// Container for the fields parsed out of the top-level TOML table.
/// Used to keep [`finish_parse_any`] under the function-length limit
/// without exposing internal state.
struct TopLevelFields {
    package: Option<Box<str>>,
    version: Option<SemVer>,
    root_namespace: Option<Box<str>>,
    dependencies: Vec<Dependency>,
    build: BuildConfig,
    profiles: ProfileSet,
    lints: LintConfig,
    codegen: CodegenConfig,
    workspace: Option<WorkspaceTable>,
    structmap: StructmapConfig,
    max_trust: Option<u32>,
    edition: Option<Box<str>>,
    authors: Vec<Box<str>>,
    license: Option<Box<str>>,
    description: Option<Box<str>>,
    kind: Option<crate::schema::PackageKind>,
}

fn parse_toml_table(
    src: &str,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<toml::Table> {
    match src.parse() {
        Ok(t) => Some(t),
        Err(e) => {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::ParseError,
                fspan(file),
                format!("TOML parse error: {e}"),
            );
            None
        }
    }
}

fn collect_top_level_fields(
    table: &toml::Table,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<TopLevelFields> {
    let mut f = TopLevelFields {
        package: None,
        version: None,
        root_namespace: None,
        dependencies: Vec::new(),
        build: BuildConfig {
            default_target: None,
            default_features: Vec::new(),
            default_profile: "dev".into(),
            emit_structmap: true,
        },
        profiles: ProfileSet::with_locked_defaults(),
        lints: LintConfig::new(),
        codegen: CodegenConfig::locked_defaults(),
        workspace: None,
        structmap: StructmapConfig::locked_defaults(),
        max_trust: None,
        edition: None,
        authors: Vec::new(),
        license: None,
        description: None,
        kind: None,
    };
    let mut had_error = false;
    for (key, value) in table {
        match key.as_str() {
            "package" => match package::parse_package_section(value, file, diags, lint_cfg) {
                Some(pkg) => {
                    f.package = Some(pkg.name);
                    f.version = Some(pkg.version);
                    f.root_namespace = Some(pkg.root_namespace);
                    f.max_trust = pkg.max_trust;
                    f.edition = pkg.edition;
                    f.authors = pkg.authors;
                    f.license = pkg.license;
                    f.description = pkg.description;
                    f.kind = pkg.kind;
                }
                None => had_error = true,
            },
            "build" => match build::parse_build(value, file, diags, lint_cfg) {
                Some(v) => f.build = v,
                None => had_error = true,
            },
            "dependencies" => match dependencies::parse_dependencies(value, file, diags, lint_cfg) {
                Some(v) => f.dependencies = v,
                None => had_error = true,
            },
            "profiles" => {
                match profiles::parse_profiles(f.profiles.clone(), value, file, diags, lint_cfg) {
                    Some(v) => f.profiles = v,
                    None => had_error = true,
                }
            }
            "lints" => match lints::parse_lints(value, file, diags, lint_cfg) {
                Some(v) => f.lints = v,
                None => had_error = true,
            },
            "codegen" => match codegen::parse_codegen(value, file, diags, lint_cfg) {
                Some(v) => f.codegen = v,
                None => had_error = true,
            },
            "workspace" => match workspace::parse_workspace(value, file, diags, lint_cfg) {
                Some(v) => f.workspace = Some(v),
                None => had_error = true,
            },
            "structmap" => match structmap::parse_structmap(value, file, diags, lint_cfg) {
                Some(v) => f.structmap = v,
                None => had_error = true,
            },
            _ => emit_unknown_key(key, file, diags, lint_cfg),
        }
    }
    if had_error { None } else { Some(f) }
}

fn finalize_loaded_manifest(
    fields: TopLevelFields,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    allow_workspace_only: bool,
) -> Option<LoadedManifest> {
    // B-011: `[workspace] discover` implies `[structmap] descendant_tree = true`.
    // The two concerns are conjoined for workspace roots — when the filesystem
    // is the source of truth for membership, the root index.toon must carry the
    // full descendant tree so a single read tells any agent the workspace shape.
    let structmap = imply_descendant_tree(fields.structmap, fields.workspace.as_ref());

    if fields.package.is_none() && allow_workspace_only && fields.workspace.is_some() {
        let workspace = fields.workspace.expect("just checked .is_some()");
        return Some(LoadedManifest::WorkspaceOnly(WorkspaceManifest {
            lints: fields.lints,
            codegen: fields.codegen,
            workspace,
            structmap,
        }));
    }
    let package = require_field(fields.package, "[package].name", file, diags, lint_cfg)?;
    let version = require_field(fields.version, "[package].version", file, diags, lint_cfg)?;
    let root_namespace = require_field(
        fields.root_namespace,
        "[package].root_namespace",
        file,
        diags,
        lint_cfg,
    )?;
    Some(LoadedManifest::Package(PackageManifest {
        package,
        version,
        root_namespace,
        dependencies: fields.dependencies,
        build: fields.build,
        profiles: fields.profiles,
        lints: fields.lints,
        codegen: fields.codegen,
        workspace: fields.workspace,
        structmap,
        max_trust: fields.max_trust,
        edition: fields.edition,
        authors: fields.authors,
        license: fields.license,
        description: fields.description,
        kind: fields.kind,
    }))
}

/// Apply B-011's "discover implies descendant_tree" override. Workspaces
/// with auto-discovery must emit the root `index.toon` in descendant-tree
/// form — one read of the root tells any agent the complete workspace
/// shape, by definition the same information they would get from the
/// discovery walk itself.
fn imply_descendant_tree(
    mut structmap: StructmapConfig,
    workspace: Option<&WorkspaceTable>,
) -> StructmapConfig {
    if workspace
        .and_then(|w| w.discover.as_ref())
        .is_some()
    {
        structmap.descendant_tree = true;
    }
    structmap
}

/// Filesystem check used by [`load_any`] to distinguish a workspace-only
/// root manifest from one that merely forgot its `[package]` section.
fn manifest_dir_has_src(path: &Path) -> bool {
    match path.parent() {
        Some(dir) => dir.join("src").is_dir(),
        None => Path::new("src").is_dir(),
    }
}

/// Unwrap a required §3 field, emitting a `parse_error` diagnostic naming
/// the missing field and returning `None` when it is absent. `name` is the
/// `[table].key` path surfaced verbatim in the diagnostic.
fn require_field<T>(
    value: Option<T>,
    name: &str,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<T> {
    match value {
        Some(v) => Some(v),
        None => {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::ParseError,
                fspan(file),
                format!("`package.toml` is missing required field `{name}`"),
            );
            None
        }
    }
}

fn emit_unknown_key(key: &str, file: FileId, diags: &mut Diagnostics, lint_cfg: &LintConfig) {
    let note = if RESERVED_KEYS.contains(&key) {
        format!("manifest key `{key}` is reserved for a future feature (build-system.md §3); ignoring")
    } else {
        format!("unknown manifest key `{key}` (build-system.md §3); ignoring")
    };
    emit(diags, lint_cfg, DiagnosticClass::UnknownManifestKey, fspan(file), note);
}

fn emit(
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    class: DiagnosticClass,
    span: Span,
    msg: impl Into<String>,
) {
    let sev = lint_cfg.effective(class);
    diags.push(Diagnostic::new(class, sev, span, msg));
}

/// Span at the start of file — used when TOML parsing loses positional info.
fn fspan(file: FileId) -> Span {
    Span::new(file, BytePos(0), BytePos(0))
}

#[cfg(test)]
mod tests;
