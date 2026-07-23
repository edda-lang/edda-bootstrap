//! Cascade link stage: build a `LinkPlan` from driver state, resolve the
//! per-target tool through `edda-link` (linker for executables, archiver
//! for static libraries), run it, and project link failures into the
//! driver diagnostics. Owns the Win64 runtime-lib / MSVC search-path
//! probing helpers and the per-target output-path composition.

use std::path::{Path, PathBuf};

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_link::{LibKind, LibSpec, LinkError, LinkPlan, OutputKind, resolve_runtime_externs};
use edda_manifest::PackageKind;
use edda_target::Os;
use std::ffi::OsString;
use edda_span::Span;

use crate::context::Driver;

use super::push_io_error;

/// Run the link stage against the compile pass's object-file outputs.
///
/// Builds a [`LinkPlan`] from driver state, resolves the per-target tool
/// through `edda-link` (a linker for the executable, an archiver for the
/// static library), runs it, and projects any [`LinkError`] into
/// [`Driver::diagnostics`] through [`LinkError::to_diagnostic`] gated by
/// [`Driver::lint_cfg`]. Short-circuits while `driver.compile_outputs`
/// is empty.
pub(super) fn drive_link(driver: &mut Driver) {
    if driver.compile_outputs.is_empty() {
        return;
    }

    let os = driver.target.triple().os();
    let mut link_inputs = driver.compile_outputs.clone();
    let kind;
    let output;
    let libs: Vec<LibSpec>;
    let search_paths: Vec<PathBuf>;
    let mut extra_args: Vec<OsString>;

    // `[package] kind` routes the link stage explicitly;
    // an absent key keeps the legacy split — entry present links
    // an executable, mainless archives a static library — so every
    // pre-`kind` manifest builds byte-for-byte as before.
    let effective_kind = match driver.manifest.kind {
        Some(PackageKind::Executable) => OutputKind::Executable,
        Some(PackageKind::StaticLibrary) => OutputKind::StaticLibrary,
        Some(PackageKind::DynamicLibrary) => OutputKind::DynamicLibrary,
        None if driver.has_entry => OutputKind::Executable,
        None => OutputKind::StaticLibrary,
    };
    if effective_kind == OutputKind::Executable && !driver.has_entry {
        let class = DiagnosticClass::ParseError;
        let severity = driver.lint_cfg.effective(class);
        driver.diagnostics.push(Diagnostic::new(
            class,
            severity,
            Span::DUMMY,
            "`[package] kind = \"executable\"` but the package declares no `function main` \
             entry point — add `main`, or request `static_library` / `dynamic_library`",
        ));
        return;
    }

    // Freestanding closeout: the
    // `--freestanding` build drops the Rust `edda-rt` static library and
    // the C runtime from the link line, linking instead against the OS
    // entry stub (`_edda_start`, emitted into the member object by the
    // compile stage) plus only the OS API import library. Windows-only
    // today — the per-target entry stub for other OSes is not yet emitted.
    let freestanding = driver.options.freestanding && os == Os::Windows;
    if effective_kind == OutputKind::Executable {
        kind = OutputKind::Executable;
        output = link_output_path(driver);
        if let Some(support) = driver.str_eq_support_object.as_ref() {
            link_inputs.push(support.clone());
        }
        if freestanding {
            // No rt.lib, no CRT: only the member object (carrying both
            // `main` and `_edda_start`) plus kernel32 for `ExitProcess`,
            // entered at `_edda_start` with the default libraries
            // suppressed (`/NODEFAULTLIB`).
            if !append_dll_import_libs(driver, &mut link_inputs) {
                return;
            }
            libs = freestanding_link_libs(os);
            search_paths = default_lib_search_paths(os);
            extra_args = freestanding_extra_link_args(os);
        } else {
            // Default path: link standalone against the edda-rt static
            // library — the rt provides the `__edda_*` symbols, which
            // `cargo build` places alongside the CLI binary. The member's
            // object is self-contained (re-lowers its full source
            // closure); sibling library archives are not consumed.
            if let Some(rt_lib) = find_edda_rt_lib() {
                link_inputs.push(rt_lib);
            }
            if !append_dll_import_libs(driver, &mut link_inputs) {
                return;
            }
            libs = default_link_libs(os);
            search_paths = default_lib_search_paths(os);
            extra_args = default_extra_link_args(os);
            if let Some(flag) = rt_shadow_override_arg(os, &driver.abi_rt_shadows) {
                extra_args.push(flag);
            }
        }
        let profile = driver
            .options
            .profile_override
            .as_deref()
            .unwrap_or_else(|| driver.manifest.build.default_profile.as_ref());
        if let Some(flag) = map_file_arg(os, &output, profile) {
            extra_args.push(flag);
        }
        extra_args.extend(debug_pdb_args(os, &output, profile));
    } else if effective_kind == OutputKind::DynamicLibrary {
        // Dynamic library: link the
        // member's objects plus the edda-rt static library into a shared
        // image, one explicit `/EXPORT:` per non-runtime `@abi` symbol on
        // COFF; ELF/Mach-O get `-shared` / `-dylib` from the LinkPlan
        // dialect and export non-hidden symbols implicitly. Unlike the
        // native compiler's `/NOENTRY` freestanding bet (its objects never
        // touch the CRT), a bootstrap object references rt trap/panic
        // helpers even for trivial bodies, pulling Rust-std members whose
        // CRT imports (`memset`, `__CxxFrameHandler3`, …) need the full
        // system-lib set — so this is the standard MSVC cdylib shape:
        // default libs + search paths + the CRT's default
        // `_DllMainCRTStartup` entry initialising the CRT at DLL attach.
        kind = OutputKind::DynamicLibrary;
        output = dynamic_lib_output_path(driver);
        if let Some(support) = driver.str_eq_support_object.as_ref() {
            link_inputs.push(support.clone());
        }
        if let Some(rt_lib) = find_edda_rt_lib() {
            link_inputs.push(rt_lib);
        }
        if !append_dll_import_libs(driver, &mut link_inputs) {
            return;
        }
        libs = default_link_libs(os);
        search_paths = default_lib_search_paths(os);
        extra_args = dynamic_extra_link_args(os, &driver.abi_exports);
        if let Some(flag) = rt_shadow_override_arg(os, &driver.abi_rt_shadows) {
            extra_args.push(flag);
        }
    } else {
        // Library member: archive the member's objects into a static
        // library under `lib/`. No entry, no runtime, no linker libs — the
        // archiver just packs the objects (their `__edda_*` references stay
        // unresolved until a consumer links the archive).
        kind = OutputKind::StaticLibrary;
        output = lib_output_path(driver);
        libs = Vec::new();
        search_paths = Vec::new();
        extra_args = Vec::new();
    }

    if kind != OutputKind::StaticLibrary && !gate_runtime_externs(driver, &link_inputs) {
        return;
    }

    if let Some(parent) = output.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            push_io_error(&mut driver.diagnostics, &driver.lint_cfg, parent, &err);
            return;
        }
    }

    let plan = LinkPlan {
        target: &driver.target,
        inputs: &link_inputs,
        output: &output,
        kind,
        libs: &libs,
        lib_search_paths: &search_paths,
        extra_args: &extra_args,
    };

    let tool = match plan.tool() {
        Ok(t) => t,
        Err(err) => {
            push_link_error(&mut driver.diagnostics, &driver.lint_cfg, &err);
            return;
        }
    };

    let outcome = match edda_link::run(&plan) {
        Ok(o) => o,
        Err(err) => {
            push_link_error(&mut driver.diagnostics, &driver.lint_cfg, &err);
            return;
        }
    };

    if let Err(err) = outcome.into_success(tool) {
        push_link_error(&mut driver.diagnostics, &driver.lint_cfg, &err);
    }
}

