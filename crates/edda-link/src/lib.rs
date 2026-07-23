//! Linker and archiver driver with section-level dead-code elimination.
//!
//! Selects mold for Linux→Linux links, `ld.lld` for Linux cross-link,
//! `ld64.lld` for macOS, `lld-link` for Windows MSVC, and `wasm-ld`
//! for WebAssembly (`docs/tooling/build-system.md` §5b). Static
//! archives are produced by `llvm-ar` (Unix `.a` / wasm objects) or
//! `llvm-lib` (Microsoft `.lib`). Drives `--gc-sections` /
//! `-dead_strip` / `/OPT:REF,/OPT:ICF` so unused symbols are stripped
//! at link time; combined with reachability-driven codegen the final
//! binary has no bigger surface than its hand-written equivalent.
//!
//! Implements: `docs/tooling/build-system.md` §5b.
//!
//! # Crate layout
//!
//! - [`linker`] — `Linker` enum, target → linker selection.
//! - [`archiver`] — `Archiver` enum, target → archiver selection.
//! - [`tool`] — `Tool` enum unifying the two, shared `$PATH` discovery.
//! - [`output`] — `OutputKind`, `LibKind`, `LibSpec`.
//! - [`plan`] — `LinkPlan` and per-tool argv construction.
//! - [`invoke`] — `run`, `LinkOutcome`, process spawn + capture.
//! - [`import`] — PE import-library synthesis for `extern ... from "dll"`.
//! - [`error`] — `LinkError` and its `edda-diag` mapping.
//! - [`resolve`] — pre-link `__edda_*` runtime-extern gate.
//!
//! # Scope cuts (deferred)
//!
//! - LTO bitcode injection (`-flto` / `-Wl,-mllvm`). v0.1 uses
//!   whole-program LTO bounded by reachability-driven codegen; v1.0
//!   moves to ABI-domain ThinLTO per `backend-choice.md` §8.
//! - Explicit cross-arch flags (`-m`, `/MACHINE`, `-arch`). Object
//!   files carry the target arch in their headers and lld infers from
//!   that; if testing reveals a gap, callers can supply the flag via
//!   `extra_args` until a follow-up models it.

mod archiver;
mod error;
mod import;
mod invoke;
mod linker;
mod output;
mod plan;
mod resolve;
mod tool;

pub use archiver::Archiver;
pub use error::{LinkError, STDERR_EXCERPT_LIMIT};
pub use import::{ImportLibSpec, synthesize_import_lib};
pub use invoke::{LinkOutcome, run};
pub use linker::Linker;
pub use output::{LibKind, LibSpec, OutputKind};
pub use plan::LinkPlan;
pub use resolve::{RUNTIME_EXTERN_PREFIX, RuntimeExternReport, resolve_runtime_externs};
pub use tool::Tool;
