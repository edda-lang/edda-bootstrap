//! MIR -> LLVM IR -> object-file backend.
//!
//! Owns the LLVM 18 binding (via inkwell), the ABI matrix, and the per-target
//! object-file emission. The six v0.1 target triples
//! (`docs/bootstrap/v0.1-scope.md`) ship through this crate; ABI rules and
//! layout attributes (`@layout`, `@align`, `@repr`, `@abi`) are honoured here.
//!
//! Implements:
//!   - `docs/bootstrap/backend-choice.md` (LLVM 18 via inkwell, ABI matrix)
//!   - `docs/tooling/abi-and-layout.md` (layout/align/repr/abi attributes)
//!   - `docs/tooling/build-system.md` §9 (target features, SIMD gating)
//!
//! The crate's public surface is unstable — the API is still being
//! designed incrementally, though `edda-driver` now calls it
//! (`Emitter::compile_program_to_object`).

mod abi_attr;
mod calling_conv;
mod code_model;
#[cfg(feature = "llvm")]
pub mod emit;
mod error;
#[cfg(feature = "llvm")]
mod lower;
mod mir_prim;
mod object_format;
mod ops;
mod reloc_model;
mod simd;
mod target_info;

pub use abi_attr::{AbiResolutionError, resolve_abi_tag, resolve_named_abi};
pub use calling_conv::{CallingConv, explicit_x86_64_sysv, explicit_x86_64_win64};
pub use code_model::CodeModel;
#[cfg(feature = "llvm")]
pub use emit::Emitter;
pub use error::{CompileError, SimdRejection};
pub use mir_prim::{align_of_prim, is_float, is_integer, is_signed_integer, size_of_prim};
pub use object_format::{ObjectFormat, object_format};
pub use ops::{LlvmUnOpShape, llvm_unop_shape};
pub use reloc_model::RelocModel;
pub use simd::{required_feature, simd_width_supported};
pub use target_info::{Endianness, endianness, llvm_triple, pointer_width};

/// Platform-default settings for a target's LLVM `TargetMachine`.
///
/// Re-exports `default_for_target` from each settings module under
/// a single namespace so call sites read `target_defaults::calling_conv(&triple)`
/// rather than `compile::calling_conv::default_for_target(&triple)`.
pub mod target_defaults {
    use edda_target::TargetTriple;

    use crate::{CallingConv, CodeModel, RelocModel};

    /// Platform-default calling convention. See
    /// [`crate::calling_conv::default_for_target`].
    pub const fn calling_conv(triple: &TargetTriple) -> CallingConv {
        crate::calling_conv::default_for_target(triple)
    }

    /// Platform-default code model. See
    /// [`crate::code_model::default_for_target`].
    pub const fn code_model(triple: &TargetTriple) -> CodeModel {
        crate::code_model::default_for_target(triple)
    }

    /// Platform-default relocation model. See
    /// [`crate::reloc_model::default_for_target`].
    pub const fn reloc_model(triple: &TargetTriple) -> RelocModel {
        crate::reloc_model::default_for_target(triple)
    }
}
