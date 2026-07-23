//! AST effect-row lowering — `ast::EffectRow` → [`EffectRow`].
//!
//! Walks each `ast::EffectMember`, classifies
//! it against the six locked pure-effect kinds (`err`, `yield`, `panic`,
//! `divergence`, `cancellation`, `nondet`) and produces a canonical
//! (sorted, deduplicated) [`EffectRow`]. Spreads (`...AliasName`) and
//! unknown pure-effect kinds emit `typecheck_error` and drop — alias
//! resolution awaits the module-level alias table.

use edda_diag::{Diagnostics, LintConfig};
use edda_syntax::ast;

use crate::effect::{EffectEntry, EffectRow, PureEffect};

use super::{LowerCx, emit_typecheck_error, ty::lower_type};

/// Lower an AST effect row to its canonical [`EffectRow`].
///
/// Walks each [`ast::EffectMember`] and classifies it:
///
/// - `Capability(ident)` with `ident == "panic"` → `Pure(Panic)` —
///   `panic` is a payload-less pure effect, not a parameter binding
///   (`effect-tracking.md §4`). Same shape for `divergence`
///   (`03-verification.md §5`), `cancellation`
///   (`05-concurrency-coherence.md §2.2`), and `nondet`
///   (`05-concurrency-coherence.md §"`nondet` effect for parallelism"`).
///   Other bare identifiers lower to `Capability(symbol)`.
/// - `Named { name: "err", ty }` → `Pure(Err(lower_type(ty)))`.
/// - `Named { name: "yield", ty }` → `Pure(Yield(lower_type(ty)))`.
/// - `Named { name: "panic" | "divergence" | "cancellation" | "nondet", ty }`
///   → diagnostic + dropped. All four kinds are payload-less.
/// - `Named` with any other kind → diagnostic + dropped. The locked set
///   is `err`, `yield`, `panic`, `divergence`, `cancellation`, and
///   `nondet`.
/// - `Spread(path)` → diagnostic + dropped. Row aliases need the
///   module-level alias table that lands in a later wave
///   (`effect-tracking.md §7`).
///
/// Returns a canonical [`EffectRow`] containing only the
/// successfully-classified entries — duplicates collapse and order is
/// normalised per `EffectRow::from_entries`'s contract.
pub(crate) fn lower_effect_row(
    row: &ast::EffectRow,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> EffectRow {
    let mut entries: Vec<EffectEntry> = Vec::with_capacity(row.members.len());
    for member in &row.members {
        if let Some(e) = classify_member(member, cx, diags, lint_cfg) {
            entries.push(e);
        }
    }
    EffectRow::from_entries(entries)
}

/// Classify one [`ast::EffectMember`] into an [`EffectEntry`] or drop
/// it (with a diagnostic) when the member isn't admitted in the
/// current wave.
fn classify_member(
    member: &ast::EffectMember,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<EffectEntry> {
    match member {
        ast::EffectMember::Capability(ident) => {
            let name = cx.interner.resolve(ident.name);
            match name {
                "panic" => Some(EffectEntry::Pure(PureEffect::Panic)),
                "divergence" => Some(EffectEntry::Pure(PureEffect::Divergence)),
                "cancellation" => Some(EffectEntry::Pure(PureEffect::Cancellation)),
                "nondet" => Some(EffectEntry::Pure(PureEffect::Nondet)),
                _ => Some(EffectEntry::Capability(ident.name)),
            }
        }
        ast::EffectMember::Named { name, ty } => {
            let kind = cx.interner.resolve(name.name);
            match kind {
                "err" => {
                    let payload = lower_type(ty, cx, diags, lint_cfg);
                    Some(EffectEntry::Pure(PureEffect::Err(payload)))
                }
                "yield" => {
                    let payload = lower_type(ty, cx, diags, lint_cfg);
                    Some(EffectEntry::Pure(PureEffect::Yield(payload)))
                }
                "panic" => {
                    emit_typecheck_error(
                        diags,
                        lint_cfg,
                        name.span,
                        "`panic` is a payload-less pure effect — write `panic` without `: T`",
                    );
                    None
                }
                "divergence" => {
                    emit_typecheck_error(
                        diags,
                        lint_cfg,
                        name.span,
                        "`divergence` is a payload-less pure effect — write `divergence` without `: T`",
                    );
                    None
                }
                "cancellation" => {
                    emit_typecheck_error(
                        diags,
                        lint_cfg,
                        name.span,
                        "`cancellation` is a payload-less pure effect — write `cancellation` without `: T`",
                    );
                    None
                }
                "nondet" => {
                    emit_typecheck_error(
                        diags,
                        lint_cfg,
                        name.span,
                        "`nondet` is a payload-less pure effect — write `nondet` without `: T`",
                    );
                    None
                }
                other => {
                    emit_typecheck_error(
                        diags,
                        lint_cfg,
                        name.span,
                        format!(
                            "unknown pure-effect kind `{other}`; the locked set is \
                             `err: T`, `yield: T`, `panic`, `divergence`, \
                             `cancellation`, and `nondet` (bare)"
                        ),
                    );
                    None
                }
            }
        }
        ast::EffectMember::Spread(path) => {
            emit_typecheck_error(
                diags,
                lint_cfg,
                path.span,
                "effect-row alias inclusion (`...AliasName`) is not yet supported",
            );
            None
        }
        ast::EffectMember::Graded { .. } => {
            // Graded entries (`alloc(bytes <= N)`, `io(calls <= N)`,
            // `time(ops <= N)`) carry their bound expression on the
            // function's `FnSig::graded_bounds` rather than the
            // `EffectRow` proper — the row tracks set membership,
            // bounds are signature-level data for call-site discharge.
            // `lower_fn_sig` extracts these before row lowering runs.
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Harness, ast_row, ident_for, path_for, ty_path};
    use crate::prim::Primitive;
    use edda_diag::DiagnosticClass;

    fn lower_row(h: &mut Harness, row: &ast::EffectRow) -> EffectRow {
        let cx = LowerCx {
            interner: &h.interner,
            ty_interner: &h.ty_interner,
            package: None,
        };
        lower_effect_row(row, &cx, &mut h.diags, &h.lint_cfg)
    }

    #[test]
    fn empty_ast_row_lowers_to_empty() {
        let mut h = Harness::new();
        let row = ast_row(vec![]);
        let lowered = lower_row(&mut h, &row);
        assert!(lowered.is_empty());
        assert!(h.diags.is_empty());
    }

    #[test]
    fn capability_lowers_to_capability_entry() {
        let mut h = Harness::new();
        let fs_ident = ident_for(&h.interner, "fs");
        let row = ast_row(vec![ast::EffectMember::Capability(fs_ident)]);
        let lowered = lower_row(&mut h, &row);
        let fs_sym = h.interner.intern("fs");
        assert_eq!(lowered.len(), 1);
        assert_eq!(lowered.entries()[0], EffectEntry::Capability(fs_sym));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn bare_panic_lowers_to_pure_panic() {
        let mut h = Harness::new();
        let panic_ident = ident_for(&h.interner, "panic");
        let row = ast_row(vec![ast::EffectMember::Capability(panic_ident)]);
        let lowered = lower_row(&mut h, &row);
        assert_eq!(lowered.len(), 1);
        assert_eq!(lowered.entries()[0], EffectEntry::Pure(PureEffect::Panic));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn bare_divergence_lowers_to_pure_divergence() {
        // Per `corpus/edda-codex/language/03-verification.md` §5,
        // `divergence` is the positive admission a function makes when
        // it cannot supply a `decreases` measure. Source form is
        // `with {divergence}` — a payload-less Capability ident.
        let mut h = Harness::new();
        let div_ident = ident_for(&h.interner, "divergence");
        let row = ast_row(vec![ast::EffectMember::Capability(div_ident)]);
        let lowered = lower_row(&mut h, &row);
        assert_eq!(lowered.len(), 1);
        assert_eq!(
            lowered.entries()[0],
            EffectEntry::Pure(PureEffect::Divergence)
        );
        assert!(h.diags.is_empty());
    }

    #[test]
    fn divergence_with_payload_emits_diagnostic_and_drops() {
        let mut h = Harness::new();
        let row = ast_row(vec![ast::EffectMember::Named {
            name: ident_for(&h.interner, "divergence"),
            ty: ty_path(&h.interner, "i32"),
        }]);
        let lowered = lower_row(&mut h, &row);
        assert!(lowered.is_empty());
        assert_eq!(h.diags.error_count(), 1);
        assert!(
            h.diags
                .iter()
                .next()
                .unwrap()
                .message
                .contains("payload-less")
        );
    }

    #[test]
    fn bare_cancellation_lowers_to_pure_cancellation() {
        // Per `corpus/edda-codex/language/05-concurrency-coherence.md`
        // §2.2, `.await`'s row is `{cancellation}`. Source form is
        // `with {cancellation}` — a payload-less Capability ident.
        let mut h = Harness::new();
        let cancel_ident = ident_for(&h.interner, "cancellation");
        let row = ast_row(vec![ast::EffectMember::Capability(cancel_ident)]);
        let lowered = lower_row(&mut h, &row);
        assert_eq!(lowered.len(), 1);
        assert_eq!(
            lowered.entries()[0],
            EffectEntry::Pure(PureEffect::Cancellation)
        );
        assert!(h.diags.is_empty());
    }

    #[test]
    fn cancellation_with_payload_emits_diagnostic_and_drops() {
        let mut h = Harness::new();
        let row = ast_row(vec![ast::EffectMember::Named {
            name: ident_for(&h.interner, "cancellation"),
            ty: ty_path(&h.interner, "i32"),
        }]);
        let lowered = lower_row(&mut h, &row);
        assert!(lowered.is_empty());
        assert_eq!(h.diags.error_count(), 1);
        assert!(
            h.diags
                .iter()
                .next()
                .unwrap()
                .message
                .contains("payload-less")
        );
    }

    #[test]
    fn bare_nondet_lowers_to_pure_nondet() {
        // Per `corpus/edda-codex/language/05-concurrency-coherence.md`
        // §"`nondet` effect for parallelism", a `scope(exec)` body using
        // `group.race` / `group.any` (and every ambient `Random` draw)
        // contributes `nondet`. Source form is `with {nondet}` — a
        // payload-less Capability ident, not a parameter binding named
        // `nondet`.
        let mut h = Harness::new();
        let nondet_ident = ident_for(&h.interner, "nondet");
        let row = ast_row(vec![ast::EffectMember::Capability(nondet_ident)]);
        let lowered = lower_row(&mut h, &row);
        assert_eq!(lowered.len(), 1);
        assert_eq!(lowered.entries()[0], EffectEntry::Pure(PureEffect::Nondet));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn nondet_with_payload_emits_diagnostic_and_drops() {
        let mut h = Harness::new();
        let row = ast_row(vec![ast::EffectMember::Named {
            name: ident_for(&h.interner, "nondet"),
            ty: ty_path(&h.interner, "i32"),
        }]);
        let lowered = lower_row(&mut h, &row);
        assert!(lowered.is_empty());
        assert_eq!(h.diags.error_count(), 1);
        assert!(
            h.diags
                .iter()
                .next()
                .unwrap()
                .message
                .contains("payload-less")
        );
    }

    #[test]
    fn err_named_lowers_with_payload() {
        let mut h = Harness::new();
        let row = ast_row(vec![ast::EffectMember::Named {
            name: ident_for(&h.interner, "err"),
            ty: ty_path(&h.interner, "i32"),
        }]);
        let lowered = lower_row(&mut h, &row);
        let i32_id = h.ty_interner.prim(Primitive::I32);
        assert_eq!(lowered.len(), 1);
        assert_eq!(
            lowered.entries()[0],
            EffectEntry::Pure(PureEffect::Err(i32_id))
        );
        assert!(h.diags.is_empty());
    }

    #[test]
    fn yield_named_lowers_with_payload() {
        let mut h = Harness::new();
        let row = ast_row(vec![ast::EffectMember::Named {
            name: ident_for(&h.interner, "yield"),
            ty: ty_path(&h.interner, "u8"),
        }]);
        let lowered = lower_row(&mut h, &row);
        let u8_id = h.ty_interner.prim(Primitive::U8);
        assert_eq!(lowered.len(), 1);
        assert_eq!(
            lowered.entries()[0],
            EffectEntry::Pure(PureEffect::Yield(u8_id))
        );
        assert!(h.diags.is_empty());
    }

    #[test]
    fn panic_with_payload_emits_diagnostic_and_drops() {
        let mut h = Harness::new();
        let row = ast_row(vec![ast::EffectMember::Named {
            name: ident_for(&h.interner, "panic"),
            ty: ty_path(&h.interner, "i32"),
        }]);
        let lowered = lower_row(&mut h, &row);
        assert!(lowered.is_empty());
        assert_eq!(h.diags.error_count(), 1);
        assert!(
            h.diags
                .iter()
                .next()
                .unwrap()
                .message
                .contains("payload-less")
        );
    }

    #[test]
    fn unknown_named_kind_emits_diagnostic_and_drops() {
        let mut h = Harness::new();
        let row = ast_row(vec![ast::EffectMember::Named {
            name: ident_for(&h.interner, "bogus"),
            ty: ty_path(&h.interner, "i32"),
        }]);
        let lowered = lower_row(&mut h, &row);
        assert!(lowered.is_empty());
        assert_eq!(h.diags.error_count(), 1);
        let msg = &h.diags.iter().next().unwrap().message;
        assert!(msg.contains("bogus"));
        assert!(msg.contains("unknown pure-effect kind"));
    }

    #[test]
    fn spread_emits_diagnostic_and_drops() {
        let mut h = Harness::new();
        let alias_path = path_for(&h.interner, &["ParseEffects"]);
        let row = ast_row(vec![ast::EffectMember::Spread(alias_path)]);
        let lowered = lower_row(&mut h, &row);
        assert!(lowered.is_empty());
        assert_eq!(h.diags.error_count(), 1);
        assert!(
            h.diags
                .iter()
                .next()
                .unwrap()
                .message
                .contains("alias inclusion")
        );
    }

    #[test]
    fn duplicate_entries_collapse() {
        let mut h = Harness::new();
        let row = ast_row(vec![
            ast::EffectMember::Capability(ident_for(&h.interner, "fs")),
            ast::EffectMember::Capability(ident_for(&h.interner, "fs")),
        ]);
        let lowered = lower_row(&mut h, &row);
        assert_eq!(lowered.len(), 1);
        assert!(h.diags.is_empty());
    }

    #[test]
    fn mixed_row_lowers_to_canonical_form() {
        let mut h = Harness::new();
        let row = ast_row(vec![
            ast::EffectMember::Capability(ident_for(&h.interner, "fs")),
            ast::EffectMember::Capability(ident_for(&h.interner, "allocator")),
            ast::EffectMember::Named {
                name: ident_for(&h.interner, "err"),
                ty: ty_path(&h.interner, "i32"),
            },
            ast::EffectMember::Capability(ident_for(&h.interner, "panic")),
            ast::EffectMember::Named {
                name: ident_for(&h.interner, "yield"),
                ty: ty_path(&h.interner, "u8"),
            },
        ]);
        let lowered = lower_row(&mut h, &row);
        assert_eq!(lowered.len(), 5);
        let i32_id = h.ty_interner.prim(Primitive::I32);
        let u8_id = h.ty_interner.prim(Primitive::U8);
        let fs_sym = h.interner.intern("fs");
        let alloc_sym = h.interner.intern("allocator");
        assert!(lowered.contains(&EffectEntry::Capability(fs_sym)));
        assert!(lowered.contains(&EffectEntry::Capability(alloc_sym)));
        assert!(lowered.contains(&EffectEntry::Pure(PureEffect::Err(i32_id))));
        assert!(lowered.contains(&EffectEntry::Pure(PureEffect::Yield(u8_id))));
        assert!(lowered.contains(&EffectEntry::Pure(PureEffect::Panic)));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn err_payload_lowering_failure_propagates_as_error_ty() {
        let mut h = Harness::new();
        let row = ast_row(vec![ast::EffectMember::Named {
            name: ident_for(&h.interner, "err"),
            ty: ty_path(&h.interner, "NotPrimitive"),
        }]);
        let lowered = lower_row(&mut h, &row);
        assert_eq!(lowered.len(), 1);
        assert_eq!(
            lowered.entries()[0],
            EffectEntry::Pure(PureEffect::Err(h.ty_interner.error()))
        );
        assert_eq!(h.diags.error_count(), 1);
    }

}
