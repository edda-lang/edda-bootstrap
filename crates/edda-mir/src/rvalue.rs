//! Rvalues: the right-hand side of an `Assign` statement.

use edda_span::Span;

use crate::ids::{AdtId, BodyId, FieldIdx, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::ty::{MirPrim, MirType};

/// An rvalue: the value-producing right-hand side of an `Assign` statement.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Rvalue {
    /// Source span of the originating expression.
    pub span: Span,
    /// Variant and operands.
    pub kind: RvalueKind,
    /// Resulting type.
    pub ty: MirType,
}

/// Every rvalue form.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum RvalueKind {
    /// Forward an operand unchanged.
    Use(Operand),
    /// Binary arithmetic / logical / comparison operation.
    BinOp {
        /// Operator.
        op: BinOp,
        /// Left-hand operand.
        lhs: Operand,
        /// Right-hand operand.
        rhs: Operand,
        /// Primitive width the operation is performed at.
        prim: MirPrim,
    },
    /// Unary operation.
    UnOp {
        /// Operator.
        op: UnOp,
        /// Operand.
        arg: Operand,
        /// Primitive width.
        prim: MirPrim,
    },
    /// Numeric / boolean / char cast between primitive widths.
    Cast {
        /// Source operand.
        src: Operand,
        /// Source primitive width.
        src_prim: MirPrim,
        /// Destination primitive width.
        dst_prim: MirPrim,
    },
    /// Build a slice / fixed-length array from a list of operands.
    MakeArray {
        /// Element operands in source order.
        elems: Vec<Operand>,
    },
    /// Build a tuple from a list of operands.
    MakeTuple {
        /// Element operands in source order.
        elems: Vec<Operand>,
    },
    /// Build a record / single-variant product from a list of field operands.
    MakeRecord {
        /// Target ADT (must be `AdtKind::Product`).
        adt: AdtId,
        /// Field operands in declaration order.
        fields: Vec<Operand>,
    },
    /// Build a sum-type value with an explicit variant tag.
    MakeVariant {
        /// Target ADT (must be `AdtKind::Sum`).
        adt: AdtId,
        /// Variant index within the ADT.
        variant: VariantIdx,
        /// Field operands in the variant's declaration order.
        fields: Vec<Operand>,
    },
    /// Read an array element by computed index.
    ArrayIndex {
        /// Array operand.
        array: Operand,
        /// Index operand (integer-typed).
        idx: Operand,
    },
    /// Build a sub-slice from a source slice and a half-open `[lo, hi)`
    /// range. The resulting fat pointer borrows the source's backing
    /// storage; no copy is performed. `lo` and `hi` must both be
    /// `usize`-typed; the typechecker proves `lo <= hi <= source.len()`
    /// — this layer trusts that proof and emits no runtime check.
    SliceSubrange {
        /// Source slice (must be `MirTypeKind::Slice(elem)`).
        source: Operand,
        /// Inclusive lower bound, `usize`-typed.
        lo: Operand,
        /// Exclusive upper bound, `usize`-typed.
        hi: Operand,
    },
    /// Read the length of a slice / array.
    ArrayLen {
        /// Array or slice operand.
        array: Operand,
    },
    /// Extract a field by index. `variant` is `Some` for sum-typed subjects.
    ExtractField {
        /// Subject operand.
        subject: Operand,
        /// Variant index, when the subject is a sum.
        variant: Option<VariantIdx>,
        /// Field index inside the chosen variant.
        field: FieldIdx,
    },
    /// Read the discriminant tag of a sum-typed value.
    ExtractTag {
        /// Subject operand (must be sum-typed).
        subject: Operand,
    },
    /// Produce a `[u8]` view of a `String` value's UTF-8 byte content.
    ///
    /// Corresponds to the `StringBytes` compiler intrinsic. The `String`
    /// runtime representation is a fat pointer `{ ptr, isize }` whose
    /// data region holds the UTF-8 bytes; this rvalue exposes the same
    /// fat pointer reinterpreted as `[u8]`, with no copy.
    StringBytes(Operand),
    /// Take the address of a body-bodied function in this program,
    /// producing a fn-pointer value. The result type is always
    /// `MirTypeKind::FnPtr(sig)` with `sig` matching the referenced
    /// body's signature. The [`BodyId`] is resolved at MIR-lowering
    /// time (the lowering pass maps `BindingId → BodyId` through
    /// `function_map`); the LLVM backend retrieves the corresponding
    /// `FunctionValue` from the same body table. Used when a function
    /// name appears as a value (`let h = f`, `pass(f)`, struct-field
    /// initialiser). Extern symbol-refs are deferred to a later slice.
    ///
    /// Under the unified fat-function-value ABI this lowers to a
    /// `{ code: ptr, env: usize }` value with `env = 0` — an env-less
    /// fat function value. Used for captureless closures and the
    /// forwarding shims synthesised for named functions used as values.
    FunctionRef(BodyId),
    /// Construct a fat function value `{ code: ptr, env: usize }` from a
    /// referenced body and an environment word. The env word holds a
    /// single inline word-sized capture by value (recovered inside the
    /// body via [`RvalueKind::Cast`]); the same slot is pointer-width so
    /// it generalises to a pointer for larger environments later. The
    /// result type is always `MirTypeKind::FnPtr(sig)` — identical to
    /// [`RvalueKind::FunctionRef`], so closures and named-fn values are
    /// interchangeable wherever a `function(...)` value is expected.
    MakeClosure {
        /// Body the code pointer references — a synthesised closure
        /// body whose *leading* parameter is the env word.
        code: BodyId,
        /// Environment word operand (the packed capture, or a zero
        /// constant for the env-less case).
        env: Operand,
    },
    /// Take the address of a place, producing a `HeapPtr` value.
    ///
    /// Materialises a return-position borrow (`-> let T` / `-> mutable
    /// T`): the function body returns the *address* of a place rooted at
    /// a by-reference parameter rather than a copy of its value, so the
    /// caller's `mutable` write threads back to the borrowed storage.
    /// The result type is always `MirPrim::HeapPtr`; the pointee type
    /// travels on the caller-side `Projection::Deref(T)` that reads the
    /// returned pointer back into a place.
    Ref {
        /// Place whose address is taken.
        place: Place,
    },
}