/// Synthesise one PE import library per `Driver::dll_imports` entry under
/// `target/edda/<triple>/imports/` and append each to `link_inputs`.
/// No-op outside Windows targets and when no extern carries a `from`
/// clause.
fn append_dll_import_libs(driver: &mut Driver, link_inputs: &mut Vec<PathBuf>) -> bool {
    if driver.dll_imports.is_empty() || driver.target.triple().os() != Os::Windows {
        return true;
    }
    let machine = match driver.target.triple().arch() {
        edda_target::Arch::X86_64 => "x64",
        edda_target::Arch::Aarch64 => "arm64",
        // No other arch pairs with Os::Windows in the locked triple
        // matrix; lld-link would reject the object anyway.
        _ => return true,
    };
    let import_dir = driver
        .package_root
        .join("target")
        .join("edda")
        .join(driver.target.triple().to_string())
        .join("imports");
    if let Err(err) = std::fs::create_dir_all(&import_dir) {
        push_io_error(&mut driver.diagnostics, &driver.lint_cfg, &import_dir, &err);
        return false;
    }
    for (dll, symbols) in &driver.dll_imports {
        let symbols: Vec<String> = symbols.iter().cloned().collect();
        let spec = edda_link::ImportLibSpec { dll, symbols: &symbols };
        match edda_link::synthesize_import_lib(&spec, &import_dir, machine) {
            Ok(lib) => link_inputs.push(lib),
            Err(err) => {
                push_link_error(&mut driver.diagnostics, &driver.lint_cfg, &err);
                return false;
            }
        }
    }
    true
}

