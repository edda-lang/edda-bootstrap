//! Integration: capability escalation detection (§6.3 + DiagnosticClass::CapabilityEscalation).
//!
//! Verifies the property from `corpus/edda-codex/language/08-packages.md` §6.3:
//! a v2 `.rune` that adds a capability not present in the v1 surface must be
//! detected as a capability escalation and rejected with a
//! `DiagnosticClass::CapabilityEscalation` diagnostic.
//!
//! The `edda update` orchestration (ι's slice H) is not yet wired; this test
//! operates at the library level by:
//! 1. Building v1/v2 surface files whose effect_hash diverges due to a new
//!    `Filesystem` capability.
//! 2. Computing `effect_hash` for both versions via slice F's walkers.
//! 3. Diffing the effect-row entry sets from both surface files.
//! 4. Asserting the diff detects `Filesystem` as a new entry.
//! 5. Asserting that a `CapabilityEscalation` diagnostic is emitted by the
//!    inline diff helper below (a v0.1 shim for the orchestrator entry point).
//!
//! TODO: When ι's `edda update` wires this flow through the orchestration
//! entry point, migrate this test to drive through that path rather than
//! the inline diff shim.
//!
//! Corresponds to slice I, Test 2.

use std::collections::BTreeSet;

use edda_diag::{DiagnosticClass, Diagnostics, LintConfig, Severity};
use edda_mimir_hash::{compute_effect_hash, compute_surface_hash};

// ---------------------------------------------------------------------------
// Inline diff helper (v0.1 shim for the update orchestrator)
//
// This logic belongs in the `edda update` flow. It is implemented here as a
// self-contained helper so the test can validate the property without waiting
// for ι to land. When ι's orchestration entry point ships, this helper should
// be removed and the test should call the real orchestrator.
// ---------------------------------------------------------------------------

/// Parse the set of effect-row tokens from a surface TOON file text.
///
/// Mirrors the logic in `edda_mimir_hash::effect` (which is not re-exported at
/// the granularity of "one file's entries"). We re-implement the minimal version
/// here to keep the test self-contained and avoid coupling to unexported internals.
fn parse_effect_entries_from_surface(text: &str) -> BTreeSet<String> {
    let mut entries: BTreeSet<String> = BTreeSet::new();
    let mut in_stable_table = false;
    let mut effect_col: Option<usize> = None;

    for line in text.lines() {
        let trimmed = line.trim_start();

        if trimmed.starts_with("stable_items[") && trimmed.contains(']') && trimmed.contains(':') {
            in_stable_table = true;
            // Find effect_row column index from the field list in the header.
            if let (Some(s), Some(e)) = (trimmed.find('{'), trimmed.find('}')) {
                let fields = &trimmed[s + 1..e];
                effect_col = fields.split(',').position(|f| f.trim() == "effect_row");
            }
            continue;
        }

        if in_stable_table {
            if trimmed.is_empty() {
                in_stable_table = false;
                continue;
            }
            if !line.starts_with(' ') && !line.starts_with('\t') {
                in_stable_table = false;
            }
        }

        if in_stable_table {
            if let Some(col) = effect_col {
                // Split on commas to get the effect_row field.
                let field = nth_csv_field(trimmed, col);
                let field = field.trim();
                if !field.is_empty() && field != "{}" {
                    let inner = if field.starts_with('{') && field.ends_with('}') {
                        &field[1..field.len() - 1]
                    } else {
                        field
                    };
                    for token in inner.split_whitespace() {
                        let t = token.trim();
                        if !t.is_empty() {
                            entries.insert(t.to_string());
                        }
                    }
                }
            }
        }
    }

    entries
}

/// Extract the 0-based Nth field from a comma-separated string.
fn nth_csv_field(s: &str, n: usize) -> &str {
    let mut start = 0;
    let mut count = 0;
    for (i, ch) in s.char_indices() {
        if ch == ',' {
            if count == n {
                return &s[start..i];
            }
            count += 1;
            start = i + 1;
        }
    }
    if count == n {
        &s[start..]
    } else {
        ""
    }
}

