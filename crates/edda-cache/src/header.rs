//! The `// @generated` artifact-header comment block.
//!
//! Per `storage.md` §7, every generated artifact (repo- and cache-tier)
//! begins with a structured comment block. The format uses Edda's native
//! line-comment syntax (`//`) so the bytes round-trip through
//! `edda_syntax::parse_file` cleanly — the cascade-restart pass reads
//! generated artifacts as ordinary source files. Example:
//!
//! ```edda
//! // @generated
//! // spec: std.stack.Stack(i32)
//! // hash: a3f2e8b1c4d5...
//! // generator: edda-codegen <version>
//! // inputs:
//! //   body_version: 0x01
//! //   nested:
//! //     std.option.Option(i32) → Option_i32__b7e91c2a8f06
//! ```
//!
//! The header is informational — editing the header on disk does not
//! invalidate downstream hashes. But every artifact MUST carry it so
//! `inspectability.md`'s reader surface can navigate by spec/hash.

use smol_str::SmolStr;

use crate::hash::{ArtifactHash, BodyVersion};
use crate::name::ArtifactName;
use crate::toon::{self, Value, Writer};

/// Marker on the first line of every artifact header. Used by readers as
/// a magic-line check (per `storage.md` §7 in spirit; the format is
/// "every running line is grep-able").
const GENERATED_MARKER: &str = "@generated";

/// Parsed or pending artifact header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactHeader {
    /// The spec invocation that produced this artifact (e.g.
    /// `std.stack.Stack(i32)`). Stored as a free-form display string;
    /// readers do not parse the call form.
    pub spec: SmolStr,
    /// Full BLAKE3 hash of the artifact's canonical body.
    pub hash: ArtifactHash,
    /// `edda-codegen <version>` or similar.
    pub generator: SmolStr,
    /// `body_version` byte from `storage.md` §2.
    pub body_version: BodyVersion,
    /// Transitive nested-invocation dependencies, in canonical order.
    pub nested: Vec<NestedDep>,
}

/// One entry in an artifact header's `nested:` block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NestedDep {
    /// Fully-qualified spec invocation (e.g. `std.option.Option(i32)`).
    pub spec_invocation: SmolStr,
    /// Resolved artifact name of the dependency.
    pub artifact: ArtifactName,
}

impl ArtifactHeader {
    /// Serialise this header to a string. The result is the prefix of an
    /// artifact `.ea` file; the caller appends the generated module's
    /// source body afterwards.
    pub fn to_text(&self) -> String {
        let mut w = Writer::commented();
        // Bare-scalar first line: `// @generated`. The marker has no key,
        // so we use `bare` rather than `scalar`.
        w.bare(GENERATED_MARKER);
        w.scalar("spec", self.spec.as_str());
        w.scalar("hash", &self.hash.to_string());
        w.scalar("generator", self.generator.as_str());
        w.block("inputs", |w| {
            w.scalar("body_version", &self.body_version.to_string());
            if self.nested.is_empty() {
                w.empty_list("nested");
            } else {
                w.block("nested", |w| {
                    for dep in &self.nested {
                        w.scalar(dep.spec_invocation.as_str(), &dep.artifact.to_string());
                    }
                });
            }
        });
        w.finish()
    }

    /// Parse an artifact-header block from the top of an artifact's
    /// source. Stops at the first non-comment line; returns the parsed
    /// header and the byte offset where the body begins.
    pub fn parse(input: &str) -> Result<(ArtifactHeader, usize), HeaderParseError> {
        let comment_block = take_comment_prefix(input);
        if comment_block.is_empty() {
            return Err(HeaderParseError::missing("artifact header missing"));
        }
        let body_offset = comment_block.len();
        let after_marker = consume_generated_marker(comment_block)?;
        let value = toon::parse_commented(after_marker).map_err(HeaderParseError::from_toon)?;
        let header = build_header(&value)?;
        Ok((header, body_offset))
    }
}

/// Strip the `// @generated` marker line. Returns the remainder for TOON
/// parsing.
fn consume_generated_marker(comment_block: &str) -> Result<&str, HeaderParseError> {
    let mut offset = 0;
    for line in comment_block.split_inclusive('\n') {
        let body = line
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .trim_start_matches(' ');
        // Blank-comment lines and bare `//` lines are allowed before the
        // marker; otherwise the first non-empty content line must be the
        // marker.
        let inner = body.strip_prefix("// ").or_else(|| {
            if body == "//" {
                Some("")
            } else {
                None
            }
        });
        match inner {
            Some(rest) if rest.trim().is_empty() => {
                offset += line.len();
                continue;
            }
            Some(rest) if rest.trim() == GENERATED_MARKER => {
                return Ok(&comment_block[offset + line.len()..]);
            }
            Some(_) => {
                return Err(HeaderParseError::missing(format!(
                    "first header line must be `// {}`",
                    GENERATED_MARKER
                )));
            }
            None => {
                return Err(HeaderParseError::missing(
                    "expected `//`-prefixed comment line",
                ));
            }
        }
    }
    Err(HeaderParseError::missing(format!(
        "missing `// {}` marker",
        GENERATED_MARKER
    )))
}

/// Failure mode for [`ArtifactHeader::parse`].
#[derive(Clone, Debug)]
pub struct HeaderParseError {
    /// Human-readable message.
    pub message: String,
}

impl HeaderParseError {
    fn missing(msg: impl Into<String>) -> Self {
        HeaderParseError { message: msg.into() }
    }

