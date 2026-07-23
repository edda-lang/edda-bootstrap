//! Typed expression nodes (`HirExpr`, `HirExprKind`, `HirBlock`).
//!
//! Each [`HirExpr`] carries the [`TyId`] of the value the expression
//! produces, along with its source [`Span`]. The variant set mirrors
//! `ast::ExprKind` one-to-one — the AST → HIR lowering uses the
//! identical shape so the mapping is mechanical.

use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::{
    BinOp, CaptureMode, EffectRow as AstEffectRow, Ident, Literal, RangeKind, UnOp,
};

use crate::effect::EffectRow;
use crate::sig::ParamMode;
use crate::ty::TyId;

use super::{HirPat, HirPath, HirStmt};

/// A typed HIR expression node.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirExpr {
    /// Source range.
    pub span: Span,
    /// Value type produced by this expression.
    pub ty: TyId,
    /// Variant and payload.
    pub kind: HirExprKind,
}

/// One segment of a typed `f"..."` interpolated string.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum HirFStringPart {
    /// Literal text run between slots (raw source bytes).
    Text(Symbol),
    /// A `{ expr }` slot — a fully-lowered sub-expression.
    Slot(Box<HirExpr>),
}

/// Every expression form the V1.0 surface admits, in typed form.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum HirExprKind {
    /// A literal value (int, float, string, bool, unit).
    Literal(Literal),
    /// `f"...{expr}..."` interpolated string: literal text runs and
    /// typed interpolation slots.
    FString(Vec<HirFStringPart>),
    /// Identifier or dotted-path reference.
    Path(HirPath),
    /// `op lhs rhs` binary operation.
    Binary {
        /// Operator symbol.
        op: BinOp,
        /// Left operand.
        lhs: Box<HirExpr>,
        /// Right operand.
        rhs: Box<HirExpr>,
    },
    /// `op expr` unary prefix operation.
    Unary {
        /// Operator symbol.
        op: UnOp,
        /// Operand.
        expr: Box<HirExpr>,
    },
    /// `callee(args...)` function call.
    Call {
        /// Function being invoked.
        callee: Box<HirExpr>,
        /// Positional arguments in declaration order. Each carries an
        /// optional call-site mode keyword (`mutable` / `take` / `init`)
        /// preserved from the AST so the §4 mode tracker fires.
        args: Box<[HirCallArg]>,
    },
    /// `receiver.name(args...)` method call.
    MethodCall {
        /// The value the method is called on.
        receiver: Box<HirExpr>,
        /// Method name (with its source span retained for diagnostics).
        name: Ident,
        /// Positional arguments. Each carries an optional call-site
        /// mode keyword (`mutable` / `take` / `init`).
        args: Box<[HirCallArg]>,
    },
    /// `receiver.field` field access.
    Field {
        /// The value being projected.
        receiver: Box<HirExpr>,
        /// Field name.
        name: Ident,
    },
    /// `receiver.N` tuple positional-field access. Lowered from
    /// `ast::ExprKind::TupleIndex`. The receiver's type must resolve to
    /// `TyKind::Tuple(_)` during inference; non-tuple receivers diagnose.
    TupleIndex {
        /// The tuple value being projected.
        receiver: Box<HirExpr>,
        /// Zero-based tuple element index.
        index: u32,
    },
    /// `receiver[index]` indexing.
    Index {
        /// The collection being indexed.
        receiver: Box<HirExpr>,
        /// Index expression.
        index: Box<HirExpr>,
    },
    /// `if cond { ... } else { ... }`.
    If {
        /// Branch condition.
        cond: Box<HirExpr>,
        /// `then` block.
        then_block: HirBlock,
        /// Optional `else` branch — another block or chained `if`.
        else_branch: Option<Box<HirExpr>>,
    },
    /// `match scrutinee { arms... }`.
    Match {
        /// Value being matched.
        scrutinee: Box<HirExpr>,
        /// Arms in source order.
        arms: Box<[HirMatchArm]>,
    },
    /// Block expression `{ stmts; trailing }`.
    Block(HirBlock),
    /// `expr as T [mode]` primitive cast. `target_ty` is the lowered
    /// target type; `mode` carries the trailing keyword
    /// (`wrapping` / `saturating` / `checked`) or
    /// [`CastMode::Trap`](edda_syntax::ast::CastMode::Trap) for the bare
    /// form.
    Cast {
        /// Value being cast.
        expr: Box<HirExpr>,
        /// Target type id.
        target_ty: TyId,
        /// Cast mode mirrored from the AST.
        mode: edda_syntax::ast::CastMode,
    },
    /// `lo..<hi`, `lo..=hi`, `..hi`, `lo..`, or `..` — endpoints are
    /// optional to admit slice-subrange forms. When used as a slice
    /// index, `None` endpoints default to `0` (lo) or the slice's
    /// length (hi).
    Range {
        /// Low endpoint, `None` for open-low forms.
        lo: Option<Box<HirExpr>>,
        /// High endpoint, `None` for open-high forms.
        hi: Option<Box<HirExpr>>,
        /// `..<` (HalfOpen) or `..=` (Closed).
        kind: RangeKind,
    },
    /// `(e1, e2, ...)` tuple constructor — minimum 2 elements.
    Tuple(Box<[HirExpr]>),
    /// `[e1, ..., en]` array / slice literal, including the empty form
    /// `[]`. Lowers to `RvalueKind::MakeArray`; the empty form's element
    /// type comes from the expected type at the check site.
    Array(Box<[HirExpr]>),
    /// `Path { field: e, ... }` record / struct literal.
    StructLit {
        /// Type path being constructed.
        path: HirPath,
        /// Field-initialisation list.
        fields: Box<[HirStructLitField]>,
    },
    /// `loop [decreases <expr>] { ... }` unbounded loop expression.
    /// Yields via `break`. An absent `decreases` clause means the loop
    /// admits `effect divergence` in the enclosing function's effect row
    /// per `corpus/edda-codex/language/03-verification.md` §5.
    Loop {
        /// Loop body.
        body: HirBlock,
        /// Optional label for nested break/continue targeting.
        label: Option<Ident>,
        /// Optional `decreases <expr>` measure expression, lowered from
        /// the AST clause. `None` when the loop has no termination
        /// measure; in that case it contributes `Pure(Divergence)` to
        /// the inferred effect row (C3).
        decreases: Option<Box<HirExpr>>,
    },
    /// `for pat in iter { ... }` bounded iteration.
    For {
        /// Binding pattern for each element.
        pat: Box<HirPat>,
        /// Iterable expression.
        iter: Box<HirExpr>,
        /// Loop body.
        body: HirBlock,
        /// Optional loop label.
        label: Option<Ident>,
    },
    /// `expr?` error-effect propagation.
    Try(Box<HirExpr>),
    /// `expr.await` task resolution.
    Await(Box<HirExpr>),
    /// `raise expr` originate an error.
    Raise(Box<HirExpr>),
    /// `panic expr` originate the panic effect.
    Panic(Box<HirExpr>),
    /// `comptime expr` evaluate at compile time.
    Comptime(Box<HirExpr>),
    /// `comptime { ... }` comptime block.
    ComptimeBlock(HirBlock),
    /// `scope(<kind>) [group] { ... }` structured-execution block.
    /// `name` is `None` only for the binder-free legacy form
    /// `scope(<kind>) { ... }`; the spec locks the binder form per
    /// `effects.md` and `05-concurrency-coherence.md`.
    Scope {
        /// Scope kind — exec (concurrency) or coherence (observational
        /// atomicity), preserved from the AST.
        kind: edda_syntax::ast::ScopeKind,
        /// Optional scope binder name (`group` in `scope(exec) group { ... }`).
        name: Option<Ident>,
        /// Scope body.
        body: HirBlock,
    },
    /// `return [expr]`.
    Return(Option<Box<HirExpr>>),
    /// `break [label] [value]`.
    Break {
        /// Optional label of the loop being broken from.
        label: Option<Ident>,
        /// Optional value yielded from the loop.
        value: Option<Box<HirExpr>>,
    },
    /// `continue [label]`.
    Continue {
        /// Optional label of the loop being continued.
        label: Option<Ident>,
    },
    /// `with { ... }` comptime literal of type `EffectRow`. Payload is the
    /// unlowered AST row; per-member semantic resolution (capability
    /// parameters, spread expansion, payload-type lowering) is a later
    /// wave's job. The `HirExpr.ty` carrier remains the error sentinel
    /// until the row-alias evaluation wave lands.
    EffectRow(AstEffectRow),
    /// `handle <effect>: <ty> [as <binder>] -> <recovery> { body }` effect handler.
    /// Discharges the named effect within `body`; evaluates `recovery`
    /// when the effect fires. Only `err: T` handlers are admitted so far.
    /// The optional `binder` names the caught payload so `recovery` can
    /// reference it.
    Handle {
        /// Effect label (must be `err` for now).
        effect: Ident,
        /// Lowered payload type the handler discharges.
        handled_ty: TyId,
        /// Optional payload binder — bound to the caught value inside
        /// `recovery`. `None` when the source elided the `as <binder>`
        /// clause (legacy form).
        binder: Option<Ident>,
        /// Recovery expression — evaluated when the named effect fires;
        /// must have the same value type as `body`.
        recovery: Box<HirExpr>,
        /// Handler body — the named effect is suppressed within this
        /// block.
        body: HirBlock,
    },
    /// `forall <bound> in <iter>: <body>` bounded universal quantifier.
    /// Admissible only in refinement positions (`where` / `requires` /
    /// `ensures`) per V1.0 refinement-fragment widening. Body must
    /// type-check as `bool`. The bound variable is in scope only inside
    /// `body`; its sort is the element sort of `iter`.
    Forall {
        /// Bound variable name.
        bound: Ident,
        /// Iterable expression — range or slice.
        iter: Box<HirExpr>,
        /// Body predicate.
        body: Box<HirExpr>,
    },
    /// `exists <bound> in <iter>: <body>` bounded existential quantifier.
    /// Mirror of [`HirExprKind::Forall`] with existential semantics.
    Exists {
        /// Bound variable name.
        bound: Ident,
        /// Iterable expression — range or slice.
        iter: Box<HirExpr>,
        /// Body predicate.
        body: Box<HirExpr>,
    },
    /// `function(params) -> ret [with {row}] [captures {caps}] { body }`
    /// closure literal. The `HirExpr.ty`
    /// wrapping this variant is the synthesised
    /// [`crate::TyKind::FnPtr`] type built from the lowered signature.
    Closure(Box<HirClosure>),
    /// `<scope-name>.spawn (take a [: T] = init, ...)? { body }`
    /// structured-concurrency task spawn. Mirrors [`edda_syntax::ast::SpawnExpr`]. Unlike
    /// [`HirExprKind::Closure`], the body admits implicit read-only
    /// capture of enclosing bindings — only the explicit `take`-mode
    /// argument list transfers ownership.
    Spawn(Box<HirSpawn>),
    /// Lowering-recovery sentinel. A diagnostic has already been emitted.
    Error,
}

