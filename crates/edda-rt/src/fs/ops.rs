//! Filesystem extern entry points — bridge std.fs.* / std.os.fs.* to std::fs.

use std::mem::ManuallyDrop;

use super::*;
use crate::abi::{EdSlice, EdStr, alloc_edstr};

/// Existence check for `path`. Returns `1` when the path resolves to
/// an entry the host can stat, `0` otherwise — including the
/// permission-denied case, which `std.fs.exists` documents as
/// collapsing to `false` because the function carries no error
/// effect.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_exists(cap: *const (), path: EdStr) -> u8 {
    let bytes = unsafe { std::slice::from_raw_parts(path.ptr as *const u8, path.len as usize) };
    let Ok(s) = std::str::from_utf8(bytes) else {
        return 0;
    };
    let Ok(resolved) = (unsafe { resolve_path(cap, s) }) else {
        return 0;
    };
    if resolved.exists() { 1 } else { 0 }
}

/// Device/volume identifier for the filesystem entry at `path`. Returns
/// `0` when the path cannot be stat'd (including permission failures),
/// which callers treat as "boundary check not applicable here."
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_dev_id(cap: *const (), path: EdStr) -> u64 {
    let bytes = unsafe { std::slice::from_raw_parts(path.ptr as *const u8, path.len as usize) };
    let Ok(s) = std::str::from_utf8(bytes) else {
        return 0;
    };
    let Ok(resolved) = (unsafe { resolve_path(cap, s) }) else {
        return 0;
    };
    let Some(resolved_str) = resolved.to_str() else {
        return 0;
    };
    dev_id_for(resolved_str)
}

#[cfg(unix)]
fn dev_id_for(path: &str) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).map(|m| m.dev()).unwrap_or(0)
}

#[cfg(windows)]
fn dev_id_for(path: &str) -> u64 {
    windows_volume_serial(path).map(u64::from).unwrap_or(0)
}

#[cfg(not(any(unix, windows)))]
fn dev_id_for(_path: &str) -> u64 {
    0
}

#[cfg(windows)]
fn windows_volume_serial(path: &str) -> Option<u32> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    #[repr(C)]
    struct FileTime {
        _low: u32,
        _high: u32,
    }

    #[repr(C)]
    struct ByHandleFileInformation {
        _file_attributes: u32,
        _creation_time: FileTime,
        _last_access_time: FileTime,
        _last_write_time: FileTime,
        volume_serial_number: u32,
        _file_size_high: u32,
        _file_size_low: u32,
        _number_of_links: u32,
        _file_index_high: u32,
        _file_index_low: u32,
    }

    unsafe extern "system" {
        fn CreateFileW(
            lpfilename: *const u16,
            dwdesiredaccess: u32,
            dwsharemode: u32,
            lpsecurityattributes: *mut c_void,
            dwcreationdisposition: u32,
            dwflagsandattributes: u32,
            htemplatefile: *mut c_void,
        ) -> *mut c_void;
        fn GetFileInformationByHandle(hfile: *mut c_void, lpfileinformation: *mut ByHandleFileInformation) -> i32;
        fn CloseHandle(hobject: *mut c_void) -> i32;
    }

    let invalid_handle_value = (-1isize) as *mut c_void;
    let wide: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == invalid_handle_value {
        return None;
    }
    let mut info: ByHandleFileInformation = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    unsafe { CloseHandle(handle) };
    if ok == 0 { None } else { Some(info.volume_serial_number) }
}

/// Byte length of the file at `path`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_file_size(cap: *const (), path: EdStr) -> ResultU64FsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultU64FsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultU64FsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved = match unsafe { resolve_path(cap, p) } {
        Ok(r) => r,
        Err(e) => {
            return ResultU64FsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultU64FsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    match std::fs::metadata(&resolved) {
        Ok(m) => ResultU64FsError {
            tag: 0,
            _pad: [0; 7],
            payload: ResultU64FsErrorPayload { ok: m.len() },
        },
        Err(e) => ResultU64FsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultU64FsErrorPayload {
                err: ManuallyDrop::new(fs_error_from_io(e, p)),
            },
        },
    }
}

