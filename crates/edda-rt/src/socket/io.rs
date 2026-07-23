//! Connection I/O — read / write.

use std::io::{Read, Write};

use crate::EdSlice;

use super::{
    SOCK_NOT_CONNECTED, ResultSock, conn_clone, err_result, ok_u64, sock_err, socket_error_from_io,
};

/// Read up to `buf.len()` bytes from `conn` into `buf`; returns the count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_read(conn: *mut u64, buf: *mut EdSlice) -> ResultSock {
    if conn.is_null() || buf.is_null() {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    }
    // SAFETY: caller asserts both pointers head live slots.
    let handle = unsafe { *conn };
    let slice = unsafe { &*buf };
    let Some(mut stream) = conn_clone(handle) else {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    };
    if slice.len == 0 || slice.ptr.is_null() {
        return ok_u64(0);
    }
    // SAFETY: caller asserts `ptr` heads `len` writable bytes.
    let dst = unsafe { std::slice::from_raw_parts_mut(slice.ptr, slice.len as usize) };
    match stream.read(dst) {
        Ok(n) => ok_u64(n as u64),
        Err(e) => err_result(socket_error_from_io(&e)),
    }
}

/// Write `data` to `conn`; returns the number of bytes written.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_net_write(conn: *mut u64, data: EdSlice) -> ResultSock {
    if conn.is_null() {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    }
    // SAFETY: caller asserts `conn` heads the live handle slot.
    let handle = unsafe { *conn };
    let Some(mut stream) = conn_clone(handle) else {
        return err_result(sock_err(SOCK_NOT_CONNECTED));
    };
    if data.len == 0 || data.ptr.is_null() {
        return ok_u64(0);
    }
    // SAFETY: caller asserts `ptr` heads `len` initialised bytes.
    let src = unsafe { std::slice::from_raw_parts(data.ptr, data.len as usize) };
    match stream.write(src) {
        Ok(n) => ok_u64(n as u64),
        Err(e) => err_result(socket_error_from_io(&e)),
    }
}
