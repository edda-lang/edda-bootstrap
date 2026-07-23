//! TCP socket runtime — the `__edda_net_*` C-ABI exports the stdlib
//! `std.net.socket` surface binds via `@abi(...)`.
//!
//! Backing model: every live [`TcpStream`] / [`TcpListener`] is stored in
//! a global handle registry; the `u64` handle inside the Edda
//! `Connection` / `Listener` record is a 1-based slot index. `close` /
//! `close_listener` drain the slot. Blocking ops (`accept` / `read` /
//! `write`) operate on a `try_clone`d handle so the global registry lock
//! is never held across a syscall — an accept loop in one connection
//! does not stall every other socket.
//!
//! Wire ABI (matches `edda-mir` / `edda-compile` lowering, cross-checked
//! against the `__edda_fs_*` family):
//!
//! - A function `-> T with {err: SocketError}` returns the synthesized
//!   `{ tag: u8, pad, payload }` sum where tag 0 = Ok(T), tag 1 =
//!   Err(SocketError). All five err-bearing shapes here are 32 bytes
//!   (the 24-byte [`EdSocketError`] payload dominates), returned by value
//!   → win64 hidden-sret, matching the compiler's aggregate-return ABI.
//! - `mutable Connection` / `mutable Listener` params lower to a `ptr` to
//!   the caller's slot (B19) → the handle is `*mut u64`.
//! - `take Connection` / `take Listener` is a single-field record passed
//!   by value → lowers to its scalar `u64` field, matching the compiler's
//!   single-field-record-to-scalar ABI for handle newtypes.
//! - `host: String` / `data: [u8]` (`let` mode) pass by value as the
//!   16-byte `EdStr` / `EdSlice` fat pointer (win64 byval, as `__edda_fs_*`).
//!
//! Memory: the `dns_lookup_failed(host)` payload `EdStr` is heap-leaked
//! per the crate's no-dealloc-ABI rule.
//!
//! `#[link(name = "ws2_32")]` is emitted on Windows so lld-link pulls the
//! winsock import library — `std::net` bottoms out in it.

mod io;
mod lifecycle;
mod narrow;
mod options;

use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;

use std::mem::ManuallyDrop;

use crate::{EdStr, alloc_edstr};

// Emit `/DEFAULTLIB:ws2_32` into the object so lld-link resolves the
// winsock symbols `std::net` references when this static lib is linked.
#[cfg(windows)]
#[link(name = "ws2_32")]
unsafe extern "C" {}

// =====================================================================
// SocketError wire shape
// =====================================================================
//
// `SocketError` is a 15-variant sum (order matches the declaration in
// `stdlib/lib/net/socket/src/socket.ea`): 13 unit variants, 1
// String-carrying variant (`dns_lookup_failed`), 1 i32-carrying variant
// (`os_error`). Max payload = String (16-byte fat pointer, align 8); tag
// is u8. The locked sum-ADT layout puts the tag at offset 0, pads to the
// payload's 8-byte alignment, then a 16-byte payload slot — total 24
// bytes, 8-aligned (same shape as `EdFsError`).

// SocketError discriminants. Order is locked to the source declaration.
pub(super) const SOCK_CONNECTION_REFUSED: u8 = 0;
pub(super) const SOCK_CONNECTION_RESET: u8 = 1;
pub(super) const SOCK_CONNECTION_ABORTED: u8 = 2;
pub(super) const SOCK_HOST_UNREACHABLE: u8 = 3;
pub(super) const SOCK_NETWORK_UNREACHABLE: u8 = 4;
pub(super) const SOCK_TIMEOUT: u8 = 5;
pub(super) const SOCK_ADDRESS_IN_USE: u8 = 6;
pub(super) const SOCK_ADDRESS_NOT_AVAILABLE: u8 = 7;
pub(super) const SOCK_DNS_LOOKUP_FAILED: u8 = 8;
pub(super) const SOCK_BROKEN_PIPE: u8 = 9;
pub(super) const SOCK_WOULD_BLOCK: u8 = 10;
pub(super) const SOCK_NOT_CONNECTED: u8 = 11;
pub(super) const SOCK_UNEXPECTED_EOF: u8 = 12;
pub(super) const SOCK_OUT_OF_MEMORY: u8 = 13;
pub(super) const SOCK_OS_ERROR: u8 = 14;

// SocketError sum: `{ tag: u8, pad: [u8;7], payload: union<EdStr, i32, ()> }`.
#[repr(C)]
pub(super) struct EdSocketError {
    sock_tag: u8,
    _pad: [u8; 7],
    payload: EdSocketErrorPayload,
}

#[repr(C)]
union EdSocketErrorPayload {
    host: ManuallyDrop<EdStr>,
    code: i32,
    _unit: ManuallyDrop<()>,
}

// Result<u64-ish, SocketError> wire shape. Covers every err-bearing
// return here: `Connection`/`Listener` (their single `handle: u64` field),
// `usize` (read/write counts), and `()` (set_*/shutdown — the Ok payload
// is unread, so `ok = 0`). Max payload = max(u64=8, EdSocketError=24) = 24.
#[repr(C)]
pub(super) struct ResultSock {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultSockPayload,
}

