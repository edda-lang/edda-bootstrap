//! The boundary between the codegen encoder and `edda-resolve`'s
//! name-resolution output.
//!
//! When the encoder ([`super::Encoder`]) walks an AST node containing a
//! [`Path`], it asks the [`QualifiedNameResolver`] for the resolved
//! fully qualified name of that path. The driver implements this trait
//! by consulting an `edda_resolve::Resolutions` map plus a
//! `ResolvedPackage` and joining the result with `.` separators.
//!
//! Why a trait rather than concrete data: the encoder ships in the
//! `edda-codegen` crate while resolution data lives in `edda-resolve`.
//! Holding a `&dyn QualifiedNameResolver` lets us keep `edda-codegen`
//! free of an `edda-resolve` dependency, which would otherwise be a
//! circular concern once the driver wires the cascade.

use edda_syntax::ast::Path;
use smol_str::SmolStr;

//   `std.option.Option`, never an import alias like `Opt`
//   identifier characters joined by `.`
/// Resolve an AST [`Path`] to its fully qualified name.
pub trait QualifiedNameResolver {
    /// Resolve `path` to its qualified name string.
    ///
    /// # Contract
    ///
    /// Callers may only pass paths that have been resolved by
    /// `edda-resolve`'s intra-function pass. Passing an
    /// unresolved or `Resolved::Error` path is a programmer error;
    /// implementations are encouraged to debug-assert on this case.
    fn resolve_path(&self, path: &Path) -> SmolStr;
}