/// Last-modified time of the file at `path`, in whole seconds since the
/// Unix epoch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_modified_unix(cap: *const (), path: EdStr) -> ResultU64FsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultU64FsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultU64FsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved = match unsafe { resolve_path(cap, p) } {
        Ok(r) => r,
        Err(e) => {
            return ResultU64FsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultU64FsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    match std::fs::metadata(&resolved) {
        Ok(m) => {
            let secs = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            ResultU64FsError {
                tag: 0,
                _pad: [0; 7],
                payload: ResultU64FsErrorPayload { ok: secs },
            }
        }
        Err(e) => ResultU64FsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultU64FsErrorPayload {
                err: ManuallyDrop::new(fs_error_from_io(e, p)),
            },
        },
    }
}

/// Read the entire UTF-8 contents of `path` into a heap-allocated
/// `EdStr`. Non-UTF-8 file contents surface as `FsError.invalid_utf8`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_read_to_string(
    cap_fs: *const (),
    _cap_alloc: *const (),
    path: EdStr,
) -> ResultEdStrFsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultEdStrFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdStrFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved = match unsafe { resolve_path(cap_fs, p) } {
        Ok(r) => r,
        Err(e) => {
            return ResultEdStrFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultEdStrFsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    match std::fs::read(&resolved) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(s) => ResultEdStrFsError {
                tag: 0,
                _pad: [0; 7],
                payload: ResultEdStrFsErrorPayload {
                    ok: ManuallyDrop::new(alloc_edstr(&s)),
                },
            },
            Err(_) => ResultEdStrFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultEdStrFsErrorPayload {
                    err: ManuallyDrop::new(fs_error_invalid_utf8()),
                },
            },
        },
        Err(_e) if resolved.is_dir() => ResultEdStrFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdStrFsErrorPayload {
                err: ManuallyDrop::new(fs_error_is_a_directory(p)),
            },
        },
        Err(e) => ResultEdStrFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdStrFsErrorPayload {
                err: ManuallyDrop::new(fs_error_from_io(e, p)),
            },
        },
    }
}

/// Read the entire byte contents of `path` into a heap-allocated
/// `[u8]` (returned through the `EdStr` wire shape).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_read_bytes(
    cap_fs: *const (),
    _cap_alloc: *const (),
    path: EdStr,
) -> ResultEdStrFsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultEdStrFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdStrFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved = match unsafe { resolve_path(cap_fs, p) } {
        Ok(r) => r,
        Err(e) => {
            return ResultEdStrFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultEdStrFsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    match std::fs::read(&resolved) {
        Ok(bytes) => {
            let boxed: Box<[u8]> = bytes.into_boxed_slice();
            let len = boxed.len() as u64;
            let ptr = Box::into_raw(boxed) as *mut u8;
            ResultEdStrFsError {
                tag: 0,
                _pad: [0; 7],
                payload: ResultEdStrFsErrorPayload {
                    ok: ManuallyDrop::new(EdStr { ptr, len }),
                },
            }
        }
        Err(e) => ResultEdStrFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdStrFsErrorPayload {
                err: ManuallyDrop::new(fs_error_from_io(e, p)),
            },
        },
    }
}

/// List the directory entries at `path` as a heap-allocated
/// `[String]` of basenames (not full paths). Entries are returned in
/// the host's `std::fs::read_dir` iteration order; the caller treats
/// the order as unspecified per the source-side `read_dir` contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_read_dir(
    cap_fs: *const (),
    _cap_alloc: *const (),
    path: EdStr,
) -> ResultEdStrFsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultEdStrFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdStrFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved = match unsafe { resolve_path(cap_fs, p) } {
        Ok(r) => r,
        Err(e) => {
            return ResultEdStrFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultEdStrFsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    match std::fs::metadata(&resolved) {
        Ok(m) if !m.is_dir() => {
            return ResultEdStrFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultEdStrFsErrorPayload {
                    err: ManuallyDrop::new(fs_error_not_a_directory(p)),
                },
            };
        }
        Err(e) => {
            return ResultEdStrFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultEdStrFsErrorPayload {
                    err: ManuallyDrop::new(fs_error_from_io(e, p)),
                },
            };
        }
        Ok(_) => {}
    }
    let iter = match std::fs::read_dir(&resolved) {
        Ok(it) => it,
        Err(e) => {
            return ResultEdStrFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultEdStrFsErrorPayload {
                    err: ManuallyDrop::new(fs_error_from_io(e, p)),
                },
            };
        }
    };
    let mut names: Vec<EdStr> = Vec::new();
    for entry in iter {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                return ResultEdStrFsError {
                    tag: 1,
                    _pad: [0; 7],
                    payload: ResultEdStrFsErrorPayload {
                        err: ManuallyDrop::new(fs_error_from_io(e, p)),
                    },
                };
            }
        };
        let name = entry.file_name();
        if let Some(s) = name.to_str() {
            names.push(alloc_edstr(s));
        }
    }
    names.shrink_to_fit();
    let mut leaked = ManuallyDrop::new(names);
    let len = leaked.len() as u64;
    let ptr = leaked.as_mut_ptr() as *mut u8;
    ResultEdStrFsError {
        tag: 0,
        _pad: [0; 7],
        payload: ResultEdStrFsErrorPayload {
            ok: ManuallyDrop::new(EdStr { ptr, len }),
        },
    }
}

