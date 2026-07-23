//! The six shadowable `_raw` alloc-family runtime externs, isolated from
//! the rest of `edda-rt` so they land in their own archive member of the
//! bundled `edda_rt.lib` staticlib.
//!
//! `std.mem.rt` (`std/lib/mem/rt/src/rt.ea`) re-defines this exact family
//! via `@abi("__edda_*")` — region-heap implementations that honor the
//! allocator handle — in any member that imports it. When the whole crate
//! was one codegen unit inside `edda-rt`, referencing ANY rt symbol
//! (`__edda_format_i64`, `__edda_panic`, …) force-pulled the object that
//! also defined the alloc family, colliding with the member object's
//! shadow definitions and leaving resolution to `/FORCE:MULTIPLE`
//! scan-order. As a separate crate, this code is its own archive member:
//! a member object that shadows all six symbols never causes it to load,
//! so no duplicate exists and the shadow deterministically wins.
//!
//! `__edda_copy_array_raw` deliberately stays in `edda-rt` — `std.mem.rt`
//! does not shadow it, so it must always be loadable without dragging the
//! six shadowable symbols along.
//!
//! Everything in this file is a verbatim move from `edda-rt/src/alloc.rs`;
//! the wire shapes are locked (see RISKS.md) and must not drift. The
//! alloc-stats ledger lives in `edda-rt-stats` — a third crate, NOT this
//! one — because `edda-rt`'s string/abi modules also record into it, and a
//! same-member `stats` would let those references force-pull this object
//! (and its extern definitions) into every link. `edda-rt` re-exports
//! `EdSlice` and `stats` so its remaining modules keep resolving
//! `crate::*` paths.

#![allow(private_interfaces)] // ABI types are FFI-only; Rust visibility rules don't apply

use std::mem::ManuallyDrop;

pub use edda_rt_stats as stats;

// (The pre-rewrite allocator extern names — `__edda_alloc`,
// `__edda_alloc_array`, `__edda_free`, `__edda_box_read`,
// `__edda_box_unbox` — previously shipped as panic stubs in this
// position. The piece-2 MIR call rewrite redirects every alloc-family
// call site onto the `_raw`-suffixed symbols below, so the bare names
// are no longer reachable from generated code and the stubs were
// removed in piece 3 to keep the runtime surface honest. Restore a
// stub here if a future direct-extern site re-introduces a bare-name
// reference.)

// ============================================================================
// `_raw`-suffixed type-erased runtime — call-site size injection target
// ============================================================================
//
// The five symbols below are the post-rewrite targets of the MIR
// alloc-family call rewrite (`crates/edda-mir/src/lower/call.rs`). The
// rewrite recognises calls to the five reserved extern names declared in
// `stdlib/mem/alloc/src/alloc.ea` (and `stdlib/collections/hashmap/src/hashmap.ea` for
// `__edda_alloc_array`), retargets each call to its `_raw`-suffixed
// counterpart, and prepends two `usize` constants — `size_of(T)` and
// `align_of(T)` — as leading arguments. The runtime is type-erased: it
// consumes the layout constants explicitly and treats the value bytes
// opaquely.
//
// # Wire shapes
//
// The Rust signatures below mirror the MIR-side rewritten extern
// signatures so the link-time C ABI agrees. The MIR sig for the alloc
// family is the `extern "..."`-clause signature in `stdlib/mem/alloc/src/alloc.ea`
// (or `stdlib/collections/hashmap/src/hashmap.ea`) with two leading `usize` params
// prepended, and (for raising entries) `register_externs`-synthesised
// `__Result<T, E>` substituted for the bare return type.
//
// # Value passing for `__edda_alloc_raw`, `__edda_box_read_raw`,
//   `__edda_box_unbox_raw`
//
// Every transfer of an opaque `T` is expressed as a `*const u8` /
// `*mut u8` plus the `size` argument the MIR call-site rewrite already
// prepends. `copy_nonoverlapping` moves the bytes; there is no
// register-width ceiling on `T`.

