//! Type-layout calculation against an [`edda_target::TargetCfg`].
//!
//! [`Layout::primitive`] returns size and alignment for a locked
//! primitive; [`Layout::of_ty`] resolves a generic
//! [`edda_types::TyId`] by walking its kind in the
//! [`edda_types::TyInterner`].
//!
//! # What this does *not* compute
//!
//! - **Slices** (`[T]`) — `types.md` *Slices* defers ownership and
//!   representation to later; the slice ABI is not yet locked.
//! - **Sum-typed nominal user types** — `TyKind::Nominal` products lay
//!   out through [`Layout::of_ty_with_decls`] + a [`TypeDeclLookup`];
//!   sum-typed records still return
//!   [`LayoutUnsupported::NominalLayoutDeferred`] until the
//!   variant-tag + max-payload algorithm lands. The decl-less
//!   [`Layout::of_ty`] defers every nominal type.
//! - **`String` / `never` / the `Type` meta-type** — `String`'s
//!   representation is not yet locked; `never` is uninhabited;
//!   `Type` is comptime-only and has no runtime form.
//!
//! All deferred cases return [`LayoutUnsupported`] so callers can
//! emit a diagnostic naming the specific blocker.

use edda_resolve::BindingId;
use edda_target::{Arch, TargetCfg};
use edda_types::{Primitive, TyId, TyInterner, TyKind, TypeDeclInfo, TypeDeclShape};

/// Size and alignment of a type's in-memory representation.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Layout {
    /// Size in bytes.
    pub size: u64,
    /// Required alignment in bytes; a non-zero power of two.
    pub align: u64,
}

impl Layout {
    /// Layout of a locked primitive against the active target.
    ///
    /// `isize`/`usize` resolve against the target's pointer width
    /// (4 bytes for `wasm32`, 8 bytes for every other locked arch per
    /// build-system.md §9). `never`, `String`, and `Type` return
    /// [`LayoutUnsupported`] — `never` is uninhabited, `String`'s
    /// representation is not yet locked, `Type` is comptime-only.
    pub fn primitive(p: Primitive, target: &TargetCfg) -> Result<Layout, LayoutUnsupported> {
        Ok(match p {
            Primitive::Bool => Layout { size: 1, align: 1 },
            Primitive::I8 | Primitive::U8 => Layout { size: 1, align: 1 },
            Primitive::I16 | Primitive::U16 => Layout { size: 2, align: 2 },
            Primitive::I32 | Primitive::U32 | Primitive::F32 | Primitive::Codepoint => {
                Layout { size: 4, align: 4 }
            }
            Primitive::I64 | Primitive::U64 | Primitive::F64 => Layout { size: 8, align: 8 },
            Primitive::I128 | Primitive::U128 => Layout {
                size: 16,
                align: 16,
            },
            Primitive::Isize | Primitive::Usize | Primitive::HeapPtr => {
                let size = pointer_bytes(target.triple().arch());
                Layout { size, align: size }
            }
            Primitive::Unit => Layout { size: 0, align: 1 },
            Primitive::Never => return Err(LayoutUnsupported::Never),
            Primitive::String => return Err(LayoutUnsupported::DeferredRepresentation("String")),
            Primitive::Type => return Err(LayoutUnsupported::MetaHasNoRuntimeForm),
        })
    }

    /// Layout of any `TyId` whose representation is currently
    /// deterministic.
    ///
    /// Supported variants:
    /// - [`TyKind::Primitive`] — delegates to [`Self::primitive`].
    /// - [`TyKind::Tuple`] — declaration-order layout with natural
    ///   alignment.
    ///
    /// Unsupported variants are listed in [`LayoutUnsupported`]:
    /// [`TyKind::Slice`] returns [`LayoutUnsupported::SliceLayoutDeferred`];
    /// [`TyKind::Error`] returns [`LayoutUnsupported::ErrorPlaceholder`]
    /// so the caller can suppress cascading diagnostics.
    pub fn of_ty(
        ty: TyId,
        ty_interner: &TyInterner,
        target: &TargetCfg,
    ) -> Result<Layout, LayoutUnsupported> {
        let kind = ty_interner.kind(ty);
        match kind {
            TyKind::Primitive(p) => Self::primitive(*p, target),
            TyKind::Tuple(parts) => layout_aggregate(parts, ty_interner, target, &mut NoDecls),
            TyKind::Slice(_) => Err(LayoutUnsupported::SliceLayoutDeferred),
            TyKind::Nominal(_) => Err(LayoutUnsupported::NominalLayoutDeferred),
            TyKind::Capability(_) => Err(LayoutUnsupported::CapabilityHasNoLayout),
            TyKind::FnPtr(_) => Err(LayoutUnsupported::FnPtrLayoutDeferred),
            TyKind::Error => Err(LayoutUnsupported::ErrorPlaceholder),
        }
    }

