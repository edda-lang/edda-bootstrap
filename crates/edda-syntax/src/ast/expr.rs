//! Expression AST.
//!
//! Edda is an expression-oriented language: `if`, `match`, `loop`, and
//! blocks all produce values. Statements (let-binding, assignment) are a
//! separate concept that lives only inside [`Block`]s.

use edda_intern::Symbol;
use edda_span::Span;

use super::{EffectRow, Ident, Param, Path, Stmt, Type};
use crate::token::IntBase;

/// A typed-by-context expression node. Carries its [`Span`] and the
/// variant payload in [`ExprKind`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Expr {
    /// Source range.
    pub span: Span,
    /// Variant and payload.
    pub kind: ExprKind,
}

/// Every expression form the V1.0 surface admits.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum ExprKind {
    /// A literal value (int, float, string, bool, unit).
    Literal(Literal),
    /// `f"...{expr}..."` interpolated string: an ordered sequence of
    /// literal text runs and parsed interpolation slots.
    /// Lowers to a left-fold of format
    /// calls + string concatenation.
    FString(Vec<FStringPart>),
    /// Identifier or dotted-path reference (`x`, `std.fs.read`).
    Path(Path),
    /// `op lhs rhs` binary operation.
    Binary {
        /// Operator symbol.
        op: BinOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
    /// `op expr` unary prefix operation.
    Unary {
        /// Operator symbol.
        op: UnOp,
        /// Operand.
        expr: Box<Expr>,
    },
    /// `callee(args...)` function call.
    Call {
        /// Function being invoked.
        callee: Box<Expr>,
        /// Positional arguments. Each carries an optional call-site
        /// mode keyword (`mutable` / `take` / `init`).
        args: Vec<CallArg>,
    },
    /// `receiver.name(args...)` method call.
    MethodCall {
        /// The value the method is called on.
        receiver: Box<Expr>,
        /// Method name.
        name: Ident,
        /// Positional arguments. Each carries an optional call-site
        /// mode keyword (`mutable` / `take` / `init`).
        args: Vec<CallArg>,
    },
    /// `receiver.field` field access.
    Field {
        /// The value being projected.
        receiver: Box<Expr>,
        /// Field name.
        name: Ident,
    },
    /// `receiver.N` positional-element access where `N` is a decimal
    /// integer literal. Positional fields are not identifiers, so the
    /// parser admits an `Int` token after the postfix `.` and lowers it
    /// to this variant. The receiver's type must resolve at inference
    /// time to a tuple (`TyKind::Tuple`, element `N`) or a sum-typed
    /// nominal (variant-`N` payload — the D-22 READ surface;
    /// the comptime expansion's
    /// sum-loop `CompField` fold synthesises this form). Other
    /// receivers diagnose. Only decimal indices are admitted —
    /// non-decimal bases and indices that overflow `u32` are rejected
    /// at parse time.
    TupleIndex {
        /// The tuple value being projected.
        receiver: Box<Expr>,
        /// Zero-based tuple element index (`t.0`, `t.1`, …).
        index: u32,
    },
    /// `receiver.(index)` comptime-indexed field access (D-22). The
    /// `index` is a comptime-evaluable expression naming the i-th field
    /// of `receiver`'s type in declaration order. Admitted only inside a
    /// spec body / `comptime`-driven context, where the pass-2 comptime
    /// expansion (`edda-comptime`) unrolls the enclosing `comptime for`
    /// and rewrites this node to a plain `Field` against the concrete
    /// field name from `field_name_at(T, index)` (product target), the
    /// positional `TupleIndex` form (sum-typed value receiver — the
    /// variant-payload READ), or
    /// the qualified variant path (sum type-name receiver). It is
    /// therefore an AST-only form — it never reaches HIR / MIR;
    /// expansion eliminates it first. Used as both an rvalue and an
    /// assignment LHS (`out.(i) = value`), driving introspection-driven
    /// record construction per `04-specs-comptime.md` §4.5.
    CompField {
        /// The aggregate value being projected.
        receiver: Box<Expr>,
        /// Comptime-evaluable field index expression.
        index: Box<Expr>,
    },
    /// `receiver[index]` indexing.
    Index {
        /// The collection being indexed.
        receiver: Box<Expr>,
        /// Index expression.
        index: Box<Expr>,
    },
    /// `if cond { ... } else { ... }`.
    If {
        /// Branch condition.
        cond: Box<Expr>,
        /// `then` branch.
        then_block: Block,
        /// Optional `else` branch — block, another `if`, or absent.
        else_branch: Option<Box<Expr>>,
    },
    /// `match scrutinee { arms... }`.
    Match {
        /// Value being matched.
        scrutinee: Box<Expr>,
        /// Arms in source order.
        arms: Vec<MatchArm>,
    },
    /// Block expression `{ stmts; trailing }`.
    Block(Block),
    /// `expr as T [mode]` primitive cast.
    ///
    /// The optional trailing `wrapping` / `saturating` / `checked`
    /// keyword carries an explicit cast mode per CLAUDE.md §"Numeric
    /// operators". The bare `expr as T` form takes the trapping default;
    /// `wrapping` admits two's-complement modular semantics on integer
    /// narrowing; `saturating` clamps to the destination's MIN/MAX;
    /// `checked` raises `err: Overflow` when the value is out of range.
    Cast {
        /// Value being cast.
        expr: Box<Expr>,
        /// Target type.
        ty: Box<Type>,
        /// Cast mode keyword, if any.
        mode: CastMode,
    },
    /// `lo..<hi`, `lo..=hi`, or any of the open-ended slice-subrange
    /// forms `lo..`, `..hi`, `..` (phase-2-locks Gap 7). Closed (`..=`)
    /// is admitted only with both endpoints; the open forms are always
    /// half-open in shape.
    Range {
        /// Inclusive low endpoint, `None` for `..hi` / `..` open-lower
        /// forms.
        lo: Option<Box<Expr>>,
        /// High endpoint, `None` for `lo..` / `..` open-upper forms.
        /// Semantics by `kind` when present.
        hi: Option<Box<Expr>>,
        /// `..<` (HalfOpen) or `..=` (Closed). `..` lowers to `HalfOpen`
        /// because the upper bound is exclusive whenever it exists.
        kind: RangeKind,
    },
    /// `(e1, e2, ...)` tuple constructor — minimum 2 elements.
    Tuple(Vec<Expr>),
    /// `[e1, e2, ..., en]` array / slice literal, including the empty
    /// form `[]`. The element type is the common type of the elements;
    /// for the empty form it is supplied by the expected type from
    /// context (let / field-initialiser annotation), lowering to a
    /// zero-length slice with no heap allocation.
    Array(Vec<Expr>),
    /// `Path { field: e, ... }` record / struct literal.
    StructLit {
        /// Type path being constructed.
        path: Path,
        /// Field-initialization list.
        fields: Vec<StructLitField>,
    },
    /// `loop [decreases <expr>] { ... }` unbounded loop expression.
    /// Yields via `break`. An absent `decreases` clause means the loop
    /// admits `effect divergence` in the enclosing function's effect row
    /// per `corpus/edda-codex/language/03-verification.md` §5.
    Loop {
        /// Loop body.
        body: Block,
        /// Optional label for nested-break/continue targeting.
        label: Option<Ident>,
        /// Optional `decreases <expr>` measure clause, written between
        /// the `loop` keyword and the body block. `None` when the loop
        /// has no termination measure; in that case it contributes
        /// `Pure(Divergence)` to the inferred effect row.
        decreases: Option<Box<Expr>>,
    },
    /// `for pat in iter { ... }` bounded iteration.
    For {
        /// Binding pattern for each element.
        pat: Box<super::Pat>,
        /// Iterable expression.
        iter: Box<Expr>,
        /// Loop body.
        body: Block,
        /// Optional loop label.
        label: Option<Ident>,
    },
    /// `expr?` error-effect propagation.
    Try(Box<Expr>),
    /// `expr.await` task resolution (postfix).
    Await(Box<Expr>),
    /// `raise expr` originate an error.
    Raise(Box<Expr>),
    /// `panic expr` originate the panic effect.
    Panic(Box<Expr>),
    /// `comptime expr` evaluate at compile time.
    Comptime(Box<Expr>),
    /// `comptime { ... }` comptime block.
    ComptimeBlock(Block),
    /// `scope(<kind>) [name] { ... }` structured-execution block.
    ///
    /// Two locked kinds per `corpus/edda-codex/language/05-concurrency-coherence.md`:
    /// `Exec` (structured concurrency) and `Coherence` (observational
    /// atomicity). Both forms admit an optional binder name; the
    /// surface grammar is `scope(<kind>) <name> { ... }`.
    Scope {
        /// Scope kind — exec (concurrency) or coherence (observational
        /// atomicity).
        kind: ScopeKind,
        /// Optional scope binder name (`group` in `scope(exec) group { ... }`).
        name: Option<Ident>,
        /// Scope body.
        body: Block,
    },
    /// `return [expr]`.
    Return(Option<Box<Expr>>),
    /// `break [label] [value]`.
    Break {
        /// Optional label of the loop being broken from.
        label: Option<Ident>,
        /// Optional value yielded from the loop.
        value: Option<Box<Expr>>,
    },
    /// `continue [label]`.
    Continue {
        /// Optional label of the loop being continued.
        label: Option<Ident>,
    },
    /// `with { ... }` comptime literal of type `EffectRow`. Admitted in
    /// comptime-pure positions (module-level `let X: EffectRow = ...`,
    /// `where` clauses, spec arguments) per
    /// `corpus/edda-codex/docs/codegen/spec-language.md` §136 and locked
    /// for row-alias bindings in `docs/types/effect-tracking.md` §234.
    EffectRow(EffectRow),
    /// `function(p1: T1, ...) -> R with {row} captures {c1, c2: take} { body }`
    /// closure literal. Locked in `docs/phase-2-locks.md` Gap 1 + Gap 2.
    Closure(Box<Closure>),
    /// `handle <effect>[: <ty>] [as <binder>] -> <recovery> <body>` effect handler.
    ///
    /// Suppresses the named effect within `body`; evaluates `recovery`
    /// when the effect fires. The effect is discharged — it does not
    /// propagate past the handler. Two shapes per
    /// `corpus/edda-codex/language/02-modes-effects-refinements.md` §4.2:
    /// a typed payload form (`handle err: SpawnError as e -> raise
    /// LinkError.wrap(e) { ... }`) and a payload-less form for the bare
    /// pure-effect kinds (`handle cancellation -> cleanup { ... }`,
    /// `handle divergence -> fallback { ... }`) — the payload-less form
    /// admits no `: <ty>` and no `as <binder>` clause, matching the
    /// locked Handler column (there is no payload to bind).
    Handle {
        /// Effect label (`err`, or a payload-less kind like `cancellation` / `divergence`).
        effect: Ident,
        /// Type of the effect payload. `None` for the payload-less
        /// pure-effect kinds (`cancellation`, `divergence`), which have
        /// no `: <ty>` clause in source.
        ty: Option<Box<Type>>,
        /// Optional payload binder — bound to the caught value inside
        /// `recovery`. `None` for the legacy `handle err: T -> recovery`
        /// form that ignores the caught value, and always `None` for
        /// the payload-less form (nothing to bind).
        binder: Option<Ident>,
        /// Expression evaluated when the effect fires; type must match the body's type.
        recovery: Box<Expr>,
        /// Handler scope — the effect is suppressed within this block.
        body: Block,
    },
    /// `forall <bound> in <iter>: <body>` bounded universal quantifier.
    ///
    /// Admissible inside refinement-clause expressions (`where` /
    /// `requires` / `ensures`) per V1.0 refinement-fragment widening
    /// (`corpus/edda-codex/language/03-verification.md` §11). The body
    /// must type-check to `bool`. The bound variable is in scope only
    /// inside `body`; its sort is the element sort of `iter`. The
    /// iterable is a range (`0..<n`, `0..=n`) or a slice (`xs`).
    Forall {
        /// Bound variable name.
        bound: Ident,
        /// Iterable expression — range or slice.
        iter: Box<Expr>,
        /// Body predicate.
        body: Box<Expr>,
    },
    /// `exists <bound> in <iter>: <body>` bounded existential quantifier.
    ///
    /// Mirror of [`ExprKind::Forall`] with existential semantics.
    Exists {
        /// Bound variable name.
        bound: Ident,
        /// Iterable expression — range or slice.
        iter: Box<Expr>,
        /// Body predicate.
        body: Box<Expr>,
    },
    /// `<scope-name>.spawn (take <name> [: <Type>] = <expr>, ...)? { <body> }`
    /// structured-concurrency task spawn.
    ///
    /// The scope-name binds the scope opened by an enclosing
    /// `scope(exec) <scope-name> { ... }` block. Locked grammar per
    /// `corpus/edda-codex/docs/syntax/effects.md` §"Structured concurrency":
    /// ```text
    /// <spawn-expr>  ::= <scope-name> "." "spawn" <spawn-args>? "{" <body> "}"
    /// <spawn-args>  ::= "(" <spawn-arg> ("," <spawn-arg>)* ")"
    /// <spawn-arg>   ::= "take" <identifier> [":" <Type>] "=" <expr>
    /// ```
    /// `let`-shareable bindings cross the boundary by implicit capture;
    /// the explicit arg list admits only `take` because single-task
    /// capabilities must transfer ownership across the spawn boundary.
    Spawn(Box<SpawnExpr>),
    /// Parser-recovery sentinel. A diagnostic has already been emitted.
    Error,
}

