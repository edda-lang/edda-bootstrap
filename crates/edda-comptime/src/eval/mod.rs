//! Direct dispatch of a comptime built-in over already-evaluated
//! [`Value`] arguments.
//!
//! This routes a call whose arguments the caller has
//! already reduced to [`Value`]s. The intended client is
//! `edda-refine` (spec `where`-clauses) and the HIR
//! expression evaluator inside this same crate.

use edda_diag::Diagnostics;
use edda_intern::Interner;
use edda_span::Span;
use edda_target::{TargetCfg, TargetHas};
use edda_types::{Primitive, TyId, TyInterner, TyKind, TypeDeclShape, VariantPayloadInfo};

use crate::builtin::{Builtin, BuiltinParamKind};
use crate::error::ComptimeError;
use crate::layout::{Layout, TypeDeclLookup};
use crate::value::{IntValue, Value, ValueKind};

mod env;
mod expr;
mod op;

pub use env::ComptimeEnv;
pub use expr::{EvalCx, MAX_DEPTH, eval_expr};

/// Evaluate a comptime built-in call.
///
/// The caller passes already-evaluated [`Value`] arguments, the span
/// of the call site, the shared [`TyInterner`] for resolving
/// [`TyId`]s during layout, the active target, an interner for
/// `target_has`'s `String` argument, and a diagnostics take. On
/// success returns the result value; on failure pushes a
/// `Diagnostic` via [`ComptimeError::to_diagnostic`] and returns
/// `None`.
pub fn eval_builtin(
    builtin: Builtin,
    args: &[Value],
    span: Span,
    ty_interner: &TyInterner,
    target: &TargetCfg,
    interner: &Interner,
    diags: &mut Diagnostics,
) -> Option<Value> {
    let mut decls = NoDecls;
    eval_builtin_with_decls(
        builtin,
        args,
        span,
        ty_interner,
        target,
        interner,
        &mut decls,
        diags,
    )
}

/// Evaluate a comptime built-in with a [`TypeDeclLookup`] that
/// resolves `TyKind::Nominal(BindingId)` handles. Callers holding an
/// [`edda_types::TyCx`] (MIR lowering, codegen) build a lookup that
/// delegates to [`edda_types::TyCx::type_decl`] and pass it through;
/// callers that never deal with nominal types (the existing
/// [`eval_builtin`] surface) can use [`eval_builtin`] which supplies
/// a no-op lookup.
pub fn eval_builtin_with_decls<L: TypeDeclLookup>(
    builtin: Builtin,
    args: &[Value],
    span: Span,
    ty_interner: &TyInterner,
    target: &TargetCfg,
    interner: &Interner,
    decls: &mut L,
    diags: &mut Diagnostics,
) -> Option<Value> {
    if let Err(err) = check_arity(builtin, args, span) {
        diags.push(err.to_diagnostic());
        return None;
    }
    if let Err(err) = check_arg_kinds(builtin, args, span) {
        diags.push(err.to_diagnostic());
        return None;
    }
    let result = match builtin {
        Builtin::SizeOf => eval_size_of(args, span, ty_interner, target, decls),
        Builtin::AlignOf => eval_align_of(args, span, ty_interner, target, decls),
        Builtin::OffsetOf => Err(ComptimeError::OffsetOfNotYetSupported { span }),
        Builtin::TargetHas => eval_target_has(args, span, target, interner),
        Builtin::FieldCount => eval_field_count(args, span, ty_interner, decls),
        Builtin::FieldNameAt => eval_field_name_at(args, span, ty_interner, decls),
        Builtin::FieldTypeAt => eval_field_type_at(args, span, ty_interner, decls),
        Builtin::IsSigned => Ok(eval_is_category(Builtin::IsSigned, args, ty_interner)),
        Builtin::IsUnsigned => Ok(eval_is_category(Builtin::IsUnsigned, args, ty_interner)),
        Builtin::IsInteger => Ok(eval_is_category(Builtin::IsInteger, args, ty_interner)),
        Builtin::IsFloating => Ok(eval_is_category(Builtin::IsFloating, args, ty_interner)),
        Builtin::IsNumeric => Ok(eval_is_category(Builtin::IsNumeric, args, ty_interner)),
        Builtin::IsPrimitive => Ok(eval_is_category(Builtin::IsPrimitive, args, ty_interner)),
        Builtin::IsSum => eval_is_sum(args, span, ty_interner, decls),
        Builtin::IsCopy => eval_is_copy(args, span, ty_interner, decls),
        Builtin::IsContainer => Ok(eval_is_container(args, ty_interner)),
    };
    match result {
        Ok(v) => Some(v),
        Err(err) => {
            diags.push(err.to_diagnostic());
            None
        }
    }
}

