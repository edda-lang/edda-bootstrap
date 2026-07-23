//! Certificate-minting pipeline: bundles canonical hashing + witness packaging.
//!
//! Every [`ProofCertificate`] the discharge layer emits carries the
//! `(obligation_hash, context_hash)` tuple the v1.0 verifier consumes per
//! `distribution/03-certificate.md` §4. This module centralises the
//! hash-then-package step so [`crate::z3_backend`] does not have to depend on
//! [`crate::canonical`] and [`crate::certificate`] directly — keeping
//! `z3_backend`'s fan-out focused on the Z3 plumbing.
//!
//! Each helper takes the [`Obligation`] (so it can compute the canonical
//! hashes itself) plus whichever witness-specific inputs the discharge path
//! has on hand, and returns a fully-populated [`ProofCertificate`].

use crate::canonical::{context_hash, obligation_hash};
use crate::certificate::{encode_z3_witness, ProofCertificate};
use crate::obligation::Obligation;
use edda_span::Span;
use smol_str::SmolStr;

/// Mint an SMT certificate from a successful Z3 unsat outcome.
///
/// Wraps the Z3 proof S-expression in the `EDDA-Z3-PROOF-v1` frame via
/// [`encode_z3_witness`], constructs the [`ProofCertificate`] through
/// [`ProofCertificate::smt`], and stamps the canonical
/// `(obligation_hash, context_hash)` tuple onto it.
pub(crate) fn mint_smt_certificate(
    obligation: &Obligation,
    solver_version: &'static str,
    proof_sexpr: &str,
) -> ProofCertificate {
    let oh = obligation_hash(&obligation.goal);
    let ch = context_hash(&obligation.context);
    // v0.1 capture-only: we don't track the unsat-core via assert_and_track,
    // so the framed witness carries an empty core list. v1.0's verifier reads
    // the framed proof and (per `03-certificate.md` §6) reconstructs the
    // proof tree without requiring the core list.
    let witness = encode_z3_witness(proof_sexpr, &[]);
    ProofCertificate::smt(solver_version, witness).with_hashes(oh, ch)
}

/// Mint a placeholder SMT certificate when Z3 returned no proof object.
///
/// Older Z3 builds (and certain non-proof tactics) can yield `Solver::get_proof
/// () == None` even with proof generation enabled. v0.1 is capture-only and
/// the verifier never reads this blob, so the discharge still succeeds; we
/// emit a placeholder certificate carrying the canonical hashes so the
/// downstream verifier slot stays populated.
pub(crate) fn mint_placeholder_certificate(obligation: &Obligation) -> ProofCertificate {
    let oh = obligation_hash(&obligation.goal);
    let ch = context_hash(&obligation.context);
    ProofCertificate::placeholder().with_hashes(oh, ch)
}

/// Mint an `@unverified` certificate for a function-level annotation route.
///
/// The discharge layer never invokes Z3 on an `@unverified` obligation per
/// `refinement-decidability.md` §9. The returned certificate carries the
/// annotation reason + function-site span and the canonical hashes.
pub(crate) fn mint_unverified_certificate(
    obligation: &Obligation,
    reason: SmolStr,
    function_site: Span,
) -> ProofCertificate {
    let oh = obligation_hash(&obligation.goal);
    let ch = context_hash(&obligation.context);
    ProofCertificate::unverified(reason, function_site).with_hashes(oh, ch)
}

/// Mint a `@trust` certificate for a per-site annotation route.
///
/// The discharge layer never invokes Z3 on a `@trust` obligation per
/// `refinement-decidability.md` §9. The returned certificate carries the
/// annotation reason + obligation-site span and the canonical hashes.
pub(crate) fn mint_trust_certificate(
    obligation: &Obligation,
    reason: SmolStr,
    obligation_site: Span,
) -> ProofCertificate {
    let oh = obligation_hash(&obligation.goal);
    let ch = context_hash(&obligation.context);
    ProofCertificate::trust(reason, obligation_site).with_hashes(oh, ch)
}
