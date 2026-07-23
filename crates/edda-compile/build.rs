//! Build script for `edda-compile`.
//!
//! When the `llvm` cargo feature is enabled, this script wires the
//! final binary against `LLVM-C` — the dynamic-library C-API surface
//! that ships in vcpkg's `x64-windows` triplet of LLVM 18 (and the
//! equivalent on Linux/macOS).
//!
//! # Why this script exists
//!
//! `inkwell` pulls in `llvm-sys-180`, which by default emits
//! `cargo:rustc-link-lib=static=LLVM*` directives for every component
//! of LLVM (108 libs at 18.1.6). On Windows + MSVC + static-md triplet
//! those directives bundle ~5 GB of native libs into the `llvm-sys`
//! `.rlib`, which both `link.exe` (LNK4003 "invalid library format")
//! and `rust-lld` ("truncated or malformed archive") refuse to load.
//!
//! The workaround is the `no-llvm-linking` feature on `llvm-sys-180`
//! (re-exported by inkwell as `llvm18-0-no-llvm-linking`): llvm-sys
//! still calls `llvm-config` for cppflags / wrapper compilation but
//! does **not** emit any `rustc-link-lib` directives — leaving the
//! linking to us. We then emit a single
//! `cargo:rustc-link-lib=dylib=LLVM-C` against `LLVM-C.lib` (the
//! ~270 KB import lib for `LLVM-C.dll`).
//!
//! # Runtime expectation
//!
//! At runtime the binary loads `LLVM-C.dll`, so the directory holding
//! it (`$LLVM_SYS_180_PREFIX/bin` on Windows) must be on `PATH`. For
//! `cargo test --features llvm`, set `PATH` to include that directory
//! before invoking cargo.

fn main() {
    println!("cargo:rerun-if-env-changed=LLVM_SYS_180_PREFIX");
    println!("cargo:rerun-if-changed=build.rs");

    if !cfg!(feature = "llvm") {
        return;
    }

    let prefix = std::env::var("LLVM_SYS_180_PREFIX").expect(
        "LLVM_SYS_180_PREFIX must be set when --features llvm is enabled. \
         Point it at an LLVM 18 install root (e.g., \
         <vcpkg-root>/installed/x64-windows) — the directory must contain \
         bin/llvm-config.exe (for llvm-sys's cppflags discovery) and \
         lib/LLVM-C.lib (for the link step below).",
    );

    println!("cargo:rustc-link-search=native={prefix}/lib");
    println!("cargo:rustc-link-lib=dylib=LLVM-C");
}