/// Closure-literal payload. Heavy enough to box inside [`ExprKind`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Closure {
    /// Source range covering from `function` through the closing body brace.
    pub span: Span,
    /// Closure-parameter list. Empty list spells `function()`.
    pub params: Vec<Param>,
    /// Return type after `->`. Mandatory per declarations.md §116.
    pub ret: Type,
    /// Optional `with { ... }` effect row. `None` when the clause is absent.
    pub effects: Option<EffectRow>,
    /// Optional `captures { ... }` clause. `None` when the clause is absent;
    /// `Some(vec![])` when the clause is written as `captures {}`.
    pub captures: Option<Vec<Capture>>,
    /// Closure body block.
    pub body: Block,
}

/// One entry in a closure's `captures { ... }` clause.
///
/// `let` captures (the default, no keyword) are read-only references to the
/// enclosing binding; `take` captures transfer ownership. `mutable` captures
/// are forbidden by phase-2-locks Gap 1 §Capture semantics.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Capture {
    /// Source range covering the name and the optional `: mode` suffix.
    pub span: Span,
    /// Captured binding name.
    pub name: Ident,
    /// Capture mode.
    pub mode: CaptureMode,
}

/// Capture-mode keyword on a closure's `captures { ... }` entry.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum CaptureMode {
    /// `let` capture — the default; read-only reference to the binding.
    Let,
    /// `take` capture — ownership transferred into the closure.
    Take,
}

