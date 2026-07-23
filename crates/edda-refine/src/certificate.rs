//! Proof certificate format.
//!
//! Per `docs/codegen/distribution/03-certificate.md`. v0.1 captures
//! certificates *write-only* — the verifier (per §6) ships at v1.0 and
//! consumes whatever this module writes. So this module owns the *minting* side
//! of the contract: every successful [`Z3Backend`](crate::Z3Backend) call
//! produces a [`ProofCertificate`], and every annotation-routed discharge
//! produces an [`UnverifiedCertificate`](CertificateWitness::Unverified) or
//! [`TrustCertificate`](CertificateWitness::Trust) cert.
//!
//! # Byte format
//!
//! Every certificate starts with a 2-byte common header (`format_version`,
//! `discriminant`) followed by a per-type payload. The full byte layout is
//! locked in `03-certificate.md` §2 / §3:
//!
//! ```text
//! cert_header:
//!   format_version: u8     // 0x01 at v0.1
//!   discriminant:   u8     // 0x00=SMT 0x01=Comptime 0x02=Implicit 0x03=Unverified 0x04=Trust
//! payload:
//!   SMT:        solver_id u8 | solver_version varint+bytes | witness varint+bytes
//!   Comptime:   witness_kind u8 | witness_value varint+bytes
//!   Implicit:   (empty)
//!   Unverified: reason varint+bytes | function_site (file_id u32_le, lo u32_le, hi u32_le)
//!   Trust:      reason varint+bytes | obligation_site (file_id u32_le, lo u32_le, hi u32_le)
//! ```
//!
//! Varints are unsigned LEB128. Multi-byte integers in the source-position
//! triple are little-endian unsigned u32.
//!
//! # Scope cuts
//!
//! - **Comptime certificates** — declared but not produced by refine. The
//!   typechecker / edda-comptime mints them at v0.1.
//! - **Implicit certificates** — declared but not produced by refine.
//!   edda-types mints them when an obligation discharges purely through type
//!   checking (e.g. field invariant re-established at construction).

use std::fmt;

use smol_str::SmolStr;

use edda_span::Span;

use crate::wire::encode_varint;

//            certificate; bumping requires coordinated v1.0 verifier update
//            per distribution/03-certificate.md §2
/// Format version stamped into the 2-byte common header. Bumps on
/// per-certificate-layout changes per
/// `distribution/03-certificate.md` §2.
pub const CERTIFICATE_FORMAT_VERSION: u8 = 0x01;

//            distribution/03-certificate.md §5; bumping to v2 requires v1.0
//            verifier coordination
/// Header tag of the Z3 unsat-core witness format used by SMT certificates.
/// `distribution/03-certificate.md` §5: 17 bytes ASCII including the trailing
/// `\n`. v2 of the witness format bumps to `EDDA-Z3-PROOF-v2\n`.
pub const Z3_PROOF_HEADER: &[u8] = b"EDDA-Z3-PROOF-v1\n";

//            distribution/03-certificate.md §1 exactly; reordering / renaming
//            is a wire-breaking change
/// SMT solver identifier byte (`distribution/03-certificate.md` §3.1). Z3 is
/// the only admitted value at v0.1; CVC5 / Yices reserved for v1.x.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum SmtSolverId {
    /// Z3.
    Z3 = 0x00,
    /// CVC5 — reserved for v1.x.
    Cvc5 = 0x01,
    /// Yices — reserved for v1.x.
    Yices = 0x02,
}

//            this module declares the catalogue but only refine-minted certs use it
/// Witness-value kind byte for comptime certificates
/// (`distribution/03-certificate.md` §3.2). This module declares the surface;
/// edda-comptime populates witness values when it mints certs.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum ComptimeWitnessKind {
    /// `bool` — 1-byte witness.
    Bool = 0x00,
    /// Integer (i8..i128 / u8..u128 / isize / usize) — little-endian per type.
    Int = 0x01,
    /// `f32` / `f64` — IEEE-754 little-endian.
    Float = 0x02,
    /// `Type` — string-table index into the proofs blob's string table.
    Type = 0x03,
    /// Tuple — recursive count + values.
    Tuple = 0x04,
    /// Record — field-count + (field-id, value) pairs.
    Record = 0x05,
    /// Sum variant — variant-id + payload value.
    SumVariant = 0x06,
    /// `()` — empty payload.
    Unit = 0xFF,
}