    /// Layout of any `TyId` against a [`TypeDeclLookup`] that resolves
    /// `TyKind::Nominal(BindingId)` handles to their field tables.
    ///
    /// Product `TypeDecl`s lay out their fields in declaration order
    /// with natural alignment (the same algorithm as
    /// [`layout_aggregate`]). Sum-typed `TypeDecl`s currently return
    /// [`LayoutUnsupported::NominalLayoutDeferred`] — the
    /// discriminant-plus-max-payload algorithm lands when sum-type
    /// codegen does. Slices, capabilities, function pointers, and the
    /// `Type` meta-primitive behave the same as in [`Self::of_ty`].
    pub fn of_ty_with_decls<L: TypeDeclLookup>(
        ty: TyId,
        ty_interner: &TyInterner,
        target: &TargetCfg,
        decls: &mut L,
    ) -> Result<Layout, LayoutUnsupported> {
        layout_with_decls(ty, ty_interner, target, decls)
    }
}

/// Resolver for `TyKind::Nominal(BindingId)` layout queries used by
/// [`Layout::of_ty_with_decls`].
///
/// The current caller (the comptime evaluator threaded through MIR
/// lowering or the codegen-side spec evaluator) holds an
/// [`edda_types::TyCx`] and implements this trait by delegating to
/// [`TyCx::type_decl`]. The trait keeps edda-comptime decoupled from
/// `TyCx` directly — `TyCx`'s `unstable` surface would otherwise leak
/// here.
pub trait TypeDeclLookup {
    /// Resolve a nominal binding to its declared layout. Returns
    /// `None` when the binding is unknown to the caller (a
    /// resolver-typechecker desync; layout falls back to
    /// [`LayoutUnsupported::NominalLayoutDeferred`]).
    fn lookup_type_decl(&self, binding: BindingId) -> Option<&TypeDeclInfo>;
}

/// Default no-op lookup used by [`Layout::of_ty`] (no nominal types).
struct NoDecls;

impl TypeDeclLookup for NoDecls {
    fn lookup_type_decl(&self, _binding: BindingId) -> Option<&TypeDeclInfo> {
        None
    }
}

/// Walk a [`TyId`] with the supplied [`TypeDeclLookup`].
fn layout_with_decls<L: TypeDeclLookup>(
    ty: TyId,
    ty_interner: &TyInterner,
    target: &TargetCfg,
    decls: &mut L,
) -> Result<Layout, LayoutUnsupported> {
    match ty_interner.kind(ty) {
        TyKind::Primitive(p) => Layout::primitive(*p, target),
        TyKind::Tuple(parts) => layout_aggregate(parts, ty_interner, target, decls),
        TyKind::Slice(_) => Err(LayoutUnsupported::SliceLayoutDeferred),
        TyKind::Nominal(binding) => match decls.lookup_type_decl(*binding) {
            None => Err(LayoutUnsupported::NominalLayoutDeferred),
            Some(info) => match &info.kind {
                TypeDeclShape::Product { fields } => {
                    let field_tys: Vec<TyId> = fields.iter().map(|f| f.ty).collect();
                    layout_aggregate(&field_tys, ty_interner, target, decls)
                }
                TypeDeclShape::Sum { .. } => Err(LayoutUnsupported::NominalLayoutDeferred),
            },
        },
        TyKind::Capability(_) => Err(LayoutUnsupported::CapabilityHasNoLayout),
        TyKind::FnPtr(_) => Err(LayoutUnsupported::FnPtrLayoutDeferred),
        TyKind::Error => Err(LayoutUnsupported::ErrorPlaceholder),
    }
}