/// Lowered closure-literal payload.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirClosure {
    /// Source range covering `function` through the closing body brace.
    pub span: Span,
    /// Parameter list (names retained for body binding + MIR lowering).
    pub params: Box<[HirClosureParam]>,
    /// Lowered return type.
    pub ret_ty: TyId,
    /// Lowered effect row (`EffectRow::empty()` when the clause is absent).
    pub effects: EffectRow,
    /// Captured outer bindings; a `captures {}` clause lowers to an empty
    /// slice. The clause is mandatory per the locked surface.
    pub captures: Box<[HirCapture]>,
    /// Closure body block.
    pub body: HirBlock,
}

/// One value parameter on a [`HirClosure`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirClosureParam {
    /// Source range covering the parameter.
    pub span: Span,
    /// Parameter name (in scope inside the body).
    pub name: Ident,
    /// Parameter mode (`let` / `mutable` / `take` / `init`).
    pub mode: ParamMode,
    /// Lowered parameter type.
    pub ty: TyId,
}

/// One entry in a [`HirClosure`]'s `captures { ... }` clause.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct HirCapture {
    /// Source range covering the capture entry.
    pub span: Span,
    /// Captured binding name — resolves to an outer binding; in scope
    /// inside the body as the captured copy.
    pub name: Ident,
    /// `let` (read-only reference) or `take` (ownership transfer).
    pub mode: CaptureMode,
}

