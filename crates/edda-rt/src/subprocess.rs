//! Subprocess capability narrowing — the C-ABI exports backing
//! `Subprocess.allowing(...)` / `.scoped_to(...)`.
//!
//! The live spawn path is entirely Edda-side syscalls
//! (`std.os.process.spawn` → `CreateProcessW` / `raw.syscall6`), so
//! there is no runtime spawn extern. Enforcement instead lives in a
//! `check` extern the Edda spawn path routes through before launching.
//!
//! Backing model: `.allowing`/`.scoped_to` mint a heap-backed
//! [`ScopedSubprocess`] behind the opaque capability pointer; an
//! unnarrowed `Subprocess` is the entry-prologue null seed (edda-mir
//! `seed_for` falls the `Subprocess` case through to `CapSeed::Null`).
//! `__edda_subprocess_check` validates each launch against the handle.
//! Mirrors the `ScopedFs` filesystem fix
//! for the `Subprocess` capability.
//!
//! All exports use `#[unsafe(no_mangle)] extern "C"` so lld-link
//! resolves the symbols against the static lib edda-rt produces.

use std::path::{Path, PathBuf};

use crate::{EdSlice, EdStr};

/// Decode an `EdStr` into a borrowed `&str`. Returns `None` if the
/// payload is not valid UTF-8 or the pointer is null with a non-zero
/// length.
unsafe fn ed_str_as_str<'a>(s: &'a EdStr) -> Option<&'a str> {
    let len = s.len as usize;
    if len == 0 {
        return Some("");
    }
    if s.ptr.is_null() {
        return None;
    }
    // SAFETY: caller asserts `ptr` is the head of `len` initialised bytes.
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr, len) };
    std::str::from_utf8(bytes).ok()
}

// =====================================================================
// Subprocess capability narrowing — Subprocess.allowing / .scoped_to
// =====================================================================

/// Discriminants returned by `__edda_subprocess_check`. `0` permits the
/// launch; the negatives map 1:1 onto the stdlib `SpawnError` variants
/// the Edda caller raises.
const CHECK_OK: i32 = 0;
const CHECK_NOT_IN_ALLOWLIST: i32 = -1;
const CHECK_CWD_OUTSIDE_SCOPE: i32 = -2;

struct ScopedSubprocess {
    /// `Some(list)` restricts spawns to executables matching an entry;
    /// `None` imposes no executable constraint. An empty list permits
    /// nothing.
    exe_allowlist: Option<Vec<String>>,
    /// `Some(root)` restricts an explicitly-set child working directory
    /// to `root` (canonicalized); `None` imposes no cwd constraint.
    cwd_scope: Option<PathBuf>,
}

fn is_unscoped(cap: *const ()) -> bool {
    cap.is_null()
}

fn trap(msg: &str) -> ! {
    unsafe { crate::io::__edda_panic(msg.as_ptr(), msg.len() as u64) }
}

unsafe fn ed_string_vec(slice: &EdSlice) -> Vec<String> {
    let n = slice.len as usize;
    if n == 0 || slice.ptr.is_null() {
        return Vec::new();
    }
    // SAFETY: caller asserts `ptr` heads `len` consecutive `EdStr` values.
    let items = unsafe { std::slice::from_raw_parts(slice.ptr as *const EdStr, n) };
    items
        .iter()
        .filter_map(|s| unsafe { ed_str_as_str(s) }.map(|x| x.to_string()))
        .collect()
}

fn exe_allowed(exe: &str, allowlist: &[String]) -> bool {
    let exe_base = Path::new(exe).file_name();
    allowlist
        .iter()
        .any(|entry| entry == exe || (exe_base.is_some() && Path::new(entry).file_name() == exe_base))
}

