//! `@abi("...")` resolution: MIR [`AbiTag`] -> [`CallingConv`].
//!
//! MIR's [`AbiTag`] (init on `FnPtr` types' layout) is the post-typecheck
//! form of the Edda source attribute `@abi("...")`. This module turns it
//! into the LLVM-level [`CallingConv`] the backend will pin on each
//! function. The mapping rules:
//!
//! - [`AbiTag::Edda`] — Edda's own convention. Today that is just
//!   [`CallingConv::C`] (LLVM picks the right platform ABI from the
//!   `TargetMachine`); a future Edda-specific convention could swap in
//!   without changing call sites.
//! - [`AbiTag::C`] — explicit C. Same lowering as `Edda` for the v0.1
//!   target matrix because LLVM's `ccc` *is* the C ABI on every
//!   supported platform.
//! - [`AbiTag::System`] — the platform's "system" convention. Resolves
//!   to [`crate::target_defaults::calling_conv`]; on Windows-x86_64
//!   that historically meant `stdcall`/`win64` rather than SysV, but
//!   the platform default is now the right answer because the
//!   `TargetMachine` carries the platform ABI.
//! - [`AbiTag::Named`] — a user-supplied string like `"sysv64"` or
//!   `"win64"`. The accepted string catalogue is spec-locked in
//!   `docs/tooling/abi-and-layout.md`, which is **not yet in
//!   `corpus/`**. [`resolve_abi_tag`] therefore returns
//!   [`AbiResolutionError::NeedsNameResolution`] for `Named`; the
//!   caller must resolve the [`edda_intern::Symbol`] to a string via
//!   the active interner and pass it to [`resolve_named_abi`].

use edda_mir::AbiTag;
use edda_target::{Arch, TargetTriple};

use crate::calling_conv::{CallingConv, explicit_x86_64_sysv, explicit_x86_64_win64};
use crate::target_defaults;

/// Resolve an [`AbiTag`] to its [`CallingConv`] for the given target.
///
/// Returns [`AbiResolutionError::NeedsNameResolution`] for
/// [`AbiTag::Named`]; the caller must resolve the symbol against the
/// active [`edda_intern::Interner`] and dispatch to [`resolve_named_abi`].
pub fn resolve_abi_tag(
    tag: &AbiTag,
    triple: &TargetTriple,
) -> Result<CallingConv, AbiResolutionError> {
    match tag {
        AbiTag::Edda => Ok(CallingConv::C),
        AbiTag::C => Ok(CallingConv::C),
        AbiTag::System => Ok(target_defaults::calling_conv(triple)),
        AbiTag::Named(_) => Err(AbiResolutionError::NeedsNameResolution),
    }
}

/// Resolve an `@abi("name")` string to a [`CallingConv`] for the given
/// target.
///
/// Returns `None` for names outside the locked catalogue. The catalogue
/// below covers the names that have a non-ambiguous LLVM mapping on
/// every v0.1 target:
///
/// - `"sysv64"` / `"sysv"` — System V AMD64; valid on `x86_64` only.
/// - `"win64"` — Microsoft x64; valid on `x86_64` only.
/// - `"c"` — explicit C (synonym for `AbiTag::C`).
/// - `"system"` — platform default (synonym for `AbiTag::System`).
///
/// Other strings (`"aapcs"`, `"fastcall"`, `"vectorcall"`, ...) are
/// reserved pending the full string catalogue in
/// `docs/tooling/abi-and-layout.md`.
pub fn resolve_named_abi(name: &str, triple: &TargetTriple) -> Option<CallingConv> {
    let arch = triple.arch();
    match name {
        "c" => Some(CallingConv::C),
        "system" => Some(target_defaults::calling_conv(triple)),
        "sysv" | "sysv64" => explicit_x86_64_sysv(arch),
        "win64" => explicit_x86_64_win64(arch),
        _ => None,
    }
}

/// Why [`resolve_abi_tag`] could not produce a [`CallingConv`].
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum AbiResolutionError {
    /// The tag is [`AbiTag::Named`]; the caller must resolve the inner
    /// [`edda_intern::Symbol`] against the active interner and call
    /// [`resolve_named_abi`] with the resulting string.
    NeedsNameResolution,
}

