//! Filesystem runtime externs (FsError-bearing) backing std.fs / std.os.fs.
//!
//! This module file owns the FsError wire types and their constructors; the
//! extern entry points live in the child `ops` module, which reaches these
//! private items as an ancestor (no re-export needed).

use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

use crate::abi::{EdStr, alloc_edstr};

mod ops;

// =====================================================================
// Filesystem ops — FsError-bearing surface
// =====================================================================
//
// `FsError` is an 8-variant sum: 5 String-carrying variants
// (not_found/permission_denied/already_exists/is_a_directory/not_a_directory),
// 1 i32-carrying variant (os_error), 2 unit variants (invalid_utf8, other).
// Max payload alignment = 8 (String's ptr field); max payload size = 16
// (String fat pointer); tag is u8. The locked sum-ADT layout per
// `edda-mir` puts the tag at offset 0, pads to the max-payload alignment
// (8), and follows with a 16-byte payload slot — total `EdFsError` = 24 bytes.

// FsError discriminants. Order matches the source declaration in
// `stdlib/os/fs/src/fs.ea`.
const FS_ERR_NOT_FOUND: u8 = 0;
const FS_ERR_PERMISSION_DENIED: u8 = 1;
const FS_ERR_ALREADY_EXISTS: u8 = 2;
const FS_ERR_IS_A_DIRECTORY: u8 = 3;
const FS_ERR_NOT_A_DIRECTORY: u8 = 4;
const FS_ERR_INVALID_UTF8: u8 = 5;
const FS_ERR_OS_ERROR: u8 = 6;
const FS_ERR_OTHER: u8 = 7;

// FsError sum: `{ tag: u8, pad: [u8;7], payload: union<EdStr, i32, ()> }`.
#[repr(C)]
struct EdFsError {
    fs_tag: u8,
    _pad: [u8; 7],
    payload: EdFsErrorPayload,
}

#[repr(C)]
union EdFsErrorPayload {
    path: ManuallyDrop<EdStr>,
    code: i32,
    _unit: ManuallyDrop<()>,
}

// Result<(), FsError> wire shape — for write_string / create / remove.
#[repr(C)]
struct ResultUnitFsError {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultUnitFsErrorPayload,
}

#[repr(C)]
union ResultUnitFsErrorPayload {
    ok: ManuallyDrop<()>,
    err: ManuallyDrop<EdFsError>,
}

// Result<EdStr, FsError> wire shape — for read_to_string and (because
// `[u8]` shares the EdStr fat-pointer layout) read_bytes.
#[repr(C)]
struct ResultEdStrFsError {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultEdStrFsErrorPayload,
}

#[repr(C)]
union ResultEdStrFsErrorPayload {
    ok: ManuallyDrop<EdStr>,
    err: ManuallyDrop<EdFsError>,
}

// Result<u64, FsError> wire shape — for file_size / modified_unix.
#[repr(C)]
struct ResultU64FsError {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultU64FsErrorPayload,
}

#[repr(C)]
union ResultU64FsErrorPayload {
    ok: u64,
    err: ManuallyDrop<EdFsError>,
}

/// Translate a `std::io::Error` into the locked `FsError` wire shape.
/// The `path` is cloned into an `EdStr` for variants whose payload is
/// a `String` — every such allocation is intentionally leaked per the
/// crate's no-dealloc-ABI rule.
fn fs_error_from_io(err: std::io::Error, path: &str) -> EdFsError {
    let (tag, payload) = match err.kind() {
        std::io::ErrorKind::NotFound => (
            FS_ERR_NOT_FOUND,
            EdFsErrorPayload { path: ManuallyDrop::new(alloc_edstr(path)) },
        ),
        std::io::ErrorKind::PermissionDenied => (
            FS_ERR_PERMISSION_DENIED,
            EdFsErrorPayload { path: ManuallyDrop::new(alloc_edstr(path)) },
        ),
        std::io::ErrorKind::AlreadyExists => (
            FS_ERR_ALREADY_EXISTS,
            EdFsErrorPayload { path: ManuallyDrop::new(alloc_edstr(path)) },
        ),
        _ => match err.raw_os_error() {
            Some(code) => (FS_ERR_OS_ERROR, EdFsErrorPayload { code }),
            None => (FS_ERR_OTHER, EdFsErrorPayload { _unit: ManuallyDrop::new(()) }),
        },
    };
    EdFsError { fs_tag: tag, _pad: [0; 7], payload }
}

/// Build an `is_a_directory(path)` FsError — used by `read_to_string`
/// when the host returns a regular IO error that
/// `std::io::ErrorKind` doesn't yet classify but the caller can
/// disambiguate.
fn fs_error_is_a_directory(path: &str) -> EdFsError {
    EdFsError {
        fs_tag: FS_ERR_IS_A_DIRECTORY,
        _pad: [0; 7],
        payload: EdFsErrorPayload { path: ManuallyDrop::new(alloc_edstr(path)) },
    }
}

/// Build a `not_a_directory(path)` FsError — used by `read_dir` when
/// `path` resolves to a regular file rather than a directory.
fn fs_error_not_a_directory(path: &str) -> EdFsError {
    EdFsError {
        fs_tag: FS_ERR_NOT_A_DIRECTORY,
        _pad: [0; 7],
        payload: EdFsErrorPayload { path: ManuallyDrop::new(alloc_edstr(path)) },
    }
}