/// Payload for [`ExprKind::Spawn`]. Heavy enough to box inside [`ExprKind`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct SpawnExpr {
    /// Source range covering from the scope name through the closing body brace.
    pub span: Span,
    /// Scope binder being spawned into (e.g. `group` in `group.spawn { ... }`).
    pub scope_name: Ident,
    /// Explicit `take`-mode argument list. Empty for the bare-block form.
    pub args: Vec<SpawnArg>,
    /// Spawned task body.
    pub body: Block,
}

/// One entry in a [`SpawnExpr`]'s argument list.
///
/// Single-task capabilities must cross the spawn boundary by ownership
/// transfer, never by reference. The `take` keyword is required at the
/// source level so the mode prefix stays visible; the AST need not carry
/// a separate mode tag because `take` is the only admitted prefix here.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct SpawnArg {
    /// Source range covering the entry from the `take` keyword to the initialiser.
    pub span: Span,
    /// Bound name visible inside the spawn body.
    pub name: Ident,
    /// Optional type annotation; inference may otherwise derive it from `init`.
    pub ty: Option<Type>,
    /// Initialiser expression — evaluated in the parent scope and moved into the body.
    pub init: Expr,
}

/// Binary operator symbols. Logical operators short-circuit; the default
/// arithmetic operators (`+ - * / %`) trap on integer overflow per
/// `spec-sweep-locks.md` S1; the explicit-mode wrapping forms
/// (`+% -% *%`) take two's-complement modular semantics; the
/// explicit-mode checked forms (`+? -? *? %?`) originate `err: Overflow`
/// on overflow; the explicit-mode saturating forms (`+| -| *|`) clamp
/// to operand-width MIN/MAX on overflow. Modulo (`%` / `%?`) is
/// integer-only — floats use `std.math.scalar.fmod`.
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
    /// `+%` wrapping (modulo two's-complement) integer addition.
    WrapAdd,
    /// `-%` wrapping (modulo two's-complement) integer subtraction.
    WrapSub,
    /// `*%` wrapping (modulo two's-complement) integer multiplication.
    WrapMul,
    /// `+?` checked integer addition; raises `err: Overflow` on overflow.
    CheckAdd,
    /// `-?` checked integer subtraction; raises `err: Overflow` on overflow.
    CheckSub,
    /// `*?` checked integer multiplication; raises `err: Overflow` on overflow.
    CheckMul,
    /// `%?` checked integer modulo; raises `err: Overflow` on `INT_MIN % -1`
    /// instead of trapping. Unsigned operands never overflow.
    CheckMod,
    /// `+|` saturating integer addition; clamps to operand-width MIN/MAX on overflow.
    SatAdd,
    /// `-|` saturating integer subtraction; clamps to operand-width MIN/MAX on overflow.
    SatSub,
    /// `*|` saturating integer multiplication; clamps to operand-width MIN/MAX on overflow.
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
    /// `&&` (short-circuit)
    And,
    /// `||` (short-circuit)
    Or,
    /// `&` bitwise AND
    BitAnd,
    /// `|` bitwise OR
    BitOr,
    /// `^` bitwise XOR
    BitXor,
    /// `<<` left shift
    Shl,
    /// `>>` right shift
    Shr,
}

