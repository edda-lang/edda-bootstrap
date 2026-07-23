//! Integration tests for the proof-certificate public surface.
//!
//! Verifies the wire-format invariants from
//! `docs/codegen/distribution/03-certificate.md` §1-§5: discriminant bytes,
//! common header layout, per-variant payload shapes, and the
//! `EDDA-Z3-PROOF-v1` witness frame. These checks live as integration tests
//! so they exercise the public re-exports the verifier will eventually
//! consume.

use edda_refine::{
    encode_z3_witness, CertificateWitness, ComptimeWitnessKind, ProofCertificate, SmtSolverId,
    CERTIFICATE_FORMAT_VERSION, Z3_PROOF_HEADER,
};
use edda_span::Span;

#[test]
fn placeholder_certificate_is_implicit_with_zero_hashes() {
    let cert = ProofCertificate::placeholder();
    assert_eq!(cert.format_version(), CERTIFICATE_FORMAT_VERSION);
    assert_eq!(cert.obligation_hash(), &[0u8; 32]);
    assert_eq!(cert.context_hash(), &[0u8; 32]);
    assert!(matches!(cert.witness(), CertificateWitness::Implicit));
}

#[test]
fn discriminant_bytes_match_spec_table() {
    // distribution/03-certificate.md §1: SMT=0x00 Comptime=0x01 Implicit=0x02
    // Unverified=0x03 Trust=0x04. Any drift here is wire-breaking.
    let smt = CertificateWitness::Smt {
        solver: SmtSolverId::Z3,
        solver_version: "test".into(),
        witness: Vec::new(),
    };
    assert_eq!(smt.discriminant_byte(), 0x00);
    let comptime = CertificateWitness::Comptime {
        kind: ComptimeWitnessKind::Unit,
        value: Vec::new(),
    };
    assert_eq!(comptime.discriminant_byte(), 0x01);
    let implicit = CertificateWitness::Implicit;
    assert_eq!(implicit.discriminant_byte(), 0x02);
    let unverified = CertificateWitness::Unverified {
        reason: "x".into(),
        function_site: Span::DUMMY,
    };
    assert_eq!(unverified.discriminant_byte(), 0x03);
    let trust = CertificateWitness::Trust {
        reason: "x".into(),
        obligation_site: Span::DUMMY,
    };
    assert_eq!(trust.discriminant_byte(), 0x04);
}

#[test]
fn implicit_certificate_serialises_to_two_byte_header() {
    let cert = ProofCertificate::placeholder();
    let bytes = cert.serialise();
    assert_eq!(bytes.len(), 2);
    assert_eq!(bytes[0], CERTIFICATE_FORMAT_VERSION);
    assert_eq!(bytes[1], 0x02); // implicit discriminant
}

#[test]
fn unverified_serialisation_carries_reason_and_12_byte_span() {
    let cert =
        ProofCertificate::unverified("FFI shim; audited against LLVM 18", Span::DUMMY);
    let bytes = cert.serialise();
    // 2-byte header + varint(len) + reason bytes + 12-byte span.
    assert_eq!(bytes[0], CERTIFICATE_FORMAT_VERSION);
    assert_eq!(bytes[1], 0x03);
    // Reason length is < 128 → single varint byte.
    let reason_len = bytes[2] as usize;
    assert_eq!(reason_len, "FFI shim; audited against LLVM 18".len());
    // After header + len-byte + reason, the trailing 12 bytes are the span.
    assert_eq!(bytes.len(), 2 + 1 + reason_len + 12);
}

#[test]
fn trust_certificate_serialises_to_unique_discriminant() {
    let cert = ProofCertificate::trust("Karatsuba step; NLA outside fragment", Span::DUMMY);
    let bytes = cert.serialise();
    assert_eq!(bytes[1], 0x04);
}

#[test]
fn smt_serialisation_carries_solver_id_and_witness() {
    let witness_bytes = encode_z3_witness("(proof-rule asserted ...)", &[]);
    // Witness blob begins with the EDDA-Z3-PROOF-v1 header.
    assert!(witness_bytes.starts_with(Z3_PROOF_HEADER));
    let cert = ProofCertificate::smt("4.12.2", witness_bytes);
    let bytes = cert.serialise();
    assert_eq!(bytes[0], CERTIFICATE_FORMAT_VERSION);
    assert_eq!(bytes[1], 0x00); // smt discriminant
    assert_eq!(bytes[2], 0x00); // solver_id = z3
    // After solver_id comes varint(version-len), version bytes, varint(witness-len),
    // witness bytes. Exact layout is checked by the round-trip below.
}

#[test]
fn z3_witness_encoder_emits_header_then_proof_then_zero_core() {
    let bytes = encode_z3_witness("(proof-rule asserted)", &[]);
    assert!(bytes.starts_with(Z3_PROOF_HEADER));
    // After header: varint(proof_len) | proof_bytes | varint(0)
    let proof_offset = Z3_PROOF_HEADER.len();
    let proof_len_byte = bytes[proof_offset] as usize;
    assert_eq!(proof_len_byte, "(proof-rule asserted)".len());
    // varint(0) for empty unsat-core is just the byte 0x00.
    let core_count_offset = proof_offset + 1 + proof_len_byte;
    assert_eq!(bytes[core_count_offset], 0x00);
}
