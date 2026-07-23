//! Typed function signatures.
//!
//! [`FnSig`] is the type-system view of a function
//! declaration: a sequence of positional [`Param`]s (each carrying a
//! mode, name, and type), a return type, and an effect row. The name
//! and visibility of a function are **not** stored here — those live on
//! whatever item table `edda-resolve` produces. `FnSig` is the
//! signature itself, suitable for call-site checking and for the
//! `T-FunCall` / `T-MethodCall` rules from `inference-rules.md §1a.4`.
//!
//! Generic parameters and refinement clauses are not part of `FnSig`:
//! generics need substitution machinery that lands with the
//! spec-instantiation pass, and refinements are `edda-refine`'s
//! responsibility. References to a generic type parameter inside a
//! signature lower to [`TyInterner::error`] (see [`crate::lower_type`])
//! with a cascade `typecheck_error`, so generic-bearing declarations
//! remain visible as malformed signatures until generics land.

use std::fmt;

use edda_intern::{Interner, Symbol};
use edda_syntax::ast;

use crate::effect::{EffectRow, GradedBound};
use crate::ty::{TyId, TyInterner};

/// Parameter mode — the type-system view of `let` (default) / `mutable` /
/// `take` / `init` from `docs/syntax/declarations.md`.
///
/// Mirrors `edda_syntax::ast::ParamMode`; kept in this crate so the
/// type-system layer owns its canonical form. The four variants are
/// the only modes admitted so far.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub enum ParamMode {
    /// No prefix in source — immutable read-only by-reference (the language default).
    Default,
    /// `mutable` — mutable by-reference; caller's binding retains validity.
    Mutable,
    /// `take` — ownership transferred to the callee; caller's binding is consumed.
    Take,
    /// `init` — uninitialised destination the callee writes into; binding becomes valid after the call.
    Init,
}

impl ParamMode {
    /// Convert from the AST representation. Total — every `ast::ParamMode`
    /// variant has a corresponding type-system [`ParamMode`].
    #[inline]
    pub const fn from_ast(mode: ast::ParamMode) -> Self {
        match mode {
            ast::ParamMode::Default => ParamMode::Default,
            ast::ParamMode::Mutable => ParamMode::Mutable,
            ast::ParamMode::Take => ParamMode::Take,
            ast::ParamMode::Init => ParamMode::Init,
        }
    }

    /// The keyword that prefixes this mode in source. `Default` returns
    /// an empty string — the language default has no source spelling.
    pub const fn keyword(self) -> &'static str {
        match self {
            ParamMode::Default => "",
            ParamMode::Mutable => "mutable",
            ParamMode::Take => "take",
            ParamMode::Init => "init",
        }
    }
}

/// Return-position borrow mode — the type-system view of `let` /
/// `mutable` on a function's return type, or `ByValue` (no prefix).
///
/// Mirrors `edda_syntax::ast::ReturnMode`. A non-`ByValue` mode means
/// the function returns a borrow whose region is tied to a by-reference
/// parameter; [`crate::return_mode`] enforces that binding so the borrow
/// cannot outlive its argument.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub enum ReturnMode {
    /// No prefix in source — by-value return (the language default).
    ByValue,
    /// `let` — immutable borrow tied to a by-reference receiver parameter.
    Let,
    /// `mutable` — mutable borrow tied to a `mutable` receiver parameter.
    Mutable,
}

