//! Refinement-type discharge: predicate IR and Z3-backed solver.
//!
//! This crate owns the SMT-side of Edda's refinement layer. Per
//! `docs/types/refinement-decidability.md` §2, the required-decidable
//! fragment is EUF + LIA + boolean + extensional arrays. The crate provides:
//! the typed predicate IR and solver-agnostic [`Solver`] trait; the LIA +
//! Bool + Array translation and the Z3 backend; EUF for records, sum
//! tag-equality, and non-narrowing integer cast; the proof-certificate byte
//! format and the `@unverified` / `@trust` annotation routes per
//! `docs/types/refinement-decidability.md` §9 and
//! `docs/codegen/distribution/03-certificate.md`; the AST-refinement-expression
//! → [`Predicate`] lifter, the seam the typechecker (`edda-types`) feeds AST
//! `where` / `requires` / `ensures` clauses through; and deterministic
//! canonical encoding + BLAKE3 cache keys so every emitted certificate
//! carries the `(obligation_hash, context_hash)` tuple the v1.0 verifier
//! consumes.
//!
//! Implements:
//!   - `docs/types/refinement-decidability.md` (predicate fragment, outcome
//!     trichotomy, escape hatches)
//!   - `docs/syntax/refinements.md` (the predicate-fragment surface)
//!   - `docs/bootstrap/backend-choice.md` (Z3 via z3.rs, vendored via
//!     `static-link-z3`)
//!   - `docs/bootstrap/v0.1-scope.md` §3.5 (Z3 as the v0.1 default solver)
//!
//! # Crate layout
//!
//! - [`sort`] — [`Sort`], [`IntSort`], [`IntWidth`], record / sum / field /
//!   variant reference types. Type-level metadata referenced by the
//!   predicate IR.
//! - [`predicate`] — [`Predicate`] IR, [`Variable`], [`IntLit`], operator
//!   enums (`ArithOp`, `CmpOp`, `BoolBinOp` — reachable via [`Predicate`]
//!   variants), smart constructors, sort inference.
//! - [`obligation`] — [`Obligation`] (goal + context + site + kind) and
//!   [`ObligationKind`] (DivByZero, ModByZero, SliceBound, NarrowingCast,
//!   RequiresAtCall, EnsuresAtReturn, FieldInvariant,
//!   VariantPayloadInvariant, TerminationDecreases, GradedBound).
//! - [`outcome`] — [`DischargeOutcome`] four-variant enum,
//!   [`Counterexample`], [`ConcreteValue`].
//! - [`annotation`] — [`DischargeRoute`] enum and the
//!   [`UnverifiedAnnotation`] / [`TrustAnnotation`] surface. Routed via
//!   [`Obligation::with_route`](crate::Obligation::with_route).
//! - [`canonical`] — deterministic [`Predicate`] → bytes encoder + BLAKE3
//!   cache keys. Commutative operators sort their operands so equivalent
//!   predicates produce identical bytes. Routes through
//!   [`edda_cache::hash_bytes`] per the workspace single-BLAKE3-binding rule.
//! - [`lift`] — AST refinement-expression → [`Predicate`] lifter. The
//!   [`PredicateEnv`] trait abstracts every type-system lookup so the
//!   typechecker integration plugs in via a single impl; every
//!   env-touching arm routes through that trait, so the wiring points
//!   are easy to enumerate.
//! - [`schema`] — [`Schema`] / [`RecordSchema`] / [`SumSchema`] /
//!   [`VariantSchema`]. The typechecker hands [`Z3Backend`] a [`Schema`]
//!   describing every record and sum the discharge layer needs to translate
//!   into Z3 datatypes.
//! - [`solver`] — [`Solver`] trait. [`StubSolver`] always returns
//!   `Unknown`; [`Z3Backend`] is the v0.1 production solver.
//! - [`translate`] — `Translator` from [`Predicate`] to `z3::ast`. Covers
//!   LIA + Bool + Array plus records / sums / cast.
//!   Tuples surface as [`TranslationError::Unsupported`] (deferred).
//! - [`z3_backend`] — [`Z3Backend`] `Solver` impl. Per-obligation context,
//!   timeout via `Params`, Sat/Timeout/Unknown projection per
//!   refinement-decidability.md §8.
//! - [`certificate`] — [`ProofCertificate`] with the byte-level format from
//!   `docs/codegen/distribution/03-certificate.md`. Five
//!   [`CertificateWitness`] variants: `Smt` (Z3 unsat-core witness wrapped
//!   in the `EDDA-Z3-PROOF-v1` frame), `Comptime` (declared; edda-comptime
//!   mints), `Implicit` (declared; edda-types mints), `Unverified` and
//!   `Trust` (refine mints from annotations).
//! - [`error`] — [`RefineError`] and its `edda-diag` mapping
//!   ([`DiagnosticClass::RefinementUnproven`]). [`DischargeFailure`]
//!   carries the per-mode tail per refinement-decidability.md §8.
//!
//! # Build requirements
//!
//! `z3-sys` builds Z3 from vendored source via CMake; the bootstrap-compiler
//! build needs CMake, Python, and a C/C++ toolchain. Workspace
//! `.cargo/config.toml` pins `CMAKE_POLICY_VERSION_MINIMUM=3.5` for CMake 4
//! compatibility. On Windows, builds also need the MSVC `INCLUDE` env var
//! set (run from a Developer Command Prompt or source `vcvars64.bat` before
//! invoking cargo) so bindgen can locate the CRT headers.
//!
//! # Canonical encoding and cache keys
//!
//! - **Predicate canonical encoding** ([`canonical_predicate`],
//!   [`canonical_context`]) — refine-IR-native deterministic byte serialiser
//!   with sorted operands for commutative operators (`Eq`, `Ne`, `Add`,
//!   `And`, `Or`). Per-variant kind tags are locked.
//! - **BLAKE3 cache keys** ([`obligation_hash`], [`context_hash`]) — every
//!   [`ProofCertificate`] carries non-zero hashes that match
//!   `distribution/03-certificate.md` §3.3 / §4 (key =
//!   `BLAKE3(predicate_canonical || context_canonical_sorted)`).
//! - **Workspace BLAKE3 binding** — hashing routes through
//!   [`edda_cache::hash_bytes`] so the workspace has a single blake3
//!   dependency, matching the `edda-codegen` crate's convention.
//!
//! # AST-to-predicate lifting
//!
//! - **AST → Predicate lifter** ([`lift_predicate`], [`lift_clause`]) —
//!   walks every predicate-fragment form per `refinements.md`'s
//!   admitted column and produces a typed [`Predicate`].
//! - **[`PredicateEnv`] trait** — the typechecker integration seam. The
//!   typechecker provides an impl; tests provide synthetic impls.
//! - **[`LiftError`]** — distinguishes "not admitted in the fragment" from
//!   "admitted but not implemented yet" so diagnostics can guide users at
//!   the right resolution.
//!
//! # Proof certificates and annotation routes
//!
//! - **Proof-certificate byte format** — five-variant
//!   [`CertificateWitness`] with byte-level serialisation matching
//!   `distribution/03-certificate.md`. v0.1 is capture-only; v1.0's
//!   verifier consumes whatever this format writes.
//! - **`@unverified` annotation route** — function-level skip. Discharge
//!   layer emits an [`Unverified`](CertificateWitness::Unverified) cert
//!   carrying reason + function-site and returns `Unsat` without invoking
//!   Z3.
//! - **`@trust` annotation route** — per-site skip. Emits a
//!   [`Trust`](CertificateWitness::Trust) cert.
//! - **Z3 proof capture** — `Config::set_proof_generation(true)` lets the
//!   SMT path emit a real `EDDA-Z3-PROOF-v1`-framed witness on success.
//!
//! # Deferred work
//!
//! - **Tuples** — `Sort::Tuple` surfaces as `TranslationError::Unsupported`.
//!   The existing Z3 datatype machinery can be reused; the per-arity
//!   declaration plumbing remains out of scope.
//! - **`LintConfig`-aware emission** — caller responsibility, not refine's.
//!
//! [`DiagnosticClass::RefinementUnproven`]: edda_diag::DiagnosticClass::RefinementUnproven
//! [`Severity::Error`]: edda_diag::Severity::Error

