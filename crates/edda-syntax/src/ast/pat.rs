//! Pattern AST — the left-hand side of `let`, the head of `match` arms,
//! and the binding form for `for` loops.

use edda_span::Span;

use super::{Expr, Ident, Literal, Path, RangeKind};

/// A pattern node.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Pat {
    /// Source range covering the pattern.
    pub span: Span,
    /// Variant and payload.
    pub kind: PatKind,
}

/// Every pattern form admitted by `match`, `let`, and `for`.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum PatKind {
    /// `_` — discards the value.
    Wildcard,
    /// `name` — binds the value to a fresh local.
    Binding(Ident),
    /// `42`, `"hello"`, `true` — matches by equality.
    Literal(Literal),
    /// `(p1, p2, ...)` — tuple destructuring.
    Tuple(Vec<Pat>),
    /// `Path.variant [payload]` — sum-variant pattern.
    Variant {
        /// Qualified variant name.
        path: Path,
        /// Payload shape (none / positional / named).
        payload: VariantPatPayload,
    },
    /// `Path { field, field: pat, .. }` — struct destructuring.
    Struct {
        /// Type path being destructured.
        path: Path,
        /// Named-field patterns.
        fields: Vec<StructPatField>,
        /// `true` if the pattern ended with `..` to ignore extras.
        rest: bool,
    },
    /// `pat where cond` — pattern with refinement guard.
    Guard {
        /// Inner pattern.
        pat: Box<Pat>,
        /// Boolean guard expression.
        cond: Expr,
    },
    /// `lo..<hi` / `lo..=hi` — closed-interval literal range pattern
    /// (§8). `lo` and `hi` are literal constants of an ordered primitive
    /// type (an integer width or `f32`/`f64`); no name is bound.
    Range {
        /// Inclusive lower bound literal.
        lo: Literal,
        /// Upper bound literal — exclusive for `HalfOpen`, inclusive for
        /// `Closed`.
        hi: Literal,
        /// `..<` (half-open) vs `..=` (closed) discriminator.
        kind: RangeKind,
    },
    /// `name @ subpattern` (§8) — binds the whole matched value to `name`
    /// and matches its shape against `inner`.
    AtBinding {
        /// The name bound to the whole matched value.
        name: Ident,
        /// Sub-pattern the value's shape is matched against.
        inner: Box<Pat>,
    },
    /// `[p, ..]` / `[head, ..tail]` / `[..init, last]` / `[]` (§8) —
    /// slice destructuring with at most one rest binding.
    Slice {
        /// Patterns before the rest binding (or all elements when
        /// `rest` is `None`).
        prefix: Vec<Pat>,
        /// The single `..` rest element, if present: `None` = no rest;
        /// `Some(None)` = bare `..`; `Some(Some(name))` = `..name`
        /// binding the remaining elements as a sub-slice.
        rest: Option<Option<Ident>>,
        /// Patterns after the rest binding (empty when `rest` is
        /// `None`).
        suffix: Vec<Pat>,
    },
    /// Parser-recovery sentinel. A diagnostic has already been emitted.
    Error,
}

/// Payload of a [`PatKind::Variant`]: unit / positional / named.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum VariantPatPayload {
    /// No payload — `Color.red`.
    None,
    /// Tuple payload — `Json.array(items)` (positional).
    Tuple(Vec<Pat>),
    /// Struct payload — `Event.click { x, y }` (named).
    Struct(Vec<StructPatField>),
}

/// A field pattern inside a struct or struct-variant pattern.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct StructPatField {
    /// Source range covering the field entry.
    pub span: Span,
    /// Field name being matched.
    pub name: Ident,
    /// Sub-pattern. For shorthand `name`, this is `PatKind::Binding(name)`.
    pub pat: Pat,
}
