//! Option-C runtime-ABI rewrite of the seven reserved alloc-family externs.
//!
//! Each call to `__edda_alloc` / `__edda_alloc_array` /
//! `__edda_realloc_array` / `__edda_free` / `__edda_box_read` /
//! `__edda_box_unbox` / `__edda_copy_array` is rewritten at the call
//! site to the `_raw`-suffixed runtime symbol with leading `size_of(T)` /
//! `align_of(T)` constants prepended. The runtime is type-erased; these
//! constants are how it sizes and aligns each allocation (and, for
//! `__edda_copy_array`, how it computes the byte count to move).
//!
//! See RISKS.md — every variant of `AllocFamily` corresponds to a
//! wire-locked extern in `edda-rt` (`__edda_alloc_raw` etc.); renaming any
//! of the seven user-visible names or their `_raw` counterparts breaks the
//! option-C contract end-to-end.

use crate::adt::AdtKind;
use crate::constant::{Const, ConstValue};
use crate::operand::Operand;
use crate::terminator::{CallArg, CallMode, FuncRef};
use crate::ty::{MirPrim, MirType, MirTypeKind, ParamMode};

use super::ctx::LoweringContext;
use super::layout::compute_size_align;

/// Per-name kind of alloc-family rewrite. Encodes how `T` is recovered
/// from the original extern's `FnSig` so the layout query knows which
/// type to size.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum AllocFamily {
    /// `__edda_alloc` — `T` is the first (non-capability) param type
    /// (the `value: take T` declared on `Box.new`).
    Alloc,
    /// `__edda_alloc_array` — `T` is the slice-element type of the
    /// return (`-> [T]`).
    AllocArray,
    /// `__edda_realloc_array` — `T` is the slice-element type of the
    /// return (`-> [T]`), recovered identically to `AllocArray`. The
    /// `data: take [T]` / `n: usize` user params follow the injected
    /// `size_of(T)` / `align_of(T)` constants at the runtime call site.
    ReallocArray,
    /// `__edda_free` — no `T` needed at the runtime level (libc
    /// `free` is layout-agnostic). Threading `(0, 1)` keeps the call
    /// shape uniform.
    Free,
    /// `__edda_box_read` — `T` is the return type (`get(b: Box) -> T`).
    BoxRead,
    /// `__edda_box_unbox` — `T` is the return type
    /// (`unbox(...) -> T`).
    BoxUnbox,
    /// `__edda_copy_array` — bulk element copy for copy-typed `[T]`.
    /// `T` is the slice-element type of
    /// the first param (`dst: mutable [T]`); recovered from a param
    /// rather than the `()` return because the copy yields no value. The
    /// runtime moves `src.len * size_of(T)` bytes from `src.ptr` to
    /// `dst.ptr`.
    CopyArray,
}

impl AllocFamily {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "__edda_alloc" => Some(Self::Alloc),
            "__edda_alloc_array" => Some(Self::AllocArray),
            "__edda_realloc_array" => Some(Self::ReallocArray),
            "__edda_free" => Some(Self::Free),
            "__edda_box_read" => Some(Self::BoxRead),
            "__edda_box_unbox" => Some(Self::BoxUnbox),
            "__edda_copy_array" => Some(Self::CopyArray),
            _ => None,
        }
    }

    fn raw_name(self) -> &'static str {
        match self {
            Self::Alloc => "__edda_alloc_raw",
            Self::AllocArray => "__edda_alloc_array_raw",
            Self::ReallocArray => "__edda_realloc_array_raw",
            Self::Free => "__edda_free_raw",
            Self::BoxRead => "__edda_box_read_raw",
            Self::BoxUnbox => "__edda_box_unbox_raw",
            Self::CopyArray => "__edda_copy_array_raw",
        }
    }

    fn extract_payload_ty(
        self,
        sig: &crate::ty::FnSig,
        program: &crate::program::MirProgram,
    ) -> Option<MirType> {
        match self {
            Self::Alloc => sig.params.first().map(|(_, ty)| ty.clone()),
            Self::AllocArray | Self::ReallocArray => {
                match &unwrap_result_ok(sig, program).kind {
                    MirTypeKind::Slice(elem) => Some((**elem).clone()),
                    _ => None,
                }
            }
            Self::Free => None,
            Self::BoxRead | Self::BoxUnbox => Some(unwrap_result_ok(sig, program).clone()),
            Self::CopyArray => match sig.params.first() {
                Some((_, ty)) => match &ty.kind {
                    MirTypeKind::Slice(elem) => Some((**elem).clone()),
                    _ => None,
                },
                None => None,
            },
        }
    }
}