/// No-op lookup for callers without a `TyCx`. Mirrors `layout`'s own
/// no-op for the trait — declared here so the inline-call surface of
/// [`eval_builtin`] does not need to expose it.
pub(crate) struct NoDecls;

impl TypeDeclLookup for NoDecls {
    fn lookup_type_decl(
        &self,
        _binding: edda_resolve::BindingId,
    ) -> Option<&edda_types::TypeDeclInfo> {
        None
    }
}

/// Arity check: locked catalogue fixes each built-in's argument count.
fn check_arity(builtin: Builtin, args: &[Value], span: Span) -> Result<(), ComptimeError> {
    let expected = builtin.signature().arity();
    if args.len() == expected {
        return Ok(());
    }
    Err(ComptimeError::ArityMismatch {
        span,
        builtin,
        expected,
        found: args.len(),
    })
}

/// Per-parameter kind check. Locked signatures use only `Type` and
/// `String` parameter shapes; everything else is a shape mismatch.
fn check_arg_kinds(builtin: Builtin, args: &[Value], span: Span) -> Result<(), ComptimeError> {
    for (i, (param, arg)) in builtin.signature().params.iter().zip(args.iter()).enumerate() {
        let actual = arg.kind();
        let matches = matches!(
            (param, actual),
            (BuiltinParamKind::Type, ValueKind::Type)
                | (BuiltinParamKind::Str, ValueKind::Str)
                | (BuiltinParamKind::Usize, ValueKind::Int)
        );
        if !matches {
            return Err(ComptimeError::ArgumentKindMismatch {
                span,
                builtin,
                param_index: i,
                expected: *param,
                found: actual,
            });
        }
    }
    Ok(())
}

/// `size_of(T: Type) -> usize`. Dispatches through
/// [`Layout::of_ty_with_decls`] so nominal user types resolve through
/// the supplied [`TypeDeclLookup`].
fn eval_size_of<L: TypeDeclLookup>(
    args: &[Value],
    span: Span,
    ty_interner: &TyInterner,
    target: &TargetCfg,
    decls: &mut L,
) -> Result<Value, ComptimeError> {
    let id = expect_type_arg(&args[0]);
    match Layout::of_ty_with_decls(id, ty_interner, target, decls) {
        Ok(layout) => Ok(usize_value(layout.size)),
        Err(reason) => Err(ComptimeError::LayoutUnavailable {
            span,
            builtin: Builtin::SizeOf,
            ty_display: ty_interner.display(id).to_string(),
            reason,
        }),
    }
}

/// `align_of(T: Type) -> usize`. Same dispatch shape as `size_of`.
fn eval_align_of<L: TypeDeclLookup>(
    args: &[Value],
    span: Span,
    ty_interner: &TyInterner,
    target: &TargetCfg,
    decls: &mut L,
) -> Result<Value, ComptimeError> {
    let id = expect_type_arg(&args[0]);
    match Layout::of_ty_with_decls(id, ty_interner, target, decls) {
        Ok(layout) => Ok(usize_value(layout.align)),
        Err(reason) => Err(ComptimeError::LayoutUnavailable {
            span,
            builtin: Builtin::AlignOf,
            ty_display: ty_interner.display(id).to_string(),
            reason,
        }),
    }
}