//            discriminant byte assigned by `kind_byte()` is wire-bytes
//          on-disk format
/// Per-discharge-method certificate witness body.
#[derive(Clone, Debug)]
pub enum CertificateWitness {
    /// SMT-discharged. Solver-specific proof witness (Z3 unsat-core at v0.1).
    Smt {
        /// Solver identifier per `SmtSolverId`.
        solver: SmtSolverId,
        /// Solver version string (e.g., `"4.12.2"`). Captured for audit;
        /// verification dispatch is on `solver` only.
        solver_version: SmolStr,
        /// Witness bytes. For Z3, this is an EDDA-Z3-PROOF-v1 framed blob:
        /// header + proof S-expression + unsat-core S-expressions, per
        /// `03-certificate.md` §5.
        witness: Vec<u8>,
    },
    /// Discharged at comptime. This module declares the variant; minting is
    /// edda-comptime's territory.
    Comptime {
        /// Kind tag for the witness payload.
        kind: ComptimeWitnessKind,
        /// Witness value bytes per the kind's encoding rule.
        value: Vec<u8>,
    },
    /// Type-system-implicit discharge — no payload. This module declares the
    /// variant; minting is edda-types' territory.
    Implicit,
    /// `@unverified(reason: "...")` admitted the obligation.
    Unverified {
        /// Reason string from the annotation.
        reason: SmolStr,
        /// Source position of the `@unverified` annotation on the enclosing
        /// function declaration.
        function_site: Span,
    },
    /// `@trust(reason: "...")` admitted the obligation.
    Trust {
        /// Reason string from the annotation.
        reason: SmolStr,
        /// Source position of the `@trust` annotation on the obligation site.
        obligation_site: Span,
    },
}

impl CertificateWitness {
    //            discriminant table — wire-breaking if reordered
    /// Discriminant byte used as the second byte of the common header.
    pub const fn discriminant_byte(&self) -> u8 {
        match self {
            CertificateWitness::Smt { .. } => 0x00,
            CertificateWitness::Comptime { .. } => 0x01,
            CertificateWitness::Implicit => 0x02,
            CertificateWitness::Unverified { .. } => 0x03,
            CertificateWitness::Trust { .. } => 0x04,
        }
    }
}

//            distribution/03-certificate.md §2-§3; field additions or
//            reorderings break v1.0 verifier forward compatibility
//            predicate / context per distribution/03-certificate.md §4
//            (populated by certificate_mint helpers via with_hashes)
//          (obligation_hash, context_hash) per distribution.md §3
/// Proof certificate. Carries the hash slots that the v1.0 verifier uses to
/// match certificates against obligations, plus the per-method witness body.
#[derive(Clone, Debug)]
pub struct ProofCertificate {
    /// Format version stamped into the 2-byte common header per
    /// `distribution/03-certificate.md` §2.
    pub(crate) format_version: u8,
    /// 32-byte BLAKE3 of the obligation's predicate canonical form.
    /// Constructors initialise this to zero; the
    /// [`certificate_mint`](crate::certificate_mint) helpers overwrite via
    /// [`ProofCertificate::with_hashes`] using
    /// [`canonical::obligation_hash`](crate::obligation_hash).
    pub(crate) obligation_hash: [u8; 32],
    /// 32-byte BLAKE3 of the discharge context canonical form. Constructors
    /// initialise this to zero; populated through
    /// [`ProofCertificate::with_hashes`] using
    /// [`canonical::context_hash`](crate::context_hash).
    pub(crate) context_hash: [u8; 32],
    /// Witness payload — selects discharge method via [`discriminant_byte`].
    ///
    /// [`discriminant_byte`]: CertificateWitness::discriminant_byte
    pub(crate) witness: CertificateWitness,
}

impl ProofCertificate {
    /// Format version byte stamped into the 2-byte common header.
    pub const fn format_version(&self) -> u8 {
        self.format_version
    }

    /// BLAKE3 of the obligation's predicate canonical form.
    pub const fn obligation_hash(&self) -> &[u8; 32] {
        &self.obligation_hash
    }

    /// BLAKE3 of the discharge context canonical form.
    pub const fn context_hash(&self) -> &[u8; 32] {
        &self.context_hash
    }

    /// Per-discharge-method witness payload.
    pub const fn witness(&self) -> &CertificateWitness {
        &self.witness
    }

    /// Builder-style: overwrite the certificate's `obligation_hash` and
    /// `context_hash` slots. Used by [`Z3Backend`](crate::Z3Backend) (and any
    /// future producer) to populate the cache key per
    /// `docs/codegen/distribution/03-certificate.md` §4 from refine's
    /// [`canonical_predicate`](crate::canonical_predicate) and
    /// [`canonical_context`](crate::canonical_context) hashes.
    pub fn with_hashes(
        mut self,
        obligation_hash: [u8; 32],
        context_hash: [u8; 32],
    ) -> ProofCertificate {
        self.obligation_hash = obligation_hash;
        self.context_hash = context_hash;
        self
    }

    /// Construct a placeholder certificate. Used when downstream code needs
    /// to return an `Unsat` outcome but hasn't built a real witness yet —
    /// the [`StubSolver`](crate::StubSolver) path and unit tests rely on
    /// this.
    pub fn placeholder() -> ProofCertificate {
        ProofCertificate {
            format_version: CERTIFICATE_FORMAT_VERSION,
            obligation_hash: [0; 32],
            context_hash: [0; 32],
            witness: CertificateWitness::Implicit,
        }
    }

    /// Construct an SMT certificate around the given Z3 witness bytes (the
    /// bytes produced by [`encode_z3_witness`]).
    pub fn smt(solver_version: impl Into<SmolStr>, witness: Vec<u8>) -> ProofCertificate {
        ProofCertificate {
            format_version: CERTIFICATE_FORMAT_VERSION,
            obligation_hash: [0; 32],
            context_hash: [0; 32],
            witness: CertificateWitness::Smt {
                solver: SmtSolverId::Z3,
                solver_version: solver_version.into(),
                witness,
            },
        }
    }