/// Lowered structured-concurrency spawn payload.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirSpawn {
    /// Source range covering from the scope name through the closing body brace.
    pub span: Span,
    /// Scope binder being spawned into (e.g. `group` in `group.spawn { ... }`).
    pub scope_name: Ident,
    /// Explicit `take`-mode argument list. Empty for the bare-block form.
    pub args: Box<[HirSpawnArg]>,
    /// Spawned task body.
    pub body: HirBlock,
}

/// One entry in a [`HirSpawn`]'s argument list. `init` is type-checked
/// in the *parent* scope (evaluated before the task starts); the
/// resulting value is moved into a fresh binding named `name`, visible
/// only inside the spawn body.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirSpawnArg {
    /// Source range covering the entry from the `take` keyword to the initialiser.
    pub span: Span,
    /// Bound name visible inside the spawn body.
    pub name: Ident,
    /// Lowered type annotation, if the source wrote one; `None` when
    /// inference must derive it from `init`.
    pub ty: Option<TyId>,
    /// Initialiser expression — evaluated in the parent scope and moved into the body.
    pub init: HirExpr,
}

/// `{ stmts; trailing }` block expression — the body shape used by
/// `function`, `if`/`else`, `loop`, `for`, `match`-arm, `comptime`,
/// and `scope`.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirBlock {
    /// Source range covering the surrounding braces.
    pub span: Span,
    /// Value type of the block.
    pub ty: TyId,
    /// Statements in source order.
    pub stmts: Box<[HirStmt]>,
    /// Trailing expression that produces the block's value, if any.
    pub trailing: Option<Box<HirExpr>>,
}

