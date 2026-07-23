//! Stdlib source-of-truth selection: env override, vendored path, worktree
//! and sibling discovery, the precedence resolver, and operator diagnostics.

mod diagnostics;
mod discover;
mod resolve;

pub(crate) use diagnostics::emit_stdlib_source_selection;
pub(crate) use discover::env_stdlib_override;
pub(crate) use resolve::resolve_stdlib_source;

#[cfg(test)]
mod tests;
