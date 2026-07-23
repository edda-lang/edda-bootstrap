//! Shared Edda runtime ABI types and heap-leaking constructors.
//!
//! `EdStr` and `EdSlice` are the fat-pointer wire shapes every `__edda_*`
//! symbol exchanges with generated code; they live here (rather than in any
//! single domain module) because every domain module — and the `crypto`,
//! `socket`, `subprocess` submodules — references them. The crate root
//! re-exports all four items as `crate::*` so existing `use crate::EdSlice`
//! call sites in those submodules keep resolving.

/// Edda String fat pointer. Matches LLVM `{ ptr, i64 }` (16 bytes on x86-64).
#[repr(C)]
pub(crate) struct EdStr {
    pub(crate) ptr: *mut u8,
    pub(crate) len: u64,
}

/// `[T]` slice fat pointer. Matches LLVM `{ ptr, i64 }` exactly the
/// same way `EdStr` does — the slice and string share wire shape on
/// every v0.1 target. Canonical definition lives in `edda-rt-alloc`
/// (its externs return it); re-exported here so every domain module
/// keeps resolving `crate::EdSlice`.
pub(crate) use edda_rt_alloc::EdSlice;

/// Allocate and leak a heap copy of `s`'s bytes, returning the owning fat-pointer.
pub(crate) fn alloc_edstr(s: &str) -> EdStr {
    let boxed: Box<[u8]> = s.as_bytes().to_vec().into_boxed_slice();
    let len = boxed.len() as u64;
    let ptr = Box::into_raw(boxed) as *mut u8;
    crate::stats::record(crate::stats::K_LEAK, len);
    EdStr { ptr, len }
}

/// Allocate and leak a heap copy of `bytes`, returning the owning fat-pointer.
pub(crate) fn alloc_edslice(bytes: &[u8]) -> EdSlice {
    let boxed: Box<[u8]> = bytes.to_vec().into_boxed_slice();
    let len = boxed.len() as u64;
    let ptr = Box::into_raw(boxed) as *mut u8;
    crate::stats::record(crate::stats::K_LEAK, len);
    EdSlice { ptr, len }
}