/// Locate the edda-rt static library next to the current Edda CLI executable.
///
/// The runtime ships beside the compiler binary under a name derived from
/// the binary's own stem. A `cargo build` tree has `edda` + `edda_rt.lib`
/// (Unix `libedda_rt.a`); the archived canonical install renames both in
/// lockstep to `edda-bootstrap.exe` + `edda-bootstrap_rt.lib` (xtask's
/// `archive_built_binary`). Probing `<exe_stem>_rt.lib` first resolves
/// either layout; the literal `edda_rt.lib` / `libedda_rt.a` is retained as
/// a fallback for any install whose stem-derived name is absent.
fn find_edda_rt_lib() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates = Vec::new();
    if let Some(stem) = exe.file_stem().and_then(|s| s.to_str()) {
        candidates.push(rt_lib_filename(stem));
    }
    candidates.push(rt_lib_filename("edda"));
    candidates
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// Compose the edda-rt static-library filename for a binary whose stem is
/// `stem`: `<stem>_rt.lib` on Windows MSVC, `lib<stem>_rt.a` elsewhere.
fn rt_lib_filename(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}_rt.lib")
    } else {
        format!("lib{stem}_rt.a")
    }
}

/// Per-target default args appended to the `LinkPlan`.
///
/// Bootstrap programs declare a bare `function main() -> i32` that
/// lowers to LLVM `@main`. On Linux/macOS that's exactly the symbol
/// the platform runtime's `_start` hands control to, so no extra
/// flags are needed. On Windows MSVC `lld-link` defaults to
/// `/SUBSYSTEM:WINDOWS` (no stdio console); we override to
/// `CONSOLE` so the resulting binary attaches to the caller's
/// console. The CRT-provided `mainCRTStartup` is the default entry,
/// which sets up argv/envp, calls `main`, then calls `ExitProcess`
/// with the return value — we link the CRT via [`default_link_libs`]
/// so that symbol resolves.
fn default_extra_link_args(os: Os) -> Vec<OsString> {
    match os {
        Os::Windows => vec![OsString::from("/SUBSYSTEM:CONSOLE")],
        _ => Vec::new(),
    }
}

