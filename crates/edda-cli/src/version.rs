//! Compiler identity record + `version` surface for the `edda` binary.
//!
//! Implements COMPILER-PROVENANCE.md §Mechanism 1: a canonical identity
//! record `{impl, impl_version, source, runtime, target}` stamped into the
//! binary at build time (see `build.rs`) and surfaced via the `version`
//! subcommand and the `--version` / `-V` flags. Human and `--json` forms
//! share one schema with the native compiler so `edda version --json` is
//! impl-agnostic.

use std::path::PathBuf;

use edda_cache::hash_bytes;

use crate::exit::SUCCESS;

/// Intercept the `version` subcommand and the `--version` / `-V` flags.
///
/// Returns `Some(exit_code)` after printing the identity record to stdout
/// when `argv` requests it, or `None` to let normal verb parsing proceed.
/// `argv[0]` is the program name and is ignored. `version --json` emits the
/// machine schema; `version`, `--version`, and `-V` emit the human form.
pub fn intercept(argv: &[String]) -> Option<u8> {
    let first = argv.get(1).map(String::as_str)?;
    match first {
        "version" => {
            let json = argv[2..].iter().any(|a| a == "--json");
            print_identity(json);
            Some(SUCCESS)
        }
        "--version" | "-V" => {
            print_identity(false);
            Some(SUCCESS)
        }
        _ => None,
    }
}

/// Print the current identity record to stdout in the requested form.
fn print_identity(json: bool) {
    let identity = Identity::current();
    if json {
        println!("{}", identity.render_json());
    } else {
        println!("{}", identity.render_human());
    }
}

/// The compiler identity record. Field set and JSON schema are locked across
/// both compiler implementations (COMPILER-PROVENANCE.md §Mechanism 1).
pub struct Identity {
    /// Compiler implementation tag: `bootstrap-rust` for this binary.
    pub impl_: &'static str,
    /// Semver of the compiler implementation.
    pub impl_version: &'static str,
    /// Source repository the binary was built from.
    pub source_repo: &'static str,
    /// Short git sha of the source tree (`"unknown"` outside a checkout).
    pub source_sha: &'static str,
    /// Working-tree dirty state at build: `"true"`, `"false"`, or `"unknown"`.
    pub source_dirty: &'static str,
    /// Default/built target triple in Edda `<arch>-<os>-<abi>` grammar.
    pub target: &'static str,
    /// Runtime archive filename the binary links against.
    pub runtime_name: String,
    /// BLAKE3 (64-hex) of the resolved runtime archive, or `None` if absent.
    pub runtime_hash: Option<String>,
}

impl Identity {
    /// Build the identity record: build-time `env!()` fields plus the
    /// run-time-resolved runtime archive hash.
    pub fn current() -> Self {
        let (runtime_name, runtime_hash) = resolve_runtime();
        Identity {
            impl_: env!("EDDA_IMPL"),
            impl_version: env!("EDDA_IMPL_VERSION"),
            source_repo: env!("EDDA_SOURCE_REPO"),
            source_sha: env!("EDDA_SOURCE_SHA"),
            source_dirty: env!("EDDA_SOURCE_DIRTY"),
            target: env!("EDDA_TARGET"),
            runtime_name,
            runtime_hash,
        }
    }

    /// Render the human-readable form (the `--version` / `version` default).
    pub fn render_human(&self) -> String {
        let dirty = if self.source_dirty == "true" { " (dirty)" } else { "" };
        let rt = match &self.runtime_hash {
            Some(h) => format!("{}@{}", self.runtime_name, short(h)),
            None => format!("{}@<unresolved>", self.runtime_name),
        };
        format!(
            "edda ({impl_}) {ver}\n  \
             impl:    {impl_}\n  \
             source:  {repo}@{sha}{dirty}\n  \
             runtime: {rt}\n  \
             target:  {target}",
            impl_ = self.impl_,
            ver = self.impl_version,
            repo = self.source_repo,
            sha = self.source_sha,
            dirty = dirty,
            rt = rt,
            target = self.target,
        )
    }

    /// Render the machine-readable `version --json` form. Schema is shared
    /// with native-edda; `runtime.hash` is JSON `null` when unresolved.
    pub fn render_json(&self) -> String {
        let dirty = self.source_dirty == "true";
        let rt_hash = match &self.runtime_hash {
            Some(h) => format!("\"{}\"", json_escape(h)),
            None => "null".to_string(),
        };
        format!(
            "{{\"impl\":\"{impl_}\",\
             \"impl_version\":\"{ver}\",\
             \"source\":{{\"repo\":\"{repo}\",\"sha\":\"{sha}\",\"dirty\":{dirty}}},\
             \"runtime\":{{\"name\":\"{rt_name}\",\"hash\":{rt_hash}}},\
             \"target\":\"{target}\"}}",
            impl_ = json_escape(self.impl_),
            ver = json_escape(self.impl_version),
            repo = json_escape(self.source_repo),
            sha = json_escape(self.source_sha),
            dirty = dirty,
            rt_name = json_escape(&self.runtime_name),
            rt_hash = rt_hash,
            target = json_escape(self.target),
        )
    }
}

/// Locate the runtime archive next to the current executable and hash it.
///
/// Mirrors the link stage's `find_edda_rt_lib` probe: prefers the exe-stem
/// derived name (`<stem>_rt.lib` / `lib<stem>_rt.a`), falling back to the
/// canonical `edda` stem. Returns the resolved filename and its BLAKE3, or
/// the canonical name with `None` when no archive is found.
fn resolve_runtime() -> (String, Option<String>) {
    let canonical = rt_lib_filename("edda");
    let Some(path) = find_runtime_lib() else {
        return (canonical, None);
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or(canonical);
    match std::fs::read(&path) {
        Ok(bytes) => (name, Some(hash_bytes(&bytes).to_string())),
        Err(_) => (name, None),
    }
}

/// Find the runtime archive beside the current exe, or `None`.
fn find_runtime_lib() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates = Vec::new();
    if let Some(stem) = exe.file_stem().and_then(|s| s.to_str()) {
        candidates.push(rt_lib_filename(stem));
    }
    candidates.push(rt_lib_filename("edda"));
    candidates
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// Compose the edda-rt static-library filename for a binary stem.
fn rt_lib_filename(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}_rt.lib")
    } else {
        format!("lib{stem}_rt.a")
    }
}

/// First 12 chars of a hex hash for the compact human form.
fn short(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}

/// Escape a string for embedding in a JSON string literal. Identity fields
/// are simple (hex / ascii names), but `"` and `\` are escaped defensively.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}