impl ReturnMode {
    /// Convert from the AST representation. Total — every
    /// `ast::ReturnMode` variant has a corresponding type-system one.
    #[inline]
    pub const fn from_ast(mode: ast::ReturnMode) -> Self {
        match mode {
            ast::ReturnMode::ByValue => ReturnMode::ByValue,
            ast::ReturnMode::Let => ReturnMode::Let,
            ast::ReturnMode::Mutable => ReturnMode::Mutable,
        }
    }

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

/// One positional parameter on an [`FnSig`].
///
/// Parameters are matched positionally at call sites; the `name` is
/// retained so diagnostics can refer to it (`"argument `path` does not
/// satisfy the callee's `path: String` precondition"`). There are no
/// keyword arguments — repositioning by name is not admitted.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Param {
    /// Source range covering the parameter declaration.
    pub span: edda_span::Span,
    /// Parameter name — interned [`Symbol`] for the identifier.
    pub name: Symbol,
    /// Mode prefix (`let` / `mutable` / `take` / `init`).
    pub mode: ParamMode,
    /// Lowered parameter type.
    pub ty: TyId,
}

/// The type-system signature of a function.
///
/// A function's identity at the type-system layer: positional
/// [`Param`]s, a [`TyId`] return type, an [`EffectRow`], and the
/// graded-bound entries from `02-modes-effects-refinements.md` §5.
/// Call-site checking matches arguments against `params` positionally,
/// unions the callee's `effects` into the caller's row per `T-FunCall`
/// (`inference-rules.md §1a.4`), and discharges each callee
/// [`GradedBound`] against the caller's matching bound.
///
/// Equality is structural — two `FnSig`s are equal iff they agree on
/// every param (name, mode, type), the return type, the row, and the
/// graded bounds.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct FnSig {
    /// Positional parameters in declaration order.
    pub params: Box<[Param]>,
    /// Return type. Absent return clauses in source (`function f()`)
    /// lower to [`crate::Primitive::Unit`] per
    /// `docs/syntax/declarations.md`, *Function declarations*.
    pub return_ty: TyId,
    /// Return-position borrow mode lifted from
    /// [`ast::FnDecl::return_mode`](edda_syntax::ast::FnDecl). `ByValue`
    /// for an ordinary by-value return; `Let` / `Mutable` when the
    /// source wrote `-> let T` / `-> mutable T`. The return-borrow
    /// region check (`crate::return_mode`) consumes it.
    pub return_mode: ReturnMode,
    /// Effect row from the `with { ... }` clause, or [`EffectRow::empty`]
    /// when the row is absent.
    pub effects: EffectRow,
    /// Graded-bound entries (`alloc(bytes <= N)`, `io(calls <= N)`,
    /// `time(ops <= N)`) extracted from the source row. Empty when the
    /// signature carries no graded entries. See
    /// [`GradedBound`](crate::GradedBound).
    pub graded_bounds: Box<[GradedBound]>,
    /// Refinement-stability marker per
    /// `corpus/edda-codex/language/03-verification.md` §7. `true` when
    /// the source declares `stable function ...`. Lifted from
    /// [`ast::FnDecl::refinement_stable`](edda_syntax::ast::FnDecl).
    /// Consumed by the stability structural check (callee whitelist,
    /// effect-row whitelist, hash-iteration ban).
    pub refinement_stable: bool,
}

impl FnSig {
    /// Number of positional parameters.
    #[inline]
    pub fn arity(&self) -> usize {
        self.params.len()
    }

    /// Returns a [`fmt::Display`] adapter for diagnostic rendering.
    ///
    /// Format: `function(<param>, ...) -> <return> [with {effects}]`.
    /// The `with` clause is elided when the row is empty.
    pub fn display<'a>(
        &'a self,
        interner: &'a Interner,
        ty_interner: &'a TyInterner,
    ) -> FnSigDisplay<'a> {
        FnSigDisplay {
            sig: self,
            interner,
            ty_interner,
        }
    }
}

/// One parameter slot in an [`FnPtrSig`].
///
/// The type-level shape of a function parameter — mode + type only, no
/// name or span. Two `function(...)` types are structurally equal iff
/// every parameter slot agrees on mode and `TyId`; the source spelling
/// `function(x: i32)` and `function(i32)` produce the same type because
/// the `name` is documentation, not part of the type.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct FnPtrParam {
    /// Mode prefix (`let` / `mutable` / `take` / `init`).
    pub mode: ParamMode,
    /// Lowered parameter type.
    pub ty: TyId,
}