/// Per-target link libraries for a `--freestanding` executable.
///
/// Only the OS base API — `kernel32`, which exports `ExitProcess` (the
/// freestanding entry stub's exit path). The C runtime libraries are
/// deliberately omitted; `/NODEFAULTLIB` (see [`freestanding_extra_link_args`])
/// keeps lld-link from auto-pulling them.
fn freestanding_link_libs(os: Os) -> Vec<LibSpec> {
    match os {
        Os::Windows => vec![LibSpec::new("kernel32", LibKind::Dynamic)],
        _ => Vec::new(),
    }
}

/// Per-target default args for a `--freestanding` executable.
///
/// Windows MSVC: `/SUBSYSTEM:CONSOLE` (attach to the caller's console),
/// `/ENTRY:_edda_start` (the compiler-emitted OS entry stub replaces the
/// CRT's `mainCRTStartup`), and `/NODEFAULTLIB` (suppress the implicit
/// CRT import libraries — only the libs we name explicitly are linked).
fn freestanding_extra_link_args(os: Os) -> Vec<OsString> {
    match os {
        Os::Windows => vec![
            OsString::from("/SUBSYSTEM:CONSOLE"),
            OsString::from("/ENTRY:_edda_start"),
            OsString::from("/NODEFAULTLIB"),
        ],
        _ => Vec::new(),
    }
}

/// The `lld-link` argument that lets a member object's `@abi("__edda_*")`
/// definition override `edda_rt.lib`'s like-named export, or `None` when
/// no such override is warranted.
///
/// Returns `Some(/FORCE:MULTIPLE)` only on Windows and only when this
/// member's object defines one or more `__edda_*` symbols that the
/// runtime also exports (`shadows` non-empty). The object is linked
/// before `edda_rt.lib`, so "first definition wins" resolves to the
/// stdlib-claimed definition the program's callers expect.
fn rt_shadow_override_arg(
    os: Os,
    shadows: &std::collections::BTreeSet<String>,
) -> Option<OsString> {
    if os == Os::Windows && !shadows.is_empty() {
        return Some(OsString::from("/FORCE:MULTIPLE"));
    }
    None
}

/// The `lld-link` `/MAP:<path>` argument that emits a symbol→RVA table
/// beside the linked executable, or `None` when no map is warranted.
///
/// Gated to Windows MSVC + the `dev` profile: the map makes native-binary
/// crash backtraces localizable under an external debugger (`cdb`) without
/// any `.pdb` / debug-info dependency. Release and bench builds opt out so
/// the shipped tree carries no extra artifact.
fn map_file_arg(os: Os, output: &Path, profile: &str) -> Option<OsString> {
    if os != Os::Windows || profile != "dev" {
        return None;
    }
    let mut arg = OsString::from("/MAP:");
    arg.push(output.with_extension("map").as_os_str());
    Some(arg)
}

/// The `lld-link` `/DEBUG` + `/PDB:<path>` arguments that emit a
/// public-symbol PDB beside the linked executable, or an empty vector when
/// no PDB is warranted.
///
/// Gated to Windows MSVC + the `dev` profile. The PDB makes native-binary
/// crash backtraces localizable in `cdb`/`windbg` (the canonical Windows
/// debugger format) by surfacing the objects' COFF symbol names; it carries
/// no source-line / locals info because the bootstrap backend emits no
/// CodeView. Release and bench builds opt out so the shipped tree carries
/// no extra artifact and no embedded build-path record.
fn debug_pdb_args(os: Os, output: &Path, profile: &str) -> Vec<OsString> {
    if os != Os::Windows || profile != "dev" {
        return Vec::new();
    }
    let mut pdb = OsString::from("/PDB:");
    pdb.push(output.with_extension("pdb").as_os_str());
    vec![OsString::from("/DEBUG"), pdb]
}