/// A single `match` arm: `pattern [where guard] => body,`.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirMatchArm {
    /// Source range covering the entire arm.
    pub span: Span,
    /// Pattern matched against the scrutinee.
    pub pat: HirPat,
    /// Optional `where` guard expression — must be `bool`-typed.
    pub guard: Option<HirExpr>,
    /// Arm body — the value produced when this arm fires.
    pub body: HirExpr,
}

/// A single field-initialisation entry inside a struct literal:
/// `name: value` (the shorthand `name` desugars to `name: name`
/// during lowering).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirStructLitField {
    /// Source range covering the field entry.
    pub span: Span,
    /// Field name.
    pub name: Ident,
    /// Optional call-site mode keyword on the value (`take` / `mutable`
    /// / `init`), mirroring [`edda_syntax::ast::StructLitField::mode`].
    pub mode: Option<HirCallMode>,
    /// Value expression.
    pub value: HirExpr,
}

/// Call-site mode keyword in HIR. Mirrors
/// [`edda_syntax::ast::CallMode`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum HirCallMode {
    /// `mutable` — caller's binding is borrowed mutably for the call's duration.
    Mutable,
    /// `take` — caller's binding is moved into the callee.
    Take,
    /// `init` — caller's binding is uninitialised on entry, initialised on return.
    Init,
}