/// `target_has(feature: String) -> bool`. Delegates to
/// [`TargetCfg::target_has`]; an `Unknown` result projects onto the
/// `unknown_target_feature` diagnostic class.
fn eval_target_has(
    args: &[Value],
    span: Span,
    target: &TargetCfg,
    interner: &Interner,
) -> Result<Value, ComptimeError> {
    let symbol = match &args[0] {
        Value::Str(sym) => *sym,
        _ => unreachable!("target_has arg-kind checked above"),
    };
    let feature = interner.resolve(symbol);
    match target.target_has(feature) {
        TargetHas::Yes => Ok(Value::Bool(true)),
        TargetHas::No => Ok(Value::Bool(false)),
        TargetHas::Unknown => Err(ComptimeError::UnknownTargetFeature {
            span,
            feature: feature.to_string(),
            arch: target.triple().arch().to_string(),
        }),
    }
}

/// `field_count(T: Type) -> usize`. Product → field count; sum →
/// variant count; tuple → element count; every other type errors.
/// Mirrors `cteval/src/builtin/layout.ea`'s `field_count`.
fn eval_field_count<L: TypeDeclLookup>(
    args: &[Value],
    span: Span,
    ty_interner: &TyInterner,
    decls: &mut L,
) -> Result<Value, ComptimeError> {
    let id = expect_type_arg(&args[0]);
    match ty_interner.kind(id) {
        TyKind::Tuple(elems) => Ok(usize_value(elems.len() as u64)),
        TyKind::Nominal(binding) => {
            let info = lookup_decl(*binding, span, Builtin::FieldCount, ty_interner, id, decls)?;
            let count = match &info.kind {
                TypeDeclShape::Product { fields } => fields.len(),
                TypeDeclShape::Sum { variants } => variants.len(),
            };
            Ok(usize_value(count as u64))
        }
        _ => Err(field_introspection_err(
            Builtin::FieldCount,
            span,
            ty_interner,
            id,
            "field_count is defined only for record, sum, and tuple types",
        )),
    }
}

/// `field_name_at(T: Type, i: usize) -> String`. Product → i-th field
/// name; sum → i-th variant name; out-of-range or tuple/other errors.
/// Mirrors `cteval/src/builtin/layout.ea`'s `field_name_at`.
fn eval_field_name_at<L: TypeDeclLookup>(
    args: &[Value],
    span: Span,
    ty_interner: &TyInterner,
    decls: &mut L,
) -> Result<Value, ComptimeError> {
    let id = expect_type_arg(&args[0]);
    let i = expect_usize_arg(&args[1]) as usize;
    let TyKind::Nominal(binding) = ty_interner.kind(id) else {
        return Err(field_introspection_err(
            Builtin::FieldNameAt,
            span,
            ty_interner,
            id,
            "field_name_at is defined only for record and sum types (tuple positions are anonymous)",
        ));
    };
    let info = lookup_decl(*binding, span, Builtin::FieldNameAt, ty_interner, id, decls)?;
    match &info.kind {
        TypeDeclShape::Product { fields } => match fields.get(i) {
            Some(field) => Ok(Value::Str(field.name)),
            None => Err(index_out_of_range_err(
                Builtin::FieldNameAt,
                span,
                i,
                fields.len(),
            )),
        },
        TypeDeclShape::Sum { variants } => match variants.get(i) {
            Some(variant) => Ok(Value::Str(variant.name)),
            None => Err(index_out_of_range_err(
                Builtin::FieldNameAt,
                span,
                i,
                variants.len(),
            )),
        },
    }
}