/// Per-target default libraries appended to the `LinkPlan`.
///
/// Windows MSVC: the bare-minimum runnable set plus every system DLL that
/// the Rust standard library (bundled into `edda_rt.lib`) imports from:
///
/// - `msvcrt`    — DLL CRT entry-point wrappers (`mainCRTStartup`, …).
/// - `ucrt`      — Universal C Runtime DLL stubs.
/// - `vcruntime` — MSVC C++/SEH runtime stubs.
/// - `kernel32`  — Win32 base API (`ExitProcess`, `CreateFile`, …).
/// - `advapi32`  — registry, security, event-log APIs.
/// - `ntdll`     — NT native API (`NtReadFile`, `RtlNtStatusToDosError`, …).
/// - `ws2_32`    — Winsock API (Rust std's net module, always linked in).
/// - `userenv`   — user-environment APIs (`GetUserProfileDirectory`, …).
/// - `bcrypt`    — cryptographic primitives (Rust's random-number APIs).
fn default_link_libs(os: Os) -> Vec<LibSpec> {
    match os {
        Os::Windows => vec![
            LibSpec::new("msvcrt", LibKind::Dynamic),
            LibSpec::new("ucrt", LibKind::Dynamic),
            LibSpec::new("vcruntime", LibKind::Dynamic),
            LibSpec::new("kernel32", LibKind::Dynamic),
            LibSpec::new("advapi32", LibKind::Dynamic),
            LibSpec::new("ntdll", LibKind::Dynamic),
            LibSpec::new("ws2_32", LibKind::Dynamic),
            LibSpec::new("userenv", LibKind::Dynamic),
            LibSpec::new("bcrypt", LibKind::Dynamic),
        ],
        _ => Vec::new(),
    }
}

/// Per-target library search paths (`/LIBPATH:`) passed to `lld-link`.
///
/// Checks `LIB` first so vcvars64.bat-sourced environments work
/// unchanged. When `LIB` is absent or empty, probes the standard
/// Visual Studio and Windows SDK install paths for x64 libraries.
fn default_lib_search_paths(os: Os) -> Vec<PathBuf> {
    match os {
        Os::Windows => windows_lib_search_paths(),
        _ => Vec::new(),
    }
}

fn windows_lib_search_paths() -> Vec<PathBuf> {
    if let Ok(lib) = std::env::var("LIB") {
        let paths: Vec<PathBuf> = lib
            .split(';')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }
    probe_msvc_paths()
}

fn probe_msvc_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    let pf86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());

    if let Some(p) = find_msvc_x64_lib(&pf) {
        paths.push(p);
    }
    let winsdk_lib = PathBuf::from(&pf86).join("Windows Kits").join("10").join("Lib");
    if let Some(ver) = latest_subdir(&winsdk_lib) {
        let base = winsdk_lib.join(ver);
        let ucrt = base.join("ucrt").join("x64");
        let um = base.join("um").join("x64");
        if ucrt.is_dir() {
            paths.push(ucrt);
        }
        if um.is_dir() {
            paths.push(um);
        }
    }
    paths
}

fn find_msvc_x64_lib(pf: &str) -> Option<PathBuf> {
    let vs_root = PathBuf::from(pf).join("Microsoft Visual Studio");
    for year in &["2022", "2019", "2017"] {
        let year_dir = vs_root.join(year);
        if !year_dir.is_dir() {
            continue;
        }
        for edition in &["Enterprise", "Professional", "Community", "BuildTools"] {
            let msvc_root = year_dir.join(edition).join("VC").join("Tools").join("MSVC");
            if let Some(ver) = latest_subdir(&msvc_root) {
                let lib_dir = msvc_root.join(ver).join("lib").join("x64");
                if lib_dir.is_dir() {
                    return Some(lib_dir);
                }
            }
        }
    }
    None
}

fn latest_subdir(dir: &Path) -> Option<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(|c: char| c.is_ascii_digit()))
        .collect();
    entries.sort_by(|a, b| b.cmp(a));
    entries.into_iter().next()
}

