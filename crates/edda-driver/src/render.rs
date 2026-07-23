//! Diagnostic rendering — the multi-line surface format from
//! `language/06-tooling.md` §6.
//!
//! Two entry points:
//!
//! - [`resolve_labels`] walks every diagnostic's primary + secondary
//!   labels and pre-resolves their spans (file path + 1-based line/col)
//!   via the live `SourceMap`. The cascade calls this in
//!   [`crate::cascade::finish`] so the `SourceMap` does not need to
//!   survive past [`crate::outcome::Outcome`].
//!
//! - [`render_diagnostic`] formats one [`Diagnostic`] as the multi-line
//!   §6 surface format. Every diagnostic surfaces the three §6.3
//!   required fields verbatim (`canonical_form`, `obligation_trace`,
//!   `counterexample`), with `<none>` when the producer left a field
//!   empty — so the bootstrap matches native parity per §6.4.
//!
//! Worked example from §6.5:
//!
//! ```text
//! error[refinement_unproven]: precondition `lo <= hi` of `geom.midpoint` not proven at call site
//!  --> src/main.ea:42:17
//!     canonical_form: geom.midpoint(lo: 5, hi: 3)
//!     obligation_trace:
//!         5 : Int
//!         3 : Int
//!         <no in-scope predicate constrains 5 <= 3>
//!     counterexample: lo = 5, hi = 3
//!     note: called from src/main.ea:41
//!     note: declared at src/geom.ea:12
//! ```

use std::fmt::Write;

use edda_diag::{Diagnostic, Diagnostics, Label, ResolvedLocation};
use edda_span::SourceMap;

/// Walk every diagnostic in `diags` and attach a [`ResolvedLocation`]
/// to each primary / secondary label whose span is not `Span::DUMMY`.
/// Idempotent: labels that already carry a `resolved` value are left
/// alone.
pub fn resolve_labels(diags: &mut Diagnostics, source_map: &SourceMap) {
    for d in diags.iter_mut() {
        resolve_label(&mut d.primary, source_map);
        for l in &mut d.secondary {
            resolve_label(l, source_map);
        }
    }
}

/// Resolve one label's span into a file/line/col snapshot. No-op when
/// the span is dummy or the label already carries a resolved value.
fn resolve_label(label: &mut Label, source_map: &SourceMap) {
    if label.resolved.is_some() || label.span.is_dummy() {
        return;
    }
    let file = label.span.file;
    let start = source_map.byte_to_linecol(file, label.span.lo);
    let end = source_map.byte_to_linecol(file, label.span.hi);
    let path = source_map.file_name(file);
    label.resolved = Some(ResolvedLocation {
        file_name: path,
        line_col_start: start,
        line_col_end: end,
    });
}

/// Render one diagnostic as the multi-line §6 surface format.
///
/// The returned string does NOT end with a newline; the caller decides
/// how to separate consecutive diagnostics (the CLI uses `eprintln!`).
pub fn render_diagnostic(d: &Diagnostic) -> String {
    let mut out = String::new();
    write_header_line(&mut out, d);
    write_location_line(&mut out, &d.primary);
    write_primary_inline_message(&mut out, &d.primary);
    write_canonical_form(&mut out, d);
    write_obligation_trace(&mut out, d);
    write_counterexample(&mut out, d);
    write_secondary_labels(&mut out, d);
    write_notes(&mut out, d);
    out
}

/// `error[refinement_unproven]: <message>`
fn write_header_line(out: &mut String, d: &Diagnostic) {
    let _ = writeln!(out, "{}[{}]: {}", d.severity.name(), d.class.name(), d.message);
}

/// ` --> <file>:<line>:<col>`
fn write_location_line(out: &mut String, primary: &Label) {
    match &primary.resolved {
        Some(loc) => {
            let _ = writeln!(
                out,
                " --> {}:{}:{}",
                loc.file_name.display(),
                loc.line_col_start.line,
                loc.line_col_start.col,
            );
        }
        None => {
            let _ = writeln!(out, " --> <unknown>");
        }
    }
}

/// Emit the primary label's inline message as `    | <message>` when
/// the producer set one. No-op when the message is absent.
fn write_primary_inline_message(out: &mut String, primary: &Label) {
    if let Some(msg) = primary.message.as_ref() {
        let _ = writeln!(out, "    | {msg}");
    }
}

/// `    canonical_form: <value>` or `<none>` per the §6.3 always-present rule.
fn write_canonical_form(out: &mut String, d: &Diagnostic) {
    match &d.canonical_form {
        Some(form) => {
            let _ = writeln!(out, "    canonical_form: {form}");
        }
        None => {
            let _ = writeln!(out, "    canonical_form: <none>");
        }
    }
}

/// Single-line `<none>` when empty; otherwise one step per line under
/// an `obligation_trace:` header. Always present per §6.3.
fn write_obligation_trace(out: &mut String, d: &Diagnostic) {
    if d.obligation_trace.is_empty() {
        let _ = writeln!(out, "    obligation_trace: <none>");
        return;
    }
    let _ = writeln!(out, "    obligation_trace:");
    for step in &d.obligation_trace {
        let _ = writeln!(out, "        {step}");
    }
}

