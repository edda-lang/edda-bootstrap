//! Statement AST — the units that compose a [`Block`]'s body.
//!
//! Edda statements are limited: bindings (`let`/`var`), assignments
//! (`=` and compound forms), and expression statements. Items at
//! statement position (nested functions) are not admitted by the
//! current surface; that variant is therefore absent from [`StmtKind`].

use edda_span::Span;

use super::{Attribute, Expr, Pat, Type};

/// A single statement inside a [`super::Block`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Stmt {
    /// Source range covering the statement.
    pub span: Span,
    /// Leading `@name(args)` attributes (e.g. site-level `@trust` /
    /// `@unverified`). Empty when the statement carries none.
    pub attributes: Vec<Attribute>,
    /// Variant and payload.
    pub kind: StmtKind,
}

/// Every statement form the locked surface admits.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum StmtKind {
    /// `let pat [: T] = init`, `var pat [: T] = init`, or `uninit pat: T`.
    Let {
        /// `let` (immutable), `var` (mutable), or `uninit` (uninitialised slot).
        mutability: BindingMode,
        /// Binding pattern.
        pat: Pat,
        /// Optional explicit type annotation (required for `uninit`).
        ty: Option<Type>,
        /// Initializer expression. `None` iff `mutability == BindingMode::Uninit`.
        init: Option<Expr>,
    },
    /// `target op rhs` assignment statement.
    Assign {
        /// Place expression being assigned to (lvalue).
        target: Expr,
        /// Assignment operator (`=`, `+=`, ...).
        op: AssignOp,
        /// Right-hand-side value.
        rhs: Expr,
    },
    /// A bare expression used for its effect (or as the block's trailer).
    Expr(Expr),
}

/// Binding form — `let` (immutable, initialised), `var` (mutable, initialised), or `uninit` (uninitialised slot).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum BindingMode {
    /// `let` — immutable binding; initialiser required.
    Immutable,
    /// `var` — mutable binding; initialiser required.
    Mutable,
    /// `uninit` — uninitialised slot; type required, initialiser forbidden. Filled by an `init`-mode call before first read.
    Uninit,
}

/// Assignment operator forms. Plain `=` and the compound forms.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum AssignOp {
    /// `=`
    Plain,
    /// `+=`
    Add,
    /// `-=`
    Sub,
    /// `*=`
    Mul,
    /// `/=`
    Div,
    /// `%=`
    Mod,
    /// `&=`
    BitAnd,
    /// `|=`
    BitOr,
    /// `^=`
    BitXor,
    /// `<<=`
    Shl,
    /// `>>=`
    Shr,
}
