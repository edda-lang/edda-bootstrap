//! std.os.raw primitive raw-memory intrinsics (`__edda_slice_from_raw`,
//! `__edda_slice_addr`, `__edda_load_*`, `__edda_store_*`).
//!
//! These back the `extern "__edda_*"` declarations in
//! `stdlib/os/raw/src/raw.ea`. The native compiler lowers the same symbols
//! *inline*, but the bootstrap treats them as ordinary
//! extern calls, so a bootstrap-compiled binary that pulls a pure-Edda stdlib
//! body using them (e.g. `str.parse_f64`'s scratch view) needs the runtime to
//! provide them as real symbols.
//!
//! Pure pointer arithmetic / load / store — target-agnostic, never touches the
//! OS. Loads and stores use the unaligned accessors because the suballocator
//! hands out byte-granular addresses with no natural-alignment guarantee.

use crate::EdSlice;

/// `-ENOSYS` — the Linux "function not implemented" errno, returned as a
/// negative `i64` per the kernel syscall return convention.
const NEG_ENOSYS: i64 = -38;

/// Raw six-argument syscall trampoline backing `std.os.raw.syscall6`.
///
/// Fail-closed stub: returns `-ENOSYS` unconditionally. See the invariants.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_syscall(
    _nr: u64,
    _a1: u64,
    _a2: u64,
    _a3: u64,
    _a4: u64,
    _a5: u64,
    _a6: u64,
) -> i64 {
    NEG_ENOSYS
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_slice_from_raw(addr: u64, len: u64) -> EdSlice {
    EdSlice {
        ptr: addr as *mut u8,
        len,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_slice_addr(buf: EdSlice) -> u64 {
    buf.ptr as u64
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_load_u8(addr: u64) -> u8 {
    unsafe { core::ptr::read_unaligned(addr as *const u8) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_load_u16(addr: u64) -> u16 {
    unsafe { core::ptr::read_unaligned(addr as *const u16) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_load_u32(addr: u64) -> u32 {
    unsafe { core::ptr::read_unaligned(addr as *const u32) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_load_u64(addr: u64) -> u64 {
    unsafe { core::ptr::read_unaligned(addr as *const u64) }
}

// `usize` is pointer-width (8 bytes on every v0.1 target), read as `u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_load_usize(addr: u64) -> u64 {
    unsafe { core::ptr::read_unaligned(addr as *const u64) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_store_u8(addr: u64, value: u8) {
    unsafe { core::ptr::write_unaligned(addr as *mut u8, value) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_store_u16(addr: u64, value: u16) {
    unsafe { core::ptr::write_unaligned(addr as *mut u16, value) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_store_u32(addr: u64, value: u32) {
    unsafe { core::ptr::write_unaligned(addr as *mut u32, value) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_store_u64(addr: u64, value: u64) {
    unsafe { core::ptr::write_unaligned(addr as *mut u64, value) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_store_usize(addr: u64, value: u64) {
    unsafe { core::ptr::write_unaligned(addr as *mut u64, value) }
}
