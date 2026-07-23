//! Sum-type variant constructor inference (`declarations.md` §252).
//!
//! `synth_variant_constructor` type-checks a `<Type>.<variant>(args)`
//! call against a sum-type declaration, dispatching on the variant's
//! payload shape (Unit / Tuple / Struct).

use edda_span::Span;

use crate::cx::{VariantInfo, VariantPayloadInfo};
use crate::hir::HirCallArg;
use crate::ty::TyKind;

use super::super::path::check_synth_against_expected;
use super::super::{InferCx, TyEnv};
use super::args::{check_arg_expr, synth_arg_expr, synth_args_for_diagnostics};

/// Type-check a sum-type variant constructor call. Handles the three
/// payload shapes: `Unit` (no args admitted), `Tuple { elems }`
/// (positional args checked against declared element types), `Struct
/// { fields }` (named args checked against declared field types; any
/// declaration order is accepted; missing / duplicate / unknown
/// fields diagnose at the call site).
pub(super) fn synth_variant_constructor(
    env: &mut TyEnv,
    sum_binding: edda_resolve::BindingId,
    sum_name: edda_intern::Symbol,
    variant_sym: edda_intern::Symbol,
    variant_info: Option<VariantInfo>,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> crate::ty::TyId {
    let nominal = ic
        .lower
        .ty_interner
        .intern_kind(TyKind::Nominal(sum_binding));
    let Some(variant) = variant_info else {
        let type_text = ic.lower.interner.resolve(sum_name).to_string();
        let variant_text = ic.lower.interner.resolve(variant_sym).to_string();
        synth_args_for_diagnostics(env, args, ic);
        ic.emit_typecheck_error(
            span,
            format!("no variant `{variant_text}` on sum type `{type_text}`"),
        );
        return ic.ty_error();
    };
    let variant_text = ic.lower.interner.resolve(variant_sym).to_string();
    let type_text = ic.lower.interner.resolve(sum_name).to_string();
    match variant.payload {
        VariantPayloadInfo::Unit => {
            if !args.is_empty() {
                synth_args_for_diagnostics(env, args, ic);
                ic.emit_typecheck_error(
                    span,
                    format!(
                        "variant `{type_text}.{variant_text}` has no payload but \
                         {n} argument(s) were supplied",
                        n = args.len(),
                    ),
                );
                return ic.ty_error();
            }
            nominal
        }
        VariantPayloadInfo::Tuple { elems } => {
            // Composite-payload form (D-22): a
            // single positional argument whose type is the payload composite
            // tuple `(T1, …, Tn)` constructs the variant directly — the
            // expander emits this for `U.(i)(payload)`. Distinct from the
            // positional-unpacked form `U.data(a, b)`; for a single-element
            // payload the composite `(T)` and the bare element `T` are
            // distinguished by type (one-element tuple vs. bare).
            if args.len() == 1 && args[0].name.is_none() {
                let composite = payload_composite_ty(ic, elems.iter().copied());
                synth_arg_expr(env, &mut args[0], ic);
                let arg_ty = args[0].expr.ty;
                if arg_ty == composite {
                    return nominal;
                }
                // Not the composite: for a single-element payload this is the
                // ordinary unpacked form `U.data(x)`. Bridge the already-synth'd
                // argument against the element type without re-synthesising it —
                // a re-synth would double-run the argument's effects (a `?`, a
                // linear move).
                if elems.len() == 1 {
                    let bridged =
                        check_synth_against_expected(arg_ty, elems[0], ic, args[0].expr.span);
                    args[0].expr.ty = bridged;
                    return nominal;
                }
                ic.emit_typecheck_error(
                    span,
                    format!(
                        "variant `{type_text}.{variant_text}` takes {declared} positional \
                         argument(s) but 1 was supplied",
                        declared = elems.len(),
                    ),
                );
                return ic.ty_error();
            }
            if check_tuple_payload(env, &elems, args, &type_text, &variant_text, ic, span) {
                return ic.ty_error();
            }
            nominal
        }
        VariantPayloadInfo::Struct { fields } => {
            // Composite-payload form: a single
            // positional argument whose type is the payload composite tuple
            // `(F1, …, Fn)` (fields in declaration order) constructs the
            // variant. Named-argument construction remains the hand-written
            // form (`U.pair(a: x, b: y)`).
            if args.len() == 1 && args[0].name.is_none() {
                let composite = payload_composite_ty(ic, fields.iter().map(|f| f.ty));
                synth_arg_expr(env, &mut args[0], ic);
                if args[0].expr.ty == composite {
                    return nominal;
                }
                ic.emit_typecheck_error(
                    args[0].span,
                    format!(
                        "struct-payload variant `{type_text}.{variant_text}` requires \
                         named arguments (`field: expr`)",
                    ),
                );
                return nominal;
            }
            check_struct_payload(env, &fields, args, &type_text, &variant_text, ic, span);
            nominal
        }
    }
}

/// Check a tuple-payload variant's positional arguments against the
/// declared element types. Returns `true` when an arity mismatch was
/// diagnosed so the caller yields the error sentinel.
fn check_tuple_payload(
    env: &mut TyEnv,
    elems: &[crate::ty::TyId],
    args: &mut [HirCallArg],
    type_text: &str,
    variant_text: &str,
    ic: &mut InferCx<'_>,
    span: Span,
) -> bool {
    if args.len() != elems.len() {
        synth_args_for_diagnostics(env, args, ic);
        ic.emit_typecheck_error(
            span,
            format!(
                "variant `{type_text}.{variant_text}` takes {declared} positional \
                 argument(s) but {supplied} were supplied",
                declared = elems.len(),
                supplied = args.len(),
            ),
        );
        return true;
    }
    for arg in args.iter() {
        if let Some(name) = &arg.name {
            let field_text = ic.lower.interner.resolve(name.name).to_string();
            ic.emit_typecheck_error(
                arg.span,
                format!(
                    "named argument `{field_text}:` is not admitted at \
                     tuple-payload variant `{type_text}.{variant_text}`",
                ),
            );
        }
    }
    for (arg, ty) in args.iter_mut().zip(elems.iter()) {
        check_arg_expr(env, arg, *ty, ic);
    }
    false
}

/// Check a struct-payload variant's named arguments against the
/// declared field types. Struct-payload variants take named args
/// (`field: expr`); any declaration order is accepted.
fn check_struct_payload(
    env: &mut TyEnv,
    fields: &[crate::cx::FieldInfo],
    args: &mut [HirCallArg],
    type_text: &str,
    variant_text: &str,
    ic: &mut InferCx<'_>,
    span: Span,
) {
    let mut seen = vec![false; fields.len()];
    for arg in args.iter_mut() {
        let Some(name) = &arg.name else {
            ic.emit_typecheck_error(
                arg.span,
                format!(
                    "struct-payload variant `{type_text}.{variant_text}` requires \
                     named arguments (`field: expr`)",
                ),
            );
            synth_arg_expr(env, arg, ic);
            continue;
        };
        let fname = name.name;
        let idx = fields.iter().position(|f| f.name == fname);
        match idx {
            Some(i) => {
                if seen[i] {
                    let field_text = ic.lower.interner.resolve(fname).to_string();
                    ic.emit_typecheck_error(
                        arg.span,
                        format!("duplicate field `{field_text}` in variant payload"),
                    );
                } else {
                    seen[i] = true;
                }
                let declared_ty = fields[i].ty;
                check_arg_expr(env, arg, declared_ty, ic);
            }
            None => {
                let field_text = ic.lower.interner.resolve(fname).to_string();
                ic.emit_typecheck_error(
                    arg.span,
                    format!(
                        "variant `{type_text}.{variant_text}` has no field \
                         `{field_text}`",
                    ),
                );
                synth_arg_expr(env, arg, ic);
            }
        }
    }
    let missing: Vec<edda_intern::Symbol> = fields
        .iter()
        .zip(seen.iter())
        .filter_map(|(f, p)| if *p { None } else { Some(f.name) })
        .collect();
    if !missing.is_empty() {
        let names: Vec<String> = missing
            .iter()
            .map(|s| format!("`{}`", ic.lower.interner.resolve(*s)))
            .collect();
        ic.emit_typecheck_error(
            span,
            format!(
                "variant `{type_text}.{variant_text}` is missing field(s) {}",
                names.join(", "),
            ),
        );
    }
}

// single payload (distinct from bare `T`) and `(T1, …, Tn)`
// otherwise; the empty guard is defensive — a payload-less variant never reaches
// here (it takes no argument)
/// Intern the positional payload *composite* for a variant constructor's
/// single-argument form — the same convention `field_type_at` / the `v.(i)`
/// READ use (`infer/comp/tuple.rs::payload_composite`). The
/// composite is what the expander-emitted `U.(i)(payload)` supplies.
fn payload_composite_ty(
    ic: &InferCx<'_>,
    elems: impl IntoIterator<Item = crate::ty::TyId>,
) -> crate::ty::TyId {
    let tys: Vec<crate::ty::TyId> = elems.into_iter().collect();
    if tys.is_empty() {
        return ic.lower.ty_interner.prim(crate::prim::Primitive::Unit);
    }
    ic.lower.ty_interner.tuple(tys)
}

#[cfg(test)]
mod tests {
    //! The single-composite-argument
    //! constructor form the expander emits for `U.(i)(payload)`. A
    //! payload-bearing variant is constructed from one positional argument
    //! whose type equals the payload composite tuple — `(T)` for a single
    //! payload, `(T1, …, Tn)` for a multi/struct payload — while the
    //! hand-written positional-unpacked and named forms stay admitted.
    use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
    use edda_intern::{Interner, Symbol};
    use edda_resolve::{BindingId, ModuleId};
    use edda_span::Span;
    use edda_syntax::ast::Ident;

    use crate::cx::{FieldInfo, TyCx, VariantInfo, VariantPayloadInfo};
    use crate::hir::{HirCallArg, HirExpr, HirExprKind, HirPath};
    use crate::infer::{InferCx, TyEnv};
    use crate::lower::LowerCx;
    use crate::prim::Primitive;
    use crate::ty::{TyId, TyInterner};

    struct Harness {
        interner: Interner,
        ty_interner: TyInterner,
        ty_cx: TyCx,
        diags: Diagnostics,
        lint_cfg: LintConfig,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                interner: Interner::new(),
                ty_interner: TyInterner::new(),
                ty_cx: TyCx::new(),
                diags: Diagnostics::new(),
                lint_cfg: LintConfig::new(),
            }
        }

        fn typecheck_count(&self) -> usize {
            self.diags
                .iter()
                .filter(|d| d.class == DiagnosticClass::TypecheckError)
                .count()
        }
    }

    fn ident(sym: Symbol) -> Ident {
        Ident { name: sym, span: Span::DUMMY }
    }

    /// A positional (no-name) argument that is a bare path reference to a
    /// binding — synthesises to that binding's type.
    fn path_arg(sym: Symbol, ty: TyId) -> HirCallArg {
        HirCallArg {
            span: Span::DUMMY,
            mode: None,
            name: None,
            expr: HirExpr {
                span: Span::DUMMY,
                ty,
                kind: HirExprKind::Path(HirPath {
                    span: Span::DUMMY,
                    segments: Box::from([ident(sym)]),
                }),
            },
        }
    }

    // Run `synth_variant_constructor` against a synthetic sum binding and
    // return the resulting `TyId` plus the typecheck-error count.
    fn construct(
        hx: &mut Harness,
        payload: VariantPayloadInfo,
        arg_binding: Symbol,
        arg_ty: TyId,
    ) -> (TyId, usize) {
        let sum_binding = BindingId::new(ModuleId::new(0), 7);
        let sum_name = hx.interner.intern("Msg");
        let variant_sym = hx.interner.intern("data");
        let variant_info = VariantInfo { span: Span::DUMMY, name: variant_sym, payload };
        let mut args = [path_arg(arg_binding, arg_ty)];
        let cx = LowerCx {
            interner: &hx.interner,
            ty_interner: &hx.ty_interner,
            package: None,
        };
        let mut ic = InferCx::new(&cx, &hx.ty_cx, &hx.lint_cfg, &mut hx.diags);
        let mut env = TyEnv::new();
        env.bind(arg_binding, arg_ty);
        let nominal = ic
            .lower
            .ty_interner
            .intern_kind(crate::ty::TyKind::Nominal(sum_binding));
        let result = super::synth_variant_constructor(
            &mut env,
            sum_binding,
            sum_name,
            variant_sym,
            Some(variant_info),
            &mut args,
            &mut ic,
            Span::DUMMY,
        );
        let mismatch = if result == nominal { 0 } else { 1 };
        let base = hx.typecheck_count();
        (result, base + mismatch)
    }

    #[test]
    fn single_payload_composite_arg_is_accepted() {
        // `Msg.data(t)` with `t: (u32)` — the composite tuple form.
        let mut hx = Harness::new();
        let u32_ty = hx.ty_interner.prim(Primitive::U32);
        let composite = hx.ty_interner.tuple(vec![u32_ty]);
        let t = hx.interner.intern("t");
        let (_, errs) = construct(
            &mut hx,
            VariantPayloadInfo::Tuple { elems: Box::from([u32_ty]) },
            t,
            composite,
        );
        assert_eq!(errs, 0, "composite `(u32)` argument should construct cleanly");
    }

    #[test]
    fn single_payload_unpacked_arg_is_accepted() {
        // `Msg.data(x)` with `x: u32` — the hand-written unpacked form,
        // distinct from the composite `(u32)` by type.
        let mut hx = Harness::new();
        let u32_ty = hx.ty_interner.prim(Primitive::U32);
        let x = hx.interner.intern("x");
        let (_, errs) = construct(
            &mut hx,
            VariantPayloadInfo::Tuple { elems: Box::from([u32_ty]) },
            x,
            u32_ty,
        );
        assert_eq!(errs, 0, "bare `u32` argument should still construct cleanly");
    }

    #[test]
    fn struct_payload_composite_arg_is_accepted() {
        // `Msg.pair(t)` with `t: (u16, u16)` — a struct-payload variant
        // constructed from the positional payload composite.
        let mut hx = Harness::new();
        let u16_ty = hx.ty_interner.prim(Primitive::U16);
        let composite = hx.ty_interner.tuple(vec![u16_ty, u16_ty]);
        let field_a = hx.interner.intern("a");
        let field_b = hx.interner.intern("b");
        let fields = Box::from([
            FieldInfo { span: Span::DUMMY, name: field_a, ty: u16_ty, refinement: None },
            FieldInfo { span: Span::DUMMY, name: field_b, ty: u16_ty, refinement: None },
        ]);
        let t = hx.interner.intern("t");
        let (_, errs) = construct(&mut hx, VariantPayloadInfo::Struct { fields }, t, composite);
        assert_eq!(errs, 0, "composite `(u16, u16)` should construct the struct-payload variant");
    }
}
