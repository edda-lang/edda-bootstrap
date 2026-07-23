//! Error types for registry fetch operations.

/// Errors that can occur while fetching from a Mímir registry.
#[derive(Debug)]
pub enum FetchError {
    /// The server returned a non-2xx HTTP status.
    Http {
        /// The HTTP status code received.
        status: u16,
        /// Human-readable message from the response body (best-effort).
        msg: String,
    },
    /// Transport-level failure: connection refused, DNS failure, timeout, etc.
    Network(String),
    /// The requested package/version was not found (HTTP 404 or missing file).
    NotFound,
    /// The index entry JSON failed to parse.
    MalformedIndex(String),
    /// A filesystem I/O error occurred (file-mode sources only).
    Io(std::io::Error),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Http { status, msg } => {
                write!(f, "HTTP error {status}: {msg}")
            }
            FetchError::Network(msg) => write!(f, "network error: {msg}"),
            FetchError::NotFound => write!(f, "not found"),
            FetchError::MalformedIndex(msg) => write!(f, "malformed index: {msg}"),
            FetchError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for FetchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FetchError::Io(e) => Some(e),
            _ => None,
        }
    }
}
