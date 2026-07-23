//! Type-expression AST plus the satellite types that travel with it:
//! parameter modes, effect rows, and refinement clauses.

use edda_span::Span;

use super::{Expr, Path};

/// A type-expression AST node.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Type {
    /// Source range covering the type expression.
    pub span: Span,
    /// Variant and payload.
    pub kind: TypeKind,
}

/// Every type form admitted by the locked surface.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum TypeKind {
    /// Named type by path: `i32`, `String`, `std.option.Option`.
    Path(Path),
    /// `(T, U, ...)` tuple type — minimum 2 elements.
    Tuple(Vec<Type>),
    /// `[T]` slice type.
    Slice(Box<Type>),
    /// `()` unit type.
    Unit,
    /// `function(P1, P2) -> R with {effects}` function-type per
    /// phase-2-locks Gap 1. Each param admits an optional name and an
    /// optional mode prefix (`name: <mode> Type`).
    Function {
        /// Parameter list in declaration order.
        params: Vec<FnTypeParam>,
        /// Return type.
        ret: Box<Type>,
        /// Optional effect row.
        effects: Option<EffectRow>,
    },
    /// `Type` — the comptime meta-type whose values are types.
    Meta,
    /// `comptime T` — a comptime-only type.
    Comptime(Box<Type>),
    /// `T where pred` — base type refined by a boolean predicate.
    Refined {
        /// Underlying type.
        base: Box<Type>,
        /// Refinement predicate.
        pred: Expr,
    },
    /// Parser-recovery sentinel. A diagnostic has already been emitted.
    Error,
}

/// One parameter in a function-type literal `function(...) -> R`.
///
/// Three surface forms, all round-tripped exactly:
/// - bare type: `T` — `name = None`, `mode = Default`
/// - moded bare type: `<mode> T` — `name = None`, `mode = <mode>`
/// - named typed: `name: <mode>? T` — `name = Some`, optional mode
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct FnTypeParam {
    /// Source range covering the whole parameter.
    pub span: Span,
    /// Optional parameter name. Absent in the bare-type forms.
    pub name: Option<super::Ident>,
    /// Optional mode prefix. `Default` means no keyword in source.
    pub mode: ParamMode,
    /// Parameter type.
    pub ty: Type,
}

/// Parameter-mode prefix on a function parameter's type: `let` (default,
/// elided in source), `mutable`, `take`, or `init`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ParamMode {
    /// No prefix — immutable read-only by-reference (the language default).
    Default,
    /// `mutable` — mutable by-reference; caller retains ownership.
    Mutable,
    /// `take` — by-value; ownership transferred to the callee.
    Take,
    /// `init` — uninitialized destination the callee writes into.
    Init,
}

/// Borrow-mode prefix on a function's return type: `let` (immutable
/// borrow) or `mutable` (mutable borrow), or `ByValue` (no prefix — the
/// language default by-value return).
///
/// A return-position borrow ties its region to a by-reference parameter
/// (`-> let T` / `-> mutable T`); the typechecker enforces that binding
/// so the borrow cannot outlive the argument it aliases.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ReturnMode {
    /// No prefix — by-value return (the language default).
    ByValue,
    /// `let` — immutable borrow tied to a by-reference receiver parameter.
    Let,
    /// `mutable` — mutable borrow tied to a `mutable` receiver parameter.
    Mutable,
}

impl ReturnMode {
    /// The keyword that prefixes this return mode in source. `ByValue`
    /// returns an empty string — the by-value default has no spelling.
    pub const fn keyword(self) -> &'static str {
        match self {
            ReturnMode::ByValue => "",
            ReturnMode::Let => "let",
            ReturnMode::Mutable => "mutable",
        }
    }
}

/// An effect row: `with { allocator, err: IoError, ...Other }`.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct EffectRow {
    /// Source range covering the `{ ... }` (the `with` keyword's span is on the parent).
    pub span: Span,
    /// Effect entries in source order.
    pub members: Vec<EffectMember>,
}

/// A single entry inside an [`EffectRow`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum EffectMember {
    /// Bare effect-member name. Two interpretations share this shape: a
    /// capability parameter (`allocator`, `clock`, `fs`) or the locked
    /// payload-free pure-effect kind `panic`. Disambiguation is a
    /// semantic-layer concern, not a syntactic one.
    Capability(super::Ident),
    /// `name: T` — named effect with payload type, e.g. `err: IoError`.
    Named {
        /// Effect-slot name.
        name: super::Ident,
        /// Payload type.
        ty: Type,
    },
    /// `...Other` — splice in another effect-row alias.
    Spread(Path),
    /// `kind(<bound>)` — graded pure-effect entry. Three locked kinds
    /// per `02-modes-effects-refinements.md` §5.2: `alloc(bytes <= N)`,
    /// `io(calls <= N)`, `time(ops <= N)`. The bound is a
    /// refinement-fragment expression (LIA over caller parameters).
    /// Mixing graded and ungraded entries of the same kind in one row
    /// is a parse error (§5.6).
    Graded {
        /// Graded kind name (`alloc`, `io`, `time`) — the bare-ident
        /// keyword preceding the parenthesised bound.
        kind: super::Ident,
        /// Bound expression. For `alloc(bytes <= 4096)` this is the
        /// predicate `bytes <= 4096`; the parser stores it as the full
        /// expression so the refinement lifter can translate it to a
        /// [`Predicate`](edda_refine::Predicate) at typecheck time.
        bound: Box<Expr>,
    },
}

/// A single refinement clause attached to a function (`requires` /
/// `ensures` / `decreases`) or to a type (`where`).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct RefinementClause {
    /// Source range covering the whole clause.
    pub span: Span,
    /// Which clause keyword introduced the predicate.
    pub kind: RefinementKind,
    /// Clause expression — boolean predicate for Where/Requires/Ensures,
    /// well-founded measure for Decreases.
    pub pred: Expr,
}

/// Discriminator for the four refinement-clause positions.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum RefinementKind {
    /// `where` — inline type refinement or struct-field refinement.
    Where,
    /// `requires` — function precondition.
    Requires,
    /// `ensures` — function postcondition (may reference `result`).
    Ensures,
    /// `decreases` — termination measure for recursive functions and
    /// unbounded loops per `03-verification.md` §5. The carried
    /// expression must be a non-negative integer-valued quantity that
    /// strictly decreases at each recursive call or loop iteration; for
    /// mutual recursion the measure is a tuple ordered by lex-product.
    Decreases,
}
