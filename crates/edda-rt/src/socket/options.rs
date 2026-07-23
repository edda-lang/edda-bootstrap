//! Socket options — timeouts / nodelay / keepalive / shutdown.

use std::net::Shutdown;
use std::time::Duration;

use super::{
    SOCK_NOT_CONNECTED, ResultSock, conn_clone, err_result, ok_unit, sock_err, socket_error_from_io,
};

/// Shared body for the timeout setters. `millis == 0` clears the timeout
/// (`None`) because `std` rejects a zero `Duration` timeout.
fn set_timeout(conn: *mut u64, millis: u32, read_half: bool) -> ResultSock {
    if conn.is_null() {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    }
    // SAFETY: caller asserts `conn` heads the live handle slot.
    let handle = unsafe { *conn };
    let Some(stream) = conn_clone(handle) else {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    };
    let dur = if millis == 0 { None } else { Some(Duration::from_millis(millis as u64)) };
    let res = if read_half {
        stream.set_read_timeout(dur)
    } else {
        stream.set_write_timeout(dur)
    };
    match res {
        Ok(()) => ok_unit(),
        Err(e) => err_result(socket_error_from_io(&e)),
    }
}

/// Set (or clear, when `millis == 0`) the read timeout on `conn`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_set_read_timeout(conn: *mut u64, millis: u32) -> ResultSock {
    set_timeout(conn, millis, true)
}

/// Set (or clear, when `millis == 0`) the write timeout on `conn`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_set_write_timeout(conn: *mut u64, millis: u32) -> ResultSock {
    set_timeout(conn, millis, false)
}

/// Enable or disable Nagle's algorithm (`TCP_NODELAY`) on `conn`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_set_nodelay(conn: *mut u64, enabled: bool) -> ResultSock {
    if conn.is_null() {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    }
    // SAFETY: caller asserts `conn` heads the live handle slot.
    let handle = unsafe { *conn };
    let Some(stream) = conn_clone(handle) else {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    };
    match stream.set_nodelay(enabled) {
        Ok(()) => ok_unit(),
        Err(e) => err_result(socket_error_from_io(&e)),
    }
}

/// Validate `conn` and return Ok — keepalive control is currently a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_set_keepalive(conn: *mut u64, _enabled: bool) -> ResultSock {
    if conn.is_null() {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    }
    // SAFETY: caller asserts `conn` heads the live handle slot.
    let handle = unsafe { *conn };
    if conn_clone(handle).is_none() {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    }
    ok_unit()
}

/// Shut down the read half, write half, or both of `conn`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_shutdown(conn: *mut u64, half: u8) -> ResultSock {
    if conn.is_null() {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    }
    // SAFETY: caller asserts `conn` heads the live handle slot.
    let handle = unsafe { *conn };
    let Some(stream) = conn_clone(handle) else {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    };
    let how = match half {
        0 => Shutdown::Read,
        1 => Shutdown::Write,
        _ => Shutdown::Both,
    };
    match stream.shutdown(how) {
        Ok(()) => ok_unit(),
        Err(e) => err_result(socket_error_from_io(&e)),
    }
}