/// Trailing cast-mode modifier on `expr as T`.
///
/// The bare `expr as T` form takes [`CastMode::Trap`] (no source
/// keyword). Explicit modes are written as a trailing keyword:
/// `expr as T wrapping` / `... saturating` / `... checked`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum CastMode {
    /// Bare `as T` form — trapping default. Panics on out-of-range
    /// integer narrowing per CLAUDE.md §"Numeric operators".
    Trap,
    /// `as T wrapping` — two's-complement modular semantics on integer
    /// narrowing.
    Wrapping,
    /// `as T saturating` — clamps to destination's MIN/MAX on integer
    /// narrowing.
    Saturating,
    /// `as T checked` — raises `err: Overflow` when the value is out of
    /// range.
    Checked,
}

impl CastMode {
    /// Source spelling of the trailing keyword, or `None` for the bare
    /// trapping default.
    pub const fn keyword(self) -> Option<&'static str> {
        match self {
            CastMode::Trap => None,
            CastMode::Wrapping => Some("wrapping"),
            CastMode::Saturating => Some("saturating"),
            CastMode::Checked => Some("checked"),
        }
    }
}

/// Unary prefix operator symbols.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum UnOp {
    /// `-` arithmetic negation
    Neg,
    /// `!` logical negation
    Not,
    /// `~` bitwise complement
    BitNot,
}

