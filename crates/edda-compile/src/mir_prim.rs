//! MIR primitive -> LLVM IR type + per-target size and alignment.
//!
//! The eventual MIR -> LLVM lowering reaches for these three queries on
//! every scalar value it emits. Pure functions of [`MirPrim`] and the
//! active [`Arch`]; no LLVM dependency required.
//!
//! # Scope cut
//!
//! Only [`MirPrim`] is handled here. Compound types ([`edda_mir::MirTypeKind::Adt`],
//! `Tuple`, `Slice`, `FnPtr`) are handled elsewhere because their LLVM lowering
//! recurses through element types and needs the AdtDef arena to assign
//! struct names. The natural place for those is alongside the IR emitter,
//! not in a pure mapping table.

use edda_mir::MirPrim;
use edda_target::Arch;

use crate::target_info::pointer_width;

/// LLVM IR type name for `prim` on `arch`.
///
/// Returns `None` for [`MirPrim::Str`] — `str` is a slice
/// (`{ptr, isize}` pair), not a primitive LLVM type. Callers
/// emitting a `str` value lower it via the slice-emission path that
/// the backend owns.
///
/// Mapping rules:
/// - `iN` / `uN` -> `iN` (LLVM does not distinguish signedness at the
///   type level; signed vs. unsigned is per-operation).
/// - `f32` -> `float`, `f64` -> `double`.
/// - `bool` -> `i1` (single-bit; widened to a byte in memory).
/// - `char` -> `i32` (Unicode scalar value).
/// - `usize` / `isize` -> `i32` on `wasm32`, `i64` everywhere else
///   in the v0.1 target matrix.
#[allow(dead_code)] // production lowering goes through inkwell types directly; retained for the mapping table and its tests
pub(crate) const fn llvm_ir_type_name(prim: MirPrim, arch: Arch) -> Option<&'static str> {
    let name = match prim {
        MirPrim::I8 | MirPrim::U8 => "i8",
        MirPrim::I16 | MirPrim::U16 => "i16",
        MirPrim::I32 | MirPrim::U32 | MirPrim::Char => "i32",
        MirPrim::I64 | MirPrim::U64 => "i64",
        MirPrim::I128 | MirPrim::U128 => "i128",
        MirPrim::F32 => "float",
        MirPrim::F64 => "double",
        MirPrim::Bool => "i1",
        MirPrim::Usize | MirPrim::Isize => {
            if pointer_width(arch) == 32 { "i32" } else { "i64" }
        }
        // Opaque pointer at the LLVM-IR level — `ptr` since LLVM 15.
        // No separate name encoding for this mapping table; production
        // lowering goes through inkwell's PointerType directly.
        MirPrim::HeapPtr => "ptr",
        MirPrim::Str => return None,
    };
    Some(name)
}

/// Size in bytes of `prim` when stored in memory on `arch`.
///
/// Memory size, not LLVM IR bit width. The crucial cases:
///
/// - `bool` is a single byte in memory (`i1` in IR, but allocations
///   align and pad to a byte).
/// - `char` is 4 bytes (Unicode scalar).
/// - `usize` / `isize` follow [`pointer_width`].
/// - `str` returns `None`: a `str` value is not a single in-memory
///   object — its physical representation is a pointer + length pair
///   the backend assembles separately.
pub const fn size_of_prim(prim: MirPrim, arch: Arch) -> Option<u32> {
    let size = match prim {
        MirPrim::I8 | MirPrim::U8 | MirPrim::Bool => 1,
        MirPrim::I16 | MirPrim::U16 => 2,
        MirPrim::I32 | MirPrim::U32 | MirPrim::F32 | MirPrim::Char => 4,
        MirPrim::I64 | MirPrim::U64 | MirPrim::F64 => 8,
        MirPrim::I128 | MirPrim::U128 => 16,
        MirPrim::Usize | MirPrim::Isize | MirPrim::HeapPtr => pointer_width(arch) / 8,
        MirPrim::Str => return None,
    };
    Some(size)
}

