//! Minimal TOON reader/writer scoped to this crate's schemas.
//!
//! TOON is the human-readable, line-oriented data format used by Edda's
//! manifests and artifact headers. The format used by cache:
//!
//! ```toon
//! \ comment line
//! schema_version: 1
//! project: "my project"
//! generated_at: 2026-05-11T14:55:00Z
//!
//! artifacts[N]{path,hash,short_name,tier,...}:
//!   - path: codegen/std/option/Option_i32__a3f2e8b1c4d5.ea
//!     hash: a3f2e8b1c4d5...
//!     inputs:
//!       body_version: 0x01
//!       argument_tuple:
//!         - kind: type
//!           value: i32
//!       nested_deps: []
//! ```
//!
//! Supported features (scoped to cache's needs):
//!   - Comments: a line whose first non-whitespace character is `\`.
//!   - Scalar key/value: `key: value` (unquoted) or `key: "value"` (quoted).
//!   - Empty lists: `key: []`.
//!   - Indented block sub-maps: `key:` followed by deeper-indented lines.
//!   - YAML-style list of maps: `key:` followed by `  - field: value` rows.
//!   - Schema annotations on list keys (`key[N]{f1,f2,...}:`): parsed and
//!     discarded; the writer can emit them via
//!     [`Writer::list_with_schema`].
//!
//! Out of scope (and rejected with a parse error):
//!   - Single-line row tables (`dev,0,full,address`).
//!   - Tabs in indentation.
//!   - Trailing comments on data lines.
//!
//! Indentation is exactly two spaces per level, the convention used by
//! every spec example. The lexer rejects odd indent counts and tabs.

mod lex;
mod parse;
mod value;
mod write;

pub use parse::{parse, parse_commented};
pub use value::{ParseError, Value};
pub use write::Writer;