/// `..<` (half-open) vs `..=` (closed) range constructor.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum RangeKind {
    /// `..<` — includes `lo`, excludes `hi`.
    HalfOpen,
    /// `..=` — includes both endpoints.
    Closed,
}

/// `scope(exec)` vs `scope(coherence)` discriminator.
///
/// Two locked kinds:
/// - [`Exec`](Self::Exec) — structured concurrency (`scope(exec) name { ... }`)
///   per `05-concurrency-coherence.md` §2.
/// - [`Coherence`](Self::Coherence) — observational atomicity
///   (`scope(coherence) name { ... }`) per
///   `05-concurrency-coherence.md` §3.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ScopeKind {
    /// `scope(exec) [name] { ... }` — structured concurrency.
    Exec,
    /// `scope(coherence) [name] { ... }` — observational atomicity.
    Coherence,
}

impl ScopeKind {
    /// Lowercase source spelling of this kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::Coherence => "coherence",
        }
    }
}

/// Literal-value forms emitted by the lexer and embedded in [`ExprKind::Literal`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Literal {
    /// Integer literal; numeric value and source-form base.
    Int {
        /// Parsed value, base 10 / 16 / 2 / 8.
        value: u128,
        /// Original base prefix (`Dec`/`Hex`/`Bin`/`Oct`).
        base: IntBase,
    },
    /// Float literal; payload is the raw source text (preserves spelling).
    Float(Symbol),
    /// Plain string literal; payload is escape-resolved content.
    Str(Symbol),
    /// `true` / `false`.
    Bool(bool),
    /// `()` unit literal.
    Unit,
}

