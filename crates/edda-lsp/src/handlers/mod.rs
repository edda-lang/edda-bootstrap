//! Per-method LSP request and notification handlers.
//!
//! Each submodule owns one LSP method (or a tight cluster — `didOpen` /
//! `didChange` / `didClose` / `didSave` ship together in `sync`). The
//! server's dispatch loop (`crate::server`) is a flat match on method
//! name that fans out into these handlers.

pub mod code_action;
pub mod completion;
pub mod hover;
pub mod semtokens;
pub mod sync;
