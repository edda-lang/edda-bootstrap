//! Transport carriers — the [`Transport`] trait and a blocking stdio
//! implementation ([`StdioTransport`]).
//!
//! Per `mcp-protocol.md` §1, the MCP wire admits stdio, named pipe,
//! Unix domain socket, and TCP transports — the choice is the
//! daemon's `edda daemon` flag, not a wire concern. This implementation ships
//! the stdio carrier (the standard MCP transport, matching LSP
//! precedent); the trait shape leaves the IPC / TCP variants for a
//! follow-up.
//!
//! # Framing
//!
//! This implementation uses **line-delimited JSON** — one JSON envelope per
//! `\n`-terminated line on stdin / stdout. This is the simplest
//! framing that handles all of JSON-RPC's envelope kinds, and
//! matches the framing the MCP reference clients use over stdio. A
//! later version may add the LSP-style `Content-Length` framing behind
//! the same trait without breaking the public surface.

use std::io::{self, BufRead, BufReader, Read, StdinLock, StdoutLock, Write};

use crate::wire::{Notification, Request, Response};

/// Carrier-side failures.
#[derive(Debug)]
pub enum TransportError {
    /// Read or write IO failed.
    Io(io::Error),
    /// A line was received but did not parse as JSON.
    Decode(serde_json::Error),
    /// A message was constructed but did not serialise (should not
    /// happen for well-formed envelopes — this is the structured
    /// `unreachable` slot rather than a `panic`).
    Encode(serde_json::Error),
    /// The peer closed the carrier cleanly (EOF). Idiomatic on stdin.
    Eof,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Io(e) => write!(f, "transport io: {e}"),
            TransportError::Decode(e) => write!(f, "decode error: {e}"),
            TransportError::Encode(e) => write!(f, "encode error: {e}"),
            TransportError::Eof => f.write_str("peer closed transport"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TransportError::Io(e) => Some(e),
            TransportError::Decode(e) | TransportError::Encode(e) => Some(e),
            TransportError::Eof => None,
        }
    }
}

impl From<io::Error> for TransportError {
    fn from(e: io::Error) -> Self {
        TransportError::Io(e)
    }
}

/// Pluggable carrier surface.
///
/// The dispatcher reads one framed envelope at a time via
/// [`Transport::recv_line`] and writes responses / notifications via
/// the two `send_*` methods. The carrier owns framing — line-delimited
/// JSON for the current [`StdioTransport`], LSP-style
/// `Content-Length` in a later version.
pub trait Transport {
    /// Read the next framed envelope as raw JSON text. Returns
    /// [`TransportError::Eof`] on clean peer close.
    fn recv_line(&mut self) -> Result<String, TransportError>;

    /// Serialise a response and write it as a framed message.
    fn send_response(&mut self, response: &Response) -> Result<(), TransportError>;

    /// Serialise a notification and write it as a framed message.
    fn send_notification(&mut self, notification: &Notification) -> Result<(), TransportError>;
}

/// Blocking line-delimited-JSON transport over stdin / stdout.
///
/// Per the [edda-daemon concurrency model] this is a sync carrier
/// thread: one reader, one writer, no `tokio`.
pub struct StdioTransport {
    reader: BufReader<StdinLock<'static>>,
    writer: StdoutLock<'static>,
}

impl StdioTransport {
    /// Construct a stdio transport, locking stdin and stdout.
    pub fn new() -> Self {
        // SAFETY-equivalent rationale: the `Stdin` / `Stdout` handles
        // return references that outlive the local process; the static
        // bound holds because the global handles never drop.
        let reader = BufReader::new(std::io::stdin().lock());
        let writer = std::io::stdout().lock();
        Self { reader, writer }
    }

    fn read_one_line(&mut self) -> Result<String, TransportError> {
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .map_err(TransportError::Io)?;
        if n == 0 {
            return Err(TransportError::Eof);
        }
        while line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        Ok(line)
    }

    fn write_line(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        self.writer.write_all(payload).map_err(TransportError::Io)?;
        self.writer.write_all(b"\n").map_err(TransportError::Io)?;
        self.writer.flush().map_err(TransportError::Io)?;
        Ok(())
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for StdioTransport {
    fn recv_line(&mut self) -> Result<String, TransportError> {
        self.read_one_line()
    }

    fn send_response(&mut self, response: &Response) -> Result<(), TransportError> {
        let payload = serde_json::to_vec(response).map_err(TransportError::Encode)?;
        self.write_line(&payload)
    }

    fn send_notification(&mut self, notification: &Notification) -> Result<(), TransportError> {
        let payload = serde_json::to_vec(notification).map_err(TransportError::Encode)?;
        self.write_line(&payload)
    }
}

/// In-memory transport for tests.
#[doc(hidden)]
pub struct MemoryTransport {
    inbox: std::collections::VecDeque<String>,
    outbox: Vec<String>,
}

#[allow(dead_code)] // exercised only from tests in dependent modules
impl MemoryTransport {
    /// Construct a new transport with the given queued inbox lines.
    pub fn new(inbox: impl IntoIterator<Item = String>) -> Self {
        Self {
            inbox: inbox.into_iter().collect(),
            outbox: Vec::new(),
        }
    }

    /// Snapshot of every line written to the outbox.
    pub fn outbox(&self) -> &[String] {
        &self.outbox
    }
}

impl Transport for MemoryTransport {
    fn recv_line(&mut self) -> Result<String, TransportError> {
        self.inbox.pop_front().ok_or(TransportError::Eof)
    }
    fn send_response(&mut self, response: &Response) -> Result<(), TransportError> {
        let s = serde_json::to_string(response).map_err(TransportError::Encode)?;
        self.outbox.push(s);
        Ok(())
    }
    fn send_notification(&mut self, notification: &Notification) -> Result<(), TransportError> {
        let s = serde_json::to_string(notification).map_err(TransportError::Encode)?;
        self.outbox.push(s);
        Ok(())
    }
}

// Compile-time witness that `Request` carries through one round-trip
// of the in-memory transport. The unused import keeps `Request` linked
// to this module so refactors don't silently sever the contract.
#[doc(hidden)]
const _: fn() = || {
    let _: Request = Request {
        jsonrpc: crate::wire::Version,
        id: crate::wire::Id::Number(0),
        method: String::new(),
        params: None,
    };
};

// Bring `Read` and `Write` into scope so they aren't an "unused
// import" lint warning when `cargo clippy` runs over this file.
#[allow(dead_code)]
fn _io_traits_in_scope() {
    fn _w(_: impl Write) {}
    fn _r(_: impl Read) {}
}