/// Natural alignment in bytes of `prim` on `arch`.
///
/// For every v0.1 primitive the natural alignment matches the in-memory
/// size — including `i128`/`u128` at 16-byte align, which is the LLVM
/// data-layout default on every v0.1 target (x86_64, aarch64, riscv64,
/// wasm32, wasm64).
///
/// `@align(...)` overrides do not flow through this query; this is the
/// *natural* layout the layout pass uses as the baseline before
/// applying overrides.
pub const fn align_of_prim(prim: MirPrim, arch: Arch) -> Option<u32> {
    // For v0.1 primitives, natural alignment == size. The function exists
    // so future targets with weaker alignment guarantees (e.g., AVR-style
    // misaligned doubles) can override at a single point.
    size_of_prim(prim, arch)
}

/// Whether `prim` is an integer type (signed or unsigned, including
/// `bool` and `char` which lower to integers in LLVM).
pub const fn is_integer(prim: MirPrim) -> bool {
    matches!(
        prim,
        MirPrim::I8
            | MirPrim::I16
            | MirPrim::I32
            | MirPrim::I64
            | MirPrim::I128
            | MirPrim::U8
            | MirPrim::U16
            | MirPrim::U32
            | MirPrim::U64
            | MirPrim::U128
            | MirPrim::Usize
            | MirPrim::Isize
            | MirPrim::Bool
            | MirPrim::Char
    )
}

/// Whether `prim` is a signed integer. `Isize` is signed; `Bool` and
/// `Char` are not.
pub const fn is_signed_integer(prim: MirPrim) -> bool {
    matches!(
        prim,
        MirPrim::I8
            | MirPrim::I16
            | MirPrim::I32
            | MirPrim::I64
            | MirPrim::I128
            | MirPrim::Isize
    )
}