/// The structural type of a function pointer.
///
/// Used as the payload of [`crate::TyKind::FnPtr`] — every
/// `function(...) -> T uses {row}` type expression and every
/// reference-to-function expression synthesises to one of these.
///
/// Distinct from [`FnSig`]: this carries no parameter names and no
/// spans, because two `function` types are equal iff their modes,
/// types, return, and row agree. [`FnSig::to_fn_ptr_sig`] performs the
/// projection from a declaration-time signature.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct FnPtrSig {
    /// Positional parameter slots in declaration order.
    pub params: Box<[FnPtrParam]>,
    /// Return type. Absent return clauses lower to [`crate::Primitive::Unit`].
    pub return_ty: TyId,
    /// Effect row from the `with { ... }` clause, or [`EffectRow::empty`].
    pub effects: EffectRow,
}

impl FnPtrSig {
    /// Number of parameter slots.
    #[inline]
    pub fn arity(&self) -> usize {
        self.params.len()
    }

    /// Returns a [`fmt::Display`] adapter for diagnostic rendering.
    ///
    /// Format: `function(<mode> <ty>, ...) -> <return> [with {row}]`.
    /// The `with` clause is elided when the row is empty.
    pub fn display<'a>(
        &'a self,
        interner: &'a Interner,
        ty_interner: &'a TyInterner,
    ) -> FnPtrSigDisplay<'a> {
        FnPtrSigDisplay {
            sig: self,
            interner,
            ty_interner,
        }
    }
}

impl FnSig {
    /// Project this declaration-time signature to its structural
    /// [`FnPtrSig`] form, dropping parameter names and spans.
    ///
    /// The result is what [`crate::TyKind::FnPtr`] carries when a
    /// function-binding path appears in value position (`let h = f`)
    /// or when a `function(...)` type annotation is lowered.
    pub fn to_fn_ptr_sig(&self) -> FnPtrSig {
        FnPtrSig {
            params: self
                .params
                .iter()
                .map(|p| FnPtrParam {
                    mode: p.mode,
                    ty: p.ty,
                })
                .collect(),
            return_ty: self.return_ty,
            effects: self.effects.clone(),
        }
    }
}

/// Display adapter returned by [`FnPtrSig::display`].
pub struct FnPtrSigDisplay<'a> {
    sig: &'a FnPtrSig,
    interner: &'a Interner,
    ty_interner: &'a TyInterner,
}

impl<'a> fmt::Display for FnPtrSigDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("function(")?;
        for (i, p) in self.sig.params.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            if p.mode != ParamMode::Default {
                f.write_str(p.mode.keyword())?;
                f.write_str(" ")?;
            }
            self.ty_interner.display(p.ty).fmt(f)?;
        }
        f.write_str(") -> ")?;
        self.ty_interner.display(self.sig.return_ty).fmt(f)?;
        if !self.sig.effects.is_empty() {
            f.write_str(" with ")?;
            self.sig
                .effects
                .display(self.interner, self.ty_interner)
                .fmt(f)?;
        }
        Ok(())
    }
}

/// Display adapter returned by [`FnSig::display`].
pub struct FnSigDisplay<'a> {
    sig: &'a FnSig,
    interner: &'a Interner,
    ty_interner: &'a TyInterner,
}