/// `[T]` slice fat pointer. Matches LLVM `{ ptr, i64 }` — the slice and
/// string share wire shape on every v0.1 target. Canonical definition
/// lives here so the alloc family is self-contained; `edda-rt` re-exports
/// it for its remaining `EdSlice`-consuming modules.
#[repr(C)]
pub struct EdSlice {
    pub ptr: *mut u8,
    pub len: u64,
}

/// Box value layout matches `type Box { ptr: HeapPtr }` after the
/// piece-1 MIR-level field synthesis — a single pointer-width field.
/// Returned by `__edda_alloc_raw` (wrapped in `__Result<Box, AllocError>`)
/// and consumed by `__edda_free_raw` / `__edda_box_*_raw`.
#[repr(C)]
struct EdBox {
    ptr: *mut u8,
}

/// `std.alloc.AllocError` wire shape. Single-variant sum
/// (`out_of_memory`), so the discriminant is the only payload.
#[repr(C)]
struct EdAllocError {
    tag: u8,
}

/// `__Result<Box, AllocError>` — return shape of `__edda_alloc_raw`.
/// Tag at offset 0, then a 7-byte pad, then the payload at offset 8
/// (Box wins the union — 8 bytes vs AllocError's 1).
#[repr(C)]
struct ResultBoxAllocError {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultBoxAllocErrorPayload,
}

#[repr(C)]
union ResultBoxAllocErrorPayload {
    ok: ManuallyDrop<EdBox>,
    err: ManuallyDrop<EdAllocError>,
}

/// `__Result<[T], AllocError>` — return shape of
/// `__edda_alloc_array_raw`. Payload union picks `EdSlice` (16 bytes)
/// over `EdAllocError` (1 byte).
#[repr(C)]
struct ResultSliceAllocError {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultSliceAllocErrorPayload,
}

#[repr(C)]
union ResultSliceAllocErrorPayload {
    ok: ManuallyDrop<EdSlice>,
    err: ManuallyDrop<EdAllocError>,
}

//   `align_of(T)`; both must be ≥ 0 and `align` must be a power of two
//   ≥ 1. The MIR rewrite enforces this — `align == 0` reaching the
//   runtime is a contract violation.
/// Build a `std::alloc::Layout` from `(size, align)`. Returns
/// `Err(())` for any invalid pair (zero-size with non-zero align, etc.)
/// so the runtime can produce an `out_of_memory` error rather than
/// abort.
fn layout_for(size: u64, align: u64) -> Result<std::alloc::Layout, ()> {
    std::alloc::Layout::from_size_align(size as usize, align as usize).map_err(|_| ())
}

fn alloc_error_result_box() -> ResultBoxAllocError {
    ResultBoxAllocError {
        tag: 1,
        _pad: [0; 7],
        payload: ResultBoxAllocErrorPayload {
            err: ManuallyDrop::new(EdAllocError { tag: 0 }),
        },
    }
}

fn alloc_error_result_slice() -> ResultSliceAllocError {
    ResultSliceAllocError {
        tag: 1,
        _pad: [0; 7],
        payload: ResultSliceAllocErrorPayload {
            err: ManuallyDrop::new(EdAllocError { tag: 0 }),
        },
    }
}

