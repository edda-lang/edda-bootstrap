//! Connection lifecycle — dial / listen / accept / close.

use std::net::{TcpListener, TcpStream, ToSocketAddrs};

use crate::EdStr;

use super::{
    CONNECTIONS, LISTENERS, SOCK_NOT_CONNECTED, SOCK_OS_ERROR, ResultSock, ed_str_as_str,
    err_result, listener_clone, ok_u64, register_conn, register_listener, sock_err, sock_err_dns,
    socket_error_from_io,
};

/// Open an outbound TCP connection to `host:port`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_dial(
    _net: *const (),
    host: EdStr,
    port: u16,
) -> ResultSock {
    // SAFETY: caller asserts `host` is a valid UTF-8 EdStr.
    let Some(h) = (unsafe { ed_str_as_str(&host) }) else {
        return err_result(sock_err(SOCK_OS_ERROR));
    };
    let mut addrs = match (h, port).to_socket_addrs() {
        Ok(it) => it,
        Err(_) => return err_result(sock_err_dns(h)),
    };
    let Some(addr) = addrs.next() else {
        return err_result(sock_err_dns(h));
    };
    match TcpStream::connect(addr) {
        Ok(stream) => ok_u64(register_conn(stream)),
        Err(e) => err_result(socket_error_from_io(&e)),
    }
}

/// Bind a TCP listener to `host:port`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_listen(
    _net: *const (),
    host: EdStr,
    port: u16,
) -> ResultSock {
    // SAFETY: caller asserts `host` is a valid UTF-8 EdStr.
    let Some(h) = (unsafe { ed_str_as_str(&host) }) else {
        return err_result(sock_err(SOCK_OS_ERROR));
    };
    let mut addrs = match (h, port).to_socket_addrs() {
        Ok(it) => it,
        Err(_) => return err_result(sock_err_dns(h)),
    };
    let Some(addr) = addrs.next() else {
        return err_result(sock_err_dns(h));
    };
    match TcpListener::bind(addr) {
        Ok(listener) => ok_u64(register_listener(listener)),
        Err(e) => err_result(socket_error_from_io(&e)),
    }
}

/// Block until an inbound connection arrives, returning a `Connection`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_accept(listener: *mut u64) -> ResultSock {
    if listener.is_null() {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    }
    // SAFETY: caller asserts `listener` points to the live handle slot.
    let handle = unsafe { *listener };
    let Some(l) = listener_clone(handle) else {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    };
    match l.accept() {
        Ok((stream, _peer)) => ok_u64(register_conn(stream)),
        Err(e) => err_result(socket_error_from_io(&e)),
    }
}

/// Close a connection, releasing the socket. Consumes the handle.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_net_close(conn: u64) {
    if conn == 0 {
        return;
    }
    let mut reg = CONNECTIONS.lock().expect("connection registry poisoned");
    if let Some(slot) = reg.get_mut((conn - 1) as usize) {
        let _ = slot.take();
    }
}

/// Close a listener, releasing the socket. Consumes the handle.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_net_close_listener(listener: u64) {
    if listener == 0 {
        return;
    }
    let mut reg = LISTENERS.lock().expect("listener registry poisoned");
    if let Some(slot) = reg.get_mut((listener - 1) as usize) {
        let _ = slot.take();
    }
}