/// Reasons [`Layout::of_ty`] cannot compute a layout.
///
/// Each variant maps to a distinct diagnostic message so the caller
/// can name the precise blocker.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum LayoutUnsupported {
    /// The `never` primitive is uninhabited; size is undefined.
    Never,
    /// A primitive whose representation hasn't been locked yet.
    /// Carries the primitive name for the diagnostic.
    DeferredRepresentation(&'static str),
    /// Slice `[T]` layout is not yet locked (`types.md` *Slices*).
    SliceLayoutDeferred,
    /// Nominal user-type layout could not be resolved. Product records
    /// lay out through [`Layout::of_ty_with_decls`] + a
    /// [`TypeDeclLookup`]; this variant is returned when no lookup was
    /// supplied ([`Layout::of_ty`]), the binding is unknown to the
    /// lookup (a resolver/typechecker desync), or the record is
    /// sum-typed (deferred to the variant-tag + max-payload algorithm).
    NominalLayoutDeferred,
    /// `Type` meta-type has no runtime representation.
    MetaHasNoRuntimeForm,
    /// Capability types (`Clock`, `MonotonicClock`, `Stdout`, `Stderr`) have
    /// no runtime representation.
    CapabilityHasNoLayout,
    /// First-class `function(...)` pointer type. The runtime
    /// representation is a target-pointer-sized value, but the codegen
    /// path for fn-ptr values has not landed yet.
    /// `size_of` / `align_of` reject until that lowering work finishes.
    FnPtrLayoutDeferred,
    /// `TyId` was the [`TyInterner::error`](edda_types::TyInterner::error)
    /// sentinel; the original cause was already reported upstream.
    ErrorPlaceholder,
}

impl LayoutUnsupported {
    /// Diagnostic-ready description of why layout is unavailable.
    pub fn message(self) -> String {
        match self {
            Self::Never => "type `never` is uninhabited; size is undefined".to_string(),
            Self::DeferredRepresentation(name) => {
                format!("layout of `{name}` is not yet locked")
            }
            Self::SliceLayoutDeferred => {
                "slice `[T]` layout is not yet locked".to_string()
            }
            Self::NominalLayoutDeferred => {
                "layout of this nominal user type is unavailable: it is sum-typed, or no \
                 type-declaration lookup resolved its fields"
                    .to_string()
            }
            Self::MetaHasNoRuntimeForm => {
                "the `Type` meta-type has no runtime representation".to_string()
            }
            Self::CapabilityHasNoLayout => {
                "capability types have no runtime representation".to_string()
            }
            Self::FnPtrLayoutDeferred => {
                "layout of `function(...)` pointer types is not yet supported".to_string()
            }
            Self::ErrorPlaceholder => "type carries a typecheck error".to_string(),
        }
    }
}

/// Lay out a sequence of TyId fields in declaration order.
///
/// The algorithm is: for each field, round the current offset up to
/// the field's alignment, then add the field's size. The aggregate's
/// alignment is the maximum field alignment; the aggregate's size is
/// rounded up to the aggregate's alignment so consecutive instances
/// can be packed in arrays.
fn layout_aggregate<L: TypeDeclLookup>(
    fields: &[TyId],
    ty_interner: &TyInterner,
    target: &TargetCfg,
    decls: &mut L,
) -> Result<Layout, LayoutUnsupported> {
    let mut size: u64 = 0;
    let mut align: u64 = 1;
    for &id in fields {
        let l = layout_with_decls(id, ty_interner, target, decls)?;
        size = round_up(size, l.align);
        size = size.saturating_add(l.size);
        if l.align > align {
            align = l.align;
        }
    }
    size = round_up(size, align);
    Ok(Layout { size, align })
}

/// Round `value` up to the next multiple of `align`. `align` must be
/// a non-zero power of two; `Layout`'s struct-level invariant
/// guarantees that.
const fn round_up(value: u64, align: u64) -> u64 {
    debug_assert!(align > 0);
    let mask = align - 1;
    (value + mask) & !mask
}