/// One segment of an `f"..."` interpolated string: either a literal
/// text run or a `{ ... }` interpolation slot holding a parsed
/// expression.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum FStringPart {
    /// Literal text between slots; escape sequences already decoded.
    Text(Symbol),
    /// A `{ expr }` interpolation slot — any one-line expression.
    Slot(Box<Expr>),
}

/// `{ stmts; trailing }` block, used as both an expression body and the
/// body of `function`, `loop`, `match`-arm, etc.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Block {
    /// Source range covering the surrounding braces.
    pub span: Span,
    /// Statements in source order.
    pub stmts: Vec<Stmt>,
    /// Trailing expression that produces the block's value, if any.
    pub trailing: Option<Box<Expr>>,
}

/// A single `match` arm: `pattern [where guard] => body,`.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct MatchArm {
    /// Source range covering the entire arm.
    pub span: Span,
    /// Pattern matched against the scrutinee.
    pub pat: super::Pat,
    /// Optional `where` guard expression.
    pub guard: Option<Expr>,
    /// Arm body — the value produced when this arm fires.
    pub body: Expr,
}

/// A single field-initialization entry inside a struct literal:
/// `name: value`, `name: take value`, or shorthand `name`.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct StructLitField {
    /// Source range covering the field entry.
    pub span: Span,
    /// Field name.
    pub name: Ident,
    /// Optional call-site mode keyword prefixing the value
    /// (`take` / `mutable` / `init`).
    /// `None` for the bare `name: value` and shorthand `name` forms.
    pub mode: Option<CallMode>,
    /// Value expression. For shorthand `name`, this is a `Path(name)`.
    pub value: Expr,
}

/// Call-site mode prefix for one positional argument.
///
/// Unlike [`super::ParamMode`] (which carries a `Default` variant for
/// the no-prefix declaration form), `CallMode` is only used to record
/// an *explicit* mode keyword. A bare argument (no keyword) is
/// represented as [`CallArg::mode`] = `None`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum CallMode {
    /// `mutable` — caller's binding is borrowed mutably for the call's duration.
    Mutable,
    /// `take` — caller's binding is moved into the callee.
    Take,
    /// `init` — caller's binding is uninitialised on entry, initialised on return.
    Init,
}

impl CallMode {
    /// Source spelling of the keyword.
    pub fn keyword(self) -> &'static str {
        match self {
            CallMode::Mutable => "mutable",
            CallMode::Take => "take",
            CallMode::Init => "init",
        }
    }
}

/// One call argument: a positional expression with optional call-site
/// mode prefix and an optional payload-field name.
///
/// `expressions.md` §521 locks function calls to *positional* arguments;
/// `declarations.md` §252 locks sum-variant payload construction to
/// *named* fields (`Phase.yellow(seconds_remaining: 3)`). The parser
/// cannot distinguish a function call from a variant constructor without
/// type information, so both forms parse into this uniform shape and the
/// typechecker validates that `name` is present iff the callee resolves
/// to a struct-payload variant constructor.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct CallArg {
    /// Source range covering the mode keyword and/or payload-field name
    /// (if any) plus the expression.
    pub span: Span,
    /// Mode keyword that prefixes the argument, if any.
    pub mode: Option<CallMode>,
    /// Payload-field name when the argument was written as `name: expr`
    /// at a variant-constructor call site; `None` for positional arguments.
    pub name: Option<Ident>,
    /// The argument expression itself.
    pub expr: Expr,
}

impl CallArg {
    /// Convenience: build a bare argument (no mode keyword, no name).
    /// `span` is taken from the expression.
    pub fn bare(expr: Expr) -> Self {
        Self {
            span: expr.span,
            mode: None,
            name: None,
            expr,
        }
    }
}
