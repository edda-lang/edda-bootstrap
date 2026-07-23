//! Labelled-span `Label`, the `Diagnostic` data carrier, and the
//! append-only `Diagnostics` take.

mod collection;
mod diag;
mod label;

pub use collection::Diagnostics;
pub use diag::Diagnostic;
pub use label::{CounterexampleValue, Label, ResolvedLocation};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::DiagnosticClass;
    use crate::severity::Severity;
    use edda_span::Span;
    use edda_span::{BytePos, SourceMap};
    use std::path::{Path, PathBuf};

    #[test]
    fn diagnostic_builder_chains() {
        let map = SourceMap::new();
        let file = map.add_file(PathBuf::from("a.ea"), "let x = 1".to_string());
        let primary = Span::new(file, BytePos(0), BytePos(3));
        let secondary = Span::new(file, BytePos(4), BytePos(5));

        let d = Diagnostic::new(
            DiagnosticClass::TypecheckError,
            Severity::Error,
            primary,
            "type mismatch",
        )
        .with_primary_message("expected `i32`")
        .with_label(secondary, "found `bool`")
        .with_note("hint: try `1` instead of `true`");

        assert_eq!(d.class, DiagnosticClass::TypecheckError);
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.message, "type mismatch");
        assert_eq!(d.primary.span, primary);
        assert_eq!(d.primary.message.as_deref(), Some("expected `i32`"));
        assert!(d.primary.resolved.is_none());
        assert_eq!(d.secondary.len(), 1);
        assert_eq!(d.secondary[0].span, secondary);
        assert_eq!(d.secondary[0].message.as_deref(), Some("found `bool`"));
        assert_eq!(d.notes, vec!["hint: try `1` instead of `true`"]);
        assert!(d.canonical_form.is_none());
        assert!(d.obligation_trace.is_empty());
        assert!(d.counterexample.is_empty());
        assert!(d.is_error());
    }

    #[test]
    fn diagnostic_carries_section_6_3_required_fields() {
        let d = Diagnostic::new(
            DiagnosticClass::RefinementUnproven,
            Severity::Error,
            Span::DUMMY,
            "precondition `lo <= hi` not proven",
        )
        .with_canonical_form("geom.midpoint(lo: 5, hi: 3)")
        .with_obligation_step("5 : Int")
        .with_obligation_step("3 : Int")
        .with_obligation_step("<no in-scope predicate constrains 5 <= 3>")
        .with_counterexample_value("lo", "5")
        .with_counterexample_value("hi", "3");

        assert_eq!(
            d.canonical_form.as_deref(),
            Some("geom.midpoint(lo: 5, hi: 3)"),
        );
        assert_eq!(d.obligation_trace.len(), 3);
        assert_eq!(d.obligation_trace[2], "<no in-scope predicate constrains 5 <= 3>");
        assert_eq!(d.counterexample.len(), 2);
        assert_eq!(d.counterexample[0].name, "lo");
        assert_eq!(d.counterexample[0].value, "5");
    }

    #[test]
    fn with_obligation_trace_replaces_existing_entries() {
        let d = Diagnostic::new(
            DiagnosticClass::RefinementUnproven,
            Severity::Error,
            Span::DUMMY,
            "x",
        )
        .with_obligation_step("first")
        .with_obligation_trace(["a", "b", "c"]);

        assert_eq!(d.obligation_trace, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn label_with_resolved_attaches_location() {
        let res = ResolvedLocation::new(
            PathBuf::from("src/main.ea"),
            edda_span::LineCol { line: 42, col: 17 },
            edda_span::LineCol { line: 42, col: 35 },
        );
        let label = Label::new(Span::DUMMY).with_resolved(res);
        let resolved = label.resolved.expect("resolved must be Some");
        assert_eq!(resolved.file_name.as_ref(), Path::new("src/main.ea"));
        assert_eq!(resolved.line_col_start.line, 42);
        assert_eq!(resolved.line_col_end.col, 35);
    }

    #[test]
    fn diagnostics_sink_tracks_counts() {
        let mut diags = Diagnostics::new();
        assert!(diags.is_empty());
        assert_eq!(diags.len(), 0);
        assert_eq!(diags.error_count(), 0);
        assert!(!diags.has_errors());

        diags.push(Diagnostic::new(
            DiagnosticClass::ParseError,
            Severity::Error,
            Span::DUMMY,
            "unexpected token",
        ));
        diags.push(Diagnostic::new(
            DiagnosticClass::DeprecatedUse,
            Severity::Warn,
            Span::DUMMY,
            "use is deprecated",
        ));
        diags.push(Diagnostic::new(
            DiagnosticClass::GcRecoverable,
            Severity::Info,
            Span::DUMMY,
            "freed 4 artifacts",
        ));

        assert_eq!(diags.len(), 3);
        assert_eq!(diags.error_count(), 1);
        assert_eq!(diags.warn_count(), 1);
        assert_eq!(diags.info_count(), 1);
        assert!(diags.has_errors());

        let classes: Vec<_> = diags.iter().map(|d| d.class).collect();
        assert_eq!(
            classes,
            vec![
                DiagnosticClass::ParseError,
                DiagnosticClass::DeprecatedUse,
                DiagnosticClass::GcRecoverable,
            ]
        );
    }

    #[test]
    fn diagnostics_into_vec_returns_items_in_push_order() {
        let mut diags = Diagnostics::new();
        diags.push(Diagnostic::new(
            DiagnosticClass::UnusedImport,
            Severity::Warn,
            Span::DUMMY,
            "first",
        ));
        diags.push(Diagnostic::new(
            DiagnosticClass::ImportCycle,
            Severity::Error,
            Span::DUMMY,
            "second",
        ));
        let v = diags.into_vec();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].message, "first");
        assert_eq!(v[1].message, "second");
    }

    #[test]
    fn label_helpers() {
        let bare = Label::new(Span::DUMMY);
        assert!(bare.message.is_none());
        let labelled = Label::with_message(Span::DUMMY, "note");
        assert_eq!(labelled.message.as_deref(), Some("note"));
    }
}
