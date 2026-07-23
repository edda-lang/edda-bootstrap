//! Diagnostics, severity, and the locked diagnostic class set.
//!
//! Owns the canonical enum of diagnostic classes from `build-system.md` §8
//! (parse_error, import_resolution_error, typecheck_error, refinement_unproven,
//! effect_row_mismatch, mode_violation, ...). Severity escalation per
//! `package.toml`'s `lints` block is applied here, not at the emission site.
//!
//! Implements: `docs/tooling/build-system.md` §8.
//!
//! Bootstrap infrastructure — not a spec'd language feature.
//!
//! # Class set is locked
//!
//! [`DiagnosticClass`] is a locked set of [`CLASS_COUNT`] (45) classes.
//! User-defined custom classes are reserved for a future feature (§8). Each
//! class has a locked default severity from the §8 table; `package.toml`'s
//! `lints` block and the `--warn-as-error` CLI flag can override that default
//! per invocation.
//!
//! # Emission pipeline
//!
//! 1. A pass (parse, typecheck, ...) selects a [`DiagnosticClass`] for the
//!    problem it found.
//! 2. The site computes the effective severity via
//!    [`LintConfig::effective`], which always returns a [`Severity`]: the
//!    `allow` opt-out was removed, so no
//!    class is suppressible and every selected diagnostic is built.
//! 3. Otherwise the site builds a [`Diagnostic`] with one primary [`Label`]
//!    plus optional secondary labels and notes, and pushes it into a
//!    [`Diagnostics`] take.
//! 4. The driver iterates the take for rendering and uses
//!    [`Diagnostics::has_errors`] to decide the build's exit code.
//!
//! Rendering — the surface format in §8 — is the driver/CLI's concern, not
//! this crate's.

mod class;
mod diagnostic;
mod severity;

pub use class::{CLASS_COUNT, DiagnosticClass};
pub use diagnostic::{CounterexampleValue, Diagnostic, Diagnostics, Label, ResolvedLocation};
pub use severity::{LintConfig, LintSeverity, Severity};