/// Return the source-level success type of `sig.ret`, unwrapping the
/// `__Result<T, E>` synthesised by `register_externs` for raising
/// externs. For non-raising externs `sig.ret` is the source-level
/// return directly and is borrowed back unchanged.
fn unwrap_result_ok<'a>(
    sig: &'a crate::ty::FnSig,
    program: &'a crate::program::MirProgram,
) -> &'a MirType {
    if sig.may_raise.is_empty() {
        return &sig.ret;
    }
    let MirTypeKind::Adt(adt_id) = sig.ret.kind else {
        return &sig.ret;
    };
    let Some(adt) = program.adts.get(adt_id) else {
        return &sig.ret;
    };
    if !matches!(adt.kind, AdtKind::Sum) {
        return &sig.ret;
    }
    let Some(v0) = adt.variants.first() else {
        return &sig.ret;
    };
    if v0.fields.len() != 1 {
        return &sig.ret;
    }
    &v0.fields[0].ty
}

/// If `func_ref` is `FuncRef::Extern` naming one of the seven reserved
/// allocator-family symbols, return the rewritten `(FuncRef, call_args)` pair
/// with the `_raw`-suffixed symbol and two leading `Usize` constants
/// (`size_of(T)`, `align_of(T)`), shifting every capability `value_arg`
/// pairing in `capabilities` past the injected constants. Otherwise
/// return the original pair unchanged.
pub(super) fn maybe_rewrite_alloc_family(
    ctx: &mut LoweringContext<'_>,
    func_ref: FuncRef,
    call_args: Vec<CallArg>,
    capabilities: &mut [crate::terminator::ThreadedCapability],
) -> (FuncRef, Vec<CallArg>) {
    let FuncRef::Extern { name, sig } = &func_ref else {
        return (func_ref, call_args);
    };
    let name_str = ctx.interner.resolve(*name);
    let Some(kind) = AllocFamily::from_name(name_str) else {
        return (func_ref, call_args);
    };
    for cap in capabilities.iter_mut() {
        if let Some(idx) = cap.value_arg.as_mut() {
            *idx += 2;
        }
    }

    let payload_ty = kind.extract_payload_ty(sig, ctx.program.program());
    let (size, align) = match payload_ty {
        Some(ty) => compute_size_align(&ty, ctx.pointer_width_bytes, ctx.program.program())
            .unwrap_or((0, 1)),
        None => (0, 1),
    };

    let size_const = ctx.program.push_const(Const {
        ty: MirType::prim(MirPrim::Usize),
        value: ConstValue::Uint(size as u128),
    });
    let align_const = ctx.program.push_const(Const {
        ty: MirType::prim(MirPrim::Usize),
        value: ConstValue::Uint(align as u128),
    });

    let mut new_params: Vec<(ParamMode, MirType)> = Vec::with_capacity(sig.params.len() + 2);
    new_params.push((ParamMode::Let, MirType::prim(MirPrim::Usize)));
    new_params.push((ParamMode::Let, MirType::prim(MirPrim::Usize)));
    new_params.extend(sig.params.iter().cloned());
    let mut new_sig: crate::ty::FnSig = (**sig).clone();
    new_sig.params = new_params;

    let mut new_args: Vec<CallArg> = Vec::with_capacity(call_args.len() + 2);
    new_args.push(CallArg {
        mode: CallMode::Read,
        operand: Operand::Const(size_const),
    });
    new_args.push(CallArg {
        mode: CallMode::Read,
        operand: Operand::Const(align_const),
    });
    new_args.extend(call_args);

    let raw_name_sym = ctx.interner.intern(kind.raw_name());
    let new_func_ref = FuncRef::Extern {
        name: raw_name_sym,
        sig: Box::new(new_sig),
    };
    (new_func_ref, new_args)
}
