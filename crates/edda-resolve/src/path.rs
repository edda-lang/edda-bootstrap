//! Canonical module paths — interned, immutable dot-separated names.

use std::fmt;

use edda_intern::{Interner, Symbol};
use edda_syntax::ast::Path as AstPath;

/// A canonical, dot-separated module path (`std.fs.read_to_string`,
/// `my_project.parser.tokens`). Constructed once and compared by handle
/// — the interner guarantees segment-symbol identity equals string
/// equality across the whole compilation unit.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct ModulePath {
    segments: Box<[Symbol]>,
}

impl ModulePath {
    /// Construct from a non-empty segment sequence.
    ///
    /// # Panics
    ///
    /// Debug-panics when `segments` is empty. Callers must validate
    /// upstream — the AST guarantees `Path.segments.len() >= 1`, and
    /// `module_identity_for_file` rejects empty results before reaching
    /// this constructor.
    pub fn new(segments: impl Into<Box<[Symbol]>>) -> Self {
        let segments = segments.into();
        debug_assert!(!segments.is_empty(), "ModulePath must have ≥ 1 segment");
        Self { segments }
    }

    /// Build from a syntax-level [`AstPath`]. The AST guarantees the
    /// path has at least one segment.
    pub fn from_ast(path: &AstPath) -> Self {
        let segments: Box<[Symbol]> = path.segments.iter().map(|i| i.name).collect();
        Self::new(segments)
    }

    /// Borrow the segment sequence.
    pub fn segments(&self) -> &[Symbol] {
        &self.segments
    }

    /// Number of segments (always ≥ 1).
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// `false` for any well-formed [`ModulePath`] (the invariant
    /// guarantees ≥ 1 segment); provided so Clippy and consumers can
    /// follow the `len`/`is_empty` convention.
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// First segment — the "head" used to dispatch §4 resolution
    /// (`std`, `<root_namespace>`, `<dep_root_namespace>`).
    pub fn first(&self) -> Symbol {
        self.segments[0]
    }

    /// Last segment — the leaf used for sibling-leaf canonicalisation.
    pub fn last(&self) -> Symbol {
        *self.segments.last().expect("ModulePath has ≥ 1 segment")
    }

    /// The path without its last segment, or `None` for a one-segment
    /// path. Used to derive the canonical path of a sibling import.
    pub fn parent(&self) -> Option<ModulePath> {
        if self.segments.len() <= 1 {
            return None;
        }
        let mut v = Vec::with_capacity(self.segments.len() - 1);
        v.extend_from_slice(&self.segments[..self.segments.len() - 1]);
        Some(ModulePath::new(v))
    }

    /// Append a segment, returning a new [`ModulePath`].
    pub fn push(&self, sym: Symbol) -> ModulePath {
        let mut v: Vec<Symbol> = self.segments.to_vec();
        v.push(sym);
        ModulePath::new(v)
    }

    /// Extend with multiple segments at once.
    pub fn extend(&self, syms: impl IntoIterator<Item = Symbol>) -> ModulePath {
        let mut v: Vec<Symbol> = self.segments.to_vec();
        v.extend(syms);
        ModulePath::new(v)
    }

    /// Borrowed display adapter that resolves segments through `interner`.
    pub fn display<'a>(&'a self, interner: &'a Interner) -> ModulePathDisplay<'a> {
        ModulePathDisplay {
            path: self,
            interner,
        }
    }

    /// Materialise as an owned dot-separated string. Allocates; prefer
    /// [`ModulePath::display`] for diagnostic-printing paths.
    pub fn to_owned_string(&self, interner: &Interner) -> String {
        let mut out = String::new();
        for (i, sym) in self.segments.iter().enumerate() {
            if i > 0 {
                out.push('.');
            }
            out.push_str(interner.resolve(*sym));
        }
        out
    }
}

/// Display adapter returned by [`ModulePath::display`]. Borrows the
/// path and an [`Interner`]; renders as `seg1.seg2.seg3`.
pub struct ModulePathDisplay<'a> {
    path: &'a ModulePath,
    interner: &'a Interner,
}

impl<'a> fmt::Display for ModulePathDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, sym) in self.path.segments.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            f.write_str(self.interner.resolve(*sym))?;
        }
        Ok(())
    }
}
