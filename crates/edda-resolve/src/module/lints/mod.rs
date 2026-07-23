//! Resolver-side lint emission pass: unused imports, binding-should-be-let,
//! exec-scope-without-spawn, mode-overgrab, trust-hatch/budget, dead private
//! functions, duplicate spec invocations, and unused closure captures.
//!
//! Split out of the module-resolution driver (module.rs); these run after
//! resolution over the finished ResolvedPackage and share no internals with it.
//!
//! Each lint family lives in its own submodule; this file owns the cross-lint
//! place-expression helper and re-exports the public `emit_*` entry points so
//! the parent `mod.rs` re-export surface is unchanged.

mod audit;
mod binding_let;
mod closure_capture;
mod dead_private;
mod exec_scope;
mod mode_overgrab;
mod spec_dup;
mod trust;
mod unused_import;

pub use audit::{emit_capability_safe_stdlib_lints, emit_trust_points_listing};
pub use binding_let::emit_binding_should_be_let_lints;
pub use closure_capture::emit_unused_closure_capture_lints;
pub use dead_private::emit_dead_private_function_lints;
pub use exec_scope::emit_exec_scope_without_spawn_lints;
pub use mode_overgrab::emit_mode_overgrab_lints;
pub use spec_dup::emit_duplicate_spec_invocation_lints;
pub use trust::{emit_trust_budget_lints, emit_trust_hatch_too_dense_lints};
pub use unused_import::emit_unused_import_lints;

use edda_intern::Symbol;
use edda_syntax::ast::{Expr, ExprKind};

//            identifier (`v`), a dotted path (`v.field` parses to
//            `ExprKind::Path(["v","field"])` because the expression parser
//            consumes dot-segments greedily into a single `Path`), or a
//            chain of `.field` / `.0` / `[i]` projections off such a head
//            mode-overgrab analyzer needs to recognise `v.capacity = …` as
//            mutating `v` even though the parsed LHS is one multi-segment
//            `Path`, not a `Field { receiver: Path([v]) }` node
/// Walk the projection chain on a place expression and return the head
/// binding name. Used by the mode-overgrab lint to attribute field
/// stores to the parameter being mutated.
pub(super) fn expr_root_path_segment(expr: &Expr) -> Option<Symbol> {
    let mut current = expr;
    loop {
        match &current.kind {
            ExprKind::Path(p) => {
                return p.segments.first().map(|seg| seg.name);
            }
            ExprKind::Field { receiver, .. }
            | ExprKind::TupleIndex { receiver, .. }
            | ExprKind::Index { receiver, .. } => {
                current = receiver;
            }
            _ => return None,
        }
    }
}