#[repr(C)]
union ResultSockPayload {
    ok: u64,
    err: ManuallyDrop<EdSocketError>,
}

/// Build an `Ok(handle/count)` result (`tag = 0`).
pub(super) fn ok_u64(value: u64) -> ResultSock {
    ResultSock {
        tag: 0,
        _pad: [0; 7],
        payload: ResultSockPayload { ok: value },
    }
}

/// Build an `Ok(())` result — Ok payload is zero and never read.
pub(super) fn ok_unit() -> ResultSock {
    ok_u64(0)
}

/// Build an `Err(SocketError)` result (`tag = 1`).
pub(super) fn err_result(e: EdSocketError) -> ResultSock {
    ResultSock {
        tag: 1,
        _pad: [0; 7],
        payload: ResultSockPayload { err: ManuallyDrop::new(e) },
    }
}

/// A unit-payload SocketError (every variant except the two with payloads).
pub(super) fn sock_err(tag: u8) -> EdSocketError {
    EdSocketError {
        sock_tag: tag,
        _pad: [0; 7],
        payload: EdSocketErrorPayload { _unit: ManuallyDrop::new(()) },
    }
}

/// A `dns_lookup_failed(host)` SocketError; `host` is heap-leaked.
pub(super) fn sock_err_dns(host: &str) -> EdSocketError {
    EdSocketError {
        sock_tag: SOCK_DNS_LOOKUP_FAILED,
        _pad: [0; 7],
        payload: EdSocketErrorPayload { host: ManuallyDrop::new(alloc_edstr(host)) },
    }
}

/// Translate a `std::io::Error` into the locked `SocketError` wire shape.
pub(super) fn socket_error_from_io(err: &std::io::Error) -> EdSocketError {
    use std::io::ErrorKind::*;
    let tag = match err.kind() {
        ConnectionRefused => SOCK_CONNECTION_REFUSED,
        ConnectionReset => SOCK_CONNECTION_RESET,
        ConnectionAborted => SOCK_CONNECTION_ABORTED,
        HostUnreachable => SOCK_HOST_UNREACHABLE,
        NetworkUnreachable => SOCK_NETWORK_UNREACHABLE,
        TimedOut => SOCK_TIMEOUT,
        AddrInUse => SOCK_ADDRESS_IN_USE,
        AddrNotAvailable => SOCK_ADDRESS_NOT_AVAILABLE,
        BrokenPipe => SOCK_BROKEN_PIPE,
        WouldBlock => SOCK_WOULD_BLOCK,
        NotConnected => SOCK_NOT_CONNECTED,
        UnexpectedEof => SOCK_UNEXPECTED_EOF,
        OutOfMemory => SOCK_OUT_OF_MEMORY,
        _ => SOCK_OS_ERROR,
    };
    if tag == SOCK_OS_ERROR {
        return EdSocketError {
            sock_tag: SOCK_OS_ERROR,
            _pad: [0; 7],
            payload: EdSocketErrorPayload { code: err.raw_os_error().unwrap_or(0) },
        };
    }
    sock_err(tag)
}

// =====================================================================
// Handle registries
// =====================================================================

static CONNECTIONS: Mutex<Vec<Option<TcpStream>>> = Mutex::new(Vec::new());

static LISTENERS: Mutex<Vec<Option<TcpListener>>> = Mutex::new(Vec::new());

/// Register a stream, returning its 1-based handle.
pub(super) fn register_conn(stream: TcpStream) -> u64 {
    let mut reg = CONNECTIONS.lock().expect("connection registry poisoned");
    for (i, slot) in reg.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(stream);
            return (i + 1) as u64;
        }
    }
    reg.push(Some(stream));
    reg.len() as u64
}

/// Register a listener, returning its 1-based handle.
pub(super) fn register_listener(listener: TcpListener) -> u64 {
    let mut reg = LISTENERS.lock().expect("listener registry poisoned");
    for (i, slot) in reg.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(listener);
            return (i + 1) as u64;
        }
    }
    reg.push(Some(listener));
    reg.len() as u64
}

/// Clone the stream behind `handle`, or `None` if the slot is empty.
pub(super) fn conn_clone(handle: u64) -> Option<TcpStream> {
    if handle == 0 {
        return None;
    }
    let reg = CONNECTIONS.lock().expect("connection registry poisoned");
    let slot = reg.get((handle - 1) as usize)?;
    slot.as_ref().and_then(|s| s.try_clone().ok())
}

/// Clone the listener behind `handle`, or `None` if the slot is empty.
pub(super) fn listener_clone(handle: u64) -> Option<TcpListener> {
    if handle == 0 {
        return None;
    }
    let reg = LISTENERS.lock().expect("listener registry poisoned");
    let slot = reg.get((handle - 1) as usize)?;
    slot.as_ref().and_then(|l| l.try_clone().ok())
}

/// Decode an `EdStr` host argument to a borrowed UTF-8 `str`.
pub(super) unsafe fn ed_str_as_str<'a>(s: &'a EdStr) -> Option<&'a str> {
    let len = s.len as usize;
    if len == 0 {
        return Some("");
    }
    if s.ptr.is_null() {
        return None;
    }
    // SAFETY: caller asserts `ptr` heads `len` initialised bytes.
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr, len) };
    std::str::from_utf8(bytes).ok()
}
