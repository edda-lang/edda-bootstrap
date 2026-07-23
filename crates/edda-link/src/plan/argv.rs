//! Per-tool argv construction for [`super::LinkPlan`].
//!
//! One builder per linker / archiver dialect: ELF (`mold` / `ld.lld`),
//! Mach-O (`ld64.lld`), COFF (`lld-link`), WebAssembly (`wasm-ld`), and
//! the two archivers (`llvm-ar` / `llvm-lib`). [`super::LinkPlan::argv`]
//! dispatches into these after tool selection. Linker dialect per
//! `build-system.md` §5b + `backend-choice.md` §6.7.

use std::ffi::{OsStr, OsString};

use crate::output::{LibKind, OutputKind};

use super::LinkPlan;

/// Win64 stack reserve (bytes) requested via `/STACK` for every linked
/// executable. 256 MiB. Edda's MIR→LLVM lowering runs no optimisation
/// passes, so fat aggregate-temp frames in the self-hosted compiler's
/// recursive pipeline overrun lld-link's 1 MB default and fault inside
/// `__chkstk`. Reserve is address space
/// only; pages commit lazily, so trivial programs pay nothing.
pub(super) const WIN64_STACK_RESERVE: u64 = 256 * 1024 * 1024;

/// Shared ELF linker argv (mold and `ld.lld`). Dialect:
///   - `-o <output>`
///   - `--gc-sections` for section-level DCE.
///   - `-shared` for dynamic libraries.
///   - `-L<path>` for library search paths.
///   - `-l<name>` (or `-l:lib<name>.a` for static linkage).
pub(super) fn argv_elf(plan: &LinkPlan<'_>) -> Vec<OsString> {
    let mut argv = Vec::with_capacity(default_capacity(plan));
    argv.push(OsString::from("-o"));
    argv.push(plan.output.as_os_str().to_os_string());
    argv.push(OsString::from("--gc-sections"));
    if matches!(plan.kind, OutputKind::DynamicLibrary) {
        argv.push(OsString::from("-shared"));
    }
    for dir in plan.lib_search_paths {
        argv.push(concat_os("-L", dir));
    }
    for input in plan.inputs {
        argv.push(input.as_os_str().to_os_string());
    }
    for lib in plan.libs {
        match lib.kind {
            LibKind::Static => argv.push(OsString::from(format!("-l:lib{}.a", lib.name))),
            LibKind::Dynamic => argv.push(OsString::from(format!("-l{}", lib.name))),
            LibKind::Framework => debug_assert!(false, "framework rejected earlier"),
        }
    }
    argv.extend(plan.extra_args.iter().cloned());
    argv
}

/// `ld64.lld` (Mach-O) argv. Dialect:
///   - `-o <output>`
///   - `-dead_strip` for section-level DCE.
///   - `-dylib` for dynamic libraries.
///   - `-L<path>` for library search paths.
///   - `-l<name>` for libraries; `-framework <name>` for frameworks.
pub(super) fn argv_ld64_lld(plan: &LinkPlan<'_>) -> Vec<OsString> {
    let mut argv = Vec::with_capacity(default_capacity(plan));
    argv.push(OsString::from("-o"));
    argv.push(plan.output.as_os_str().to_os_string());
    argv.push(OsString::from("-dead_strip"));
    if matches!(plan.kind, OutputKind::DynamicLibrary) {
        argv.push(OsString::from("-dylib"));
    }
    for dir in plan.lib_search_paths {
        argv.push(concat_os("-L", dir));
    }
    for input in plan.inputs {
        argv.push(input.as_os_str().to_os_string());
    }
    for lib in plan.libs {
        match lib.kind {
            LibKind::Static => argv.push(OsString::from(format!("-l{}", lib.name))),
            LibKind::Dynamic => argv.push(OsString::from(format!("-l{}", lib.name))),
            LibKind::Framework => {
                argv.push(OsString::from("-framework"));
                argv.push(OsString::from(lib.name.as_str()));
            }
        }
    }
    argv.extend(plan.extra_args.iter().cloned());
    argv
}

