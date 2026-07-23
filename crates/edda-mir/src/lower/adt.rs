//! ADT registration pre-pass.
//!
//! This lifts each `edda_types::TypeDeclInfo` referenced by a
//! [`super::TypeDeclInput`] into an [`crate::AdtDef`] and records the
//! resulting [`crate::AdtId`] in `ctx.adt_map`. The map is read by
//! [`super::ty::lower_ty`] to translate `TyKind::Nominal(BindingId)` into
//! `MirTypeKind::Adt(AdtId)`, and (in future) by `Call` /
//! `MakeRecord` / `MakeVariant` / `ExtractField` lowering to find the
//! per-variant payload layout.
//!
//! The pass runs once at the top of [`super::lower`], before any function
//! body is walked. It is structural only — layout resolution (the
//! `@layout` / `@align` / `@repr` attributes) is a separate later pass.

use std::collections::HashMap;

use edda_intern::Interner;
use edda_resolve::BindingId;
use edda_types::{FieldInfo, TyInterner, TypeDeclShape, VariantPayloadInfo};

use crate::adt::{AdtDef, AdtKind, FieldDef, VariantDef};
use crate::ids::AdtId;
use crate::layout::{AbiTag, LayoutInfo, LayoutPolicy, ReprKind};
use crate::ty::{MirPrim, MirType, MirTypeKind};

use super::ty::lower_ty;
use super::LoweringContext;
use super::TypeDeclInput;

