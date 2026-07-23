//! Per-connection session state.
//!
//! A [`Session`] tracks handshake completion, the negotiated feature
//! set, and the in-flight request set (for `client.cancel`). Dispatch
//! is sync and single-threaded on the carrier thread so the in-flight
//! set is a [`std::collections::HashSet`] under no lock; a future
//! version will wrap it in [`parking_lot::Mutex`] when worker threads land.

use std::collections::HashSet;

use crate::handshake::{FeatureMap, SessionFeatures};
use crate::wire::Id;

/// Per-connection state. One [`Session`] per MCP connection.
#[derive(Debug, Default)]
pub struct Session {
    /// `true` once `client.handshake` has completed successfully.
    /// Re-issuing the handshake is idempotent per `mcp-protocol.md` §3.
    pub(crate) handshake_complete: bool,
    /// Negotiated feature set. Meaningful only after handshake.
    pub(crate) features: SessionFeatures,
    /// Free-form client identity from the handshake (for diagnostics).
    pub(crate) client_name: String,
    /// Free-form client version from the handshake (for diagnostics).
    pub(crate) client_version: String,
    /// Requests currently being serviced. Dispatch currently handles one at a
    /// time on the carrier thread so this is always either empty or a
    /// single-id set; a future version with worker threads gains real
    /// concurrency.
    pub(crate) in_flight: HashSet<String>,
}

impl Session {
    /// Construct a fresh session. No handshake has been performed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the session has completed `client.handshake`.
    pub fn is_handshake_complete(&self) -> bool {
        self.handshake_complete
    }

    /// The negotiated feature set. Default-valued until the handshake
    /// completes.
    pub fn features(&self) -> SessionFeatures {
        self.features
    }

    /// The client's self-reported name (free-form text from the
    /// handshake). Empty before the handshake completes.
    pub fn client_name(&self) -> &str {
        &self.client_name
    }

    /// The client's self-reported version. Empty before the handshake
    /// completes.
    pub fn client_version(&self) -> &str {
        &self.client_version
    }

    /// Record a successful handshake.
    pub(crate) fn complete_handshake(
        &mut self,
        client_name: String,
        client_version: String,
        client_features: &FeatureMap,
        server_features: &FeatureMap,
    ) {
        self.handshake_complete = true;
        self.client_name = client_name;
        self.client_version = client_version;
        self.features = SessionFeatures::negotiate(client_features, server_features);
    }

    /// Mark a request as in-flight (called on dispatch).
    pub(crate) fn enter_request(&mut self, id: &Id) {
        self.in_flight.insert(id.as_string());
    }

    /// Mark a request as complete (called on response).
    pub(crate) fn exit_request(&mut self, id: &Id) {
        self.in_flight.remove(&id.as_string());
    }

    /// Whether the named request is currently in-flight.
    pub fn is_in_flight(&self, id: &Id) -> bool {
        self.in_flight.contains(&id.as_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handshake::FeatureMap;

    #[test]
    fn fresh_session_is_pre_handshake() {
        let s = Session::new();
        assert!(!s.is_handshake_complete());
        assert!(!s.features.streaming);
    }

    #[test]
    fn complete_handshake_negotiates_features() {
        let mut s = Session::new();
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
        s.complete_handshake("test".to_string(), "0.0".to_string(), &client, &server);
        assert!(s.is_handshake_complete());
        assert!(!s.features.streaming);
        assert!(s.features.cancellation);
    }

    #[test]
    fn in_flight_tracking() {
        let mut s = Session::new();
        let id = Id::Number(7);
        assert!(!s.is_in_flight(&id));
        s.enter_request(&id);
        assert!(s.is_in_flight(&id));
        s.exit_request(&id);
        assert!(!s.is_in_flight(&id));
    }
}