/// Narrow a `Filesystem` capability handle to `ReadOnlyFilesystem`.
/// At the wire level capability handles are opaque pointers (per the
/// recent edda-mir change that lowered capability types to `opaque
/// ptr`), so the narrowing is a no-op — the runtime returns the input
/// pointer unchanged and the LLVM-side type information enforces that
/// the returned handle can only be passed to symbols that accept
/// `ReadOnlyFilesystem`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_read_only(cap_fs: *const ()) -> *const () {
    cap_fs
}

/// Narrow a `ReadOnlyFilesystem` capability handle to the subtree rooted
/// at `prefix`. Returns a new opaque capability pointer backed by a
/// `ScopedFs` handle; every read op against the returned capability is
/// contained to that subtree.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_scoped_to(cap_fs: *const (), prefix: EdStr) -> *const () {
    let Some(p) = (unsafe { ed_str_as_utf8(&prefix) }) else {
        trap("scoped_to: prefix is not valid UTF-8");
    };
    unsafe { scoped_to_impl(cap_fs, p) }
}

/// Narrow a `Filesystem` capability handle to the subtree rooted at
/// `prefix`. See `__edda_fs_scoped_to`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_scoped_to_w(cap_fs: *const (), prefix: EdStr) -> *const () {
    let Some(p) = (unsafe { ed_str_as_utf8(&prefix) }) else {
        trap("scoped_to_w: prefix is not valid UTF-8");
    };
    unsafe { scoped_to_impl(cap_fs, p) }
}

/// Resolve `path` against `cap_fs`'s scope, returning the validated
/// absolute path a caller should operate on instead of the raw input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_resolve_scoped(
    cap_fs: *const (),
    _cap_alloc: *const (),
    path: EdStr,
) -> ResultEdStrFsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultEdStrFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdStrFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    match unsafe { resolve_path(cap_fs, p) } {
        Ok(resolved) => {
            let resolved_str = resolved.to_string_lossy();
            ResultEdStrFsError {
                tag: 0,
                _pad: [0; 7],
                payload: ResultEdStrFsErrorPayload {
                    ok: ManuallyDrop::new(alloc_edstr(&resolved_str)),
                },
            }
        }
        Err(e) => ResultEdStrFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdStrFsErrorPayload { err: ManuallyDrop::new(e) },
        },
    }
}

/// Write `contents` to `path`, replacing any existing file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_write_string(
    cap_fs: *const (),
    path: EdStr,
    contents: EdStr,
) -> ResultUnitFsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved = match unsafe { resolve_path(cap_fs, p) } {
        Ok(r) => r,
        Err(e) => {
            return ResultUnitFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultUnitFsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    let body = unsafe { std::slice::from_raw_parts(contents.ptr as *const u8, contents.len as usize) };
    match std::fs::write(&resolved, body) {
        Ok(()) => ResultUnitFsError {
            tag: 0,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                ok: ManuallyDrop::new(()),
            },
        },
        Err(e) => ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_from_io(e, p)),
            },
        },
    }
}