/// `lld-link` (COFF) argv. Dialect mirrors `link.exe`:
///   - `/OUT:<output>` (no space after `:`).
///   - `/OPT:REF /OPT:ICF` for section-level DCE.
///   - `/STACK:<reserve>` on executables — large win64 stack reserve.
///   - `/DLL` for dynamic libraries.
///   - `/LIBPATH:<dir>` for library search paths.
///   - `<name>.lib` as a positional input for each library
///     (regardless of LibKind::Static vs Dynamic — the dynamic case
///     resolves through the import library at link time).
pub(super) fn argv_lld_link(plan: &LinkPlan<'_>) -> Vec<OsString> {
    let mut argv = Vec::with_capacity(default_capacity(plan));
    argv.push(concat_os("/OUT:", plan.output));
    argv.push(OsString::from("/OPT:REF"));
    // In diagnostic map mode keep ICF off so every function keeps a
    // distinct address — folded symbols make crash-RVA → symbol lookup
    // and address breakpoints unreliable.
    if std::env::var_os("EDDA_LINK_MAP").is_none() {
        argv.push(OsString::from("/OPT:ICF"));
    }
    // Edda's MIR→LLVM lowering runs no optimisation passes, so every
    // local and every aggregate temp (`mv.sum`, `sret.tmp`, byval-arg
    // copies, …) is a distinct, never-reused stack slot emitted at the
    // builder's current position rather than a coalesced entry-block
    // frame. Recursive-descent pipeline functions in the self-hosted
    // compiler accumulate ~1 MB frames that blow lld-link's default
    // 1 MB stack reserve and fault inside `__chkstk` (the probe itself
    // is emitted correctly — the demand simply exceeds the reserve).
    // Reserve a large stack so deep recursion over fat frames survives.
    // This is reserve only (address space); pages commit lazily via the
    // stack probe, so a trivial program still starts at ~one page.
    if matches!(plan.kind, OutputKind::Executable) {
        argv.push(OsString::from(format!("/STACK:{WIN64_STACK_RESERVE}")));
    }
    // Diagnostic-only (env-gated): emit an lld symbol/address map next to
    // the output so a crash RVA in the stripped native binary can be
    // mapped back to its Edda function. Off unless `EDDA_LINK_MAP` is set.
    if std::env::var_os("EDDA_LINK_MAP").is_some() {
        let mut map = plan.output.as_os_str().to_os_string();
        map.push(".map");
        argv.push(concat_os("/lldmap:", &map));
    }
    if matches!(plan.kind, OutputKind::DynamicLibrary) {
        argv.push(OsString::from("/DLL"));
    }
    for dir in plan.lib_search_paths {
        argv.push(concat_os("/LIBPATH:", dir));
    }
    for input in plan.inputs {
        argv.push(input.as_os_str().to_os_string());
    }
    for lib in plan.libs {
        match lib.kind {
            LibKind::Static | LibKind::Dynamic => {
                argv.push(OsString::from(format!("{}.lib", lib.name)));
            }
            LibKind::Framework => debug_assert!(false, "framework rejected earlier"),
        }
    }
    argv.extend(plan.extra_args.iter().cloned());
    argv
}

/// `wasm-ld` argv. Dialect mirrors lld's ELF port:
///   - `-o <output>`
///   - `--gc-sections` for section-level DCE.
///   - `--no-entry` for the dynamic-library / library-style output
///     (wasm has no `-shared` flag; absence of a `_start` symbol is
///     surfaced via `--no-entry`).
///   - `-L<path>` for library search paths.
///   - `-l<name>` for libraries.
pub(super) fn argv_wasm_ld(plan: &LinkPlan<'_>) -> Vec<OsString> {
    let mut argv = Vec::with_capacity(default_capacity(plan));
    argv.push(OsString::from("-o"));
    argv.push(plan.output.as_os_str().to_os_string());
    argv.push(OsString::from("--gc-sections"));
    if matches!(plan.kind, OutputKind::DynamicLibrary) {
        argv.push(OsString::from("--no-entry"));
    }
    for dir in plan.lib_search_paths {
        argv.push(concat_os("-L", dir));
    }
    for input in plan.inputs {
        argv.push(input.as_os_str().to_os_string());
    }
    for lib in plan.libs {
        match lib.kind {
            LibKind::Static | LibKind::Dynamic => {
                argv.push(OsString::from(format!("-l{}", lib.name)));
            }
            LibKind::Framework => debug_assert!(false, "framework rejected earlier"),
        }
    }
    argv.extend(plan.extra_args.iter().cloned());
    argv
}

/// `llvm-ar` (System V / BSD `.a`) argv. Dialect mirrors `ar`:
///   - `crs <output> <inputs...>`
///   - `c` — create the archive.
///   - `r` — replace existing members of the same name.
///   - `s` — write a symbol-table index into the archive.
///   - Library deps and search paths do not apply.
pub(super) fn argv_llvm_ar(plan: &LinkPlan<'_>) -> Vec<OsString> {
    let mut argv = Vec::with_capacity(2 + plan.inputs.len() + plan.extra_args.len());
    argv.push(OsString::from("crs"));
    argv.push(plan.output.as_os_str().to_os_string());
    for input in plan.inputs {
        argv.push(input.as_os_str().to_os_string());
    }
    argv.extend(plan.extra_args.iter().cloned());
    argv
}

/// `llvm-lib` (Microsoft `.lib`) argv. Dialect mirrors `lib.exe`:
///   - `/OUT:<output>` (no space after `:`).
///   - Inputs are positional.
///   - Library deps and search paths do not apply.
pub(super) fn argv_llvm_lib(plan: &LinkPlan<'_>) -> Vec<OsString> {
    let mut argv = Vec::with_capacity(1 + plan.inputs.len() + plan.extra_args.len());
    argv.push(concat_os("/OUT:", plan.output));
    for input in plan.inputs {
        argv.push(input.as_os_str().to_os_string());
    }
    argv.extend(plan.extra_args.iter().cloned());
    argv
}

/// Estimate the argv capacity for linker dialects to avoid mid-build
/// reallocations. Two args per input/lib/search-path, plus a generous
/// header allowance.
fn default_capacity(plan: &LinkPlan<'_>) -> usize {
    8 + plan.inputs.len() + plan.libs.len() + plan.lib_search_paths.len() + plan.extra_args.len()
}

/// `OsString`-safe `format!("{prefix}{path}")` — concatenates a UTF-8
/// prefix and an `OsStr` without UTF-8 validation on the path.
fn concat_os(prefix: &str, path: impl AsRef<OsStr>) -> OsString {
    let mut s = OsString::with_capacity(prefix.len() + path.as_ref().len());
    s.push(prefix);
    s.push(path);
    s
}
