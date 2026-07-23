//! Call-site typing for the locked comptime built-ins (§C10).
//!
//! Implements the call-site rule for the locked comptime built-ins from
//! `corpus/edda-codex/language/04-specs-comptime.md`
//! *Comptime built-ins* and `06-tooling.md` §9. The resolver does not
//! record catalogue heads in its `resolutions` map, so [`super::call`]
//! routes here before the resolver lookup. Each built-in has a fixed
//! arity and locked parameter shape:
//!
//! ```text
//! size_of(comptime T: Type)                       -> usize
//! align_of(comptime T: Type)                      -> usize
//! offset_of(comptime T: Type, comptime f: ident)  -> usize
//! target_has(comptime feature: String)            -> bool
//! field_count(comptime T: Type)                   -> usize
//! field_name_at(comptime T: Type, comptime i: usize) -> String
//! field_type_at(comptime T: Type, comptime i: usize) -> Type
//! is_signed(comptime T: Type)                     -> bool
//! is_unsigned(comptime T: Type)                   -> bool
//! is_integer(comptime T: Type)                    -> bool
//! is_floating(comptime T: Type)                   -> bool
//! is_numeric(comptime T: Type)                    -> bool
//! is_primitive(comptime T: Type)                  -> bool
//! is_sum(comptime T: Type)                        -> bool
//! ```
//!
//! `Type`-shaped arguments must check against the `Type` meta-primitive
//! (`HirPath` resolves to a primitive or user type per
//! [`super::path::synth_path`]'s comptime branch). `String`-shaped
//! arguments check against [`crate::Primitive::String`]. `offset_of`'s
//! second `ident` parameter is accepted as a single-segment `Path`
//! whose name will be used by the evaluator as the field key — the
//! typechecker admits the call so the evaluator's
//! "offset_of not yet supported" diagnostic still fires from a
//! type-checked call site.

use edda_span::Span;

use crate::comptime_builtin::ComptimeBuiltin;
use crate::hir::{HirCallArg, HirExprKind};
use crate::prim::Primitive;
use crate::ty::TyId;

use super::path::check_synth_against_expected;
use super::{InferCx, TyEnv, check_expr, synth_expr};

/// Type-check a `<builtin>(args)` call where `<builtin>` is one of the
/// locked comptime built-ins. Records the resolved
/// [`ComptimeBuiltin`] on `ic.comptime_builtin_calls` keyed by `span`
/// so MIR lowering / the codegen-side evaluator can recover the
/// resolution without re-walking the resolver catalogue.
pub(super) fn synth_comptime_builtin_call(
    env: &mut TyEnv,
    path: &crate::hir::HirPath,
    builtin: ComptimeBuiltin,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let name = builtin.name();
    let arity = builtin.arity();

    // Reject named-payload / mode-decorated arguments. Comptime
    // built-ins are positional pure functions; admitting either form
    // would let the syntactic shape silently drift from the spec.
    if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
        let field = arg
            .name
            .as_ref()
            .map(|n| ic.lower.interner.resolve(n.name).to_string())
            .unwrap_or_default();
        ic.emit_typecheck_error(
            arg.span,
            format!("named argument `{field}:` is not admitted on comptime built-in `{name}`"),
        );
    }
    if let Some(arg) = args.iter().find(|a| a.mode.is_some()) {
        ic.emit_typecheck_error(
            arg.span,
            format!("call-site mode keyword is not admitted on comptime built-in `{name}`"),
        );
    }

    if args.len() != arity {
        synth_args_unchecked(env, args, ic);
        ic.emit_typecheck_error(
            span,
            format!(
                "built-in `{name}` takes {arity} argument(s) but {found} were supplied",
                found = args.len()
            ),
        );
        return ic.ty_error();
    }

    match builtin {
        ComptimeBuiltin::SizeOf | ComptimeBuiltin::AlignOf => {
            type_check_type_arg(env, &mut args[0], ic);
            record_call(ic, path.span, span, builtin);
            ic.lower.ty_interner.prim(Primitive::Usize)
        }
        ComptimeBuiltin::OffsetOf => {
            type_check_type_arg(env, &mut args[0], ic);
            type_check_ident_arg(env, &mut args[1], ic);
            record_call(ic, path.span, span, builtin);
            ic.lower.ty_interner.prim(Primitive::Usize)
        }
        ComptimeBuiltin::TargetHas => {
            type_check_string_arg(env, &mut args[0], ic);
            record_call(ic, path.span, span, builtin);
            ic.lower.ty_interner.prim(Primitive::Bool)
        }
        ComptimeBuiltin::FieldCount => {
            type_check_type_arg(env, &mut args[0], ic);
            record_call(ic, path.span, span, builtin);
            ic.lower.ty_interner.prim(Primitive::Usize)
        }
        ComptimeBuiltin::FieldNameAt => {
            type_check_type_arg(env, &mut args[0], ic);
            type_check_usize_arg(env, &mut args[1], ic);
            record_call(ic, path.span, span, builtin);
            ic.lower.ty_interner.prim(Primitive::String)
        }
        ComptimeBuiltin::FieldTypeAt => {
            type_check_type_arg(env, &mut args[0], ic);
            type_check_usize_arg(env, &mut args[1], ic);
            record_call(ic, path.span, span, builtin);
            ic.lower.ty_interner.prim(Primitive::Type)
        }
        ComptimeBuiltin::IsSigned
        | ComptimeBuiltin::IsUnsigned
        | ComptimeBuiltin::IsInteger
        | ComptimeBuiltin::IsFloating
        | ComptimeBuiltin::IsNumeric
        | ComptimeBuiltin::IsPrimitive
        | ComptimeBuiltin::IsSum
        | ComptimeBuiltin::IsCopy
        | ComptimeBuiltin::IsContainer => {
            type_check_type_arg(env, &mut args[0], ic);
            record_call(ic, path.span, span, builtin);
            ic.lower.ty_interner.prim(Primitive::Bool)
        }
    }
}