/// The locked indent step. Every spec example uses two spaces; the parser
/// and writer both enforce it.
pub(crate) const INDENT_STEP: usize = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_as_u32_parses_decimal() {
        let v = parse("count: 42\n").unwrap();
        assert_eq!(v.get("count").unwrap().as_u32(), Some(42));
        let bad = parse("count: not-a-number\n").unwrap();
        assert_eq!(bad.get("count").unwrap().as_u32(), None);
    }

    #[test]
    fn parse_top_level_scalars() {
        let input = "schema_version: 1\nproject: my_project\n";
        let v = parse(input).unwrap();
        assert_eq!(v.get("schema_version").unwrap().as_str(), Some("1"));
        assert_eq!(v.get("project").unwrap().as_str(), Some("my_project"));
    }

    #[test]
    fn parse_quoted_string() {
        let input = "name: \"hello world\"\n";
        let v = parse(input).unwrap();
        assert_eq!(v.get("name").unwrap().as_str(), Some("hello world"));
    }

    #[test]
    fn parse_empty_inline_list() {
        let input = "nested_deps: []\n";
        let v = parse(input).unwrap();
        let list = v.get("nested_deps").unwrap().as_list().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn parse_indented_block() {
        let input = "inputs:\n  body_version: 0x01\n  spec_qualified_name: std.option.Option\n";
        let v = parse(input).unwrap();
        let inputs = v.get("inputs").unwrap();
        assert_eq!(
            inputs.get("body_version").unwrap().as_u8_hex(),
            Some(0x01)
        );
        assert_eq!(
            inputs.get("spec_qualified_name").unwrap().as_str(),
            Some("std.option.Option"),
        );
    }

    #[test]
    fn parse_skips_comments() {
        let input = "\\ comment line\nschema_version: 1\n\\ another\n";
        let v = parse(input).unwrap();
        assert_eq!(v.get("schema_version").unwrap().as_u32(), Some(1));
    }

    #[test]
    fn parse_rejects_tabs() {
        let input = "key:\n\tvalue\n";
        let err = parse(input).unwrap_err();
        assert!(err.message.contains("tab"));
    }

    #[test]
    fn parse_rejects_odd_indent() {
        let input = "key:\n value\n";
        let err = parse(input).unwrap_err();
        assert!(err.message.contains("indent"));
    }

    #[test]
    fn parse_list_of_maps() {
        let input = "items:\n  - name: a\n    value: 1\n  - name: b\n    value: 2\n";
        let v = parse(input).unwrap();
        let items = v.get("items").unwrap().as_list().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get("name").unwrap().as_str(), Some("a"));
        assert_eq!(items[0].get("value").unwrap().as_u32(), Some(1));
        assert_eq!(items[1].get("name").unwrap().as_str(), Some("b"));
    }

    #[test]
    fn parse_schema_annotation_is_dropped() {
        let input = "artifacts[2]{name,value}:\n  - name: a\n    value: 1\n  - name: b\n    value: 2\n";
        let v = parse(input).unwrap();
        let arts = v.get("artifacts").unwrap().as_list().unwrap();
        assert_eq!(arts.len(), 2);
    }

    #[test]
    fn parse_commented_header() {
        // The `// @generated` marker line is the header module's
        // responsibility; this test exercises the TOON-in-comments
        // parser directly with key:value lines only.
        let input = "// spec: std.stack.Stack(i32)\n// hash: deadbeef\n";
        let v = parse_commented(input).unwrap();
        assert_eq!(v.get("spec").unwrap().as_str(), Some("std.stack.Stack(i32)"));
        assert_eq!(v.get("hash").unwrap().as_str(), Some("deadbeef"));
    }

    #[test]
    fn parse_commented_nested_block() {
        let input =
            "// inputs:\n//   body_version: 0x01\n//   nested_deps: []\n";
        let v = parse_commented(input).unwrap();
        let inputs = v.get("inputs").unwrap();
        assert_eq!(
            inputs.get("body_version").unwrap().as_u8_hex(),
            Some(0x01)
        );
        assert!(inputs.get("nested_deps").unwrap().as_list().unwrap().is_empty());
    }

    #[test]
    fn writer_scalars_round_trip() {
        let mut w = Writer::new();
        w.scalar("schema_version", "1");
        w.scalar("project", "my project"); // forces quoting
        let s = w.finish();
        let v = parse(&s).unwrap();
        assert_eq!(v.get("project").unwrap().as_str(), Some("my project"));
        assert_eq!(v.get("schema_version").unwrap().as_u32(), Some(1));
    }

    #[test]
    fn writer_block_round_trip() {
        let mut w = Writer::new();
        w.block("inputs", |w| {
            w.scalar("body_version", "0x01");
            w.empty_list("nested_deps");
        });
        let s = w.finish();
        let v = parse(&s).unwrap();
        let inputs = v.get("inputs").unwrap();
        assert_eq!(inputs.get("body_version").unwrap().as_u8_hex(), Some(0x01));
        assert!(inputs.get("nested_deps").unwrap().as_list().unwrap().is_empty());
    }

    #[test]
    fn writer_commented_round_trip() {
        let mut w = Writer::commented();
        w.scalar("hash", "deadbeef");
        w.block("inputs", |w| {
            w.scalar("body_version", "0x01");
        });
        let s = w.finish();
        // Every non-blank line must start with `//`.
        for line in s.lines() {
            if !line.is_empty() {
                assert!(
                    line.starts_with("//"),
                    "commented writer produced non-comment line: {:?}",
                    line
                );
            }
        }
        let v = parse_commented(&s).unwrap();
        assert_eq!(v.get("hash").unwrap().as_str(), Some("deadbeef"));
    }

    #[test]
    fn writer_list_with_schema_round_trip() {
        let mut w = Writer::new();
        w.list_with_schema("artifacts", &["path", "hash"], 2, |w| {
            w.list_item("path", "a.ea", |w| {
                w.scalar("hash", "deadbeef");
            });
            w.list_item("path", "b.ea", |w| {
                w.scalar("hash", "cafef00d");
            });
        });
        let s = w.finish();
        // Verify the schema annotation is present on disk.
        assert!(
            s.starts_with("artifacts[2]{path,hash}:"),
            "missing schema annotation: {:?}",
            s,
        );
        let v = parse(&s).unwrap();
        let items = v.get("artifacts").unwrap().as_list().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get("path").unwrap().as_str(), Some("a.ea"));
        assert_eq!(items[0].get("hash").unwrap().as_str(), Some("deadbeef"));
        assert_eq!(items[1].get("path").unwrap().as_str(), Some("b.ea"));
        assert_eq!(items[1].get("hash").unwrap().as_str(), Some("cafef00d"));
    }

    #[test]
    fn writer_quotes_special_characters() {
        let mut w = Writer::new();
        w.scalar("path", "a:b");
        w.scalar("nested", "x,y");
        let s = w.finish();
        assert!(s.contains("\"a:b\""));
        assert!(s.contains("\"x,y\""));
    }

    #[test]
    fn backslash_scalar_round_trips_through_write_and_parse() {
        let original = r".edda\cache\codegen\7b\Vec_Dependency__7b7c25946a9a.ea";
        let mut w = Writer::new();
        w.scalar("path", original);
        let s = w.finish();
        let v = parse(&s).unwrap();
        assert_eq!(v.get("path").unwrap().as_str(), Some(original));
    }

    #[test]
    fn backslash_scalar_is_stable_across_repeated_write_parse_cycles() {
        // A write/parse asymmetry doubled the
        // backslash count in Windows paths on every cascade-commit cycle,
        // eventually tripping the manifest's sanity-budget guard after weeks
        // of shared-checkout builds. Ten round-trips must be a no-op.
        let mut value = r".edda\cache\codegen\7b\Vec_Dependency__7b7c25946a9a.ea".to_string();
        for _ in 0..10 {
            let mut w = Writer::new();
            w.scalar("path", &value);
            let s = w.finish();
            let v = parse(&s).unwrap();
            value = v.get("path").unwrap().as_str().unwrap().to_string();
        }
        assert_eq!(
            value,
            r".edda\cache\codegen\7b\Vec_Dependency__7b7c25946a9a.ea"
        );
    }

    #[test]
    fn quoted_scalar_unescapes_embedded_quote() {
        let mut w = Writer::new();
        w.scalar("msg", "she said \"hi\"");
        let s = w.finish();
        let v = parse(&s).unwrap();
        assert_eq!(v.get("msg").unwrap().as_str(), Some("she said \"hi\""));
    }
}