/// Whether `prim` is a floating-point type.
pub const fn is_float(prim: MirPrim) -> bool {
    matches!(prim, MirPrim::F32 | MirPrim::F64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_type_names_drop_signedness() {
        for arch in Arch::ALL {
            assert_eq!(llvm_ir_type_name(MirPrim::I8, arch), Some("i8"));
            assert_eq!(llvm_ir_type_name(MirPrim::U8, arch), Some("i8"));
            assert_eq!(llvm_ir_type_name(MirPrim::I32, arch), Some("i32"));
            assert_eq!(llvm_ir_type_name(MirPrim::U32, arch), Some("i32"));
            assert_eq!(llvm_ir_type_name(MirPrim::I128, arch), Some("i128"));
            assert_eq!(llvm_ir_type_name(MirPrim::U128, arch), Some("i128"));
        }
    }

    #[test]
    fn floats_map_to_llvm_keywords() {
        for arch in Arch::ALL {
            assert_eq!(llvm_ir_type_name(MirPrim::F32, arch), Some("float"));
            assert_eq!(llvm_ir_type_name(MirPrim::F64, arch), Some("double"));
        }
    }

    #[test]
    fn bool_maps_to_i1() {
        for arch in Arch::ALL {
            assert_eq!(llvm_ir_type_name(MirPrim::Bool, arch), Some("i1"));
        }
    }

    #[test]
    fn char_maps_to_i32() {
        for arch in Arch::ALL {
            assert_eq!(llvm_ir_type_name(MirPrim::Char, arch), Some("i32"));
        }
    }

    #[test]
    fn usize_isize_follow_pointer_width() {
        // wasm32 -> i32, everything else -> i64.
        assert_eq!(llvm_ir_type_name(MirPrim::Usize, Arch::Wasm32), Some("i32"));
        assert_eq!(llvm_ir_type_name(MirPrim::Isize, Arch::Wasm32), Some("i32"));
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64, Arch::Wasm64] {
            assert_eq!(llvm_ir_type_name(MirPrim::Usize, arch), Some("i64"));
            assert_eq!(llvm_ir_type_name(MirPrim::Isize, arch), Some("i64"));
        }
    }

    #[test]
    fn str_has_no_single_llvm_type() {
        for arch in Arch::ALL {
            assert_eq!(llvm_ir_type_name(MirPrim::Str, arch), None);
            assert_eq!(size_of_prim(MirPrim::Str, arch), None);
            assert_eq!(align_of_prim(MirPrim::Str, arch), None);
        }
    }

    #[test]
    fn primitive_sizes_match_table() {
        let arch = Arch::X86_64;
        assert_eq!(size_of_prim(MirPrim::I8, arch), Some(1));
        assert_eq!(size_of_prim(MirPrim::U8, arch), Some(1));
        assert_eq!(size_of_prim(MirPrim::Bool, arch), Some(1));
        assert_eq!(size_of_prim(MirPrim::I16, arch), Some(2));
        assert_eq!(size_of_prim(MirPrim::U16, arch), Some(2));
        assert_eq!(size_of_prim(MirPrim::I32, arch), Some(4));
        assert_eq!(size_of_prim(MirPrim::U32, arch), Some(4));
        assert_eq!(size_of_prim(MirPrim::F32, arch), Some(4));
        assert_eq!(size_of_prim(MirPrim::Char, arch), Some(4));
        assert_eq!(size_of_prim(MirPrim::I64, arch), Some(8));
        assert_eq!(size_of_prim(MirPrim::U64, arch), Some(8));
        assert_eq!(size_of_prim(MirPrim::F64, arch), Some(8));
        assert_eq!(size_of_prim(MirPrim::I128, arch), Some(16));
        assert_eq!(size_of_prim(MirPrim::U128, arch), Some(16));
    }

    #[test]
    fn pointer_sized_primitive_sizes_follow_arch() {
        assert_eq!(size_of_prim(MirPrim::Usize, Arch::Wasm32), Some(4));
        assert_eq!(size_of_prim(MirPrim::Isize, Arch::Wasm32), Some(4));
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64, Arch::Wasm64] {
            assert_eq!(size_of_prim(MirPrim::Usize, arch), Some(8));
            assert_eq!(size_of_prim(MirPrim::Isize, arch), Some(8));
        }
    }

    #[test]
    fn natural_alignment_matches_size_for_every_v0_1_primitive() {
        for arch in Arch::ALL {
            for prim in [
                MirPrim::I8,
                MirPrim::U8,
                MirPrim::I16,
                MirPrim::U16,
                MirPrim::I32,
                MirPrim::U32,
                MirPrim::I64,
                MirPrim::U64,
                MirPrim::I128,
                MirPrim::U128,
                MirPrim::F32,
                MirPrim::F64,
                MirPrim::Bool,
                MirPrim::Char,
                MirPrim::Usize,
                MirPrim::Isize,
            ] {
                assert_eq!(
                    align_of_prim(prim, arch),
                    size_of_prim(prim, arch),
                    "{prim:?} on {arch:?} has align != size",
                );
            }
        }
    }

    #[test]
    fn classification_predicates() {
        assert!(is_integer(MirPrim::I32));
        assert!(is_integer(MirPrim::U64));
        assert!(is_integer(MirPrim::Bool));
        assert!(is_integer(MirPrim::Char));
        assert!(is_integer(MirPrim::Usize));
        assert!(!is_integer(MirPrim::F32));
        assert!(!is_integer(MirPrim::F64));
        assert!(!is_integer(MirPrim::Str));

        assert!(is_signed_integer(MirPrim::I32));
        assert!(is_signed_integer(MirPrim::I128));
        assert!(is_signed_integer(MirPrim::Isize));
        assert!(!is_signed_integer(MirPrim::U32));
        assert!(!is_signed_integer(MirPrim::Usize));
        assert!(!is_signed_integer(MirPrim::Bool));
        assert!(!is_signed_integer(MirPrim::Char));
        assert!(!is_signed_integer(MirPrim::F32));

        assert!(is_float(MirPrim::F32));
        assert!(is_float(MirPrim::F64));
        assert!(!is_float(MirPrim::I32));
        assert!(!is_float(MirPrim::Bool));
    }
}