mod annotation;
mod canonical;
mod certificate;
mod certificate_mint;
mod error;
mod lift;
mod obligation;
mod outcome;
mod predicate;
mod schema;
mod solver;
mod sort;
// The translate and z3_backend modules import from the `z3` crate.
// They are only compiled when the `refine` feature is active so that
// crates depending only on the solver-agnostic surface (Predicate IR,
// Obligation, ProofCertificate, Solver trait) can opt out of the Z3
// CMake build entirely.
#[cfg(feature = "refine")]
mod translate;
mod wire;
#[cfg(feature = "refine")]
mod z3_backend;

pub use annotation::{DischargeRoute, TrustAnnotation, UnverifiedAnnotation};
pub use canonical::{canonical_context, canonical_predicate, context_hash, obligation_hash};
pub use certificate::{
    encode_z3_witness, CertificateWitness, ComptimeWitnessKind, ProofCertificate, SmtSolverId,
    CERTIFICATE_FORMAT_VERSION, Z3_PROOF_HEADER,
};
pub use error::{AnnotationError, DischargeFailure, LiftError, RefineError};
#[cfg(feature = "refine")]
pub use error::TranslationError;
pub use lift::{lift_clause, lift_predicate, PredicateEnv};
pub use obligation::{Obligation, ObligationKind};
pub use outcome::{ConcreteValue, Counterexample, DischargeOutcome};
pub use predicate::{CmpOp, IntLit, IntLitValue, Predicate, Variable};
pub use schema::{RecordSchema, Schema, SumSchema, VariantSchema};
pub use solver::{Solver, StubSolver};
pub use sort::{FieldRef, IntSort, IntWidth, RecordRef, Sort, SumRef, VariantRef};
#[cfg(feature = "refine")]
pub use z3_backend::Z3Backend;
