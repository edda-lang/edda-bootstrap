//! `cargo xtask package` — stage the release build plus vendored
//! `std/`/`runes/` into a per-platform plug-and-play install archive.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use crate::{cargo_build_release, workspace_root};

/// The current build's release-archive platform label. Panics on an
/// unsupported host — the four native triples below are the only ones
/// `package_cmd` knows how to stage; wasm32 ships through a different
/// story (no native binary + runtime staticlib pair to archive).
fn platform_label() -> &'static str {
    match (env::consts::OS, env::consts::ARCH) {
        ("windows", "x86_64") => "x86-64-windows-msvc",
        ("linux", "x86_64") => "x86-64-linux-gnu",
        ("linux", "aarch64") => "aarch64-linux-gnu",
        ("macos", "aarch64") => "aarch64-macos-darwin",
        (os, arch) => panic!("xtask package: unsupported host platform {os}-{arch}"),
    }
}

/// `(binary name, runtime staticlib name)` as `cargo build --release`
/// names them in `target/release/`.
fn binary_names() -> (&'static str, &'static str) {
    if cfg!(target_os = "windows") {
        ("edda.exe", "edda_rt.lib")
    } else {
        ("edda", "libedda_rt.a")
    }
}

/// Locate the monorepo checkout that vendors `std/` and `runes/` for
/// this packaging run. Dev machines get this for free from the same
/// `EDDA_DEV_STDLIB_ROOT` baked path `edda-driver` uses for its own
/// dev-fallback stdlib discovery; CI sets `EDDA_MONOREPO_ROOT`
/// explicitly after checking out the monorepo as a sibling directory.
fn resolve_monorepo_root() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("EDDA_MONOREPO_ROOT") {
        let root = PathBuf::from(dir);
        if looks_like_monorepo_root(&root) {
            return Some(root);
        }
    }
    let baked = option_env!("EDDA_DEV_STDLIB_ROOT")?;
    if baked.is_empty() {
        return None;
    }
    let root = Path::new(baked).parent()?;
    looks_like_monorepo_root(root).then(|| root.to_path_buf())
}

fn looks_like_monorepo_root(root: &Path) -> bool {
    root.join("std").is_dir() && root.join("runes").is_dir()
}

fn io_err(what: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, what.into())
}

const WINDOWS_LLVM_RUNTIME_DLLS: &[&str] = &["LLVM-C.dll", "z.dll", "zstd.dll"];

fn bundle_runtime_libs_windows(llvm_prefix: &Path, bin_dir: &Path) -> std::io::Result<()> {
    let llvm_bin = llvm_prefix.join("bin");
    for name in WINDOWS_LLVM_RUNTIME_DLLS {
        let src = llvm_bin.join(name);
        if !src.is_file() {
            return Err(io_err(format!("required runtime DLL not found: {}", src.display())));
        }
        std::fs::copy(&src, bin_dir.join(name))?;
    }
    Ok(())
}

fn bundle_runtime_libs_linux(staged_bin: &Path, lib_dir: &Path, llvm_prefix: &Path) -> std::io::Result<()> {
    let output = Command::new("ldd").arg(staged_bin).output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut bundled_any = false;
    for line in stdout.lines() {
        let Some((_, rest)) = line.split_once("=>") else { continue };
        let resolved = rest.trim().split_whitespace().next().unwrap_or("");
        let resolved_path = Path::new(resolved);
        if !resolved_path.starts_with(llvm_prefix) || !resolved_path.is_file() {
            continue;
        }
        std::fs::create_dir_all(lib_dir)?;
        let name = resolved_path.file_name().expect("ldd path has a filename");
        std::fs::copy(resolved_path, lib_dir.join(name))?;
        bundled_any = true;
    }
    if !bundled_any {
        return Err(io_err(format!(
            "ldd against {} reported no shared library under {} - expected at least one LLVM shared object to bundle",
            staged_bin.display(),
            llvm_prefix.display(),
        )));
    }
    let status = Command::new("patchelf")
        .args(["--set-rpath", "$ORIGIN/../lib"])
        .arg(staged_bin)
        .status()?;
    if !status.success() {
        return Err(io_err("patchelf --set-rpath failed"));
    }
    Ok(())
}

