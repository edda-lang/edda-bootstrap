//! JSON-RPC 2.0 envelope types — `Request`, `Response`, `Notification`.
//!
//! Per `mcp-protocol.md` §2, every message on the MCP wire is exactly
//! one of three envelope kinds. The structural discrimination follows
//! the JSON-RPC 2.0 spec:
//!
//! - **Request**: `{"jsonrpc":"2.0","id":...,"method":"...","params":{...}}`
//! - **Response**: `{"jsonrpc":"2.0","id":...,"result":{...}}` or
//!   `{"jsonrpc":"2.0","id":...,"error":{...}}` — exactly one of
//!   `result` / `error`.
//! - **Notification**: `{"jsonrpc":"2.0","method":"...","params":{...}}`
//!   with no `id`. No response is sent.
//!
//! The types here are the load-bearing wire surface. They are
//! `serde::{Serialize,Deserialize}` and round-trip against
//! [`serde_json`]; changing a field name, the `jsonrpc` literal, or the
//! `Id` admitted shapes is a wire break.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Client-allocated request identifier.
///
/// Per JSON-RPC 2.0 the server must echo the request's `id` verbatim in
/// the response, with no normalisation. Edda admits the two non-null
/// shapes the spec lists: integer (compact, recommended for tools) and
/// string (human-readable for tracing).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// Integer id, e.g. `42`. Stored as `i64` to admit the negative
    /// integers some clients use.
    Number(i64),
    /// String id, e.g. `"req-001"`. Recommended for human traces.
    String(String),
}

impl Id {
    /// Render the id as the canonical human string. Used in
    /// notifications that carry `request_id` (streaming chunks,
    /// cancellation).
    pub fn as_string(&self) -> String {
        match self {
            Id::Number(n) => n.to_string(),
            Id::String(s) => s.clone(),
        }
    }
}

/// The JSON-RPC version tag. Always serialises as the literal `"2.0"`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version;

impl Version {
    /// The single admitted version literal.
    pub const LITERAL: &'static str = "2.0";
}

impl From<Version> for String {
    fn from(_: Version) -> Self {
        Version::LITERAL.to_string()
    }
}

impl TryFrom<String> for Version {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == Version::LITERAL {
            Ok(Version)
        } else {
            Err(format!(
                "jsonrpc version must be exactly \"{}\", got {value:?}",
                Version::LITERAL
            ))
        }
    }
}

/// JSON-RPC 2.0 request envelope.
///
/// A request expects exactly one response carrying the same `id`. Per
/// `mcp-protocol.md` §2, unrecognised fields in `params` are rejected
/// with the error class `arg_shape_invalid` (mapped via
/// [`crate::error::ErrorClass`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    /// Always `"2.0"` per JSON-RPC 2.0 §1.
    pub jsonrpc: Version,
    /// Client-allocated identifier. Echoed verbatim in the response.
    pub id: Id,
    /// Operation name, e.g. `"build.typecheck"`. See [`crate::methods`].
    pub method: String,
    /// Operation-specific arguments. The shape is fixed per the
    /// `mcp-protocol.md` §§5-10 tables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 response envelope.
///
/// The response carries either a `result` (success) or an `error`
/// (failure); never both. Diagnostics that did not fail the operation
/// (warnings, info, lint output) ride alongside in the `diagnostics`
/// field of the per-operation `result` shape, not in the `error` block —
/// see `mcp-protocol.md` §2.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: Version,
    /// Mirrors the request's `id` byte-for-byte.
    pub id: Id,
    /// Success payload. Mutually exclusive with `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Failure payload. Mutually exclusive with `result`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