impl std::fmt::Display for AbiResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AbiResolutionError::NeedsNameResolution => {
                f.write_str("AbiTag::Named requires interner resolution before calling-convention lookup")
            }
        }
    }
}

impl std::error::Error for AbiResolutionError {}

/// `true` if the given arch admits an explicit `sysv64` / `win64`
/// `@abi("...")` annotation. Outside `x86_64` the names have no
/// well-defined LLVM CC and `resolve_named_abi` returns `None` for them.
#[allow(dead_code)] // string-catalogue validation seam; reserved pending abi-and-layout.md landing in corpus/
pub(crate) const fn arch_admits_x86_64_abi_overrides(arch: Arch) -> bool {
    matches!(arch, Arch::X86_64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_intern::Interner;
    use edda_target::{AbiVariant, Os};

    fn linux_x86_64() -> TargetTriple {
        TargetTriple::new(Arch::X86_64, Os::Linux, AbiVariant::Gnu)
    }

    fn linux_aarch64() -> TargetTriple {
        TargetTriple::new(Arch::Aarch64, Os::Linux, AbiVariant::Gnu)
    }

    fn windows_x86_64() -> TargetTriple {
        TargetTriple::new(Arch::X86_64, Os::Windows, AbiVariant::Msvc)
    }

    #[test]
    fn edda_and_c_resolve_to_c() {
        let triple = linux_x86_64();
        assert_eq!(resolve_abi_tag(&AbiTag::Edda, &triple), Ok(CallingConv::C));
        assert_eq!(resolve_abi_tag(&AbiTag::C, &triple), Ok(CallingConv::C));
    }

    #[test]
    fn system_resolves_to_target_default() {
        // Today every target defaults to `C`; pinning the resolution path.
        for triple in [linux_x86_64(), linux_aarch64(), windows_x86_64()] {
            assert_eq!(
                resolve_abi_tag(&AbiTag::System, &triple),
                Ok(CallingConv::C),
                "non-C system default for {triple}",
            );
        }
    }

    #[test]
    fn named_returns_needs_name_resolution() {
        // The tag carries a symbol — value irrelevant for the error path.
        let interner = Interner::new();
        let sym = interner.intern("sysv64");
        assert_eq!(
            resolve_abi_tag(&AbiTag::Named(sym), &linux_x86_64()),
            Err(AbiResolutionError::NeedsNameResolution),
        );
    }

    #[test]
    fn resolve_named_abi_handles_c_and_system() {
        assert_eq!(resolve_named_abi("c", &linux_x86_64()), Some(CallingConv::C));
        assert_eq!(
            resolve_named_abi("system", &windows_x86_64()),
            Some(CallingConv::C),
        );
    }

    #[test]
    fn resolve_named_abi_x86_64_overrides_only_on_x86_64() {
        // Both names work on x86_64...
        assert_eq!(
            resolve_named_abi("sysv64", &linux_x86_64()),
            Some(CallingConv::X86_64SysV),
        );
        assert_eq!(
            resolve_named_abi("sysv", &linux_x86_64()),
            Some(CallingConv::X86_64SysV),
        );
        assert_eq!(
            resolve_named_abi("win64", &linux_x86_64()),
            Some(CallingConv::X86_64Win64),
        );
        // ...and are rejected on other arches.
        assert_eq!(resolve_named_abi("sysv64", &linux_aarch64()), None);
        assert_eq!(resolve_named_abi("win64", &linux_aarch64()), None);
    }

    #[test]
    fn resolve_named_abi_rejects_off_catalogue_strings() {
        for name in ["aapcs", "fastcall", "vectorcall", "thiscall", "", "C", "SystemV"] {
            assert_eq!(
                resolve_named_abi(name, &linux_x86_64()),
                None,
                "unexpectedly accepted {name:?}",
            );
        }
    }

    #[test]
    fn arch_admits_check_is_x86_64_only() {
        assert!(arch_admits_x86_64_abi_overrides(Arch::X86_64));
        for other in [Arch::Aarch64, Arch::Riscv64, Arch::Wasm32, Arch::Wasm64] {
            assert!(!arch_admits_x86_64_abi_overrides(other));
        }
    }

    #[test]
    fn error_displays_with_context() {
        let s = format!("{}", AbiResolutionError::NeedsNameResolution);
        assert!(s.contains("Named"));
        assert!(s.contains("interner"));
    }
}
