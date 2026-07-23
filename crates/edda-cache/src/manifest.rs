//! Cache-tier `manifest.toon` schema (version 1).
//!
//! Per `migration.md` §4, every cache-tier directory under
//! `.edda/cache/codegen/` carries a `manifest.toon` recording the
//! artifacts that live there, their hashes, their inputs, and the source
//! files that reach them. The schema:
//!
//! ```toon
//! schema_version: 1
//! project: my_project
//! generated_at: 2026-05-11T14:55:00Z
//! last_gc_run: 2026-05-11T00:00:00Z
//!
//! artifacts[N]{path,hash,short_name,tier,inputs,reachable_from,generated_at}:
//!   - path: codegen/std/option/Option_i32__a3f2e8b1c4d5.ea
//!     hash: a3f2e8b1c4d5...
//!     short_name: Option_i32
//!     tier: repo
//!     inputs:
//!       body_version: 0x01
//!       spec_qualified_name: std.option.Option
//!       argument_tuple:
//!         - kind: type
//!           value: i32
//!       nested_deps: []
//!     reachable_from:
//!       sources:
//!         - src/main.ea
//!       artifacts: []
//!     generated_at: 2026-05-11T14:55:00Z
//! ```
//!
//! The `last_gc_run` field is required by `build-system.md` §7 to avoid
//! re-running scheduled GC ("first build of the day/week") within a
//! single window; `migration.md` §4 reserves it as an optional top-level
//! field under `schema_version: 1`. Missing in older manifests is fine
//! and treated as "GC has never run."

use smol_str::SmolStr;
use time::OffsetDateTime;
use time::format_description::well_known::Iso8601;

use crate::error::CacheError;
use crate::hash::{ArtifactHash, BodyVersion};
use crate::tier::Tier;
use crate::toon::{self, Value, Writer};

/// Locked schema version for `manifest.toon`. v0.1 ships `1`; the
/// migration table is empty.
pub const SCHEMA_VERSION: u32 = 1;

/// Field schema used in the `artifacts[N]{...}:` annotation when
/// emitting `manifest.toon`. Order matters and must match the on-disk
/// form recorded in `migration.md` §4.
const ARTIFACT_FIELDS: &[&str] = &[
    "path",
    "hash",
    "short_name",
    "tier",
    "inputs",
    "reachable_from",
    "generated_at",
];

/// Parsed cache-tier `manifest.toon`.
#[derive(Clone, Debug)]
pub struct Manifest {
    /// Always [`SCHEMA_VERSION`] (1 for v0.1). Stored for forward
    /// compatibility checks.
    pub schema_version: u32,
    /// Project name copied from `package.toon`.
    pub project: SmolStr,
    /// UTC timestamp of the last full manifest write.
    pub generated_at: OffsetDateTime,
    /// UTC timestamp of the last GC run (`build-system.md` §7). `None`
    /// when GC has never run for this project.
    pub last_gc_run: Option<OffsetDateTime>,
    /// One entry per known artifact, in deterministic order.
    pub artifacts: Vec<ArtifactEntry>,
}

/// One artifact's record in `manifest.toon`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactEntry {
    /// Repo-relative path of the artifact file.
    pub path: SmolStr,
    /// Full BLAKE3 hash.
    pub hash: ArtifactHash,
    /// Mangled short name (no `__<prefix>` suffix).
    pub short_name: SmolStr,
    /// Which tier the artifact lives in.
    pub tier: Tier,
    /// Inputs that determine the hash (for staleness detection).
    pub inputs: ArtifactInputs,
    /// Which sources / other artifacts transitively reach this one.
    pub reachable_from: ReachableFrom,
    /// When the artifact was emitted.
    pub generated_at: OffsetDateTime,
}

/// The hash-input record for staleness detection. Mirrors `storage.md`
/// §2 verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactInputs {
    /// Body-version byte that was active when this artifact was emitted.
    pub body_version: BodyVersion,
    /// Fully-qualified name of the spec (e.g. `std.option.Option`).
    pub spec_qualified_name: SmolStr,
    /// Comptime-argument tuple, in declaration order.
    pub argument_tuple: Vec<ArgumentEntry>,
    /// Short names of artifacts this one depends on (the `nested:` set
    /// from the artifact header).
    pub nested_deps: Vec<SmolStr>,
}