impl Response {
    /// Construct a success response with the operation's `result`.
    pub fn success(id: Id, result: Value) -> Self {
        Self {
            jsonrpc: Version,
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Construct a failure response with a structured error.
    pub fn failure(id: Id, error: ErrorObject) -> Self {
        Self {
            jsonrpc: Version,
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// JSON-RPC 2.0 notification envelope.
///
/// Notifications flow in both directions and never receive a response.
/// Edda uses them for `stream.chunk` (server → client) and
/// `client.cancel` (client → server).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: Version,
    /// Notification method name.
    pub method: String,
    /// Method-specific payload. May be absent for parameter-less
    /// notifications (none yet locked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Construct a notification with a typed payload.
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: Version,
            method: method.into(),
            params: Some(params),
        }
    }
}

/// JSON-RPC 2.0 error block, extended with Edda's structured fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorObject {
    /// Integer code per [`crate::error::ErrorCode`]. JSON-RPC standard
    /// codes (parse_error, invalid_request, ...) use the spec-defined
    /// negative integers; Edda-specific codes use `-32000` to `-32099`.
    pub code: i32,
    /// Human-readable one-line summary. Not a stable contract — clients
    /// dispatch on `class`, not `message`.
    pub message: String,
    /// Canonical Edda error class. See [`crate::error::ErrorClass`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Structured locator (module, file, function, item). Shape mirrors
    /// `structural-edits.md` §2.4's target. Free-form `Value` because
    /// the spec admits per-class target shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Value>,
    /// Structured fix suggestions. Free-form per class.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<Value>,
    /// Optional `streaming` sub-object for `cancelled` errors during
    /// streamed requests (`mcp-protocol.md` §12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<StreamingErrorInfo>,
}

/// Streaming-side detail attached to a `cancelled` error for streamed
/// requests. Carries the count and last chunk index emitted before the
/// cancellation was processed, plus a `result_partial: true` flag.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StreamingErrorInfo {
    /// Total chunks emitted by the server before the abort.
    pub chunks_emitted: u32,
    /// `chunk_index` of the last emitted chunk (or `None` if none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_chunk_index: Option<u32>,
    /// Always `true` for streaming `cancelled` errors. Wire-locked so
    /// clients can match on it directly.
    pub result_partial: bool,
}

/// Payload of a `stream.chunk` notification.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StreamChunkPayload {
    /// The id of the request whose result is being streamed.
    pub request_id: Id,
    /// 0-based monotonically increasing chunk index for this request.
    pub chunk_index: u32,
    /// Additive partial of the operation's `result` shape. Absent on
    /// the final chunk (`final: true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_result: Option<Value>,
    /// `true` exactly on the last chunk for this request. After
    /// `final: true` no further chunks for this `request_id` will
    /// arrive; the server emits the canonical response carrying the
    /// complete `result` next.
    #[serde(rename = "final")]
    pub finished: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_round_trips_only_on_2_0() {
        let v = serde_json::to_value(Version).unwrap();
        assert_eq!(v, serde_json::Value::String("2.0".to_string()));
        let parsed: Version = serde_json::from_value(v).unwrap();
        let _ = parsed; // unit struct equality is trivial
        let bad = serde_json::Value::String("1.0".to_string());
        assert!(serde_json::from_value::<Version>(bad).is_err());
    }

    #[test]
    fn request_round_trips_with_string_id() {
        let req = Request {
            jsonrpc: Version,
            id: Id::String("req-001".to_string()),
            method: "build.typecheck".to_string(),
            params: Some(serde_json::json!({"project_root": "/tmp"})),
        };
        let bytes = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&bytes).unwrap();
        assert_eq!(back.id, Id::String("req-001".to_string()));
        assert_eq!(back.method, "build.typecheck");
    }

    #[test]
    fn response_omits_absent_field() {
        let r = Response::success(Id::Number(7), serde_json::json!({"ok": true}));
        let bytes = serde_json::to_string(&r).unwrap();
        assert!(bytes.contains("\"result\""));
        assert!(!bytes.contains("\"error\""));
    }

    #[test]
    fn id_round_trips_negative_integer() {
        let id = Id::Number(-3);
        let v = serde_json::to_value(&id).unwrap();
        let back: Id = serde_json::from_value(v).unwrap();
        assert_eq!(back, id);
    }
}