impl HirCallMode {
    /// Source spelling of the keyword.
    pub fn keyword(self) -> &'static str {
        match self {
            HirCallMode::Mutable => "mutable",
            HirCallMode::Take => "take",
            HirCallMode::Init => "init",
        }
    }
}

/// One argument in a [`HirExprKind::Call`] or
/// [`HirExprKind::MethodCall`]. Mirrors
/// [`edda_syntax::ast::CallArg`] with a [`HirExpr`] payload, carrying
/// the optional call-site mode keyword and the optional payload-field
/// name (`Phase.yellow(seconds_remaining: 3)`).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirCallArg {
    /// Source range covering the mode keyword and/or payload-field name
    /// (if any) plus the expression.
    pub span: Span,
    /// Mode keyword that prefixes the argument, if any.
    pub mode: Option<HirCallMode>,
    /// Payload-field name when the argument was written as `name: expr`
    /// at a variant-constructor call site; `None` for positional arguments.
    pub name: Option<Ident>,
    /// The argument expression itself.
    pub expr: HirExpr,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prim::Primitive;
    use crate::ty::TyInterner;
    use edda_intern::Interner;
    use edda_syntax::ast::{BinOp, Literal, UnOp};

    fn lit_int(interner: &Interner, ty: &TyInterner, value: u128) -> HirExpr {
        let _ = interner; // not needed for an integer literal
        HirExpr {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::I64),
            kind: HirExprKind::Literal(Literal::Int {
                value,
                base: edda_syntax::IntBase::Dec,
            }),
        }
    }

    #[test]
    fn literal_expr_carries_ty() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let e = lit_int(&interner, &ty, 42);
        assert_eq!(e.ty, ty.prim(Primitive::I64));
        assert!(matches!(
            e.kind,
            HirExprKind::Literal(Literal::Int { value: 42, .. })
        ));
    }

    #[test]
    fn binary_recursion_through_box() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let lhs = lit_int(&interner, &ty, 1);
        let rhs = lit_int(&interner, &ty, 2);
        let plus = HirExpr {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::I64),
            kind: HirExprKind::Binary {
                op: BinOp::Add,
                lhs: Box::new(lhs.clone()),
                rhs: Box::new(rhs.clone()),
            },
        };
        match &plus.kind {
            HirExprKind::Binary { op, lhs: l, rhs: r } => {
                assert_eq!(*op, BinOp::Add);
                assert_eq!(**l, lhs);
                assert_eq!(**r, rhs);
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn block_carries_ty_and_trailing() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let trailing = lit_int(&interner, &ty, 7);
        let block = HirBlock {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::I64),
            stmts: Box::from([]),
            trailing: Some(Box::new(trailing)),
        };
        assert_eq!(block.ty, ty.prim(Primitive::I64));
        assert!(block.trailing.is_some());
        assert!(block.stmts.is_empty());
    }

    #[test]
    fn empty_block_has_unit_ty_by_convention() {
        // The AST → HIR lowering's responsibility, but the data type
        // admits the shape.
        let ty = TyInterner::new();
        let block = HirBlock {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::Unit),
            stmts: Box::from([]),
            trailing: None,
        };
        assert_eq!(block.ty, ty.prim(Primitive::Unit));
        assert!(block.trailing.is_none());
    }

    #[test]
    fn error_variant_round_trips() {
        let ty = TyInterner::new();
        let e = HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::Error,
        };
        let cloned = e.clone();
        assert_eq!(e, cloned);
    }

    #[test]
    fn unary_neg_round_trips() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let inner = lit_int(&interner, &ty, 5);
        let neg = HirExpr {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::I64),
            kind: HirExprKind::Unary {
                op: UnOp::Neg,
                expr: Box::new(inner.clone()),
            },
        };
        match &neg.kind {
            HirExprKind::Unary { op: UnOp::Neg, expr } => assert_eq!(**expr, inner),
            _ => panic!("expected Unary(Neg)"),
        }
    }
}
