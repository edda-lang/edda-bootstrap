//! Minimal layout descriptor attached to every [`crate::MirType`].
//!
//! The field is carried on the type and currently populated with the
//! natural-layout default for primitives, tuples, and slices. The typed-HIR
//! lowering pass replaces this with the real layout resolved from `@layout`,
//! `@align`, `@repr`, and `@abi` attributes per `docs/tooling/abi-and-layout.md`.

use edda_intern::Symbol;

/// Layout descriptor: policy + representation + ABI tag + optional explicit
/// alignment.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct LayoutInfo {
    /// Field-ordering policy.
    pub policy: LayoutPolicy,
    /// Memory-layout representation.
    pub repr: ReprKind,
    /// Calling-convention tag (only relevant on `FnPtr` types).
    pub abi: AbiTag,
    /// Optional explicit alignment override.
    pub align: Option<AlignBytes>,
}

impl LayoutInfo {
    /// The default layout used by primitives, tuples, slices, and any type
    /// that has not yet been through the dedicated layout pass.
    pub fn natural() -> Self {
        LayoutInfo {
            policy: LayoutPolicy::Natural,
            repr: ReprKind::Edda,
            abi: AbiTag::Edda,
            align: None,
        }
    }
}

impl Default for LayoutInfo {
    fn default() -> Self {
        LayoutInfo::natural()
    }
}

/// Field-ordering policy applied to record-shaped types.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum LayoutPolicy {
    /// Source-declared field order, no implicit padding minimisation.
    Natural,
    /// Order specified by an explicit `@layout(...)` attribute.
    Declared,
    /// Compiler reorders fields by descending alignment to minimise padding.
    Sorted,
    /// `@layout(packed)` — no padding, alignment forced to 1.
    Packed,
}

/// Memory-layout representation tag.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ReprKind {
    /// Default Edda representation.
    Edda,
    /// `@repr(c)` — C-compatible layout for FFI.
    C,
    /// `@repr(transparent)` — single non-zero-sized field defines the layout.
    Transparent,
    /// `@repr(simd)` — vector-width layout for SIMD intrinsics.
    Simd,
    /// `@repr(opaque)` — opaque to non-friend modules.
    Opaque,
}

/// Calling-convention tag carried on function-pointer types.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum AbiTag {
    /// Edda's default calling convention.
    Edda,
    /// `@abi("C")`.
    C,
    /// `@abi("system")` — platform-conditional (`stdcall` on Win32, `C` elsewhere).
    System,
    /// `@abi("name")` for any non-standard ABI string.
    Named(Symbol),
}

/// Explicit alignment override in bytes.
///
/// Prefer [`AlignBytes::new`] for construction — it enforces the
/// power-of-two invariant. The inner field remains public so callers that
/// need to serialise or debug-print the byte count without going through an
/// accessor can do so directly.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct AlignBytes(pub u32);

impl AlignBytes {
    /// Construct an alignment, returning `None` if `bytes` is zero or not a
    /// power of two.
    pub fn new(bytes: u32) -> Option<AlignBytes> {
        if bytes == 0 || !bytes.is_power_of_two() {
            None
        } else {
            Some(AlignBytes(bytes))
        }
    }

    /// Raw byte count of this alignment.
    pub fn get(self) -> u32 {
        self.0
    }
}