/// `field_type_at(T: Type, i: usize) -> Type`. Tuple → i-th element
/// type; product → i-th field type; sum → i-th variant payload type.
/// Out-of-range, composite payloads, or other types error. Mirrors
/// `cteval/src/builtin/layout.ea`'s `field_type_at`.
fn eval_field_type_at<L: TypeDeclLookup>(
    args: &[Value],
    span: Span,
    ty_interner: &TyInterner,
    decls: &mut L,
) -> Result<Value, ComptimeError> {
    let id = expect_type_arg(&args[0]);
    let i = expect_usize_arg(&args[1]) as usize;
    match ty_interner.kind(id) {
        TyKind::Tuple(elems) => match elems.get(i) {
            Some(ty) => Ok(Value::Type(*ty)),
            None => Err(index_out_of_range_err(
                Builtin::FieldTypeAt,
                span,
                i,
                elems.len(),
            )),
        },
        TyKind::Nominal(binding) => {
            let info = lookup_decl(*binding, span, Builtin::FieldTypeAt, ty_interner, id, decls)?;
            match &info.kind {
                TypeDeclShape::Product { fields } => match fields.get(i) {
                    Some(field) => Ok(Value::Type(field.ty)),
                    None => Err(index_out_of_range_err(
                        Builtin::FieldTypeAt,
                        span,
                        i,
                        fields.len(),
                    )),
                },
                TypeDeclShape::Sum { variants } => {
                    let Some(variant) = variants.get(i) else {
                        return Err(index_out_of_range_err(
                            Builtin::FieldTypeAt,
                            span,
                            i,
                            variants.len(),
                        ));
                    };
                    variant_payload_type(&variant.payload, span)
                }
            }
        }
        _ => Err(field_introspection_err(
            Builtin::FieldTypeAt,
            span,
            ty_interner,
            id,
            "field_type_at is defined only for tuple, record, and sum types",
        )),
    }
}

/// Pick the single payload [`TyId`] of a sum variant for
/// `field_type_at`. `Unit` has no type; a single-element tuple payload
/// yields its sole element; multi-element tuple and struct payloads are
/// composite and have no single field type. The bootstrap stores
/// variant payloads structurally, so a composite payload is a clear
/// error rather than a single type.
fn variant_payload_type(
    payload: &VariantPayloadInfo,
    span: Span,
) -> Result<Value, ComptimeError> {
    match payload {
        VariantPayloadInfo::Tuple { elems } if elems.len() == 1 => Ok(Value::Type(elems[0])),
        VariantPayloadInfo::Unit => Err(ComptimeError::FieldIntrospection {
            span,
            builtin: Builtin::FieldTypeAt,
            message: "variant has a unit payload; field_type_at has no single type to return"
                .to_string(),
        }),
        VariantPayloadInfo::Tuple { .. } | VariantPayloadInfo::Struct { .. } => {
            Err(ComptimeError::FieldIntrospection {
                span,
                builtin: Builtin::FieldTypeAt,
                message: "variant has a composite payload; field_type_at returns a single type"
                    .to_string(),
            })
        }
    }
}

/// `is_<category>(T: Type) -> bool`. A `TyKind::Primitive(p)` answers
/// the per-category predicate; every other type is `false`. Mirrors
/// `cteval/src/builtin/category.ea` (the bootstrap `TyKind` has no
/// refined variant, so non-primitives are simply `false`).
fn eval_is_category(builtin: Builtin, args: &[Value], ty_interner: &TyInterner) -> Value {
    let id = expect_type_arg(&args[0]);
    let answer = match ty_interner.kind(id) {
        TyKind::Primitive(p) => match builtin {
            Builtin::IsSigned => p.is_signed_integer(),
            Builtin::IsUnsigned => p.is_unsigned_integer(),
            Builtin::IsInteger => p.is_integer(),
            Builtin::IsFloating => p.is_float(),
            Builtin::IsNumeric => p.is_numeric(),
            Builtin::IsPrimitive => true,
            _ => unreachable!("eval_is_category dispatched on a non-predicate built-in"),
        },
        _ => false,
    };
    Value::Bool(answer)
}

