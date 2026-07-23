//! Network capability placeholder type.

/// Documents the spec's "Network capability only" requirement at the API level.
///
/// This is a placeholder type — the bootstrap doesn't yet enforce capability
/// typestate at the Rust level. When `Network` lands as a Rust-side capability
/// type, replace this with the real one. Until then, callers pass `NetworkCap`
/// to make the requirement visible at every call site.
#[derive(Debug, Clone, Copy)]
pub struct NetworkCap;
