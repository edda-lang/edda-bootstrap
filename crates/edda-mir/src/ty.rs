//! MIR type representation.
//!
//! Post-typecheck types: every `ParamMode` is explicit (no `Default`),
//! refinement predicates are gone, and ADTs are referenced by [`AdtId`]
//! rather than by name.

use crate::effect::CapabilityKind;
use crate::ids::AdtId;
use crate::layout::LayoutInfo;

/// A MIR type: kind plus layout descriptor.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct MirType {
    /// Type-form variant.
    pub kind: MirTypeKind,
    /// Layout descriptor (natural layout by default; filled in by the typed-HIR lowering pass).
    pub layout: LayoutInfo,
}

impl MirType {
    /// Build a type with natural layout from a `MirTypeKind`.
    pub fn new(kind: MirTypeKind) -> Self {
        MirType {
            kind,
            layout: LayoutInfo::natural(),
        }
    }

    /// Construct `Unit` with natural layout.
    pub fn unit() -> Self {
        MirType::new(MirTypeKind::Unit)
    }

    /// Construct `Never` with natural layout.
    pub fn never() -> Self {
        MirType::new(MirTypeKind::Never)
    }

    /// Construct a primitive type with natural layout.
    pub fn prim(p: MirPrim) -> Self {
        MirType::new(MirTypeKind::Prim(p))
    }
}

/// Every type form the MIR can carry.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum MirTypeKind {
    /// Primitive scalar (`i32`, `bool`, `str`, ...).
    Prim(MirPrim),
    /// Algebraic data type referenced by `AdtId`.
    Adt(AdtId),
    /// Anonymous tuple `(T1, T2, ...)` — minimum 2 elements.
    Tuple(Vec<MirType>),
    /// Slice `[T]`.
    Slice(Box<MirType>),
    /// Unit type `()`.
    Unit,
    /// Never (bottom) type — produced by `return`, `raise`, `panic`, infinite loops.
    Never,
    /// Capability handle (`allocator`, `clock`, `fs`, ...).
    Capability(CapabilityKind),
    /// Function-pointer type carrying a signature.
    FnPtr(Box<FnSig>),
}

/// Primitive scalar kinds.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum MirPrim {
    /// Signed 8-bit integer.
    I8,
    /// Signed 16-bit integer.
    I16,
    /// Signed 32-bit integer.
    I32,
    /// Signed 64-bit integer.
    I64,
    /// Signed 128-bit integer.
    I128,
    /// Unsigned 8-bit integer.
    U8,
    /// Unsigned 16-bit integer.
    U16,
    /// Unsigned 32-bit integer.
    U32,
    /// Unsigned 64-bit integer.
    U64,
    /// Unsigned 128-bit integer.
    U128,
    /// 32-bit IEEE-754 float.
    F32,
    /// 64-bit IEEE-754 float.
    F64,
    /// Boolean (`true` / `false`).
    Bool,
    /// Unicode scalar value.
    Char,
    /// Pointer-sized unsigned integer.
    Usize,
    /// Pointer-sized signed integer.
    Isize,
    /// UTF-8 string slice.
    Str,
    /// Opaque raw heap pointer. Pointer-width on every target; lowered
    /// to LLVM's opaque pointer type. Carries no element type at the
    /// MIR level — element-size knowledge lives at intrinsic call sites
    /// that thread `size_of(T)` / `align_of(T)` as explicit arguments.
    HeapPtr,
}

impl MirPrim {
    /// Lowercase canonical spelling, matching the surface language token.
    pub fn as_str(self) -> &'static str {
        match self {
            MirPrim::I8 => "i8",
            MirPrim::I16 => "i16",
            MirPrim::I32 => "i32",
            MirPrim::I64 => "i64",
            MirPrim::I128 => "i128",
            MirPrim::U8 => "u8",
            MirPrim::U16 => "u16",
            MirPrim::U32 => "u32",
            MirPrim::U64 => "u64",
            MirPrim::U128 => "u128",
            MirPrim::F32 => "f32",
            MirPrim::F64 => "f64",
            MirPrim::Bool => "bool",
            MirPrim::Char => "char",
            MirPrim::Usize => "usize",
            MirPrim::Isize => "isize",
            MirPrim::Str => "str",
            MirPrim::HeapPtr => "heap_ptr",
        }
    }
}

/// Parameter mode after typecheck. The surface `Default` is collapsed into
/// `Let`; MIR carries the resolved mode for every parameter.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ParamMode {
    /// Immutable by-reference (the surface default).
    Let,
    /// `mutable` — mutable by-reference; caller retains ownership.
    Mutable,
    /// `take` — by-value ownership transfer to callee.
    Take,
    /// `init` — uninitialised destination the callee writes into.
    Init,
}

impl ParamMode {
    /// Lowercase keyword spelling used by the pretty-printer.
    pub fn as_str(self) -> &'static str {
        match self {
            ParamMode::Let => "let",
            ParamMode::Mutable => "mutable",
            ParamMode::Take => "take",
            ParamMode::Init => "init",
        }
    }
}

/// Function signature: parameters (with modes), return type, and the effect-row
/// projection (capability list, error list, panic bit).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct FnSig {
    /// Parameters in declaration order.
    pub params: Vec<(ParamMode, MirType)>,
    /// Return type.
    pub ret: MirType,
    /// Capability kinds threaded into the call ABI.
    pub capabilities: Vec<CapabilityKind>,
    /// Error ADTs this function may `raise`.
    pub may_raise: Vec<AdtId>,
    /// Whether this function may panic.
    pub may_panic: bool,
}