/// Binary operators.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `+%` wrapping integer addition (modulo two's-complement).
    WrapAdd,
    /// `-%` wrapping integer subtraction.
    WrapSub,
    /// `*%` wrapping integer multiplication.
    WrapMul,
    /// `+|` saturating integer addition (clamps to operand-width MIN/MAX).
    SatAdd,
    /// `-|` saturating integer subtraction.
    SatSub,
    /// `*|` saturating integer multiplication.
    SatMul,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// Non-short-circuit boolean `and` (short-circuit was lowered to `SwitchBool`).
    And,
    /// Non-short-circuit boolean `or`.
    Or,
    /// Bitwise `&`.
    BitAnd,
    /// Bitwise `|`.
    BitOr,
    /// Bitwise `^`.
    BitXor,
    /// Left shift.
    Shl,
    /// Right shift (arithmetic for signed, logical for unsigned).
    Shr,
}

impl BinOp {
    /// Short uppercase mnemonic used in the pretty-printer.
    pub fn mnemonic(self) -> &'static str {
        match self {
            BinOp::Add => "Add",
            BinOp::Sub => "Sub",
            BinOp::Mul => "Mul",
            BinOp::Div => "Div",
            BinOp::Mod => "Mod",
            BinOp::WrapAdd => "WrapAdd",
            BinOp::WrapSub => "WrapSub",
            BinOp::WrapMul => "WrapMul",
            BinOp::SatAdd => "SatAdd",
            BinOp::SatSub => "SatSub",
            BinOp::SatMul => "SatMul",
            BinOp::Eq => "Eq",
            BinOp::Ne => "Ne",
            BinOp::Lt => "Lt",
            BinOp::Le => "Le",
            BinOp::Gt => "Gt",
            BinOp::Ge => "Ge",
            BinOp::And => "And",
            BinOp::Or => "Or",
            BinOp::BitAnd => "BitAnd",
            BinOp::BitOr => "BitOr",
            BinOp::BitXor => "BitXor",
            BinOp::Shl => "Shl",
            BinOp::Shr => "Shr",
        }
    }
}

/// Unary operators.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum UnOp {
    /// Arithmetic negation.
    Neg,
    /// Boolean `not`.
    Not,
    /// Bitwise complement.
    BitNot,
}

impl UnOp {
    /// Short mnemonic used in the pretty-printer.
    pub fn mnemonic(self) -> &'static str {
        match self {
            UnOp::Neg => "Neg",
            UnOp::Not => "Not",
            UnOp::BitNot => "BitNot",
        }
    }
}