/// Pointer width in bytes for a locked architecture (build-system.md §9).
const fn pointer_bytes(arch: Arch) -> u64 {
    match arch {
        Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 | Arch::Wasm64 => 8,
        Arch::Wasm32 => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_target::{AbiVariant, Os, TargetTriple};
    use edda_types::TyInterner;

    fn x86_64_target() -> TargetCfg {
        TargetCfg::new(TargetTriple::new(
            Arch::X86_64,
            Os::Linux,
            AbiVariant::Gnu,
        ))
    }

    fn wasm32_target() -> TargetCfg {
        TargetCfg::new(TargetTriple::new(
            Arch::Wasm32,
            Os::Wasi,
            AbiVariant::WasiPreview1,
        ))
    }

    #[test]
    fn primitive_layouts_match_locked_table() {
        let t = x86_64_target();
        assert_eq!(
            Layout::primitive(Primitive::Bool, &t).unwrap(),
            Layout { size: 1, align: 1 }
        );
        assert_eq!(
            Layout::primitive(Primitive::I32, &t).unwrap(),
            Layout { size: 4, align: 4 }
        );
        assert_eq!(
            Layout::primitive(Primitive::F64, &t).unwrap(),
            Layout { size: 8, align: 8 }
        );
        assert_eq!(
            Layout::primitive(Primitive::U128, &t).unwrap(),
            Layout {
                size: 16,
                align: 16,
            }
        );
        assert_eq!(
            Layout::primitive(Primitive::Unit, &t).unwrap(),
            Layout { size: 0, align: 1 }
        );
    }

    #[test]
    fn usize_follows_arch_pointer_width() {
        let x86 = x86_64_target();
        let wasm = wasm32_target();
        assert_eq!(
            Layout::primitive(Primitive::Usize, &x86).unwrap(),
            Layout { size: 8, align: 8 }
        );
        assert_eq!(
            Layout::primitive(Primitive::Usize, &wasm).unwrap(),
            Layout { size: 4, align: 4 }
        );
    }

    #[test]
    fn never_layout_is_unsupported() {
        let t = x86_64_target();
        assert_eq!(
            Layout::primitive(Primitive::Never, &t).unwrap_err(),
            LayoutUnsupported::Never
        );
    }

    #[test]
    fn string_layout_is_deferred() {
        let t = x86_64_target();
        let err = Layout::primitive(Primitive::String, &t).unwrap_err();
        assert!(matches!(err, LayoutUnsupported::DeferredRepresentation(_)));
    }

    #[test]
    fn type_meta_has_no_layout() {
        let t = x86_64_target();
        assert_eq!(
            Layout::primitive(Primitive::Type, &t).unwrap_err(),
            LayoutUnsupported::MetaHasNoRuntimeForm
        );
    }

    #[test]
    fn tuple_layout_packs_naturally() {
        let ty = TyInterner::new();
        let t = x86_64_target();
        // (i32, i32) — both 4/4 → size 8, align 4
        let pair = ty.tuple([ty.prim(Primitive::I32), ty.prim(Primitive::I32)]);
        assert_eq!(
            Layout::of_ty(pair, &ty, &t).unwrap(),
            Layout { size: 8, align: 4 }
        );

        // (bool, i32) — bool: 1/1, gap 3 for align(i32)=4, i32: 4/4 → size 8, align 4
        let mixed = ty.tuple([ty.prim(Primitive::Bool), ty.prim(Primitive::I32)]);
        assert_eq!(
            Layout::of_ty(mixed, &ty, &t).unwrap(),
            Layout { size: 8, align: 4 }
        );
    }

    #[test]
    fn tuple_aligns_to_max_field_alignment() {
        let ty = TyInterner::new();
        let t = x86_64_target();
        // (u8, u64) — u8: 1/1, gap 7 for align(u64)=8, u64: 8/8 → size 16, align 8
        let mixed = ty.tuple([ty.prim(Primitive::U8), ty.prim(Primitive::U64)]);
        assert_eq!(
            Layout::of_ty(mixed, &ty, &t).unwrap(),
            Layout { size: 16, align: 8 }
        );
    }

    #[test]
    fn slice_layout_is_deferred() {
        let ty = TyInterner::new();
        let t = x86_64_target();
        let slice = ty.slice(ty.prim(Primitive::U8));
        let err = Layout::of_ty(slice, &ty, &t).unwrap_err();
        assert_eq!(err, LayoutUnsupported::SliceLayoutDeferred);
    }

    #[test]
    fn error_placeholder_propagates_through_of_ty() {
        let ty = TyInterner::new();
        let t = x86_64_target();
        assert_eq!(
            Layout::of_ty(ty.error(), &ty, &t).unwrap_err(),
            LayoutUnsupported::ErrorPlaceholder
        );
    }

    #[test]
    fn nested_tuple_layout() {
        let ty = TyInterner::new();
        let t = x86_64_target();
        // ((u8, u8), u64) — inner (u8, u8): size 2 align 1; outer:
        // gap 6, then u64 size 8 align 8 → size 16, align 8.
        let inner = ty.tuple([ty.prim(Primitive::U8), ty.prim(Primitive::U8)]);
        let outer = ty.tuple([inner, ty.prim(Primitive::U64)]);
        assert_eq!(
            Layout::of_ty(outer, &ty, &t).unwrap(),
            Layout { size: 16, align: 8 }
        );
    }
}
