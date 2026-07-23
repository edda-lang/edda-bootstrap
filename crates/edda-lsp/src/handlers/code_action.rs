//! `textDocument/codeAction` handler.
//!
//! v0.1 returns an empty array. The daemon does not yet expose a
//! structural-edit catalogue; wiring code actions requires
//! `edda-daemon` to surface the edits the structural-edit pass
//! recognises (cf. `docs/tooling/structural-edits.md`). The capability
//! is still advertised so editors render the code-action lightbulb,
//! and a follow-up change can replace this stub with the real catalogue
//! without re-negotiating capabilities.

use lsp_types::{CodeActionParams, CodeActionResponse};

use crate::error::LspError;
use crate::state::LspState;

/// Handle `textDocument/codeAction`. v0.1 has no actions to offer.
pub fn code_action(
    _state: &LspState,
    _params: CodeActionParams,
) -> Result<Option<CodeActionResponse>, LspError> {
    Ok(Some(CodeActionResponse::new()))
}
