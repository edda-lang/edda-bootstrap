//! `client.handshake` shapes and the negotiated feature set.
//!
//! Per `mcp-protocol.md` §3 the handshake locks the protocol version
//! and the feature set for the rest of the session. Both sides
//! announce the version range and the features they support; the
//! negotiated values are the intersection (or the higher of the
//! versions, per §3's "negotiated version is the highest version both
//! parties announce").

use serde::{Deserialize, Serialize};

use crate::methods;

/// The single protocol version this implementation supports.
pub const NEGOTIATED_PROTOCOL_VERSION: u32 = 1;

/// Server-side identity as it appears in the handshake response.
pub const SERVER_NAME: &str = "edda-daemon";

/// `params` shape of `client.handshake`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandshakeParams {
    /// Free-form client identity (e.g. `"edda-llm-agent"`).
    pub client_name: String,
    /// Free-form client version (e.g. `"0.3.0"`).
    pub client_version: String,
    /// Protocol versions the client speaks. The server picks the
    /// highest version both sides announce.
    pub protocol_versions: Vec<u32>,
    /// Features the client supports.
    #[serde(default)]
    pub features: FeatureMap,
}

/// Client / server feature declaration map.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FeatureMap {
    /// Whether the party supports `stream.chunk` notifications.
    #[serde(default)]
    pub streaming: bool,
    /// Whether the party supports `client.cancel`.
    #[serde(default)]
    pub cancellation: bool,
    /// Whether the party admits the `edit.*` namespace.
    #[serde(default)]
    pub structural_edits: bool,
    /// Whether the party admits the `auto_redirect` option on edits.
    #[serde(default)]
    pub auto_redirect: bool,
    /// Reserved binary-payloads fast path; both sides ship `false`
    /// until the binary encoding is admitted.
    #[serde(default)]
    pub binary_payloads: bool,
}

/// `result` shape of `client.handshake`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandshakeResult {
    /// Always `"edda-daemon"` per `mcp-protocol.md` §3.
    pub server_name: String,
    /// Server's semver (the crate's `CARGO_PKG_VERSION`).
    pub server_version: String,
    /// Negotiated protocol version (the max of the intersection).
    pub protocol_version: u32,
    /// Supported namespaces (the seven §4 namespaces this build admits).
    pub supported_namespaces: Vec<String>,
    /// Every method this server admits at the negotiated version.
    pub supported_operations: Vec<String>,
    /// Operations admitted as streamable per `mcp-protocol.md` §11.
    pub streamable_operations: Vec<String>,
    /// Negotiated feature set (per-feature logical AND).
    pub features: FeatureMap,
}

/// The post-handshake feature set this session operates with.
#[derive(Copy, Clone, Debug, Default)]
pub struct SessionFeatures {
    /// Streaming chunk notifications are admitted.
    pub streaming: bool,
    /// `client.cancel` is admitted.
    pub cancellation: bool,
    /// `edit.*` namespace is admitted.
    pub structural_edits: bool,
    /// `auto_redirect` option on edits is admitted.
    pub auto_redirect: bool,
}

impl SessionFeatures {
    /// Compute the negotiated session features from the client's and
    /// server's declarations.
    pub fn negotiate(client: &FeatureMap, server: &FeatureMap) -> Self {
        Self {
            streaming: client.streaming && server.streaming,
            cancellation: client.cancellation && server.cancellation,
            structural_edits: client.structural_edits && server.structural_edits,
            auto_redirect: client.auto_redirect && server.auto_redirect,
        }
    }

    /// Project to the wire form for inclusion in [`HandshakeResult::features`].
    pub fn to_feature_map(self) -> FeatureMap {
        FeatureMap {
            streaming: self.streaming,
            cancellation: self.cancellation,
            structural_edits: self.structural_edits,
            auto_redirect: self.auto_redirect,
            binary_payloads: false,
        }
    }
}

/// The server's static feature declarations.
pub const SERVER_FEATURES: FeatureMap = FeatureMap {
    streaming: false,
    cancellation: true,
    structural_edits: true,
    auto_redirect: false,
    binary_payloads: false,
};

/// The seven locked namespaces per `mcp-protocol.md` §4.
pub const SUPPORTED_NAMESPACES: &[&str] = &[
    "build", "codegen", "inspect", "edit", "typecheck", "layout", "client",
];

/// The flat list of method names this server admits.
pub fn supported_operations() -> Vec<String> {
    let mut ops = Vec::with_capacity(64);
    ops.push(methods::client::HANDSHAKE.to_string());
    ops.push(methods::client::CANCEL.to_string());
    ops.push(methods::client::OPEN_PROJECT.to_string());
    ops.push(methods::client::CLOSE_PROJECT.to_string());
    ops.push(methods::client::OPEN_DOCUMENT.to_string());
    ops.push(methods::client::APPLY_CHANGE.to_string());
    ops.push(methods::client::CLOSE_DOCUMENT.to_string());
    ops.push(methods::client::SERVER_INFO.to_string());
    ops.push(methods::build::COMPILE.to_string());
    ops.push(methods::build::TYPECHECK.to_string());
    ops.push(methods::build::RUN.to_string());
    ops.push(methods::build::TEST.to_string());
    ops.push(methods::build::BENCH.to_string());
    ops.push(methods::build::FORMAT.to_string());
    ops.push(methods::build::LINT.to_string());
    ops.push(methods::build::CLEAN.to_string());
    ops.push(methods::codegen::PROMOTE.to_string());
    ops.push(methods::codegen::DEMOTE.to_string());
    ops.push(methods::codegen::REGENERATE.to_string());
    ops.push(methods::codegen::GC.to_string());
    ops.push(methods::codegen::FULL_HASH.to_string());
    ops.push(methods::inspect::PARSED_AST.to_string());
    ops.push(methods::inspect::DIAGNOSTICS.to_string());
    ops.push(methods::typecheck::TYPE_AT.to_string());
    ops.push(methods::edit::DECLARATION_RENAME.to_string());
    ops.push(methods::layout::SIZE_OF.to_string());
    ops
}

/// Methods marked streamable per `mcp-protocol.md` §11. Currently
/// returns an empty list (the server does not yet stream anything);
/// the wire surface is committed.
pub fn streamable_operations() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_is_one() {
        assert_eq!(NEGOTIATED_PROTOCOL_VERSION, 1);
    }

    #[test]
    fn feature_negotiation_is_logical_and() {
        let client = FeatureMap {
            streaming: true,
            cancellation: true,
            structural_edits: false,
            auto_redirect: true,
            binary_payloads: false,
        };
        let server = FeatureMap {
            streaming: false,
            cancellation: true,
            structural_edits: true,
            auto_redirect: false,
            binary_payloads: false,
        };
        let neg = SessionFeatures::negotiate(&client, &server);
        assert!(!neg.streaming);
        assert!(neg.cancellation);
        assert!(!neg.structural_edits);
        assert!(!neg.auto_redirect);
    }

    #[test]
    fn supported_namespaces_match_spec() {
        assert_eq!(SUPPORTED_NAMESPACES.len(), 7);
        for ns in ["build", "codegen", "inspect", "edit", "typecheck", "layout", "client"] {
            assert!(SUPPORTED_NAMESPACES.contains(&ns), "missing namespace {ns}");
        }
    }
}