/// Type-check a `target.supports(Cap)` call.
///
/// The single argument is a capability type-name, surfaced as the
/// `Type` meta-primitive and recorded on `ic.comptime_type_paths` by
/// [`super::path::synth_path`]'s capability branch so the comptime
/// evaluator recovers the `TyKind::Capability` and answers against
/// [`edda_target::TargetTriple::supports_capability`]. Returns `bool`.
/// A non-capability argument (`target.supports(i32)`) is rejected here
/// rather than silently answering `false`.
pub(super) fn synth_target_supports_call(
    env: &mut TyEnv,
    path: &crate::hir::HirPath,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let _ = path;
    if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
        ic.emit_typecheck_error(
            arg.span,
            "named argument is not admitted on comptime built-in `target.supports`".to_string(),
        );
    }
    if let Some(arg) = args.iter().find(|a| a.mode.is_some()) {
        ic.emit_typecheck_error(
            arg.span,
            "call-site mode keyword is not admitted on comptime built-in `target.supports`"
                .to_string(),
        );
    }
    if args.len() != 1 {
        synth_args_unchecked(env, args, ic);
        ic.emit_typecheck_error(
            span,
            format!(
                "built-in `target.supports` takes 1 argument(s) but {found} were supplied",
                found = args.len()
            ),
        );
        return ic.ty_error();
    }
    let is_cap = matches!(
        &args[0].expr.kind,
        HirExprKind::Path(p)
            if p.segments.len() == 1
                && crate::capability::CapabilityType::from_name(
                    ic.lower.interner.resolve(p.segments[0].name),
                )
                .is_some()
    );
    type_check_type_arg(env, &mut args[0], ic);
    if !is_cap {
        ic.emit_typecheck_error(
            args[0].expr.span,
            "comptime built-in `target.supports` expects a capability type-name (e.g. `Subprocess`)"
                .to_string(),
        );
        return ic.ty_error();
    }
    ic.lower.ty_interner.prim(Primitive::Bool)
}

/// Check that an argument has the `Type` meta-type. The expression is
/// walked through synth so [`super::path::synth_path`]'s comptime
/// branch records the resolved `TyId` on `ic.comptime_type_paths` —
/// the actual type the caller passed.
fn type_check_type_arg(env: &mut TyEnv, arg: &mut HirCallArg, ic: &mut InferCx<'_>) {
    let span = arg.expr.span;
    let synth = synth_expr(env, &mut arg.expr, ic);
    let expected = ic.lower.ty_interner.prim(Primitive::Type);
    check_synth_against_expected(synth, expected, ic, span);
}

/// Check that an argument has type `String`. Uses the standard
/// bidirectional check so a string-literal narrows correctly.
fn type_check_string_arg(env: &mut TyEnv, arg: &mut HirCallArg, ic: &mut InferCx<'_>) {
    let expected = ic.lower.ty_interner.prim(Primitive::String);
    check_expr(env, &mut arg.expr, expected, ic);
}

/// Check that an argument has type `usize` — the index parameter of
/// `field_name_at` / `field_type_at`. Uses the standard bidirectional
/// check so an integer literal narrows to `usize`.
fn type_check_usize_arg(env: &mut TyEnv, arg: &mut HirCallArg, ic: &mut InferCx<'_>) {
    let expected = ic.lower.ty_interner.prim(Primitive::Usize);
    check_expr(env, &mut arg.expr, expected, ic);
}

/// Accept an `ident` argument — a single-segment `Path` whose head
/// will be used by the evaluator as a field name. The typechecker
/// admits the call so the evaluator can surface `offset_of_not_yet_supported`
/// from a type-checked site; if the shape is not a single-segment
/// path, a diagnostic fires here instead of cascading from the
/// evaluator. The argument's surface type is the `Type` meta-primitive
/// — a token-level identifier has no run-time form.
fn type_check_ident_arg(_env: &mut TyEnv, arg: &mut HirCallArg, ic: &mut InferCx<'_>) {
    let span = arg.expr.span;
    let ok = matches!(&arg.expr.kind, HirExprKind::Path(p) if p.segments.len() == 1);
    if !ok {
        ic.emit_typecheck_error(
            span,
            "expected a bare field identifier (single-segment path) for `offset_of`'s field arg",
        );
        arg.expr.ty = ic.ty_error();
        return;
    }
    // Record the path span resolves to the `Type` meta-primitive — the
    // evaluator reads `arg.expr` directly to learn the field name.
    arg.expr.ty = ic.lower.ty_interner.prim(Primitive::Type);
}

/// Walk every argument for its side-effect when the call shape is
/// already known to be wrong. Mirrors the existing
/// [`super::call::synth_args_for_diagnostics`] pattern.
fn synth_args_unchecked(env: &mut TyEnv, args: &mut [HirCallArg], ic: &mut InferCx<'_>) {
    for arg in args.iter_mut() {
        synth_expr(env, &mut arg.expr, ic);
    }
}

/// Record the call-site resolution on the inference context.
fn record_call(ic: &mut InferCx<'_>, _path_span: Span, call_span: Span, builtin: ComptimeBuiltin) {
    ic.comptime_builtin_calls.insert(call_span, builtin);
}
