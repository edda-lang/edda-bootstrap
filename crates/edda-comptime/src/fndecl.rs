//! Function-declaration lookup seam for comptime user-function calls.
//!
//! The evaluator interprets a call to a user-defined function by
//! resolving the callee's [`edda_resolve::BindingId`] (recorded per
//! call span by the typechecker, like `comptime_type_paths`) to the
//! function's declaration — its [`FnSig`] and typed-HIR body. The
//! evaluator itself holds no package tables, so callers that do (MIR
//! lowering holds the `FunctionInput` slice) implement
//! [`FnDeclLookup`] and thread it through
//! [`crate::EvalCx::with_fn_decls`] — the same seam shape as
//! [`crate::TypeDeclLookup`].

use edda_intern::Symbol;
use edda_resolve::BindingId;
use edda_types::{FnSig, HirBlock};

/// One resolved function declaration: name, signature, typed body.
#[derive(Copy, Clone)]
pub struct FnDeclInfo<'a> {
    /// Source-declared function name.
    pub name: Symbol,
    /// Lowered signature (params, return type, effect row).
    pub sig: &'a FnSig,
    /// Typed-HIR body block.
    pub body: &'a HirBlock,
}

/// Resolve a function's resolver-side [`BindingId`] to its
/// declaration. Mirror of [`crate::TypeDeclLookup`] for functions.
pub trait FnDeclLookup {
    /// Look up one function declaration. `None` when the binding does
    /// not name a function with a lowered body in the caller's tables
    /// (extern declarations have no Edda-side body and return `None`).
    fn lookup_fn_decl(&self, binding: BindingId) -> Option<FnDeclInfo<'_>>;
}
