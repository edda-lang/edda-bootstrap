//! `textDocument/hover` handler.
//!
//! v0.1 returns `None` — hover text requires a resolved-binding query
//! the daemon does not yet expose. The handler is wired so the
//! capability advertisement (`hover_provider: true`) does not 404; a
//! later change will fill in resolved-type / signature / doc text once
//! `edda-daemon` adds a `binding_at(file, pos)` query.

use lsp_types::{Hover, HoverParams};

use crate::error::LspError;
use crate::state::LspState;

/// Handle `textDocument/hover`. v0.1 returns no information.
pub fn hover(_state: &LspState, _params: HoverParams) -> Result<Option<Hover>, LspError> {
    Ok(None)
}