/// One comptime-argument entry in an [`ArtifactInputs::argument_tuple`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgumentEntry {
    /// Argument kind (Type / EffectRow / PrimitiveValue / UserValue).
    pub kind: ArgumentKind,
    /// Printable form of the argument value. The cache does not parse
    /// the value further; `edda-codegen` produces the canonical form.
    pub value: SmolStr,
}

/// Comptime-argument kind tag from `storage.md` §3.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ArgumentKind {
    /// `Type` argument (e.g. `i32`, `std.option.Option`).
    Type,
    /// `EffectRow` argument (canonical-row-ordered).
    EffectRow,
    /// Primitive value argument (`i32`, `bool`, `String`, ...).
    PrimitiveValue,
    /// User-defined value argument.
    UserValue,
    /// Function-reference argument — the resolved qualified name of a
    /// top-level function bound to a `comptime f: function(...)` spec
    /// parameter.
    Function,
}

impl ArgumentKind {
    /// Lowercase name used in the manifest.
    pub const fn name(self) -> &'static str {
        match self {
            ArgumentKind::Type => "type",
            ArgumentKind::EffectRow => "effect_row",
            ArgumentKind::PrimitiveValue => "primitive_value",
            ArgumentKind::UserValue => "user_value",
            ArgumentKind::Function => "function",
        }
    }

    /// Parse a kind name from the manifest.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "type" => Some(ArgumentKind::Type),
            "effect_row" => Some(ArgumentKind::EffectRow),
            "primitive_value" => Some(ArgumentKind::PrimitiveValue),
            "user_value" => Some(ArgumentKind::UserValue),
            "function" => Some(ArgumentKind::Function),
            _ => None,
        }
    }
}

/// The set of source files and other artifacts that transitively reach
/// an artifact. Used by GC: an artifact with empty `reachable_from`
/// (both fields) is GC-eligible.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReachableFrom {
    /// Source `.ea` files that directly invoke this artifact's spec.
    pub sources: Vec<SmolStr>,
    /// Short names of other artifacts whose `nested_deps` include this
    /// one.
    pub artifacts: Vec<SmolStr>,
}

impl Manifest {
    /// Construct an empty manifest for a freshly-created project.
    /// Used on `Store::open` when no `manifest.toon` exists on disk.
    pub fn empty(project: impl Into<SmolStr>, now: OffsetDateTime) -> Self {
        Manifest {
            schema_version: SCHEMA_VERSION,
            project: project.into(),
            generated_at: now,
            last_gc_run: None,
            artifacts: Vec::new(),
        }
    }

    /// Parse a `manifest.toon` from its on-disk text. `source_path` is
    /// only used to attribute parse errors.
    pub fn parse(source_path: &std::path::Path, text: &str) -> Result<Self, CacheError> {
        let value = toon::parse(text).map_err(|e| CacheError::ManifestParse {
            path: source_path.to_path_buf(),
            line: e.line,
            message: e.message,
        })?;
        build_manifest(source_path, &value)
    }

    /// Serialise this manifest to TOON text.
    pub fn to_text(&self) -> String {
        let mut w = Writer::new();
        w.comment("manifest.toon — codegen artifact manifest");
        w.comment("Generated automatically; do not edit by hand.");
        w.blank_line();
        w.scalar("schema_version", &self.schema_version.to_string());
        w.scalar("project", self.project.as_str());
        w.scalar("generated_at", &format_iso8601(self.generated_at));
        if let Some(ts) = self.last_gc_run {
            w.scalar("last_gc_run", &format_iso8601(ts));
        }
        w.blank_line();
        w.list_with_schema(
            "artifacts",
            ARTIFACT_FIELDS,
            self.artifacts.len(),
            |w| {
                for entry in &self.artifacts {
                    write_artifact_entry(w, entry);
                }
            },
        );
        w.finish()
    }
}

