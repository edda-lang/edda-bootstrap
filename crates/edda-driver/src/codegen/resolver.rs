//! The codegen → resolution data-injection seam. `DriverResolver`
//! answers `edda_codegen::body::Encoder`'s `QualifiedNameResolver`
//! queries from the active [`ResolvedPackage`].

use edda_codegen::body::QualifiedNameResolver;
use edda_intern::Interner;
use edda_resolve::{Resolved, ResolvedPackage};
use edda_syntax::ast::Path as AstPath;
use smol_str::SmolStr;

use super::qualified_name::binding_qualified_name;
use super::support::join_segments;

/// Production [`QualifiedNameResolver`] backed by a [`ResolvedPackage`].
///
/// Consulted by `edda_codegen::body::Encoder` while walking the
/// pre-substitution spec body — every embedded `Path` AST node
/// resolves to the canonical qualified name through this resolver.
pub(super) struct DriverResolver<'a> {
    pub resolved: &'a ResolvedPackage,
    pub interner: &'a Interner,
}

impl<'a> QualifiedNameResolver for DriverResolver<'a> {
    fn resolve_path(&self, path: &AstPath) -> SmolStr {
        if let Some(resolution) = self.resolved.resolutions().lookup_path(path.span) {
            match resolution {
                Resolved::Binding(id) => {
                    let binding = self.resolved.binding(id);
                    return SmolStr::new(binding_qualified_name(binding, self.resolved, self.interner));
                }
                Resolved::Module(id) => {
                    let module = self.resolved.module_entry(id);
                    return SmolStr::new(module.canonical_path.to_owned_string(self.interner));
                }
                Resolved::Error => {}
            }
        }
        // Fallback: dot-joined source segments. This branch is only
        // reached on resolution failures; the encoder's contract
        // permits an approximation since the surrounding diagnostics
        // already flagged the underlying problem.
        let joined: String = join_segments(path, self.interner);
        SmolStr::new(joined)
    }
}
