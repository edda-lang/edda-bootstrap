//! Certificate-minting integration tests for the Z3 backend.
//!
//! Covers the end-to-end pipeline that produces the `(obligation_hash,
//! context_hash)` tuple + Z3 proof witness on a successful unsat, plus the
//! byte-layout round-trip on a `@unverified` certificate. The witness frame
//! is `EDDA-Z3-PROOF-v1` per `docs/codegen/distribution/03-certificate.md`
//! §5; hashes match §3.3 / §4.

mod common;

use std::time::Duration;

use common::{i32_sort, lit_i32, obligation, var};
use edda_refine::{
    CertificateWitness, CmpOp, DischargeOutcome, DischargeRoute, ObligationKind, Predicate, Solver,
    Sort, UnverifiedAnnotation, Z3Backend, Z3_PROOF_HEADER,
};
use edda_span::Span;

#[test]
fn smt_unsat_certificate_carries_edda_z3_proof_witness() {
    // den != 0 from `where den != 0` discharges — and now the SMT
    // certificate should carry the EDDA-Z3-PROOF-v1 framed witness.
    let backend = Z3Backend::new();
    let den = var("den", Sort::Int(i32_sort()));
    let goal = Predicate::cmp(CmpOp::Ne, den.clone(), lit_i32(0));
    let ctx_pred = Predicate::cmp(CmpOp::Ne, den, lit_i32(0));
    let o = obligation(goal, vec![ctx_pred], ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    match outcome {
        DischargeOutcome::Unsat { certificate } => match certificate.witness() {
            CertificateWitness::Smt { witness, .. } => {
                assert!(
                    witness.starts_with(Z3_PROOF_HEADER),
                    "witness must begin with EDDA-Z3-PROOF-v1 header"
                );
            }
            other => panic!("expected Smt witness, got {other:?}"),
        },
        other => panic!("expected Unsat, got {other:?}"),
    }
}

#[test]
fn smt_certificate_hashes_are_populated_from_canonical_form() {
    // The SMT path emits non-zero obligation/context
    // hashes via canonical_predicate + canonical_context. The hashes must
    // be populated (non-zero) on every emitted SMT certificate.
    let backend = Z3Backend::new();
    let den = var("den", Sort::Int(i32_sort()));
    let goal = Predicate::cmp(CmpOp::Ne, den.clone(), lit_i32(0));
    let ctx_pred = Predicate::cmp(CmpOp::Ne, den, lit_i32(0));
    let o = obligation(goal, vec![ctx_pred], ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    let cert = match outcome {
        DischargeOutcome::Unsat { certificate } => certificate,
        other => panic!("expected Unsat, got {other:?}"),
    };
    assert_ne!(
        cert.obligation_hash(),
        &[0u8; 32],
        "obligation_hash unpopulated"
    );
    assert_ne!(cert.context_hash(), &[0u8; 32], "context_hash unpopulated");
}

#[test]
fn unverified_route_serialises_to_byte_layout_per_spec() {
    // End-to-end: route an obligation through @unverified, take the
    // resulting certificate, serialise to bytes, confirm the byte layout
    // matches `distribution/03-certificate.md` §3.4.
    let backend = Z3Backend::new();
    let ann = UnverifiedAnnotation::new("audited", Span::DUMMY).unwrap();
    let den = var("den", Sort::Int(i32_sort()));
    let goal = Predicate::cmp(CmpOp::Ne, den, lit_i32(0));
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero)
        .with_route(DischargeRoute::Unverified(ann));
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    let cert = match outcome {
        DischargeOutcome::Unsat { certificate } => certificate,
        other => panic!("expected Unsat, got {other:?}"),
    };
    let bytes = cert.serialise();
    // Header: 0x01 0x03 (format-version, unverified discriminant).
    assert_eq!(bytes[0], edda_refine::CERTIFICATE_FORMAT_VERSION);
    assert_eq!(bytes[1], 0x03);
    // Then varint(7) = 0x07, then "audited", then 12-byte span.
    assert_eq!(bytes[2], 7);
    assert_eq!(&bytes[3..10], b"audited");
    assert_eq!(bytes.len(), 2 + 1 + 7 + 12);
}