fn bundle_runtime_libs_macos(staged_bin: &Path, lib_dir: &Path, llvm_prefix: &Path) -> std::io::Result<()> {
    let output = Command::new("otool").arg("-L").arg(staged_bin).output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut bundled_any = false;
    for line in stdout.lines().skip(1) {
        let Some(path_str) = line.trim().split_whitespace().next() else { continue };
        let resolved_path = Path::new(path_str);
        if !resolved_path.starts_with(llvm_prefix) || !resolved_path.is_file() {
            continue;
        }
        std::fs::create_dir_all(lib_dir)?;
        let name = resolved_path.file_name().expect("otool path has a filename");
        std::fs::copy(resolved_path, lib_dir.join(name))?;
        let new_ref = format!("@executable_path/../lib/{}", name.to_string_lossy());
        let status = Command::new("install_name_tool")
            .args(["-change", path_str, &new_ref])
            .arg(staged_bin)
            .status()?;
        if !status.success() {
            return Err(io_err("install_name_tool -change failed"));
        }
        bundled_any = true;
    }
    if !bundled_any {
        return Err(io_err(format!(
            "otool -L against {} reported no dylib under {} - expected at least one LLVM dylib to bundle",
            staged_bin.display(),
            llvm_prefix.display(),
        )));
    }
    Ok(())
}

/// Bundle the LLVM shared library (and its own transitive shared-lib
/// deps) that `edda` dynamically links against, so the release archive
/// is self-contained — no separate LLVM install on the end user's
/// machine, only the multi-hundred-KB-to-tens-of-MB shared library
/// files shipped alongside the binary.
fn bundle_runtime_libs(staged_bin: &Path, staging: &Path) -> std::io::Result<()> {
    let Some(llvm_prefix) = env::var_os("LLVM_SYS_180_PREFIX").map(PathBuf::from) else {
        return Err(io_err(
            "LLVM_SYS_180_PREFIX not set - cannot locate the LLVM shared library edda links against",
        ));
    };
    if cfg!(target_os = "windows") {
        let bin_dir = staged_bin.parent().expect("staged binary has a parent directory");
        bundle_runtime_libs_windows(&llvm_prefix, bin_dir)
    } else if cfg!(target_os = "macos") {
        bundle_runtime_libs_macos(staged_bin, &staging.join("lib"), &llvm_prefix)
    } else {
        bundle_runtime_libs_linux(staged_bin, &staging.join("lib"), &llvm_prefix)
    }
}

/// Recursively copy `src` into `dst`, creating directories as needed.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" || name == ".edda" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn render_release_readme(label: &str) -> String {
    let linker_note = match label {
        "x86-64-windows-msvc" => {
            "Windows: the MSVC linker (`link.exe` / `lld-link`) from Visual Studio Build \
             Tools — install the \"Desktop development with C++\" workload."
        }
        "x86-64-linux-gnu" | "aarch64-linux-gnu" => {
            "Linux: a system linker — `apt install lld` (or use your distro's `mold` / \
             GNU `ld` from binutils/build-essential)."
        }
        "aarch64-macos-darwin" => {
            "macOS: Xcode Command Line Tools (`xcode-select --install`) for `ld64`."
        }
        _ => "A system linker for your platform (LLVM `lld`, GNU `ld`, or your platform's native linker).",
    };
    format!(
        "# Edda bootstrap compiler — {label}\n\
        \n\
        ## Which compiler am I using?\n\
        \n\
        Today you're building with Edda's reference compiler — the Rust bootstrap. The \
        native compiler is written in Edda itself, type-checks its own full source, and \
        emits binaries through its own backend; we're closing the last behavioral-parity \
        gap before it replaces the bootstrap entirely. Same language, same checks, either \
        way — the bootstrap is simply the mature path today, and everything you build now \
        carries forward.\n\
        \n\
        ## Install\n\
        \n\
        Unpack this archive anywhere; `bin/`, `std/`, `runes/` (and `lib/`, on Linux/macOS) \
        must stay side by side. Add `bin/` to your `PATH`. No `EDDA_STDLIB_ROOT` needed — \
        the compiler finds the sibling `std/` next to its own binary automatically. The \
        compiler links against a bundled copy of LLVM shipped in this archive (`bin/` on \
        Windows, `lib/` on Linux/macOS) — no separate LLVM install needed either.\n\
        \n\
        ## System requirements\n\
        \n\
        {linker_note} Z3 is statically linked into the compiler — no separate solver \
        install needed.\n\
        \n\
        ## Quickstart\n\
        \n\
        ```sh\n\
        edda version\n\
        mkdir hello && cd hello\n\
        # package.toml + src/main.ea — see the language docs for the hello-world shape\n\
        edda check\n\
        edda build\n\
        edda run\n\
        ```\n\
        \n\
        ## Using a vendored rune\n\
        \n\
        Runes ship inside this archive under `runes/lib/<name>/`. Reference one from your \
        project's `package.toml` with an absolute path to where you unpacked this archive, \
        for example:\n\
        \n\
        ```toml\n\
        [[dependencies]]\n\
        name = \"slug\"\n\
        version = \"0.1.0\"\n\
        source = \"path+/absolute/path/to/this/unpacked/archive/runes/lib/slug\"\n\
        ```\n\
        \n\
        Substitute the actual absolute path where you unpacked this archive (forward \
        slashes work on every platform, including Windows). `edda add` targets the Mimir \
        registry, which is not live yet — hand-edit `package.toml` for vendored runes \
        until then.\n"
    )
}

