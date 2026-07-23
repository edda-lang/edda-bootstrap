//! Output-artifact kinds and library-link specs.
//!
//! Inputs to [`crate::plan::LinkPlan`]. Locked enums per
//! `build-system.md` §5 (`.edda/build/<target>/<profile>/{bin,lib,deps}/`).
//! `Framework` linkage is macOS-specific (`-framework`); other targets
//! reject it at plan time.

use smol_str::SmolStr;

/// Kind of artifact the linker produces.
///
/// `Executable` lands under `.edda/build/<target>/<profile>/bin/`;
/// `StaticLibrary` and `DynamicLibrary` land under
/// `.edda/build/<target>/<profile>/lib/` (`build-system.md` §5).
///
/// `Executable` and `DynamicLibrary` route to a `Linker`; `StaticLibrary`
/// routes to an `Archiver` (`llvm-ar` / `llvm-lib`) via [`crate::LinkPlan::tool`].
/// A kind a given tool cannot produce surfaces as `UnsupportedKindForTool`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum OutputKind {
    /// Position-dependent or PIE executable.
    Executable,
    /// `.a` / `.lib` static archive.
    StaticLibrary,
    /// `.so` / `.dylib` / `.dll` dynamic library.
    DynamicLibrary,
}

impl OutputKind {
    /// Lowercase tag used in diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Executable => "executable",
            Self::StaticLibrary => "static_library",
            Self::DynamicLibrary => "dynamic_library",
        }
    }
}

/// Linkage kind for a single library dependency.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum LibKind {
    /// Static linkage — pull symbols out of `lib<name>.a` / `<name>.lib`.
    Static,
    /// Dynamic linkage — record a runtime dependency on `lib<name>.so`
    /// / `lib<name>.dylib` / `<name>.dll`.
    Dynamic,
    /// Apple framework linkage (`-framework <name>`). macOS targets
    /// only; other targets reject this at plan time.
    Framework,
}

impl LibKind {
    /// Lowercase tag used in diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Dynamic => "dynamic",
            Self::Framework => "framework",
        }
    }
}

/// One library to link.
///
/// `name` is the bare identifier (no `lib` prefix, no extension). The
/// per-linker dialect in [`crate::plan`] wraps it appropriately:
/// `-l<name>` on ELF/Mach-O, `<name>.lib` on COFF, `-framework <name>`
/// on macOS when [`LibKind::Framework`] is set.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct LibSpec {
    /// Library identifier.
    pub name: SmolStr,
    /// Linkage kind.
    pub kind: LibKind,
}

impl LibSpec {
    /// Construct a `LibSpec` from a name and kind.
    pub fn new(name: impl Into<SmolStr>, kind: LibKind) -> Self {
        Self {
            name: name.into(),
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_kind_names_are_stable() {
        assert_eq!(OutputKind::Executable.name(), "executable");
        assert_eq!(OutputKind::StaticLibrary.name(), "static_library");
        assert_eq!(OutputKind::DynamicLibrary.name(), "dynamic_library");
    }

    #[test]
    fn lib_kind_names_are_stable() {
        assert_eq!(LibKind::Static.name(), "static");
        assert_eq!(LibKind::Dynamic.name(), "dynamic");
        assert_eq!(LibKind::Framework.name(), "framework");
    }

    #[test]
    fn lib_spec_roundtrips_name() {
        let spec = LibSpec::new("pthread", LibKind::Dynamic);
        assert_eq!(spec.name.as_str(), "pthread");
        assert_eq!(spec.kind, LibKind::Dynamic);
    }
}
