//! Resolver-side `import_resolution_error` builders for the failure
//! modes the intra-function path walk surfaces: unresolved paths, unresolved
//! module prefixes, unresolved cross-module items, and visibility
//! violations.

use edda_diag::DiagnosticClass;
use edda_intern::Symbol;
use edda_syntax::ast::Path as AstPath;

use crate::graph::ModuleId;
use crate::path::ModulePath;

use super::{Resolver, emit_diag, render_ast_path};

impl<'a, 'i> Resolver<'a, 'i> {
    pub(super) fn emit_unresolved(&mut self, path: &AstPath) {
        emit_diag(
            self.diags,
            self.lint_cfg,
            DiagnosticClass::ImportResolutionError,
            path.span,
            format!(
                "unresolved path `{}`",
                render_ast_path(path, self.cx)
            ),
            "expected a local binding, top-level item, import leaf, or qualified module path",
        );
    }

    pub(super) fn emit_unresolved_modulepath(&mut self, path: &AstPath, mp: &ModulePath) {
        emit_diag(
            self.diags,
            self.lint_cfg,
            DiagnosticClass::ImportResolutionError,
            path.span,
            format!(
                "unresolved module path `{}`",
                mp.display(self.cx.interner)
            ),
            "the path's module prefix did not match any in-scope module",
        );
    }

    pub(super) fn emit_unresolved_item(&mut self, path: &AstPath, item: Symbol, target: ModuleId) {
        let target_path = &self.modules[target.as_usize()];
        emit_diag(
            self.diags,
            self.lint_cfg,
            DiagnosticClass::ImportResolutionError,
            path.span,
            format!(
                "unresolved item `{}` in module `{}`",
                self.cx.interner.resolve(item),
                self.graph
                    .module(target_path.id)
                    .canonical_path
                    .display(self.cx.interner)
            ),
            "the named module does not declare a top-level item with this name",
        );
    }

    pub(super) fn emit_visibility(&mut self, path: &AstPath, item: Symbol, target: ModuleId) {
        emit_diag(
            self.diags,
            self.lint_cfg,
            DiagnosticClass::ImportResolutionError,
            path.span,
            format!(
                "item `{}` in module `{}` is not `public`",
                self.cx.interner.resolve(item),
                self.graph
                    .module(target)
                    .canonical_path
                    .display(self.cx.interner)
            ),
            "only `public` items are visible outside their owning module",
        );
    }
}