/// Compose the per-target / per-profile output path per
/// `build-system.md` §5 (`.edda/build/<triple>/<profile>/bin/<name>`).
///
/// The basename is suffixed per [`executable_suffix`] so Windows
/// produces `<name>.exe` and other platforms produce a plain
/// `<name>` — the shell convention on each OS.
fn link_output_path(driver: &Driver) -> PathBuf {
    let triple = driver.target.triple().to_string();
    let profile = driver
        .options
        .profile_override
        .as_deref()
        .unwrap_or_else(|| driver.manifest.build.default_profile.as_ref());
    let name = format!(
        "{}{}",
        driver.manifest.package.as_ref(),
        executable_suffix(driver.target.triple().os()),
    );
    driver
        .package_root
        .join(".edda")
        .join("build")
        .join(triple)
        .join(profile)
        .join("bin")
        .join(name)
}

/// Conventional executable suffix for the target OS.
fn executable_suffix(os: Os) -> &'static str {
    match os {
        Os::Windows => ".exe",
        Os::Wasi | Os::Browser => ".wasm",
        _ => "",
    }
}

/// Compose the per-target / per-profile static-library output path for a
/// library member (`.edda/build/<triple>/<profile>/lib/<name>`). The
/// basename follows the archiver's platform convention via
/// [`static_lib_filename`].
fn lib_output_path(driver: &Driver) -> PathBuf {
    let triple = driver.target.triple().to_string();
    let profile = driver
        .options
        .profile_override
        .as_deref()
        .unwrap_or_else(|| driver.manifest.build.default_profile.as_ref());
    let name = static_lib_filename(driver.target.triple().os(), driver.manifest.package.as_ref());
    driver
        .package_root
        .join(".edda")
        .join("build")
        .join(triple)
        .join(profile)
        .join("lib")
        .join(name)
}

/// Conventional static-library filename for the target OS: `<name>.lib`
/// on Windows MSVC, `lib<name>.a` elsewhere.
fn static_lib_filename(os: Os, name: &str) -> String {
    match os {
        Os::Windows => format!("{name}.lib"),
        _ => format!("lib{name}.a"),
    }
}

/// Compose the per-target / per-profile dynamic-library output path
/// (`.edda/build/<triple>/<profile>/lib/<name>`), basename per
/// [`dynamic_lib_filename`].
fn dynamic_lib_output_path(driver: &Driver) -> PathBuf {
    let triple = driver.target.triple().to_string();
    let profile = driver
        .options
        .profile_override
        .as_deref()
        .unwrap_or_else(|| driver.manifest.build.default_profile.as_ref());
    let name = dynamic_lib_filename(driver.target.triple().os(), driver.manifest.package.as_ref());
    driver
        .package_root
        .join(".edda")
        .join("build")
        .join(triple)
        .join(profile)
        .join("lib")
        .join(name)
}

/// Conventional dynamic-library filename for the target OS.
fn dynamic_lib_filename(os: Os, name: &str) -> String {
    match os {
        Os::Windows => format!("{name}.dll"),
        Os::Macos => format!("lib{name}.dylib"),
        _ => format!("lib{name}.so"),
    }
}

/// Per-target extra args for a `kind = "dynamic_library"` link: on
/// Windows, one `/EXPORT:<sym>` per non-runtime `@abi` export
/// (`Driver::abi_exports`); empty elsewhere.
fn dynamic_extra_link_args(
    os: Os,
    exports: &std::collections::BTreeSet<String>,
) -> Vec<OsString> {
    if os != Os::Windows {
        return Vec::new();
    }
    exports
        .iter()
        .map(|name| OsString::from(format!("/EXPORT:{name}")))
        .collect()
}

