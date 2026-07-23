//! Long-lived compiler-as-service surface around `edda-driver`.
//!
//! # Current state
//!
//! As of 2026-05-18 the daemon ships the in-memory project state, the
//! read-only query surface, and the document overlay layer (open /
//! close / version-tracked edits). The daemon owns one
//! [`edda_driver::Driver`] per active project, drives the parse +
//! import-resolve prefix of the cascade through
//! [`edda_driver::run_cascade`], partitions diagnostics by
//! primary-span file, and exposes lookups by filesystem path through
//! [`query`].
//!
//! Still owed: filesystem watcher, persistent index under
//! `.edda/cache/index/`, transport dispatch, and invalidation fanout.
//! The running history for any specific item lives in
//! `git log --oneline crates/edda-daemon/`.
//!
//! Per the corpus's "compiler is part of the language" framing
//! (CHARTER.md Article VIII). The `edda-lsp` and `edda-mcp` crates
//! layer concrete protocols on top of this daemon; this crate's
//! public surface stays transport-agnostic (no JSON-RPC, LSP, or MCP
//! types reach in).
//!
//! Implements: `corpus/edda-codex/language/06-tooling.md` §
//! compiler-as-service.

mod daemon;
mod document;
mod error;
pub mod query;
mod reporter;
mod state;

pub use daemon::Daemon;
pub use document::{DocumentParseResult, DocumentVersion};
pub use error::DaemonError;
