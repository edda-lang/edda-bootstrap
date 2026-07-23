//! stdout/stderr/stdin runtime externs and their IoError Result wire shape.

use std::io::{BufRead, Write};
use std::mem::ManuallyDrop;

use crate::abi::{EdStr, alloc_edstr};

// IoError sum ADT. Matches LLVM `{ i8, { i32 } }` (8 bytes on x86-64).
// tag: 0=os_error, 1=unexpected_eof, 2=broken_pipe, 3=other.
#[repr(C)]
struct EdIoError {
    io_tag: u8,
    _pad: [u8; 3],
    code: i32,
}

// Result<String, IoError>. Matches LLVM `{ i8, { ptr, i64 } }` (24 bytes on x86-64).
// Max payload slot = String (16 bytes) > IoError (8 bytes), so `{ ptr, i64 }` wins.
// tag 0 = Ok(String); tag 1 = Err(IoError).
#[repr(C)]
struct ResultEdStr {
    tag: u8,
    payload: ResultEdStrPayload,
}

#[repr(C)]
union ResultEdStrPayload {
    ok: ManuallyDrop<EdStr>,
    err: ManuallyDrop<EdIoError>,
}

/// Write EdStr to stdout followed by a newline.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_stdout_write_line(_cap: *const (), msg: EdStr) {
    let bytes = unsafe { std::slice::from_raw_parts(msg.ptr as *const u8, msg.len as usize) };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(bytes);
    let _ = out.write_all(b"\n");
}

/// Write EdStr to stdout without a trailing newline.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_stdout_write(_cap: *const (), msg: EdStr) {
    let bytes = unsafe { std::slice::from_raw_parts(msg.ptr as *const u8, msg.len as usize) };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(bytes);
}

/// Write EdStr to stderr followed by a newline.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_stderr_write_line(_cap: *const (), msg: EdStr) {
    let bytes = unsafe { std::slice::from_raw_parts(msg.ptr as *const u8, msg.len as usize) };
    let stderr = std::io::stderr();
    let mut err = stderr.lock();
    let _ = err.write_all(bytes);
    let _ = err.write_all(b"\n");
}

/// Write EdStr to stderr without a trailing newline.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_stderr_write(_cap: *const (), msg: EdStr) {
    let bytes = unsafe { std::slice::from_raw_parts(msg.ptr as *const u8, msg.len as usize) };
    let stderr = std::io::stderr();
    let mut err = stderr.lock();
    let _ = err.write_all(bytes);
}

// process — the diverging runtime handler for the MIR `Panic` terminator. Takes
// the message as a raw (ptr, len) pair rather than EdStr so the LLVM call site
// avoids the Win64 indirect-aggregate convention; a null ptr prints no message.
/// Print a panic message to stderr and abort. Never returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_panic(ptr: *const u8, len: u64) -> ! {
    let bytes: &[u8] = if ptr.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }
    };
    let stderr = std::io::stderr();
    let mut err = stderr.lock();
    let _ = err.write_all(b"edda: panic: ");
    let _ = err.write_all(bytes);
    let _ = err.write_all(b"\n");
    let _ = err.flush();
    std::process::abort();
}

/// Read one line from stdin including the trailing newline.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_stdin_read_line(_cap: *const ()) -> ResultEdStr {
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) => ResultEdStr {
            tag: 1,
            payload: ResultEdStrPayload {
                err: ManuallyDrop::new(EdIoError { io_tag: 1, _pad: [0; 3], code: 0 }),
            },
        },
        Ok(_) => ResultEdStr {
            tag: 0,
            payload: ResultEdStrPayload { ok: ManuallyDrop::new(alloc_edstr(&line)) },
        },
        Err(e) => {
            let io_tag = match e.kind() {
                std::io::ErrorKind::UnexpectedEof => 1,
                std::io::ErrorKind::BrokenPipe => 2,
                _ => 3,
            };
            ResultEdStr {
                tag: 1,
                payload: ResultEdStrPayload {
                    err: ManuallyDrop::new(EdIoError {
                        io_tag,
                        _pad: [0; 3],
                        code: e.raw_os_error().unwrap_or(0),
                    }),
                },
            }
        }
    }
}