/// Walk every type declaration in `type_decls`, build its [`AdtDef`], push it
/// onto `ctx.program`, and record `(binding, AdtId)` in `ctx.adt_map`.
///
/// Two-phase to allow ADT-typed fields to reference other ADTs in the same
/// batch:
///
/// 1. Predict each decl's [`AdtId`] from `program.adts.len() + index` and
///    populate `adt_map`. `ProgramBuilder::push_adt` assigns IDs in push
///    order, so predictions match the IDs that step 2 will actually issue.
/// 2. Build each [`AdtDef`] with field types lowered through [`lower_ty`],
///    which now sees a fully-populated `adt_map` and resolves
///    `TyKind::Nominal` correctly. Push in declaration order.
pub(super) fn register_type_decls(
    ctx: &mut LoweringContext<'_>,
    type_decls: &[TypeDeclInput<'_>],
) {
    let base = ctx.program.program().adts.len();
    for (i, decl) in type_decls.iter().enumerate() {
        let adt_id = AdtId::from_raw((base + i) as u32);
        ctx.adt_map.insert(decl.binding, adt_id);
    }
    for decl in type_decls {
        let adt_def = build_adt_def(decl, ctx.interner, ctx.ty_interner, &ctx.adt_map);
        let pushed = ctx.program.push_adt(adt_def);
        debug_assert_eq!(ctx.adt_map.get(&decl.binding).copied(), Some(pushed));
    }
}

/// Build a single [`AdtDef`] from a [`TypeDeclInput`]'s borrowed
/// [`TypeDeclInfo`]. Layout overrides from `@align` / `@repr` /
/// `@layout` (carried on the `TypeDeclInput`) overlay the natural
/// default field-by-field; unset overrides keep the natural value.
///
/// When `decl.synthesize_box_ptr` is true the resulting product
/// variant is prefixed with a synthetic `ptr: HeapPtr` field — this
/// is how `spec std.mem.alloc.Box(T)`'s opaque `type Box {}` body is
/// given a typed storage slot for the runtime's heap pointer (the
/// parser does not admit `HeapPtr<T>` in field position, so the
/// synthesis happens at MIR-lowering time).
fn build_adt_def(
    decl: &TypeDeclInput<'_>,
    interner: &Interner,
    ty_interner: &TyInterner,
    adt_map: &HashMap<BindingId, AdtId>,
) -> AdtDef {
    let layout = layout_from_overrides(decl);
    match &decl.info.kind {
        TypeDeclShape::Product { fields } => {
            let mut lowered_fields = lower_named_fields(fields, ty_interner, adt_map);
            if decl.synthesize_box_ptr {
                let ptr_field = FieldDef {
                    name: interner.intern("ptr"),
                    span: decl.info.span,
                    ty: MirType::prim(MirPrim::HeapPtr),
                };
                lowered_fields.insert(0, ptr_field);
            }
            AdtDef {
                name: decl.name,
                span: decl.info.span,
                kind: AdtKind::Product,
                variants: vec![VariantDef {
                    name: decl.name,
                    span: decl.info.span,
                    fields: lowered_fields,
                    discriminant: None,
                }],
                layout,
                tag_width: None,
            }
        }
        TypeDeclShape::Sum { variants } => {
            let tag_width = pick_tag_width(variants.len());
            let lowered_variants: Vec<VariantDef> = variants
                .iter()
                .enumerate()
                .map(|(i, v)| VariantDef {
                    name: v.name,
                    span: v.span,
                    fields: lower_variant_payload(&v.payload, interner, ty_interner, adt_map),
                    discriminant: Some(i as u64),
                })
                .collect();
            AdtDef {
                name: decl.name,
                span: decl.info.span,
                kind: AdtKind::Sum,
                variants: lowered_variants,
                layout,
                tag_width: Some(tag_width),
            }
        }
    }
}

/// Overlay slice-2 layout overrides on top of `LayoutInfo::natural()`.
/// Unset (`None`) fields keep the natural-default value.
fn layout_from_overrides(decl: &TypeDeclInput<'_>) -> LayoutInfo {
    LayoutInfo {
        policy: decl.layout.unwrap_or(LayoutPolicy::Natural),
        repr: decl.repr.unwrap_or(ReprKind::Edda),
        abi: AbiTag::Edda,
        align: decl.align,
    }
}

/// Lower a named-field list (product types and `Struct` variant payloads).
fn lower_named_fields(
    fields: &[FieldInfo],
    ty_interner: &TyInterner,
    adt_map: &HashMap<BindingId, AdtId>,
) -> Vec<FieldDef> {
    fields
        .iter()
        .map(|f| FieldDef {
            name: f.name,
            span: f.span,
            ty: lower_ty(ty_interner, adt_map, f.ty),
        })
        .collect()
}

/// Lower one variant's payload to a flat `Vec<FieldDef>`. Unit payloads
/// produce an empty vec; tuple payloads synthesise positional names.
fn lower_variant_payload(
    payload: &VariantPayloadInfo,
    interner: &Interner,
    ty_interner: &TyInterner,
    adt_map: &HashMap<BindingId, AdtId>,
) -> Vec<FieldDef> {
    match payload {
        VariantPayloadInfo::Unit => Vec::new(),
        VariantPayloadInfo::Tuple { elems } => elems
            .iter()
            .enumerate()
            .map(|(i, ty_id)| {
                let name = interner.intern(&format!("_{i}"));
                FieldDef {
                    name,
                    span: edda_span::Span::DUMMY,
                    ty: lower_ty(ty_interner, adt_map, *ty_id),
                }
            })
            .collect(),
        VariantPayloadInfo::Struct { fields } => lower_named_fields(fields, ty_interner, adt_map),
    }
}

/// Pick the discriminant width for a sum ADT based on variant count.
pub(super) fn pick_tag_width(variant_count: usize) -> MirPrim {
    if variant_count <= u8::MAX as usize + 1 {
        MirPrim::U8
    } else if variant_count <= u16::MAX as usize + 1 {
        MirPrim::U16
    } else {
        MirPrim::U32
    }
}

/// Synthesize a `Result<T, E1, ...En>` sum ADT for a raising function's
/// return type. Variant 0 is `Ok(success_ty)`; each subsequent variant is
/// `Err_i(Adt(err_adt_id))`. The ADT is pushed onto the program builder and
/// its [`AdtId`] returned.
pub(super) fn synthesize_result_adt(
    ctx: &mut LoweringContext<'_>,
    success_ty: MirType,
    err_adts: Vec<(edda_intern::Symbol, AdtId)>,
    span: edda_span::Span,
) -> AdtId {
    let ok_sym = ctx.interner.intern("ok");
    let field_sym = ctx.interner.intern("0");
    let ok_fields = if matches!(success_ty.kind, MirTypeKind::Unit) {
        Vec::new()
    } else {
        vec![FieldDef { name: field_sym, span, ty: success_ty }]
    };
    let ok_variant = VariantDef {
        name: ok_sym,
        span,
        fields: ok_fields,
        discriminant: Some(0),
    };
    let mut variants = vec![ok_variant];
    for (i, (err_name, err_adt_id)) in err_adts.into_iter().enumerate() {
        variants.push(VariantDef {
            name: err_name,
            span,
            fields: vec![FieldDef {
                name: field_sym,
                span,
                ty: MirType::new(MirTypeKind::Adt(err_adt_id)),
            }],
            discriminant: Some(i as u64 + 1),
        });
    }
    let tag_width = pick_tag_width(variants.len());
    let result_name = ctx.interner.intern("__Result");
    let adt = AdtDef {
        name: result_name,
        span,
        kind: AdtKind::Sum,
        variants,
        layout: LayoutInfo::natural(),
        tag_width: Some(tag_width),
    };
    ctx.program.push_adt(adt)
}

/// Project a (possibly raising) signature's return type to its
/// wire-level form: `__Result<T, E1, ...>` when `may_raise` is
/// non-empty, else `ret` unchanged.
///
/// Centralises the projection `register_externs` /
/// `register_function_bodies` apply to source-bodied and extern raising
/// callees so the indirect-call site (`super::call::lower_indirect_call`)
/// reconciles a fn-VALUE's bare `ret` to the same `{ tag, payload }`
/// shape its body/shim actually returns.
pub(super) fn wire_level_ret(
    ctx: &mut LoweringContext<'_>,
    ret: MirType,
    may_raise: &[AdtId],
    span: edda_span::Span,
) -> MirType {
    if may_raise.is_empty() {
        return ret;
    }
    let err_adts: Vec<(edda_intern::Symbol, AdtId)> = may_raise
        .iter()
        .map(|&adt_id| {
            let name = ctx
                .program
                .program()
                .adts
                .get(adt_id)
                .map(|def| def.name)
                .unwrap_or_else(|| ctx.interner.intern("__Err"));
            (name, adt_id)
        })
        .collect();
    let result_adt_id = synthesize_result_adt(ctx, ret, err_adts, span);
    MirType::new(MirTypeKind::Adt(result_adt_id))
}