/// Allocate `size`-aligned-to-`align` bytes, copy the supplied value
/// into the new slot, return the owning `Box`. Layout-agnostic — the
/// `value_ptr` / `size` pair ships any `T` regardless of its register
/// layout, so this works uniformly for primitives, records, and
/// arbitrarily large sums.
///
/// Wire signature mirrors the MIR-side rewritten extern after the
/// LLVM-layer alloc-family ABI translation. Capability args lead by
/// convention so the slot order is:
/// `(allocator: Allocator, size: usize, align: usize, value_ptr: *const u8)
///   -> __Result<Box, AllocError>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_alloc_raw(
    _allocator: *const (),
    size: u64,
    align: u64,
    value_ptr: *const u8,
) -> ResultBoxAllocError {
    if size == 0 {
        return ResultBoxAllocError {
            tag: 0,
            _pad: [0; 7],
            payload: ResultBoxAllocErrorPayload {
                ok: ManuallyDrop::new(EdBox { ptr: std::ptr::null_mut() }),
            },
        };
    }
    let Ok(layout) = layout_for(size, align) else {
        return alloc_error_result_box();
    };
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        return alloc_error_result_box();
    }
    unsafe { std::ptr::copy_nonoverlapping(value_ptr, ptr, size as usize) };
    stats::record(stats::K_BOX, size);
    ResultBoxAllocError {
        tag: 0,
        _pad: [0; 7],
        payload: ResultBoxAllocErrorPayload {
            ok: ManuallyDrop::new(EdBox { ptr }),
        },
    }
}

/// Allocate a fresh, zero-initialised slice of `n` elements of size
/// `size` aligned to `align`. Returns the owning slice fat-pointer
/// wrapped in `__Result<[T], AllocError>`. Layout-agnostic — works for
/// any `T` because the slice itself is just a `(ptr, len)` pair.
///
/// Wire signature mirrors the MIR-side rewritten extern. Capability
/// args lead by convention so the slot order is:
/// `(allocator: Allocator, elem_size: usize, elem_align: usize, n: usize)
///   -> __Result<[T], AllocError>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_alloc_array_raw(
    _allocator: *const (),
    size: u64,
    align: u64,
    n: u64,
) -> ResultSliceAllocError {
    if n == 0 || size == 0 {
        return ResultSliceAllocError {
            tag: 0,
            _pad: [0; 7],
            payload: ResultSliceAllocErrorPayload {
                ok: ManuallyDrop::new(EdSlice { ptr: std::ptr::null_mut(), len: n }),
            },
        };
    }
    let total = match (size as usize).checked_mul(n as usize) {
        Some(t) => t,
        None => return alloc_error_result_slice(),
    };
    let Ok(layout) = std::alloc::Layout::from_size_align(total, align as usize) else {
        return alloc_error_result_slice();
    };
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        return alloc_error_result_slice();
    }
    stats::record(stats::K_ARRAY, total as u64);
    ResultSliceAllocError {
        tag: 0,
        _pad: [0; 7],
        payload: ResultSliceAllocErrorPayload {
            ok: ManuallyDrop::new(EdSlice { ptr, len: n }),
        },
    }
}

/// Free the heap slot backing `b`. Layout-reconstructed from
/// `(size, align)` so the global allocator's bookkeeping stays
/// consistent — Rust's `std::alloc::dealloc` requires the original
/// `Layout`.
///
/// Wire signature mirrors the MIR-side rewritten extern:
/// `(size: usize, align: usize, b: take Box) -> ()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_free_raw(size: u64, align: u64, b: EdBox) {
    if b.ptr.is_null() || size == 0 {
        return;
    }
    let Ok(layout) = layout_for(size, align) else {
        return;
    };
    unsafe { std::alloc::dealloc(b.ptr, layout) };
    stats::record_free(size);
}

/// Read `T` from the heap slot backing `b` into the caller-provided
/// `out_ptr`. Layout-agnostic — the `size` argument prepended by the
/// MIR call-site rewrite tells the runtime how many bytes to copy, so
/// `T` may be any width.
///
/// Wire signature mirrors the MIR-side rewritten extern after the
/// LLVM-layer alloc-family ABI translation:
/// `(size: usize, align: usize, b: Box, out_ptr: *mut u8) -> ()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_box_read_raw(
    size: u64,
    _align: u64,
    b: EdBox,
    out_ptr: *mut u8,
) {
    if b.ptr.is_null() || size == 0 || out_ptr.is_null() {
        return;
    }
    unsafe { std::ptr::copy_nonoverlapping(b.ptr, out_ptr, size as usize) };
}