impl<'a> fmt::Display for FnSigDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("function(")?;
        for (i, p) in self.sig.params.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            if p.mode != ParamMode::Default {
                f.write_str(p.mode.keyword())?;
                f.write_str(" ")?;
            }
            f.write_str(self.interner.resolve(p.name))?;
            f.write_str(": ")?;
            self.ty_interner.display(p.ty).fmt(f)?;
        }
        f.write_str(") -> ")?;
        self.ty_interner.display(self.sig.return_ty).fmt(f)?;
        if !self.sig.effects.is_empty() {
            f.write_str(" with ")?;
            self.sig.effects.display(self.interner, self.ty_interner).fmt(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{EffectEntry, PureEffect};
    use crate::prim::Primitive;

    #[test]
    fn param_mode_round_trips_from_ast() {
        for (ast, ts) in [
            (ast::ParamMode::Default, ParamMode::Default),
            (ast::ParamMode::Mutable, ParamMode::Mutable),
            (ast::ParamMode::Take, ParamMode::Take),
            (ast::ParamMode::Init, ParamMode::Init),
        ] {
            assert_eq!(ParamMode::from_ast(ast), ts);
        }
    }

    #[test]
    fn keyword_strings_match_source() {
        assert_eq!(ParamMode::Default.keyword(), "");
        assert_eq!(ParamMode::Mutable.keyword(), "mutable");
        assert_eq!(ParamMode::Take.keyword(), "take");
        assert_eq!(ParamMode::Init.keyword(), "init");
    }

    #[test]
    fn arity_counts_params() {
        let ty = TyInterner::new();
        let sig = FnSig {
            params: Box::from([]),
            return_ty: ty.prim(Primitive::Unit),
            effects: EffectRow::empty(),
            return_mode: ReturnMode::ByValue,
            graded_bounds: Box::from([]),
            refinement_stable: false,
        };
        assert_eq!(sig.arity(), 0);
    }

    #[test]
    fn display_renders_signature_with_effects() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let path_sym = interner.intern("path");
        let alloc_sym = interner.intern("allocator");
        let io_err = ty.prim(Primitive::U64);

        let sig = FnSig {
            params: Box::from([
                Param {
                    span: edda_span::Span::DUMMY,
                    name: path_sym,
                    mode: ParamMode::Default,
                    ty: ty.prim(Primitive::String),
                },
                Param {
                    span: edda_span::Span::DUMMY,
                    name: alloc_sym,
                    mode: ParamMode::Default,
                    ty: ty.prim(Primitive::String),
                },
            ]),
            return_ty: ty.prim(Primitive::I64),
            effects: EffectRow::from_entries([
                EffectEntry::Capability(alloc_sym),
                EffectEntry::Pure(PureEffect::Err(io_err)),
            ]),
            return_mode: ReturnMode::ByValue,
            graded_bounds: Box::from([]),
            refinement_stable: false,
        };
        // Capability sort order is by Symbol id (insertion order):
        // path=0, allocator=1, so the displayed row has allocator at index 1.
        let s = sig.display(&interner, &ty).to_string();
        assert_eq!(
            s,
            "function(path: String, allocator: String) -> i64 with {allocator, err: u64}"
        );
    }

    #[test]
    fn display_elides_empty_effects() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let sig = FnSig {
            params: Box::from([Param {
                span: edda_span::Span::DUMMY,
                name: interner.intern("x"),
                mode: ParamMode::Mutable,
                ty: ty.prim(Primitive::I32),
            }]),
            return_ty: ty.prim(Primitive::Unit),
            effects: EffectRow::empty(),
            return_mode: ReturnMode::ByValue,
            graded_bounds: Box::from([]),
            refinement_stable: false,
        };
        assert_eq!(
            sig.display(&interner, &ty).to_string(),
            "function(mutable x: i32) -> ()"
        );
    }

    #[test]
    fn structural_equality() {
        let ty = TyInterner::new();
        let a = FnSig {
            params: Box::from([]),
            return_ty: ty.prim(Primitive::Unit),
            effects: EffectRow::empty(),
            return_mode: ReturnMode::ByValue,
            graded_bounds: Box::from([]),
            refinement_stable: false,
        };
        let b = a.clone();
        assert_eq!(a, b);
        let c = FnSig {
            params: Box::from([]),
            return_ty: ty.prim(Primitive::I32),
            effects: EffectRow::empty(),
            return_mode: ReturnMode::ByValue,
            graded_bounds: Box::from([]),
            refinement_stable: false,
        };
        assert_ne!(a, c);
    }
}