/// Build an `invalid_utf8` FsError — used when a `read_to_string`
/// call reads bytes that aren't valid UTF-8.
fn fs_error_invalid_utf8() -> EdFsError {
    EdFsError {
        fs_tag: FS_ERR_INVALID_UTF8,
        _pad: [0; 7],
        payload: EdFsErrorPayload { _unit: ManuallyDrop::new(()) },
    }
}

/// Build an `other` FsError — used when `path` is not valid UTF-8 and
/// no further specialisation is possible.
fn fs_error_other() -> EdFsError {
    EdFsError {
        fs_tag: FS_ERR_OTHER,
        _pad: [0; 7],
        payload: EdFsErrorPayload { _unit: ManuallyDrop::new(()) },
    }
}

/// Decode an `EdStr` filename argument to a host UTF-8 `str` slice.
/// Returns `None` when the bytes are not valid UTF-8; callers surface
/// `FsError.other` because the locked variant set has no
/// `invalid_path` discriminant — `invalid_utf8` is reserved for the
/// file-contents path.
unsafe fn ed_str_as_utf8<'a>(s: &'a EdStr) -> Option<&'a str> {
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    std::str::from_utf8(bytes).ok()
}

// =====================================================================
// Filesystem capability narrowing — Filesystem/ReadOnlyFilesystem.scoped_to
// =====================================================================
//
// Capability handles are opaque `ptr`-typed values (edda-mir lowers every
// capability to a single word). `main`'s entry prologue seeds an
// unnarrowed Filesystem/ReadOnlyFilesystem local with the POSIX
// `AT_FDCWD` sentinel (crates/edda-mir/src/entry.rs), the same bit
// pattern on every target including Windows. A `scoped_to`-narrowed
// capability instead carries a heap pointer to a `ScopedFs`, exploiting
// the fact that the type system — not the pointer's bit pattern — is
// what the source-side `ReadOnlyFilesystem` / `Filesystem` distinction
// enforces.

const CAP_FS_UNSCOPED: usize = 0xFFFF_FFFF_FFFF_FF9C_usize;

fn is_unscoped(cap_fs: *const ()) -> bool {
    let addr = cap_fs as usize;
    addr == CAP_FS_UNSCOPED || addr == 0
}

struct ScopedFs {
    root: PathBuf,
}

fn fs_error_permission_denied(path: &str) -> EdFsError {
    EdFsError {
        fs_tag: FS_ERR_PERMISSION_DENIED,
        _pad: [0; 7],
        payload: EdFsErrorPayload { path: ManuallyDrop::new(alloc_edstr(path)) },
    }
}

fn trap(msg: &str) -> ! {
    unsafe { crate::io::__edda_panic(msg.as_ptr(), msg.len() as u64) }
}

unsafe fn scoped_to_impl(cap_fs: *const (), prefix: &str) -> *const () {
    let base: PathBuf = if is_unscoped(cap_fs) {
        std::env::current_dir().unwrap_or_else(|_| trap("scoped_to: cannot resolve the current directory"))
    } else {
        (unsafe { &*(cap_fs as *const ScopedFs) }).root.clone()
    };
    let joined = base.join(prefix);
    let canonical = match std::fs::canonicalize(&joined) {
        Ok(p) if p.is_dir() => p,
        _ => trap("scoped_to: prefix does not name an existing directory"),
    };
    if !is_unscoped(cap_fs) {
        let existing = unsafe { &*(cap_fs as *const ScopedFs) };
        if !canonical.starts_with(&existing.root) {
            trap("scoped_to: prefix escapes the existing scope");
        }
    }
    Box::into_raw(Box::new(ScopedFs { root: canonical })) as *const ()
}

unsafe fn resolve_path(cap_fs: *const (), path: &str) -> Result<PathBuf, EdFsError> {
    if is_unscoped(cap_fs) {
        return Ok(PathBuf::from(path));
    }
    let scoped = unsafe { &*(cap_fs as *const ScopedFs) };
    let root = scoped.root.as_path();

    let absolute = Path::new(path).is_absolute();
    let mut joined = if absolute {
        PathBuf::new()
    } else {
        root.to_path_buf()
    };
    for component in Path::new(path).components() {
        match component {
            std::path::Component::Normal(seg) => joined.push(seg),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !joined.pop() || (!absolute && !joined.starts_with(root)) {
                    return Err(fs_error_permission_denied(path));
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                if !absolute {
                    return Err(fs_error_permission_denied(path));
                }
                joined.push(component.as_os_str());
            }
        }
    }

    let mut probe = joined.clone();
    let mut trailing: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(&probe) {
            Ok(canonical) => {
                if !canonical.starts_with(root) {
                    return Err(fs_error_permission_denied(path));
                }
                let mut resolved = canonical;
                for seg in trailing.into_iter().rev() {
                    resolved.push(seg);
                }
                return Ok(resolved);
            }
            Err(_) => {
                let Some(name) = probe.file_name().map(|s| s.to_os_string()) else {
                    return Err(fs_error_permission_denied(path));
                };
                trailing.push(name);
                if !probe.pop() {
                    return Err(fs_error_permission_denied(path));
                }
            }
        }
    }
}
