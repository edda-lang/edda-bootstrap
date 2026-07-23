//! Non-shadowable remainder of the `_raw` alloc-family runtime externs.
//!
//! The six symbols `std.mem.rt` shadows via `@abi("__edda_*")` —
//! `__edda_alloc_raw` / `__edda_alloc_array_raw` / `__edda_realloc_array_raw`
//! / `__edda_free_raw` / `__edda_box_read_raw` / `__edda_box_unbox_raw` —
//! live in the `edda-rt-alloc` crate so they occupy their own archive
//! member of the bundled staticlib and are never force-pulled alongside
//! this crate's io/str/panic symbols.
//! `__edda_copy_array_raw` stays here: `std.mem.rt` does not shadow it,
//! so it must be loadable without dragging the shadowable six along.

use crate::abi::EdSlice;

/// Copy `src.len` elements of `size` bytes each from `src` into `dst`
/// (memcpy semantics — non-overlapping). Layout-agnostic: the prepended
/// `size` is `size_of(T)`, so this works for any copy-typed `T`. Backs
/// the bulk fast-path of `slice.copy` / `Vec.extend_from_slice`.
///
/// Wire signature mirrors the MIR-side rewritten extern:
/// `(size: usize, align: usize, dst: mutable [T], src: [T]) -> ()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_copy_array_raw(
    size: u64,
    _align: u64,
    dst: EdSlice,
    src: EdSlice,
) {
    if src.ptr.is_null() || dst.ptr.is_null() || src.len == 0 || size == 0 {
        return;
    }
    // Defensive backstop: never write past `dst`. The safe wrappers'
    // `where dst.len() >= src.len()` refinement makes the clamp a no-op
    // in practice (`n == src.len`).
    let n = src.len.min(dst.len) as usize;
    let bytes = match n.checked_mul(size as usize) {
        Some(b) => b,
        None => return,
    };
    // SAFETY: `src.ptr` heads `src.len * size` readable bytes and `dst.ptr`
    // heads `dst.len * size` writable bytes (both prior alloc-family
    // allocations); `bytes <= min(src.len, dst.len) * size` stays within
    // both. Non-overlap is the caller's contract (memcpy semantics).
    unsafe { std::ptr::copy_nonoverlapping(src.ptr, dst.ptr, bytes) };
}