fn resolve_existing_dir(base: &Path, dir: &str) -> Option<PathBuf> {
    let joined = if Path::new(dir).is_absolute() {
        PathBuf::from(dir)
    } else {
        base.join(dir)
    };
    match std::fs::canonicalize(&joined) {
        Ok(p) if p.is_dir() => Some(p),
        _ => None,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_subprocess_allowing(cap: *const (), allowlist: EdSlice) -> *const () {
    let requested = unsafe { ed_string_vec(&allowlist) };
    let (merged, cwd_scope) = if is_unscoped(cap) {
        (requested, None)
    } else {
        // SAFETY: a non-null Subprocess cap is a live `ScopedSubprocess` (invariant above).
        let existing = unsafe { &*(cap as *const ScopedSubprocess) };
        let merged = match &existing.exe_allowlist {
            Some(prev) => prev.iter().filter(|e| requested.contains(e)).cloned().collect(),
            None => requested,
        };
        (merged, existing.cwd_scope.clone())
    };
    Box::into_raw(Box::new(ScopedSubprocess {
        exe_allowlist: Some(merged),
        cwd_scope,
    })) as *const ()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_subprocess_scoped_to(cap: *const (), dir: EdStr) -> *const () {
    let Some(dir_str) = (unsafe { ed_str_as_str(&dir) }) else {
        trap("scoped_to: dir is not valid UTF-8");
    };
    let (base, existing_scope, exe_allowlist): (PathBuf, Option<PathBuf>, Option<Vec<String>>) =
        if is_unscoped(cap) {
            let cwd = std::env::current_dir()
                .unwrap_or_else(|_| trap("scoped_to: cannot resolve the current directory"));
            (cwd, None, None)
        } else {
            // SAFETY: a non-null Subprocess cap is a live `ScopedSubprocess` (invariant above).
            let existing = unsafe { &*(cap as *const ScopedSubprocess) };
            let base = existing.cwd_scope.clone().unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| trap("scoped_to: cannot resolve the current directory"))
            });
            (base, existing.cwd_scope.clone(), existing.exe_allowlist.clone())
        };
    let Some(canonical) = resolve_existing_dir(&base, dir_str) else {
        trap("scoped_to: dir does not name an existing directory");
    };
    if let Some(prev) = &existing_scope {
        if !canonical.starts_with(prev) {
            trap("scoped_to: dir escapes the existing scope");
        }
    }
    Box::into_raw(Box::new(ScopedSubprocess {
        exe_allowlist,
        cwd_scope: Some(canonical),
    })) as *const ()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_subprocess_check(cap: *const (), exe: EdStr, cwd: EdStr) -> i32 {
    if is_unscoped(cap) {
        return CHECK_OK;
    }
    // SAFETY: a non-null Subprocess cap is a live `ScopedSubprocess` (invariant above).
    let handle = unsafe { &*(cap as *const ScopedSubprocess) };
    if let Some(list) = &handle.exe_allowlist {
        let exe_str = unsafe { ed_str_as_str(&exe) }.unwrap_or("");
        if !exe_allowed(exe_str, list) {
            return CHECK_NOT_IN_ALLOWLIST;
        }
    }
    if let Some(scope) = &handle.cwd_scope {
        let cwd_str = unsafe { ed_str_as_str(&cwd) }.unwrap_or("");
        if !cwd_str.is_empty() {
            let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            match resolve_existing_dir(&base, cwd_str) {
                Some(canonical) if canonical.starts_with(scope) => {}
                _ => return CHECK_CWD_OUTSIDE_SCOPE,
            }
        }
    }
    CHECK_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed_str_from(s: &str) -> EdStr {
        EdStr {
            ptr: s.as_ptr() as *mut u8,
            len: s.len() as u64,
        }
    }

    // Build a borrowed `[String]` EdSlice over `entries`; the caller keeps
    // `entries` alive for the duration of the extern call.
    fn ed_slice_over(entries: &[EdStr]) -> EdSlice {
        EdSlice {
            ptr: entries.as_ptr() as *mut u8,
            len: entries.len() as u64,
        }
    }

    #[test]
    fn unscoped_check_permits_any_exe() {
        let code = unsafe {
            __edda_subprocess_check(std::ptr::null(), ed_str_from("anything"), ed_str_from(""))
        };
        assert_eq!(code, CHECK_OK);
    }

    #[test]
    fn allowing_then_check_gates_exe_by_basename() {
        let entries = [ed_str_from("git"), ed_str_from("cargo")];
        let allowlist = ed_slice_over(&entries);
        let cap = unsafe { __edda_subprocess_allowing(std::ptr::null(), allowlist) };
        assert!(!cap.is_null(), "allowing minted a null handle");
        // A listed command is permitted, whether named bare or by full path.
        assert_eq!(
            unsafe { __edda_subprocess_check(cap, ed_str_from("git"), ed_str_from("")) },
            CHECK_OK
        );
        assert_eq!(
            unsafe { __edda_subprocess_check(cap, ed_str_from("/usr/bin/git"), ed_str_from("")) },
            CHECK_OK
        );
        // An unlisted command is rejected.
        assert_eq!(
            unsafe { __edda_subprocess_check(cap, ed_str_from("rm"), ed_str_from("")) },
            CHECK_NOT_IN_ALLOWLIST
        );
    }

    #[test]
    fn allowing_intersects_across_calls() {
        let first = [ed_str_from("git"), ed_str_from("cargo")];
        let cap1 = unsafe { __edda_subprocess_allowing(std::ptr::null(), ed_slice_over(&first)) };
        let second = [ed_str_from("cargo"), ed_str_from("rm")];
        let cap2 = unsafe { __edda_subprocess_allowing(cap1, ed_slice_over(&second)) };
        // Only the intersection (`cargo`) survives; `git` and `rm` are gone.
        assert_eq!(
            unsafe { __edda_subprocess_check(cap2, ed_str_from("cargo"), ed_str_from("")) },
            CHECK_OK
        );
        assert_eq!(
            unsafe { __edda_subprocess_check(cap2, ed_str_from("git"), ed_str_from("")) },
            CHECK_NOT_IN_ALLOWLIST
        );
        assert_eq!(
            unsafe { __edda_subprocess_check(cap2, ed_str_from("rm"), ed_str_from("")) },
            CHECK_NOT_IN_ALLOWLIST
        );
    }

    #[test]
    fn empty_allowlist_permits_nothing() {
        let cap = unsafe { __edda_subprocess_allowing(std::ptr::null(), ed_slice_over(&[])) };
        assert_eq!(
            unsafe { __edda_subprocess_check(cap, ed_str_from("git"), ed_str_from("")) },
            CHECK_NOT_IN_ALLOWLIST
        );
    }

    #[test]
    fn scoped_to_then_check_gates_cwd() {
        // Scope to `base/inside`; `base/outside` is a sibling that must be
        // rejected, and a nested `base/inside/deep` must be permitted.
        let base = std::env::temp_dir().join(format!("edda_sub_scope_{}", std::process::id()));
        let inside = base.join("inside");
        let deep = inside.join("deep");
        let outside = base.join("outside");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let inside_str = std::fs::canonicalize(&inside).unwrap().to_str().unwrap().to_string();
        let cap = unsafe { __edda_subprocess_scoped_to(std::ptr::null(), ed_str_from(&inside_str)) };
        assert!(!cap.is_null(), "scoped_to minted a null handle");

        // The scope root itself is in scope.
        assert_eq!(
            unsafe { __edda_subprocess_check(cap, ed_str_from("true"), ed_str_from(&inside_str)) },
            CHECK_OK,
            "the scope root cwd should be permitted"
        );

        // A directory nested under the scope is in scope.
        let deep_str = std::fs::canonicalize(&deep).unwrap().to_str().unwrap().to_string();
        assert_eq!(
            unsafe { __edda_subprocess_check(cap, ed_str_from("true"), ed_str_from(&deep_str)) },
            CHECK_OK,
            "a cwd nested under the scope should be permitted"
        );

        // A sibling outside the scope is rejected.
        let outside_str = std::fs::canonicalize(&outside).unwrap().to_str().unwrap().to_string();
        assert_eq!(
            unsafe { __edda_subprocess_check(cap, ed_str_from("true"), ed_str_from(&outside_str)) },
            CHECK_CWD_OUTSIDE_SCOPE,
            "an out-of-scope cwd should be rejected"
        );

        // An empty cwd (inherit the parent's) is permitted regardless of scope.
        assert_eq!(
            unsafe { __edda_subprocess_check(cap, ed_str_from("true"), ed_str_from("")) },
            CHECK_OK,
            "an inherited (empty) cwd should be permitted"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scoped_to_preserves_allowlist_and_narrows_further() {
        let base = std::env::temp_dir().join(format!("edda_sub_both_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let root_str = std::fs::canonicalize(&base).unwrap().to_str().unwrap().to_string();

        let entries = [ed_str_from("git")];
        let allow_cap = unsafe { __edda_subprocess_allowing(std::ptr::null(), ed_slice_over(&entries)) };
        let both_cap = unsafe { __edda_subprocess_scoped_to(allow_cap, ed_str_from(&root_str)) };

        // The allowlist survives the further cwd narrowing.
        assert_eq!(
            unsafe { __edda_subprocess_check(both_cap, ed_str_from("git"), ed_str_from(&root_str)) },
            CHECK_OK
        );
        assert_eq!(
            unsafe { __edda_subprocess_check(both_cap, ed_str_from("rm"), ed_str_from(&root_str)) },
            CHECK_NOT_IN_ALLOWLIST
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
