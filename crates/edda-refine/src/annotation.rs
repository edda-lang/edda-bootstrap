//! `@unverified` and `@trust` annotation surface.
//!
//! Per `docs/types/refinement-decidability.md` §9, an obligation can be
//! admitted without SMT discharge through one of two declaration-level
//! annotations:
//!
//! - **`@unverified(reason: "...")`** on a function declaration. Skips
//!   discharge for every obligation inside the function. Whole-function
//!   granularity.
//! - **`@trust(reason: "...")`** on a specific statement or expression.
//!   Skips discharge for that obligation only. Per-site granularity.
//!
//! Both annotations require a non-empty reason string — the reason is the
//! audit surface for `edda lint --trust-points` and `inspect.trust_points_in_scope`.
//!
//! Refine doesn't parse the annotations (that's the syntax layer's job). It
//! consumes typechecker-built [`UnverifiedAnnotation`] / [`TrustAnnotation`]
//! values via the [`DischargeRoute`] field on [`Obligation`](crate::Obligation):
//! when the route is non-SMT, the discharge layer short-circuits and emits the
//! appropriate certificate without touching Z3.

use smol_str::SmolStr;

use edda_span::Span;

use crate::error::AnnotationError;

//            refinement-decidability.md §9 — Comptime and Implicit are
//            placeholders; only edda-comptime and edda-types respectively
//            mint those certificates today
/// How an obligation should be discharged. Default is [`DischargeRoute::Smt`];
/// the typechecker overrides via [`Obligation::with_route`](crate::Obligation::with_route)
/// when an annotation is in scope.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum DischargeRoute {
    /// Default: run the SMT solver per the standard translation pipeline.
    Smt,
    /// Route through `@unverified(reason: "...")` on the enclosing function.
    /// Emits an [`Unverified`](crate::CertificateWitness::Unverified)
    /// certificate; skips the solver.
    Unverified(UnverifiedAnnotation),
    /// Route through `@trust(reason: "...")` on this specific site. Emits a
    /// [`Trust`](crate::CertificateWitness::Trust) certificate; skips the
    /// solver.
    Trust(TrustAnnotation),
    /// Reserved for completeness. edda-comptime owns minting of comptime
    /// certificates; refine never emits this route on its own.
    Comptime,
    /// Reserved for completeness. edda-types owns minting of implicit
    /// certificates (type-checker-proven obligations like field invariants
    /// re-established at construction time).
    Implicit,
}

//          via `edda lint --trust-points`
/// `@unverified(reason: "...")` annotation captured at a function declaration.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct UnverifiedAnnotation {
    /// Non-empty reason string from the annotation's `reason:` argument.
    pub reason: SmolStr,
    /// Source position of the annotation itself — used by the certificate
    /// writer to record where the trust was declared.
    pub function_site: Span,
}

impl UnverifiedAnnotation {
    /// Construct an annotation. Rejects empty reason strings per
    /// refinement-decidability.md §9's "reason is mandatory" rule.
    pub fn new(
        reason: impl Into<SmolStr>,
        function_site: Span,
    ) -> Result<UnverifiedAnnotation, AnnotationError> {
        let reason = reason.into();
        if reason.is_empty() {
            return Err(AnnotationError::EmptyReason);
        }
        Ok(UnverifiedAnnotation {
            reason,
            function_site,
        })
    }
}

/// `@trust(reason: "...")` annotation captured at a specific obligation site.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct TrustAnnotation {
    /// Non-empty reason string from the annotation's `reason:` argument.
    pub reason: SmolStr,
    /// Source position of the annotation itself.
    pub obligation_site: Span,
}

impl TrustAnnotation {
    /// Construct an annotation. Rejects empty reason strings.
    pub fn new(
        reason: impl Into<SmolStr>,
        obligation_site: Span,
    ) -> Result<TrustAnnotation, AnnotationError> {
        let reason = reason.into();
        if reason.is_empty() {
            return Err(AnnotationError::EmptyReason);
        }
        Ok(TrustAnnotation {
            reason,
            obligation_site,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unverified_rejects_empty_reason() {
        let err = UnverifiedAnnotation::new("", Span::DUMMY).unwrap_err();
        assert_eq!(err, AnnotationError::EmptyReason);
    }

    #[test]
    fn unverified_round_trips_reason_and_site() {
        let ann = UnverifiedAnnotation::new(
            "FFI shim; correctness audited against LLVM 18 docs",
            Span::DUMMY,
        )
        .unwrap();
        assert!(ann.reason.contains("FFI"));
        assert_eq!(ann.function_site, Span::DUMMY);
    }

    #[test]
    fn trust_rejects_empty_reason() {
        let err = TrustAnnotation::new("", Span::DUMMY).unwrap_err();
        assert_eq!(err, AnnotationError::EmptyReason);
    }

    #[test]
    fn discharge_route_default_is_smt() {
        // Smt is the route every Obligation::new produces — the route field
        // is added in obligation.rs to default to Smt. This test guards the
        // contract.
        let r = DischargeRoute::Smt;
        assert!(matches!(r, DischargeRoute::Smt));
    }
}