    fn from_toon(err: toon::ParseError) -> Self {
        HeaderParseError {
            message: err.to_string(),
        }
    }
}

impl std::fmt::Display for HeaderParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HeaderParseError {}

/// Walk `input` line by line, keeping the prefix where every line is a
/// `//`-comment. Stops at the first data line (or end of file). The
/// returned slice is the comment block; the caller computes the body
/// offset from its length.
fn take_comment_prefix(input: &str) -> &str {
    let mut end = 0;
    for line in input.split_inclusive('\n') {
        let trimmed_start = line.trim_start_matches(' ');
        let line_body = trimmed_start.trim_end_matches('\n').trim_end_matches('\r');
        let is_blank = line_body.is_empty();
        let is_comment = line_body.starts_with("//");
        if is_blank || is_comment {
            end += line.len();
        } else {
            break;
        }
    }
    &input[..end]
}

/// Build an `ArtifactHeader` from a parsed TOON value tree.
fn build_header(value: &Value) -> Result<ArtifactHeader, HeaderParseError> {
    let spec = required_scalar(value, "spec")?;
    let hash_str = required_scalar(value, "hash")?;
    let hash = ArtifactHash::parse_hex(hash_str)
        .ok_or_else(|| HeaderParseError::missing(format!("invalid hash: {}", hash_str)))?;
    let generator = required_scalar(value, "generator")?;
    let inputs = value
        .get("inputs")
        .ok_or_else(|| HeaderParseError::missing("missing `inputs` block"))?;
    let body_version = inputs
        .get("body_version")
        .and_then(|v| v.as_u8_hex())
        .map(BodyVersion)
        .ok_or_else(|| HeaderParseError::missing("missing or malformed `body_version`"))?;
    let nested = read_nested(inputs)?;
    Ok(ArtifactHeader {
        spec: SmolStr::new(spec),
        hash,
        generator: SmolStr::new(generator),
        body_version,
        nested,
    })
}

/// Extract a required scalar field by name; produce a helpful error if
/// missing or non-scalar.
fn required_scalar<'a>(value: &'a Value, key: &str) -> Result<&'a str, HeaderParseError> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| HeaderParseError::missing(format!("missing `{}` field", key)))
}

/// Read the `nested:` block (either an empty list or a map of spec
/// invocations to artifact names).
fn read_nested(inputs: &Value) -> Result<Vec<NestedDep>, HeaderParseError> {
    let nested = match inputs.get("nested") {
        Some(v) => v,
        None => return Ok(Vec::new()),
    };
    if let Some(list) = nested.as_list() {
        if list.is_empty() {
            return Ok(Vec::new());
        }
        return Err(HeaderParseError::missing(
            "`nested:` must be an empty list or a block of key:value entries",
        ));
    }
    let map = nested
        .as_map()
        .ok_or_else(|| HeaderParseError::missing("`nested:` must be a map or empty list"))?;
    let mut out = Vec::with_capacity(map.len());
    for (key, value) in map {
        let name_str = value
            .as_str()
            .ok_or_else(|| HeaderParseError::missing("`nested:` value must be a scalar"))?;
        let artifact = ArtifactName::parse(name_str).map_err(|e| {
            HeaderParseError::missing(format!("`nested:` artifact name parse: {}", e))
        })?;
        out.push(NestedDep {
            spec_invocation: key.clone(),
            artifact,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_bytes;

    fn sample_header() -> ArtifactHeader {
        let hash = hash_bytes(b"abc");
        ArtifactHeader {
            spec: SmolStr::new("std.option.Option(i32)"),
            hash,
            generator: SmolStr::new("edda-codegen 0.0.0"),
            body_version: BodyVersion::CURRENT,
            nested: Vec::new(),
        }
    }

    #[test]
    fn round_trip_no_nested() {
        let header = sample_header();
        let text = header.to_text();
        // Every line must be a `//`-comment line.
        for line in text.lines() {
            assert!(
                line.is_empty() || line.starts_with("//"),
                "non-comment line in header: {:?}",
                line,
            );
        }
        // Verify the first line is the `@generated` marker.
        assert!(text.starts_with("// @generated"));
        let (parsed, body_offset) = ArtifactHeader::parse(&text).unwrap();
        assert_eq!(parsed, header);
        assert_eq!(body_offset, text.len());
    }

    #[test]
    fn round_trip_with_nested() {
        let mut header = sample_header();
        let dep_hash = hash_bytes(b"dep");
        let dep_name = ArtifactName::new("Option_i32", dep_hash.prefix()).unwrap();
        header.nested.push(NestedDep {
            spec_invocation: SmolStr::new("std.option.Option(i32)"),
            artifact: dep_name,
        });
        let text = header.to_text();
        let (parsed, _) = ArtifactHeader::parse(&text).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn parse_rejects_missing_marker() {
        // Body without any `\`-comment lines.
        let body = "fn main() {}\n";
        let err = ArtifactHeader::parse(body).unwrap_err();
        assert!(err.message.contains("missing"));
    }

    #[test]
    fn parse_stops_at_first_non_comment_line() {
        let header = sample_header().to_text();
        let combined = format!("{}\nfn body() {{}}\n", header);
        let (_, body_offset) = ArtifactHeader::parse(&combined).unwrap();
        // body_offset should land just past the header block (which ends
        // at the newline after the last `\` line plus the blank line we
        // injected).
        assert!(body_offset <= combined.len());
        let body = &combined[body_offset..];
        assert!(body.contains("fn body()"));
    }
}
