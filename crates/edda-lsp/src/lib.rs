//! Language Server Protocol surface over the edda-daemon.
//!
//! Maps LSP requests onto daemon queries: text-document/diagnostics
//! from the typecheck pass, completion from the structural index,
//! semantic tokens from the parser. Owns the semantic-token legend and
//! the editor-side caching contract.
//!
//! # Architecture
//!
//! - [`server::run_stdio`] is the canonical entry point: stdin/stdout
//!   transport using [`lsp_server::Connection`], no async runtime. Per
//!   project memory, the daemon's concurrency model is sync core +
//!   parking_lot + worker threads + crossbeam channels; LSP over stdio
//!   matches that pattern (no `tokio`).
//! - [`state::LspState`] wraps a single [`edda_daemon::Daemon`] plus the
//!   negotiated [`position::PositionEncoding`] and per-document version
//!   bookkeeping.
//! - [`handlers`] holds one submodule per LSP method (or per tight
//!   cluster, like the four `did{Open,Change,Close,Save}` notifications
//!   in [`handlers::sync`]).
//! - [`position`] owns the UTF-8 ↔ UTF-16 ↔ codepoint conversion. LSP
//!   defaults to UTF-16; the server prefers UTF-8 because
//!   [`edda_span::BytePos`] is UTF-8 byte-based.
//! - [`diagnostics`] maps [`edda_diag::Diagnostic`] onto the LSP
//!   `Diagnostic` shape.
//! - [`semtokens`] owns the semantic-token legend and the
//!   delta-encoded wire format.
//! - [`completion`] holds the locked keyword catalogue used by
//!   [`handlers::completion`] for v0.1 completions.
//!
//! # Coverage in v0.1
//!
//! - `initialize` / `initialized` / `shutdown` / `exit` lifecycle
//!   (handled by the dispatch loop in [`server`]).
//! - `textDocument/didOpen` / `didChange` / `didClose` / `didSave`
//!   ([`handlers::sync`]).
//! - `textDocument/publishDiagnostics` push after every overlay
//!   transition.
//! - `textDocument/semanticTokens/full` ([`handlers::semtokens`]).
//! - `textDocument/completion` ([`handlers::completion`]).
//! - `textDocument/hover` — capability advertised but
//!   [`handlers::hover`] returns `None` until the daemon adds a
//!   `binding_at(file, pos)` query.
//! - `$/cancelRequest` — handled implicitly by `lsp_server::Connection`.
//!
//! # Deferred
//!
//! - `textDocument/codeAction` — the daemon does not yet expose a
//!   structural-edit catalogue. Returning an empty array when asked is
//!   the v0.1 surface; the capability is advertised so a follow-up
//!   can wire it without re-negotiating.
//! - Resolver-precise completion (identifiers, types, scoped bindings)
//!   awaits an [`edda_daemon`] `completion_at(file, pos)` query.
//! - Multi-line semantic tokens: the lexer can emit cross-line spans
//!   (block string literals); v0.1 drops those tokens rather than
//!   splitting per line.
//!
//! Implements: `docs/tooling/lsp-mapping.md`.

mod capabilities;
mod completion;
mod diagnostics;
mod error;
mod handlers;
mod position;
mod semtokens;
mod server;
mod state;
mod uri;

pub use error::LspError;
pub use position::PositionEncoding;
pub use server::{ServerError, run_stdio};
pub use state::LspState;
