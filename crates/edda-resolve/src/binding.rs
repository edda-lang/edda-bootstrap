//! Binding identifiers and their classification.
//!
//! [`BindingId`] is the stable handle every other resolution-pass
//! output keys against; the two-tuple `(ModuleId, index)` lets a
//! module renumber its own bindings (the intra-function pass
//! appends Param and Local entries) without renumbering siblings,
//! keeping incremental rebuild cheap at module granularity per
//! `build-system.md §5`.

use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::Visibility;

use crate::graph::ModuleId;

/// Stable identifier for a binding within a [`crate::ResolvedPackage`].
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct BindingId {
    /// Owning module.
    pub module: ModuleId,
    /// Dense intra-module index.
    pub index: u32,
}

impl BindingId {
    /// Construct from raw fields.
    pub fn new(module: ModuleId, index: u32) -> Self {
        Self { module, index }
    }
}

/// Origin of a binding — what declaration form produced it.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum BindingKind {
    /// Top-level `function` declaration.
    Function,
    /// Top-level `type` declaration (product or sum).
    TypeDecl,
    /// Top-level `spec` declaration.
    Spec,
    /// Top-level `spec Path(args)` invocation (CA1 pass-1 placeholder).
    /// Bound under the syntactically-mangled short name (`Box_Expr`,
    /// `StringMap_f64`) so subsequent resolution can see the name codegen
    /// will materialise. The generated module's actual items land at
    /// codegen time per `docs/coverage-audit.md` CA1 pass 2.
    SpecInvocation,
    /// Top-level `let` constant — module-scope compile-time-evaluated
    /// binding per `declarations.md` §"Module-level let".
    Const,
    /// Leaf name introduced by an `import` statement; resolves to a
    /// sibling [`ModuleId`] in the source graph.
    ImportLeaf,
    /// Function or generic parameter (assigned by the intra-function pass).
    Param,
    /// `let` / `var` / pattern binding inside a function body
    /// (assigned by the intra-function pass).
    Local,
}

/// One binding's metadata. The [`crate::ResolvedPackage`] owns a
/// flat `Vec<BindingEntry>` per module that [`BindingId::index`]
/// indexes into.
#[derive(Clone, Debug)]
pub struct BindingEntry {
    /// Kind of declaration that introduced this binding.
    pub kind: BindingKind,
    /// Interned identifier text.
    pub name: Symbol,
    /// Source location of the binding occurrence.
    pub span: Span,
    /// Owning module (mirror of [`BindingId::module`]).
    pub module: ModuleId,
    /// Visibility — `Public` for `public`-marked items, `Module` for
    /// everything else. Locals and Params are always `Module`.
    pub visibility: Visibility,
}