/// `is_sum(T: Type) -> bool`. A `TyKind::Nominal` resolving to a
/// [`edda_types::TypeDeclShape::Sum`] declaration answers `true`; every
/// other type — primitives, tuples, slices, product nominals — answers
/// `false`. Mirrors `cteval/src/builtin/category.ea`'s `is_sum` from the
/// native D-22 READ seam (the native's
/// `refined(base)` recursion has no bootstrap counterpart because the
/// bootstrap `TyKind` has no refined variant). A nominal the supplied
/// [`TypeDeclLookup`] cannot resolve errors loudly (same policy as
/// `field_count`) rather than silently answering `false`.
fn eval_is_sum<L: TypeDeclLookup>(
    args: &[Value],
    span: Span,
    ty_interner: &TyInterner,
    decls: &mut L,
) -> Result<Value, ComptimeError> {
    let id = expect_type_arg(&args[0]);
    let answer = match ty_interner.kind(id) {
        TyKind::Nominal(binding) => {
            let info = lookup_decl(*binding, span, Builtin::IsSum, ty_interner, id, decls)?;
            matches!(info.kind, TypeDeclShape::Sum { .. })
        }
        _ => false,
    };
    Ok(Value::Bool(answer))
}

/// `is_container(T: Type) -> bool`. Spec-instance container predicate
/// (`Box_*`, `Vec_*`, `Option_*`, map instances). The decidable answer
/// is minted by `edda-types`'s `comptime_if` folding, where a
/// spec-instantiation-generated type is a mangled leaf absent from the
/// `ShapeIndex`. By the time this post-monomorphisation evaluator runs,
/// such a type has collapsed to its concrete `TyKind` shape and is no
/// longer a distinguishable spec instance, so this path answers `false`
/// for a collapsed spec instance. A given `comptime if is_container(…)`
/// is folded by exactly one path, so the two never disagree. A **slice**
/// `[E]`, however, keeps a
/// distinguishable `TyKind::Slice` shape post-monomorphisation, so this
/// path recognises it directly — a slice is a built-in container walked
/// element-by-element, matching the `comptime_if` fold's `true`.
fn eval_is_container(args: &[Value], ty_interner: &TyInterner) -> Value {
    let id = expect_type_arg(&args[0]);
    Value::Bool(matches!(ty_interner.kind(id), TyKind::Slice(_)))
}

/// `is_copy(T: Type) -> bool`. `true` iff `T` is non-`linear`,
/// non-`affine`, and transitively contains no `linear`/`affine` field —
/// broadens the `comptime if
/// is_primitive(T)` bulk-copy fast-path gate to freely-copyable user
/// records. Mirrors `cteval/src/builtin/category.ea`'s `is_copy`.
fn eval_is_copy<L: TypeDeclLookup>(
    args: &[Value],
    span: Span,
    ty_interner: &TyInterner,
    decls: &mut L,
) -> Result<Value, ComptimeError> {
    let id = expect_type_arg(&args[0]);
    let answer = ty_is_copy(id, span, ty_interner, decls)?;
    Ok(Value::Bool(answer))
}