    /// Construct an `@unverified` certificate.
    pub fn unverified(reason: impl Into<SmolStr>, function_site: Span) -> ProofCertificate {
        ProofCertificate {
            format_version: CERTIFICATE_FORMAT_VERSION,
            obligation_hash: [0; 32],
            context_hash: [0; 32],
            witness: CertificateWitness::Unverified {
                reason: reason.into(),
                function_site,
            },
        }
    }

    /// Construct a `@trust` certificate.
    pub fn trust(reason: impl Into<SmolStr>, obligation_site: Span) -> ProofCertificate {
        ProofCertificate {
            format_version: CERTIFICATE_FORMAT_VERSION,
            obligation_hash: [0; 32],
            context_hash: [0; 32],
            witness: CertificateWitness::Trust {
                reason: reason.into(),
                obligation_site,
            },
        }
    }

    //            structmap audit must catch any drift in this serialiser
    /// Serialise to the wire byte layout described at the top of this module
    /// (and in `distribution/03-certificate.md` §2 / §3). The result is a
    /// self-contained certificate byte slice — the surrounding proofs-blob
    /// index supplies the length per `01-blob-format.md` §5.2.
    pub fn serialise(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        // Common 2-byte header.
        out.push(self.format_version);
        out.push(self.witness.discriminant_byte());
        // Per-type payload.
        match &self.witness {
            CertificateWitness::Smt {
                solver,
                solver_version,
                witness,
            } => {
                out.push(*solver as u8);
                write_varint_bytes(&mut out, solver_version.as_bytes());
                write_varint_bytes(&mut out, witness);
            }
            CertificateWitness::Comptime { kind, value } => {
                out.push(*kind as u8);
                write_varint_bytes(&mut out, value);
            }
            CertificateWitness::Implicit => {
                // No payload — header is the entire certificate.
            }
            CertificateWitness::Unverified {
                reason,
                function_site,
            } => {
                write_varint_bytes(&mut out, reason.as_bytes());
                write_span_12_bytes(&mut out, *function_site);
            }
            CertificateWitness::Trust {
                reason,
                obligation_site,
            } => {
                write_varint_bytes(&mut out, reason.as_bytes());
                write_span_12_bytes(&mut out, *obligation_site);
            }
        }
        out
    }
}

//            EDDA-Z3-PROOF-v1 layout: 17-byte header + proof varint+bytes +
//            core_count varint + per-core varint+bytes
//          cert layer; this helper produces the framed blob the verifier
//          parses
/// Build the byte blob that goes into a [`CertificateWitness::Smt`]'s
/// `witness` field, given the Z3 proof S-expression (`Ast::to_string()` on
/// `solver.get_proof().unwrap()`) and the unsat-core S-expressions (which
/// may be empty for capture-only minting).
pub fn encode_z3_witness(proof_sexpr: &str, unsat_core_sexprs: &[String]) -> Vec<u8> {
    let mut out = Vec::with_capacity(Z3_PROOF_HEADER.len() + proof_sexpr.len() + 16);
    out.extend_from_slice(Z3_PROOF_HEADER);
    write_varint_bytes(&mut out, proof_sexpr.as_bytes());
    encode_varint(unsat_core_sexprs.len() as u64, &mut out);
    for clause in unsat_core_sexprs {
        write_varint_bytes(&mut out, clause.as_bytes());
    }
    out
}

// Write a length-prefixed byte blob: varint(len) followed by `bytes`.
fn write_varint_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    encode_varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

// Write a span as the 12-byte (file_id, lo, hi) little-endian tuple.
fn write_span_12_bytes(out: &mut Vec<u8>, span: Span) {
    out.extend_from_slice(&span.file.to_u32().to_le_bytes());
    out.extend_from_slice(&span.lo.to_u32().to_le_bytes());
    out.extend_from_slice(&span.hi.to_u32().to_le_bytes());
}

impl fmt::Display for SmtSolverId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SmtSolverId::Z3 => "z3",
            SmtSolverId::Cvc5 => "cvc5",
            SmtSolverId::Yices => "yices",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_encodes_small_values_as_single_byte() {
        let mut out = Vec::new();
        encode_varint(0, &mut out);
        assert_eq!(out, vec![0x00]);
        let mut out = Vec::new();
        encode_varint(127, &mut out);
        assert_eq!(out, vec![0x7F]);
    }

    #[test]
    fn varint_encodes_large_values_multi_byte() {
        let mut out = Vec::new();
        encode_varint(128, &mut out);
        // 128 = 0b1000_0000 → low 7 bits = 0 with high bit set, then 1.
        assert_eq!(out, vec![0x80, 0x01]);
        let mut out = Vec::new();
        encode_varint(300, &mut out);
        // 300 = 0b1_0010_1100 → 0xAC then 0x02.
        assert_eq!(out, vec![0xAC, 0x02]);
    }
}
