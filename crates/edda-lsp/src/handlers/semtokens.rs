//! `textDocument/semanticTokens/full` handler.
//!
//! Re-lexes the file's current text (either the daemon's overlay copy
//! or the LSP state's text cache) and emits the LSP delta-encoded
//! semantic-token stream from the locked token catalogue.

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::{FileId, SourceMap};
use lsp_types::{
    SemanticTokens, SemanticTokensParams, SemanticTokensResult,
};

use crate::error::LspError;
use crate::semtokens::encode_semantic_tokens;
use crate::state::LspState;
use crate::uri::uri_to_path;

/// Handle `textDocument/semanticTokens/full`. Returns
/// [`SemanticTokensResult::Tokens`] with the file's lex-derived tokens.
pub fn semantic_tokens_full(
    state: &LspState,
    params: SemanticTokensParams,
) -> Result<Option<SemanticTokensResult>, LspError> {
    let path = uri_to_path(&params.text_document.uri)?;
    let Some(text) = state.cached_text(&path) else {
        // No overlay open for this file; return an empty token array
        // rather than failing the request. Clients tolerate empty
        // responses.
        return Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: Vec::new(),
        })));
    };

    let tokens = lex_text(&text);
    let data = encode_semantic_tokens(&text, &tokens, state.encoding());
    Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data,
    })))
}

/// Run the Edda lexer over `text` with throwaway interner / source map.
fn lex_text(text: &str) -> Vec<edda_syntax::Lexed> {
    let source_map = SourceMap::new();
    let file_id: FileId = source_map.add_file(
        std::path::PathBuf::from("(lsp-semantic-tokens)"),
        text.to_string(),
    );
    let interner = Interner::new();
    let mut diags = Diagnostics::new();
    let cfg = LintConfig::new();
    edda_syntax::lex(text, file_id, &interner, &mut diags, &cfg)
}