/// Recursive worker for [`eval_is_copy`]: `true` iff `id` is a
/// primitive, a tuple/product/sum of exclusively copy elements, and
/// (for a nominal type) carries no `linearity` of its own.
fn ty_is_copy<L: TypeDeclLookup>(
    id: TyId,
    span: Span,
    ty_interner: &TyInterner,
    decls: &mut L,
) -> Result<bool, ComptimeError> {
    match ty_interner.kind(id) {
        TyKind::Primitive(_) => Ok(true),
        TyKind::Tuple(elems) => {
            for elem in elems.iter() {
                if !ty_is_copy(*elem, span, ty_interner, decls)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        TyKind::Nominal(binding) => {
            let binding = *binding;
            let (linear, children) = {
                let info = lookup_decl(binding, span, Builtin::IsCopy, ty_interner, id, decls)?;
                if info.linearity.is_some() {
                    (true, Vec::new())
                } else {
                    let children: Vec<TyId> = match &info.kind {
                        TypeDeclShape::Product { fields } => {
                            fields.iter().map(|f| f.ty).collect()
                        }
                        TypeDeclShape::Sum { variants } => variants
                            .iter()
                            .flat_map(|v| variant_payload_tys(&v.payload))
                            .collect(),
                    };
                    (false, children)
                }
            };
            if linear {
                return Ok(false);
            }
            for child in children {
                if !ty_is_copy(child, span, ty_interner, decls)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Flatten one sum variant's payload into the [`TyId`]s [`ty_is_copy`]
/// must recurse into — empty for a unit payload.
fn variant_payload_tys(payload: &VariantPayloadInfo) -> Vec<TyId> {
    match payload {
        VariantPayloadInfo::Unit => Vec::new(),
        VariantPayloadInfo::Tuple { elems } => elems.to_vec(),
        VariantPayloadInfo::Struct { fields } => fields.iter().map(|f| f.ty).collect(),
    }
}

/// Resolve a `TyKind::Nominal` binding to its [`edda_types::TypeDeclInfo`],
/// erroring with a `field_introspection` message when the lookup yields
/// nothing (no `TyCx` attached, or a cross-file binding the lookup
/// cannot reach).
fn lookup_decl<'a, L: TypeDeclLookup>(
    binding: edda_resolve::BindingId,
    span: Span,
    builtin: Builtin,
    ty_interner: &TyInterner,
    id: TyId,
    decls: &'a mut L,
) -> Result<&'a edda_types::TypeDeclInfo, ComptimeError> {
    match decls.lookup_type_decl(binding) {
        Some(info) => Ok(info),
        None => Err(field_introspection_err(
            builtin,
            span,
            ty_interner,
            id,
            "type-decl not found for nominal type (no type-decl lookup attached or cross-file binding)",
        )),
    }
}

/// Build a `field_introspection` error naming the offending type.
fn field_introspection_err(
    builtin: Builtin,
    span: Span,
    ty_interner: &TyInterner,
    id: TyId,
    what: &str,
) -> ComptimeError {
    ComptimeError::FieldIntrospection {
        span,
        builtin,
        message: format!("{what} (type `{}`)", ty_interner.display(id)),
    }
}

/// Build a `field_introspection` error for an out-of-range index.
fn index_out_of_range_err(builtin: Builtin, span: Span, got: usize, max: usize) -> ComptimeError {
    ComptimeError::FieldIntrospection {
        span,
        builtin,
        message: format!("index {got} is out of range (the type has {max} member(s))"),
    }
}

/// Extract the [`TyId`] from an argument already kind-checked as `Type`.
fn expect_type_arg(value: &Value) -> TyId {
    match value {
        Value::Type(id) => *id,
        _ => unreachable!("arg-kind check verified Type variant"),
    }
}

/// Extract a `u128` index from an argument already kind-checked as an
/// integer. The typechecker pins the index parameter to `usize`, so the
/// payload reads back through [`IntValue::as_u128`]; a defensive `0`
/// fallback covers any signed/odd-width value that slipped the kind
/// check (the index then lands out of range and errors cleanly).
fn expect_usize_arg(value: &Value) -> u128 {
    match value {
        Value::Int(int) => int.as_u128().unwrap_or(0),
        _ => unreachable!("arg-kind check verified Int variant"),
    }
}

/// Build a `Value::Int` of type `usize` whose payload is the given
/// layout-derived byte count.
fn usize_value(bytes: u64) -> Value {
    Value::Int(IntValue::new_unsigned(Primitive::Usize, bytes as u128))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod hir_tests;