/// `    counterexample: name1 = value1, name2 = value2` or `<none>`.
/// Always present per §6.3.
fn write_counterexample(out: &mut String, d: &Diagnostic) {
    if d.counterexample.is_empty() {
        let _ = writeln!(out, "    counterexample: <none>");
        return;
    }
    let rendered = d
        .counterexample
        .iter()
        .map(|c| format!("{} = {}", c.name, c.value))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "    counterexample: {rendered}");
}

/// Emit each secondary label as `    note: <file>:<line>:<col> <message?>`.
/// Labels without a resolved location surface as `<unknown>` so the
/// reader can still tell the diagnostic referenced an additional site.
fn write_secondary_labels(out: &mut String, d: &Diagnostic) {
    for label in &d.secondary {
        let location = match &label.resolved {
            Some(loc) => format!(
                "{}:{}:{}",
                loc.file_name.display(),
                loc.line_col_start.line,
                loc.line_col_start.col,
            ),
            None => "<unknown>".to_string(),
        };
        match label.message.as_ref() {
            Some(msg) => {
                let _ = writeln!(out, "    note: {location}: {msg}");
            }
            None => {
                let _ = writeln!(out, "    note: {location}");
            }
        }
    }
}

/// Emit free-form footer notes (one `note:` line each).
fn write_notes(out: &mut String, d: &Diagnostic) {
    for note in &d.notes {
        let _ = writeln!(out, "    note: {note}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_diag::{CounterexampleValue, DiagnosticClass, Severity};
    use edda_span::{BytePos, Span};
    use std::path::PathBuf;

    fn header_line(s: &str) -> &str {
        s.lines().next().unwrap()
    }

    #[test]
    fn renders_all_three_required_fields_when_populated() {
        let map = SourceMap::new();
        let file = map.add_file(
            PathBuf::from("src/main.ea"),
            "let x = midpoint(5, 3)\n".to_string(),
        );
        let span = Span::new(file, BytePos(8), BytePos(22));

        let mut d = Diagnostic::new(
            DiagnosticClass::RefinementUnproven,
            Severity::Error,
            span,
            "precondition `lo <= hi` of `geom.midpoint` not proven at call site",
        )
        .with_canonical_form("geom.midpoint(lo: 5, hi: 3)")
        .with_obligation_step("5 : Int")
        .with_obligation_step("3 : Int")
        .with_obligation_step("<no in-scope predicate constrains 5 <= 3>")
        .with_counterexample_value("lo", "5")
        .with_counterexample_value("hi", "3");

        let mut diags = Diagnostics::new();
        diags.push(d.clone());
        resolve_labels(&mut diags, &map);
        d = diags.into_vec().pop().unwrap();

        let rendered = render_diagnostic(&d);
        assert!(rendered.contains("error[refinement_unproven]"));
        assert!(rendered.contains(" --> "));
        assert!(rendered.contains("src/main.ea:1:9") || rendered.contains("src\\main.ea:1:9"));
        assert!(rendered.contains("canonical_form: geom.midpoint(lo: 5, hi: 3)"));
        assert!(rendered.contains("obligation_trace:"));
        assert!(rendered.contains("5 : Int"));
        assert!(rendered.contains("<no in-scope predicate constrains 5 <= 3>"));
        assert!(rendered.contains("counterexample: lo = 5, hi = 3"));
    }

    #[test]
    fn renders_all_three_field_labels_even_when_empty() {
        let d = Diagnostic::new(
            DiagnosticClass::ParseError,
            Severity::Error,
            Span::DUMMY,
            "unexpected token",
        );
        let rendered = render_diagnostic(&d);
        assert!(rendered.contains("canonical_form: <none>"));
        assert!(rendered.contains("obligation_trace: <none>"));
        assert!(rendered.contains("counterexample: <none>"));
        assert!(rendered.contains(" --> <unknown>"));
        assert_eq!(header_line(&rendered), "error[parse_error]: unexpected token");
    }

    #[test]
    fn resolve_labels_is_idempotent() {
        let map = SourceMap::new();
        let file = map.add_file(PathBuf::from("a.ea"), "abc".to_string());
        let span = Span::new(file, BytePos(0), BytePos(3));
        let mut diags = Diagnostics::new();
        diags.push(Diagnostic::new(
            DiagnosticClass::ParseError,
            Severity::Error,
            span,
            "boom",
        ));
        resolve_labels(&mut diags, &map);
        let first = diags.iter().next().unwrap().primary.resolved.clone().unwrap();
        resolve_labels(&mut diags, &map);
        let second = diags.iter().next().unwrap().primary.resolved.clone().unwrap();
        // Idempotency: pointer equality on the Arc<Path> survives the
        // second call (the second call short-circuits without re-issuing
        // a new Arc).
        assert!(std::sync::Arc::ptr_eq(&first.file_name, &second.file_name));
        assert_eq!(first.line_col_start, second.line_col_start);
    }

    #[test]
    fn counterexample_value_round_trip() {
        let cv = CounterexampleValue::new("lo", "5");
        assert_eq!(cv.name, "lo");
        assert_eq!(cv.value, "5");
    }
}
