//! Lowering-pass error variants.
//!
//! The typed-HIR -> MIR lowering pass surfaces these via
//! [`crate::error::MirError::Lowering`]. The family is shaped around the real
//! `edda-types` HIR — the item-level resolution variants (`UnknownAdt` /
//! `UnknownFunction`) now fire only on genuine resolution failures (item
//! resolution is implemented via `adt_map` / `function_map` /
//! `function_externs`), alongside dedicated variants for the
//! genuinely-unsupported HIR shapes the current inference produces.

use edda_intern::Symbol;
use edda_span::Span;

/// Problems detected by the typed-HIR -> MIR lowering pass.
///
/// Each variant carries a [`Span`] borrowed from the originating HIR view so
/// the diagnostic renderer can highlight the offending source. Symbol-based
/// variants name the unresolved identifier so the user can correct it without
/// having to recover from an opaque internal id.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum LoweringError {
    /// A `Local` or `Binding` reference names an identifier that the lowering
    /// pass has not seen a binding for in the active body's scope.
    UnknownBinding {
        /// The unresolved binding name.
        name: Symbol,
        /// Span of the reference.
        span: Span,
    },
    /// A type reference, pattern, or `raise` site names an ADT that the
    /// lowering pass has not seen an `adt` view for in the program.
    UnknownAdt {
        /// The unresolved ADT name.
        name: Symbol,
        /// Span of the reference.
        span: Span,
    },
    /// A `Path` expression names a function that the lowering pass has not
    /// seen a `function` view for in the program.
    UnknownFunction {
        /// The unresolved function name.
        name: Symbol,
        /// Span of the reference.
        span: Span,
    },
    /// A `Call` carries a capability name that the active body did not
    /// declare in its `capabilities` view.
    UnknownCapability {
        /// The unresolved capability name.
        name: Symbol,
        /// Span of the reference.
        span: Span,
    },
    /// A `break` expression appeared outside any loop scope.
    BreakOutsideLoop {
        /// Span of the `break`.
        span: Span,
    },
    /// A `continue` expression appeared outside any loop scope.
    ContinueOutsideLoop {
        /// Span of the `continue`.
        span: Span,
    },
    /// A `?` (try) operator appeared in a function whose effect row does not
    /// allow `?` propagation (no error ADTs in `may_raise`).
    TryOutsideErrorScope {
        /// Span of the `?` operator.
        span: Span,
    },
    /// An HIR variant the lowering body does not yet cover. The
    /// `variant` field carries a `&'static` tag for diagnostic display so
    /// callers can read the failure without resolving symbols.
    UnsupportedHirVariant {
        /// Tag for the offending HIR variant the lowering body does not yet cover.
        variant: &'static str,
        /// Span of the originating HIR node.
        span: Span,
    },
    /// A pattern shape the lowering body does not yet cover.
    UnsupportedPattern {
        /// Tag for the offending pattern shape that has no MIR lowering.
        kind: &'static str,
        /// Span of the originating HIR node.
        span: Span,
    },
    /// An assignment target whose HIR shape does not lower to a single
    /// addressable [`crate::Place`] (field- and index-projected targets now
    /// lower via `resolve_place`; only non-place LHS shapes fail here).
    UnsupportedAssignTarget {
        /// Span of the offending LHS.
        span: Span,
    },
    /// A primitive cast whose source/destination pair has no MIR lowering.
    UnsupportedCast {
        /// Source primitive name.
        from: &'static str,
        /// Destination primitive name.
        to: &'static str,
        /// Span of the `as` expression.
        span: Span,
    },
    /// An `err: T` row entry whose payload type cannot be resolved to an
    /// `AdtId` — fires only when the payload type is not a registered nominal
    /// ADT.
    UnsupportedErrTypeInRow {
        /// Span of the offending function declaration.
        span: Span,
    },
    /// A `yield: T` pure-effect entry. The MIR has no yield lowering yet.
    UnsupportedYieldEffect {
        /// Span of the offending function declaration.
        span: Span,
    },
    /// A path in value position (`let h = f`, `pass(f)`) resolves to an
    /// `extern`-bodied function. Value-position lowering for named
    /// functions (`lower_function_ref_by_path`) only materialises a
    /// forwarding shim for source-bodied (`function_map`-registered)
    /// callees today; an extern callee's implementation declares its
    /// capability slots in DECLARED PARAMETER order (the caps-first
    /// wire contract) while the type-erased `FnPtrSig` a fn-value's type
    /// carries can only encode canonical (Symbol-Ord) order (`FnPtrParam`
    /// has no name to match against).
    /// Threading an extern-bodied fn-value's capabilities through the
    /// existing canonical-order shim machinery would silently reproduce
    /// a handle-swap bug, so this fires instead of miscompiling.
    ExternFnValueUnsupported {
        /// The extern function's source-level name.
        name: Symbol,
        /// Span of the value-position reference.
        span: Span,
    },
    /// A multi-segment path expression. Single-segment path lookup goes
    /// through `ctx.bindings`; multi-segment paths need item-level
    /// resolution that is not yet implemented.
    MultiSegmentPath {
        /// Span of the offending path.
        span: Span,
    },
    /// A catchall for impossible-but-defensive cases hit by the lowering
    /// pass. Carries a short human-readable message describing the failure.
    InternalError {
        /// Free-form description of the internal failure.
        message: String,
        /// Span of the originating construct, when available.
        span: Span,
    },
}

