//! Process/environment runtime externs: exit, argv, working directory, env read.

use std::mem::ManuallyDrop;

use crate::abi::{EdStr, alloc_edstr};

/// Terminate the host process with the given exit status. Matches the
/// `never`-returning signature of `std.process.exit`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_process_exit(code: i32) -> ! {
    std::process::exit(code)
}

/// Expose the host process's command-line arguments (excluding the
/// program path) as a heap-allocated `[String]`. Backs
/// `std.os.process.args`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_process_argv(_cap_alloc: *const ()) -> EdStr {
    let mut args: Vec<EdStr> = std::env::args().skip(1).map(|a| alloc_edstr(&a)).collect();
    args.shrink_to_fit();
    let mut leaked = ManuallyDrop::new(args);
    let len = leaked.len() as u64;
    let ptr = leaked.as_mut_ptr() as *mut u8;
    EdStr { ptr, len }
}

/// Expose the host process's current working directory as a
/// heap-allocated `String`. Backs `std.os.env.current_dir`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_cwd(_cap_alloc: *const ()) -> EdStr {
    match std::env::current_dir() {
        Ok(path) => alloc_edstr(&path.to_string_lossy()),
        Err(_) => alloc_edstr(""),
    }
}

/// Expose the canonical absolute path of the running binary as a
/// heap-allocated `String`. Foundation for self-exe-relative stdlib
/// resolution.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_self_exe_path(_cap_alloc: *const ()) -> EdStr {
    match self_exe_path() {
        Some(path) => alloc_edstr(&path),
        None => alloc_edstr(""),
    }
}

/// Resolve the running binary to its canonical absolute path, returning
/// `None` on any host error.
fn self_exe_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
    Some(strip_verbatim_prefix(&canonical.to_string_lossy()))
}

/// Normalize a host path string, removing the Windows verbatim prefix.
#[cfg(windows)]
fn strip_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

/// Normalize a host path string (no-op off Windows — POSIX paths carry no verbatim prefix).
#[cfg(not(windows))]
fn strip_verbatim_prefix(path: &str) -> String {
    path.to_string()
}

/// Read an environment variable by name. Returns `1` and writes the
/// heap-owned value into `*out_value` when set (empty included), or `0`
/// when unset. Backs `std.os.env.read_optional`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_getenv(
    _cap_alloc: *const (),
    name: EdStr,
    out_value: *mut EdStr,
) -> i32 {
    let name_bytes = unsafe { std::slice::from_raw_parts(name.ptr, name.len as usize) };
    let Ok(name_str) = std::str::from_utf8(name_bytes) else {
        return 0;
    };
    match std::env::var_os(name_str) {
        Some(value) => {
            unsafe { *out_value = alloc_edstr(&value.to_string_lossy()) };
            1
        }
        None => 0,
    }
}