/// Read `T` from the heap slot backing `b` into the caller-provided
/// `out_ptr`, then free the slot. Layout-agnostic — the `size`
/// argument tells the runtime how many bytes to copy, so `T` may be
/// any width.
///
/// Wire signature mirrors the MIR-side rewritten extern after the
/// LLVM-layer alloc-family ABI translation. Capability args lead by
/// convention so the slot order is:
/// `(allocator: mutable Allocator, size: usize, align: usize,
///   b: take Box, out_ptr: *mut u8) -> ()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_box_unbox_raw(
    _allocator: *const (),
    size: u64,
    align: u64,
    b: EdBox,
    out_ptr: *mut u8,
) {
    if b.ptr.is_null() || size == 0 || out_ptr.is_null() {
        return;
    }
    unsafe { std::ptr::copy_nonoverlapping(b.ptr, out_ptr, size as usize) };
    if let Ok(layout) = layout_for(size, align) {
        unsafe { std::alloc::dealloc(b.ptr, layout) };
        stats::record_free(size);
    }
}

/// Reallocate the array backing `data` to hold `n` elements of size
/// `size` aligned to `align`. Returns a fresh owning slice fat-pointer
/// wrapped in `__Result<[T], AllocError>`; the leading
/// `min(data.len, n)` elements are copied, any growth tail is
/// zero-initialised, and the old backing allocation is freed on the
/// success path. Layout-agnostic — works for any `T` because the
/// element bytes are moved opaquely.
///
/// Wire signature mirrors the MIR-side rewritten extern. Capability
/// args lead by convention so the slot order is:
/// `(allocator: Allocator, elem_size: usize, elem_align: usize,
///   data: take [T], n: usize) -> __Result<[T], AllocError>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_realloc_array_raw(
    _allocator: *const (),
    size: u64,
    align: u64,
    data: EdSlice,
    n: u64,
) -> ResultSliceAllocError {
    if n == 0 || size == 0 {
        return ResultSliceAllocError {
            tag: 0,
            _pad: [0; 7],
            payload: ResultSliceAllocErrorPayload {
                ok: ManuallyDrop::new(EdSlice { ptr: std::ptr::null_mut(), len: n }),
            },
        };
    }
    let total = match (size as usize).checked_mul(n as usize) {
        Some(t) => t,
        None => return alloc_error_result_slice(),
    };
    let Ok(layout) = std::alloc::Layout::from_size_align(total, align as usize) else {
        return alloc_error_result_slice();
    };
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        return alloc_error_result_slice();
    }
    if !data.ptr.is_null() && data.len > 0 {
        let keep = data.len.min(n) as usize;
        let bytes = keep.saturating_mul(size as usize);
        // SAFETY: `data.ptr` heads `data.len * size` readable bytes (a prior
        // alloc-family allocation); `ptr` heads `total >= bytes` writable bytes.
        unsafe { std::ptr::copy_nonoverlapping(data.ptr, ptr, bytes) };
        if let Some(old_total) = (data.len as usize).checked_mul(size as usize) {
            if let Ok(old_layout) =
                std::alloc::Layout::from_size_align(old_total, align as usize)
            {
                // SAFETY: `data` is callee-owned (`take [T]`) and its backing
                // allocation was produced by a prior alloc-family call with this
                // exact (`data.len * size`, `align`) layout; the surviving bytes
                // were copied into `ptr` above, so nothing reads the old buffer
                // after this point.
                unsafe { std::alloc::dealloc(data.ptr, old_layout) };
                stats::record_free(old_total as u64);
            }
        }
    }
    stats::record(stats::K_REALLOC, total as u64);
    stats::record_realloc_elem(size, total as u64, data.len, n);
    ResultSliceAllocError {
        tag: 0,
        _pad: [0; 7],
        payload: ResultSliceAllocErrorPayload {
            ok: ManuallyDrop::new(EdSlice { ptr, len: n }),
        },
    }
}