/// Diff effect-row entry sets and emit `CapabilityEscalation` diagnostics for
/// any entry in `new_entries` that is not in `max_effects`.
///
/// `max_effects` models the `max_effects` field from `[[dependencies]]` in
/// `package.toml`, which declares the ceiling that the update checker enforces.
///
/// Returns the set of escalated entries (capabilities that exceeded the ceiling).
///
/// TODO: replace with the real `edda update` orchestrator entry point when ι lands.
fn detect_capability_escalation(
    max_effects: &BTreeSet<String>,
    new_entries: &BTreeSet<String>,
    function_label: &str,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> BTreeSet<String> {
    use edda_span::Span;

    let escalated: BTreeSet<String> = new_entries.difference(max_effects).cloned().collect();

    for cap in &escalated {
        let severity = lint_cfg.effective(DiagnosticClass::CapabilityEscalation);
        let msg = format!(
            "capability escalation in `{}`: `{}` was added but is not in `max_effects`",
            function_label, cap
        );
        diags.push(edda_diag::Diagnostic::new(
            DiagnosticClass::CapabilityEscalation,
            severity,
            Span::DUMMY,
            msg,
        ));
    }

    escalated
}

// ---------------------------------------------------------------------------
// Test 2a: detect Filesystem escalation — core property
// ---------------------------------------------------------------------------

#[test]
fn capability_escalation_filesystem_detected() {
    // v1: function effect_row column (index 2) has only `Allocator`.
    // Note: effects are in the effect_row column (3rd CSV field, index 2),
    // not embedded in the signature column. The signature is "(path: String) -> Vec_u8".
    // Simple capability names without spaces to avoid tokenization ambiguity.
    let surface_v1_text = "schema_version: 3\nmodule: io\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  read_data,(path: String) -> Vec_u8,Allocator,,Read data.,\n";
    // v2: same function, effect_row now also includes `Filesystem` (escalation).
    let surface_v2_text = "schema_version: 3\nmodule: io\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  read_data,(path: String) -> Vec_u8,Allocator Filesystem,,Read data.,\n";

    let surface_v1 = vec![("io".to_string(), surface_v1_text.as_bytes().to_vec())];
    let surface_v2 = vec![("io".to_string(), surface_v2_text.as_bytes().to_vec())];

    // Verify the effect_hash changes between v1 and v2.
    let effect_v1 = compute_effect_hash(&surface_v1).expect("v1 effect_hash should succeed");
    let effect_v2 = compute_effect_hash(&surface_v2).expect("v2 effect_hash should succeed");
    assert_ne!(
        effect_v1, effect_v2,
        "adding Filesystem capability must change effect_hash"
    );

    // Simulate the update check: v1 surface becomes the max_effects ceiling.
    let entries_v1 = parse_effect_entries_from_surface(surface_v1_text);
    let entries_v2 = parse_effect_entries_from_surface(surface_v2_text);

    // max_effects = everything the v1 surface declared (or what the consumer pinned).
    let max_effects = entries_v1;

    let mut diags = Diagnostics::new();
    let lint_cfg = LintConfig::new();

    let escalated = detect_capability_escalation(
        &max_effects,
        &entries_v2,
        "read_data",
        &mut diags,
        &lint_cfg,
    );

    // The diff must identify `Filesystem` as the escalated entry.
    assert!(
        escalated.contains("Filesystem"),
        "escalated set must contain 'Filesystem'; got: {:?}",
        escalated
    );

    // Exactly one diagnostic must have been emitted.
    assert_eq!(
        diags.len(),
        1,
        "exactly one CapabilityEscalation diagnostic must be emitted; got {}",
        diags.len()
    );
    let diag = diags.iter().next().unwrap();
    assert_eq!(
        diag.class,
        DiagnosticClass::CapabilityEscalation,
        "diagnostic class must be CapabilityEscalation"
    );
    assert_eq!(
        diag.severity,
        Severity::Error,
        "CapabilityEscalation must default to Error severity"
    );

    // The diagnostic message must name the offending function.
    assert!(
        diag.message.contains("read_data"),
        "diagnostic must name the offending function; message: {:?}",
        diag.message
    );

    // The diagnostic message must name the escalated capability.
    assert!(
        diag.message.contains("Filesystem"),
        "diagnostic must name the escalated capability; message: {:?}",
        diag.message
    );
}

// ---------------------------------------------------------------------------
// Test 2b: no escalation when new capabilities are within max_effects
// ---------------------------------------------------------------------------

#[test]
fn no_escalation_when_within_max_effects() {
    // Effects in the effect_row column (index 2): "Network Allocator"
    // Use simple capability names (no spaces in token) so tokenization is unambiguous.
    let surface_text = "schema_version: 3\nmodule: net\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  connect,(host: String) -> (),Network Allocator,,Connect.,\n";

    // max_effects explicitly allows both Network and Allocator.
    let mut max_effects: BTreeSet<String> = BTreeSet::new();
    max_effects.insert("Network".to_string());
    max_effects.insert("Allocator".to_string());

    let entries = parse_effect_entries_from_surface(surface_text);

    let mut diags = Diagnostics::new();
    let lint_cfg = LintConfig::new();

    let escalated = detect_capability_escalation(
        &max_effects,
        &entries,
        "connect",
        &mut diags,
        &lint_cfg,
    );

    assert!(
        escalated.is_empty(),
        "no escalation expected when all new entries are within max_effects; got: {:?}",
        escalated
    );
    assert!(
        diags.is_empty(),
        "no diagnostics expected when no escalation; got {} diag(s)",
        diags.len()
    );
}

// ---------------------------------------------------------------------------
// Test 2c: surface_hash changes independently of effect_hash on stable-sig edit
// ---------------------------------------------------------------------------

#[test]
fn surface_hash_changes_on_stable_sig_without_effect_change() {
    // Same effect_row column, different function signature — surface_hash changes but effect_hash stays.
    // Effects are in the effect_row column (index 2): "panic".
    let surface_v1 = vec![(
        "core".to_string(),
        b"schema_version: 3\nmodule: core\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  push,(s: mutable Vec_T) -> (),panic,,Push.,\n".to_vec(),
    )];
    let surface_v2 = vec![(
        "core".to_string(),
        b"schema_version: 3\nmodule: core\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  push,(s: mutable Vec_T val: i64) -> (),panic,,Push.,\n".to_vec(),
    )];

    let surface_h1 = compute_surface_hash(&surface_v1).unwrap();
    let surface_h2 = compute_surface_hash(&surface_v2).unwrap();
    let effect_h1 = compute_effect_hash(&surface_v1).unwrap();
    let effect_h2 = compute_effect_hash(&surface_v2).unwrap();

    assert_ne!(
        surface_h1, surface_h2,
        "surface_hash must change when the stable signature changes"
    );
    assert_eq!(
        effect_h1, effect_h2,
        "effect_hash must not change when only the signature changes (same effect row)"
    );
}