impl LoweringError {
    /// Span of the originating HIR construct, always carried by every variant.
    pub fn span(&self) -> Span {
        match self {
            LoweringError::UnknownBinding { span, .. }
            | LoweringError::UnknownAdt { span, .. }
            | LoweringError::UnknownFunction { span, .. }
            | LoweringError::UnknownCapability { span, .. }
            | LoweringError::BreakOutsideLoop { span }
            | LoweringError::ContinueOutsideLoop { span }
            | LoweringError::TryOutsideErrorScope { span }
            | LoweringError::UnsupportedHirVariant { span, .. }
            | LoweringError::UnsupportedPattern { span, .. }
            | LoweringError::UnsupportedAssignTarget { span }
            | LoweringError::UnsupportedCast { span, .. }
            | LoweringError::UnsupportedErrTypeInRow { span }
            | LoweringError::UnsupportedYieldEffect { span }
            | LoweringError::ExternFnValueUnsupported { span, .. }
            | LoweringError::MultiSegmentPath { span }
            | LoweringError::InternalError { span, .. } => *span,
        }
    }
}

impl std::fmt::Display for LoweringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoweringError::UnknownBinding { name, .. } => write!(
                f,
                "lowering: unknown binding (symbol id {})",
                name.as_u32(),
            ),
            LoweringError::UnknownAdt { name, .. } => write!(
                f,
                "lowering: unknown ADT (symbol id {})",
                name.as_u32(),
            ),
            LoweringError::UnknownFunction { name, .. } => write!(
                f,
                "lowering: unknown function (symbol id {})",
                name.as_u32(),
            ),
            LoweringError::UnknownCapability { name, .. } => write!(
                f,
                "lowering: unknown capability (symbol id {})",
                name.as_u32(),
            ),
            LoweringError::BreakOutsideLoop { .. } => {
                f.write_str("lowering: break expression outside any loop")
            }
            LoweringError::ContinueOutsideLoop { .. } => {
                f.write_str("lowering: continue expression outside any loop")
            }
            LoweringError::TryOutsideErrorScope { .. } => {
                f.write_str("lowering: ? operator used in a function with no error effect")
            }
            LoweringError::UnsupportedHirVariant { variant, .. } => write!(
                f,
                "lowering: HIR variant `{}` is not yet supported",
                variant,
            ),
            LoweringError::UnsupportedPattern { kind, .. } => write!(
                f,
                "lowering: pattern shape `{}` is not yet supported",
                kind,
            ),
            LoweringError::UnsupportedAssignTarget { .. } => {
                f.write_str("lowering: assignment target shape is not yet supported")
            }
            LoweringError::UnsupportedCast { from, to, .. } => write!(
                f,
                "lowering: cast from `{}` to `{}` is not yet supported",
                from, to,
            ),
            LoweringError::UnsupportedErrTypeInRow { .. } => {
                f.write_str("lowering: `err: T` effect entry cannot resolve to an ADT yet")
            }
            LoweringError::UnsupportedYieldEffect { .. } => {
                f.write_str("lowering: `yield` effect is not yet supported in MIR")
            }
            LoweringError::ExternFnValueUnsupported { name, .. } => write!(
                f,
                "lowering: extern-bodied function (symbol id {}) cannot be used as a \
                 first-class value yet",
                name.as_u32(),
            ),
            LoweringError::MultiSegmentPath { .. } => {
                f.write_str("lowering: multi-segment path lookup is not yet supported")
            }
            LoweringError::InternalError { message, .. } => {
                write!(f, "lowering: internal error: {}", message)
            }
        }
    }
}

impl std::error::Error for LoweringError {}