/// Write the raw byte buffer `contents` to `path`, replacing any
/// existing file. Binary counterpart to `__edda_fs_write_string` for
/// `edda build`'s object-file emission.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_write_bytes(
    cap_fs: *const (),
    path: EdStr,
    contents: EdSlice,
) -> ResultUnitFsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved = match unsafe { resolve_path(cap_fs, p) } {
        Ok(r) => r,
        Err(e) => {
            return ResultUnitFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultUnitFsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    let body: &[u8] = if contents.len == 0 || contents.ptr.is_null() {
        &[]
    } else {
        // SAFETY: caller asserts `ptr` heads `len` initialised bytes.
        unsafe { std::slice::from_raw_parts(contents.ptr as *const u8, contents.len as usize) }
    };
    match std::fs::write(&resolved, body) {
        Ok(()) => ResultUnitFsError {
            tag: 0,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                ok: ManuallyDrop::new(()),
            },
        },
        Err(e) => ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_from_io(e, p)),
            },
        },
    }
}

/// Create a new empty file at `path`. Fails if the path already
/// exists (matches `std.fs.create`'s locked semantics).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_create(cap_fs: *const (), path: EdStr) -> ResultUnitFsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved = match unsafe { resolve_path(cap_fs, p) } {
        Ok(r) => r,
        Err(e) => {
            return ResultUnitFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultUnitFsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&resolved) {
        Ok(_) => ResultUnitFsError {
            tag: 0,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                ok: ManuallyDrop::new(()),
            },
        },
        Err(e) => ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_from_io(e, p)),
            },
        },
    }
}

/// Create the directory `path` and every missing parent directory.
/// Idempotent: succeeds if the directory already exists. Backs
/// `edda build`'s output-directory creation before object-file emission.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_create_dir_all(cap_fs: *const (), path: EdStr) -> ResultUnitFsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved = match unsafe { resolve_path(cap_fs, p) } {
        Ok(r) => r,
        Err(e) => {
            return ResultUnitFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultUnitFsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    match std::fs::create_dir_all(&resolved) {
        Ok(()) => ResultUnitFsError {
            tag: 0,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                ok: ManuallyDrop::new(()),
            },
        },
        Err(e) => ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_from_io(e, p)),
            },
        },
    }
}

/// Remove the file or empty directory at `path`. Non-empty directories
/// surface the host IO error; missing entries raise `not_found`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_remove(cap_fs: *const (), path: EdStr) -> ResultUnitFsError {
    let Some(p) = (unsafe { ed_str_as_utf8(&path) }) else {
        return ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved = match unsafe { resolve_path(cap_fs, p) } {
        Ok(r) => r,
        Err(e) => {
            return ResultUnitFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultUnitFsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    if resolved.is_dir() {
        return match std::fs::remove_dir(&resolved) {
            Ok(()) => ResultUnitFsError {
                tag: 0,
                _pad: [0; 7],
                payload: ResultUnitFsErrorPayload {
                    ok: ManuallyDrop::new(()),
                },
            },
            Err(e) => ResultUnitFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultUnitFsErrorPayload {
                    err: ManuallyDrop::new(fs_error_from_io(e, p)),
                },
            },
        };
    }
    match std::fs::remove_file(&resolved) {
        Ok(()) => ResultUnitFsError {
            tag: 0,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                ok: ManuallyDrop::new(()),
            },
        },
        Err(e) => ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_from_io(e, p)),
            },
        },
    }
}

/// Rename the filesystem entry at `from_path` to `to_path`, replacing
/// any existing destination. Backs the staged-then-rename atomic commit
/// in `compiler/lib/cache`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_fs_rename(
    cap_fs: *const (),
    from_path: EdStr,
    to_path: EdStr,
) -> ResultUnitFsError {
    let Some(from) = (unsafe { ed_str_as_utf8(&from_path) }) else {
        return ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let Some(to) = (unsafe { ed_str_as_utf8(&to_path) }) else {
        return ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_other()),
            },
        };
    };
    let resolved_from = match unsafe { resolve_path(cap_fs, from) } {
        Ok(r) => r,
        Err(e) => {
            return ResultUnitFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultUnitFsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    let resolved_to = match unsafe { resolve_path(cap_fs, to) } {
        Ok(r) => r,
        Err(e) => {
            return ResultUnitFsError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultUnitFsErrorPayload { err: ManuallyDrop::new(e) },
            };
        }
    };
    match std::fs::rename(&resolved_from, &resolved_to) {
        Ok(()) => ResultUnitFsError {
            tag: 0,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                ok: ManuallyDrop::new(()),
            },
        },
        Err(e) => ResultUnitFsError {
            tag: 1,
            _pad: [0; 7],
            payload: ResultUnitFsErrorPayload {
                err: ManuallyDrop::new(fs_error_from_io(e, from)),
            },
        },
    }
}