fn stage_release(monorepo_root: &Path, label: &str) -> std::io::Result<PathBuf> {
    let staging = workspace_root().join("target").join("package").join(label);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    let bin_dir = staging.join("bin");
    std::fs::create_dir_all(&bin_dir)?;

    let (exe_name, rt_name) = binary_names();
    let release_dir = workspace_root().join("target").join("release");
    let staged_exe = bin_dir.join(exe_name);
    std::fs::copy(release_dir.join(exe_name), &staged_exe)?;
    std::fs::copy(release_dir.join(rt_name), bin_dir.join(rt_name))?;

    bundle_runtime_libs(&staged_exe, &staging)?;

    copy_tree(&monorepo_root.join("std"), &staging.join("std"))?;
    copy_tree(&monorepo_root.join("runes"), &staging.join("runes"))?;
    std::fs::write(staging.join("README.md"), render_release_readme(label))?;
    Ok(staging)
}

fn archive_staging_dir(staging: &Path, label: &str) -> std::io::Result<PathBuf> {
    let parent = staging.parent().expect("staging dir has a parent");
    let dir_name = staging.file_name().expect("staging dir has a name");

    if cfg!(target_os = "windows") {
        let archive = parent.join(format!("edda-bootstrap-{label}.zip"));
        let _ = std::fs::remove_file(&archive);
        let ps_cmd = format!(
            "Compress-Archive -Path '{}' -DestinationPath '{}' -Force",
            staging.display(),
            archive.display(),
        );
        let status = Command::new("powershell").args(["-NoProfile", "-Command", &ps_cmd]).status()?;
        if !status.success() {
            return Err(io_err("Compress-Archive failed"));
        }
        Ok(archive)
    } else {
        let archive = parent.join(format!("edda-bootstrap-{label}.tar.gz"));
        let _ = std::fs::remove_file(&archive);
        let status = Command::new("tar")
            .arg("czf")
            .arg(&archive)
            .arg("-C")
            .arg(parent)
            .arg(dir_name)
            .status()?;
        if !status.success() {
            return Err(io_err("tar czf failed"));
        }
        Ok(archive)
    }
}

/// `cargo xtask package` — build the release binary, then stage it
/// plus vendored `std/`/`runes/` from a monorepo checkout into a
/// per-platform archive under `target/package/`. Requires
/// `EDDA_MONOREPO_ROOT` (or the dev-only baked `EDDA_DEV_STDLIB_ROOT`
/// fallback) to resolve to a checkout with `std/` and `runes/` at its
/// root; exits 2 with an actionable message when neither is available.
pub(crate) fn package_cmd() -> ExitCode {
    match cargo_build_release(&[]) {
        Ok(true) => {}
        Ok(false) => return ExitCode::from(1),
        Err(msg) => {
            eprintln!("xtask package: {msg}");
            return ExitCode::from(2);
        }
    }

    let Some(monorepo_root) = resolve_monorepo_root() else {
        eprintln!(
            "xtask package: could not locate a monorepo checkout with `std/` and `runes/`; \
             set EDDA_MONOREPO_ROOT=<path/to/monorepo>"
        );
        return ExitCode::from(2);
    };

    let label = platform_label();
    let staging = match stage_release(&monorepo_root, label) {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("xtask package: failed to stage release: {err}");
            return ExitCode::from(2);
        }
    };

    match archive_staging_dir(&staging, label) {
        Ok(archive) => {
            println!("xtask package: archived {}", archive.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("xtask package: failed to archive {}: {err}", staging.display());
            ExitCode::from(2)
        }
    }
}