/// Decode the top-level manifest TOON value into a `Manifest`.
fn build_manifest(path: &std::path::Path, value: &Value) -> Result<Manifest, CacheError> {
    let schema_version = value
        .get("schema_version")
        .and_then(|v| v.as_u32())
        .ok_or_else(|| parse_err(path, "schema_version is missing or not a u32"))?;
    if schema_version != SCHEMA_VERSION {
        return Err(CacheError::SchemaVersionMismatch {
            path: path.to_path_buf(),
            found: schema_version,
            supported: SCHEMA_VERSION,
        });
    }
    let project = scalar_field(path, value, "project")?;
    let generated_at = parse_iso8601(path, scalar_field(path, value, "generated_at")?)?;
    let last_gc_run = match value.get("last_gc_run").and_then(|v| v.as_str()) {
        Some(ts) => Some(parse_iso8601(path, ts)?),
        None => None,
    };
    let artifacts = value
        .get("artifacts")
        .and_then(|v| v.as_list())
        .map(|list| {
            list.iter()
                .map(|item| build_artifact_entry(path, item))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(Manifest {
        schema_version,
        project: SmolStr::new(project),
        generated_at,
        last_gc_run,
        artifacts,
    })
}

/// Decode one `artifacts[]` entry.
fn build_artifact_entry(
    path: &std::path::Path,
    value: &Value,
) -> Result<ArtifactEntry, CacheError> {
    let entry_path = scalar_field(path, value, "path")?;
    let hash_str = scalar_field(path, value, "hash")?;
    let hash = ArtifactHash::parse_hex(hash_str)
        .ok_or_else(|| parse_err(path, format!("invalid hash: {}", hash_str)))?;
    let short_name = scalar_field(path, value, "short_name")?;
    let tier_str = scalar_field(path, value, "tier")?;
    let tier = Tier::from_name(tier_str)
        .ok_or_else(|| parse_err(path, format!("unknown tier: {}", tier_str)))?;
    let inputs = build_inputs(path, value)?;
    let reachable_from = build_reachable(value);
    let generated_at = parse_iso8601(path, scalar_field(path, value, "generated_at")?)?;
    Ok(ArtifactEntry {
        path: SmolStr::new(entry_path),
        hash,
        short_name: SmolStr::new(short_name),
        tier,
        inputs,
        reachable_from,
        generated_at,
    })
}

/// Decode an entry's `inputs` block.
fn build_inputs(path: &std::path::Path, entry: &Value) -> Result<ArtifactInputs, CacheError> {
    let inputs = entry
        .get("inputs")
        .ok_or_else(|| parse_err(path, "artifact entry missing `inputs`"))?;
    let body_version = inputs
        .get("body_version")
        .and_then(|v| v.as_u8_hex())
        .map(BodyVersion)
        .ok_or_else(|| parse_err(path, "inputs.body_version missing or malformed"))?;
    let spec_qualified_name = scalar_field(path, inputs, "spec_qualified_name")?;
    let argument_tuple = inputs
        .get("argument_tuple")
        .and_then(|v| v.as_list())
        .map(|list| {
            list.iter()
                .map(|v| build_argument_entry(path, v))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let nested_deps = inputs
        .get("nested_deps")
        .and_then(|v| v.as_list())
        .map(|list| {
            list.iter()
                .map(|v| {
                    v.as_str()
                        .map(SmolStr::new)
                        .ok_or_else(|| parse_err(path, "nested_deps entry must be a scalar"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(ArtifactInputs {
        body_version,
        spec_qualified_name: SmolStr::new(spec_qualified_name),
        argument_tuple,
        nested_deps,
    })
}

/// Decode one entry in the `argument_tuple` list.
fn build_argument_entry(
    path: &std::path::Path,
    value: &Value,
) -> Result<ArgumentEntry, CacheError> {
    let kind_str = scalar_field(path, value, "kind")?;
    let kind = ArgumentKind::from_name(kind_str)
        .ok_or_else(|| parse_err(path, format!("unknown argument kind: {}", kind_str)))?;
    let value_str = scalar_field(path, value, "value")?;
    Ok(ArgumentEntry {
        kind,
        value: SmolStr::new(value_str),
    })
}

/// Decode the `reachable_from` block.
fn build_reachable(entry: &Value) -> ReachableFrom {
    let block = match entry.get("reachable_from") {
        Some(v) => v,
        None => return ReachableFrom::default(),
    };
    let sources = block
        .get("sources")
        .and_then(|v| v.as_list())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(SmolStr::new))
                .collect()
        })
        .unwrap_or_default();
    let artifacts = block
        .get("artifacts")
        .and_then(|v| v.as_list())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(SmolStr::new))
                .collect()
        })
        .unwrap_or_default();
    ReachableFrom { sources, artifacts }
}

/// Read a required scalar field on a TOON value, producing a structured
/// parse error if it is missing or non-scalar.
fn scalar_field<'a>(
    path: &std::path::Path,
    value: &'a Value,
    key: &str,
) -> Result<&'a str, CacheError> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| parse_err(path, format!("missing or non-scalar `{}` field", key)))
}

/// Construct a `CacheError::ManifestParse` with a generic line number
/// (the structural parse errors emerge from the value tree, not from
/// the lexer, so we don't have a precise line).
fn parse_err(path: &std::path::Path, message: impl Into<String>) -> CacheError {
    CacheError::ManifestParse {
        path: path.to_path_buf(),
        line: 0,
        message: message.into(),
    }
}

/// Parse an ISO-8601 timestamp and require UTC.
fn parse_iso8601(path: &std::path::Path, s: &str) -> Result<OffsetDateTime, CacheError> {
    let ts = OffsetDateTime::parse(s, &Iso8601::DEFAULT)
        .map_err(|e| parse_err(path, format!("invalid ISO-8601 timestamp `{}`: {}", s, e)))?;
    if ts.offset() != time::UtcOffset::UTC {
        return Err(parse_err(
            path,
            format!("timestamp `{}` is not UTC", s),
        ));
    }
    Ok(ts)
}

/// Format an ISO-8601 UTC timestamp; the manifest contract requires the
/// stored form to be UTC.
fn format_iso8601(ts: OffsetDateTime) -> String {
    let utc = ts.to_offset(time::UtcOffset::UTC);
    utc.format(&Iso8601::DEFAULT)
        .unwrap_or_else(|_| String::from("invalid"))
}

/// Emit one `- path: ... \n    hash: ...` artifact entry.
fn write_artifact_entry(w: &mut Writer, entry: &ArtifactEntry) {
    w.list_item("path", entry.path.as_str(), |w| {
        w.scalar("hash", &entry.hash.to_string());
        w.scalar("short_name", entry.short_name.as_str());
        w.scalar("tier", entry.tier.name());
        w.block("inputs", |w| write_inputs(w, &entry.inputs));
        w.block("reachable_from", |w| write_reachable(w, &entry.reachable_from));
        w.scalar("generated_at", &format_iso8601(entry.generated_at));
    });
}

/// Emit an `inputs:` block.
fn write_inputs(w: &mut Writer, inputs: &ArtifactInputs) {
    w.scalar("body_version", &inputs.body_version.to_string());
    w.scalar("spec_qualified_name", inputs.spec_qualified_name.as_str());
    if inputs.argument_tuple.is_empty() {
        w.empty_list("argument_tuple");
    } else {
        w.block("argument_tuple", |w| {
            for arg in &inputs.argument_tuple {
                w.list_item("kind", arg.kind.name(), |w| {
                    w.scalar("value", arg.value.as_str());
                });
            }
        });
    }
    if inputs.nested_deps.is_empty() {
        w.empty_list("nested_deps");
    } else {
        w.block("nested_deps", |w| {
            for dep in &inputs.nested_deps {
                w.list_item_scalar(dep.as_str());
            }
        });
    }
}

/// Emit a `reachable_from:` block.
fn write_reachable(w: &mut Writer, r: &ReachableFrom) {
    if r.sources.is_empty() {
        w.empty_list("sources");
    } else {
        w.block("sources", |w| {
            for s in &r.sources {
                w.list_item_scalar(s.as_str());
            }
        });
    }
    if r.artifacts.is_empty() {
        w.empty_list("artifacts");
    } else {
        w.block("artifacts", |w| {
            for a in &r.artifacts {
                w.list_item_scalar(a.as_str());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_bytes;
    use std::path::Path;
    use time::macros::datetime;

    fn sample_entry() -> ArtifactEntry {
        let hash = hash_bytes(b"abc");
        ArtifactEntry {
            path: SmolStr::new("codegen/std/option/Option_i32__deadbeef0000.ea"),
            hash,
            short_name: SmolStr::new("Option_i32"),
            tier: Tier::Repo,
            inputs: ArtifactInputs {
                body_version: BodyVersion::CURRENT,
                spec_qualified_name: SmolStr::new("std.option.Option"),
                argument_tuple: vec![ArgumentEntry {
                    kind: ArgumentKind::Type,
                    value: SmolStr::new("i32"),
                }],
                nested_deps: Vec::new(),
            },
            reachable_from: ReachableFrom {
                sources: vec![SmolStr::new("src/main.ea")],
                artifacts: Vec::new(),
            },
            generated_at: datetime!(2026-05-11 14:55:00 UTC),
        }
    }

    #[test]
    fn empty_manifest_round_trip() {
        let m = Manifest::empty("my_project", datetime!(2026-05-11 00:00:00 UTC));
        let text = m.to_text();
        let parsed = Manifest::parse(Path::new("manifest.toon"), &text).unwrap();
        assert_eq!(parsed.project, "my_project");
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.last_gc_run, None);
        assert!(parsed.artifacts.is_empty());
    }

    #[test]
    fn manifest_with_artifact_round_trip() {
        let m = Manifest {
            schema_version: SCHEMA_VERSION,
            project: SmolStr::new("my_project"),
            generated_at: datetime!(2026-05-11 14:55:00 UTC),
            last_gc_run: Some(datetime!(2026-05-11 00:00:00 UTC)),
            artifacts: vec![sample_entry()],
        };
        let text = m.to_text();
        let parsed = Manifest::parse(Path::new("manifest.toon"), &text).unwrap();
        assert_eq!(parsed.artifacts.len(), 1);
        assert_eq!(parsed.artifacts[0], sample_entry());
        assert_eq!(parsed.last_gc_run, m.last_gc_run);
    }

    #[test]
    fn manifest_with_windows_path_is_stable_across_repeated_round_trips() {
        // `ArtifactEntry.path` carries
        // OS-native separators (backslashes on Windows). A write/parse
        // escaping asymmetry in the TOON layer used to double the
        // backslash count on every cascade-commit cycle; ten round-trips
        // through `to_text`/`parse` must leave the path byte-identical.
        let mut entry = sample_entry();
        entry.path = SmolStr::new(r".edda\cache\codegen\7b\Vec_Dependency__7b7c25946a9a.ea");
        let mut m = Manifest {
            schema_version: SCHEMA_VERSION,
            project: SmolStr::new("my_project"),
            generated_at: datetime!(2026-05-11 14:55:00 UTC),
            last_gc_run: None,
            artifacts: vec![entry.clone()],
        };
        for _ in 0..10 {
            let text = m.to_text();
            m = Manifest::parse(Path::new("manifest.toon"), &text).unwrap();
        }
        assert_eq!(m.artifacts[0].path, entry.path);
    }

    #[test]
    fn manifest_rejects_unknown_schema_version() {
        let body = "schema_version: 99\nproject: x\ngenerated_at: 2026-05-11T00:00:00.000000000Z\n";
        let err = Manifest::parse(Path::new("manifest.toon"), body).unwrap_err();
        match err {
            CacheError::SchemaVersionMismatch { found, supported, .. } => {
                assert_eq!(found, 99);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersionMismatch, got {:?}", other),
        }
    }

    #[test]
    fn manifest_rejects_non_utc_timestamps() {
        let body = "schema_version: 1\nproject: x\ngenerated_at: 2026-05-11T14:55:00+05:30\n";
        let err = Manifest::parse(Path::new("manifest.toon"), body).unwrap_err();
        match err {
            CacheError::ManifestParse { message, .. } => {
                assert!(message.contains("not UTC"), "got {}", message);
            }
            other => panic!("expected ManifestParse, got {:?}", other),
        }
    }

    #[test]
    fn argument_kind_round_trip() {
        for &k in &[
            ArgumentKind::Type,
            ArgumentKind::EffectRow,
            ArgumentKind::PrimitiveValue,
            ArgumentKind::UserValue,
        ] {
            assert_eq!(ArgumentKind::from_name(k.name()), Some(k));
        }
    }
}