/// Push a [`LinkError`] into the diagnostic take, respecting the
/// configured severity for the placeholder class.
///
/// `edda-link` routes every variant through
/// [`DiagnosticClass::ParseError`] until `build-system.md` §8 admits a
/// dedicated `linker_failure` class (see
/// [`LinkError::to_diagnostic`]). Match that choice so the manifest's
/// `lints` block can demote / suppress link failures uniformly with
/// other parse-error-classed diagnostics.
fn push_link_error(diagnostics: &mut Diagnostics, lint_cfg: &LintConfig, err: &LinkError) {
    let class = DiagnosticClass::ParseError;
    let severity = lint_cfg.effective(class);
    diagnostics.push(Diagnostic::new(class, severity, Span::DUMMY, err.to_string()));
}

/// Run the pre-link `unprovided_runtime_extern` / `duplicate_runtime_extern`
/// gates against the finalized `link_inputs`. Returns `true` when the link
/// may proceed. Mirrors the native compiler's pre-link gate —
/// this is the check that turns a cryptic `lld-link: undefined symbol:
/// __edda_syscall` into a one-line attributable compiler diagnostic.
fn gate_runtime_externs(driver: &mut Driver, link_inputs: &[PathBuf]) -> bool {
    let report = match resolve_runtime_externs(link_inputs) {
        Ok(report) => report,
        Err(err) => {
            push_link_error(&mut driver.diagnostics, &driver.lint_cfg, &err);
            return false;
        }
    };
    for name in &report.duplicates {
        // An `@abi("__edda_*")` shadow this member's own object claims is an
        // intentional, deterministic override — the object is listed ahead of
        // `edda_rt.lib`, so first-definition-wins resolves to the claimed
        // definition (see `rt_shadow_override_arg`). Warn only on duplicates
        // the member did NOT claim: those really are resolved arbitrarily.
        if driver.abi_rt_shadows.contains(name) {
            continue;
        }
        push_duplicate_extern_diag(&mut driver.diagnostics, &driver.lint_cfg, name);
    }
    if report.unprovided.is_empty() {
        return true;
    }
    for name in &report.unprovided {
        push_unprovided_extern_diag(&mut driver.diagnostics, &driver.lint_cfg, name);
    }
    false
}

/// Push one `error[unprovided_runtime_extern]` diagnostic naming the
/// symbol and the fix. Wording mirrors the native compiler's
/// `link.ea::emit_unprovided_diag` message, with a bootstrap impl tag so
/// the two implementations stay legible against each other.
fn push_unprovided_extern_diag(diagnostics: &mut Diagnostics, lint_cfg: &LintConfig, name: &str) {
    let class = DiagnosticClass::UnprovidedRuntimeExtern;
    let severity = lint_cfg.effective(class);
    let msg = format!(
        "unprovided runtime extern '{name}' (impl=bootstrap-rust): referenced by an emitted \
         object but neither inlined away nor defined in a link input (edda_rt.lib) — provide \
         the runtime impl in edda-rt or lower the call so the reference disappears"
    );
    diagnostics.push(Diagnostic::new(class, severity, Span::DUMMY, msg));
}

/// Push one `warn[duplicate_runtime_extern]` diagnostic naming the symbol
/// defined in more than one link input. Advisory only -- unlike
/// [`push_unprovided_extern_diag`], this never aborts the link (the
/// linker still resolves to one definition; the diagnostic exists so the
/// choice is not silent). Wording mirrors the native compiler's
/// `link.ea::emit_duplicate_diag` message, with a bootstrap impl tag so
/// the two implementations stay legible against each other.
fn push_duplicate_extern_diag(diagnostics: &mut Diagnostics, lint_cfg: &LintConfig, name: &str) {
    let class = DiagnosticClass::DuplicateRuntimeExtern;
    let severity = lint_cfg.effective(class);
    let msg = format!(
        "duplicate runtime extern '{name}' (impl=bootstrap-rust): defined in more than one \
         link input -- the linker resolves it to one definition arbitrarily; keep a single \
         definition in edda_rt.lib"
    );
    diagnostics.push(Diagnostic::new(class, severity, Span::DUMMY, msg));
}
