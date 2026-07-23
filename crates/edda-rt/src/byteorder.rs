//! std.mem.byteorder fixed-width little/big-endian read and write externs.

use crate::abi::EdSlice;

// ============================================================================
// std.mem.byteorder — fixed-width little/big-endian read and write
// ============================================================================
//
// Each symbol backs a `@abi("__edda_byteorder_*")` function in
// `stdlib/mem/byteorder/src/byteorder.ea`. Reads take the source buffer
// by value as a `[u8]` `EdSlice` plus a `usize` offset and return the
// decoded scalar; writes take the destination as a `mutable [u8]` slot
// (`*mut EdSlice`, B19) plus offset and value. The source `where offset +
// W <= buf.len()` refinement is discharged at compile time, so the
// runtime trusts the bounds — `read_at` / `write_at` defensively re-check
// against the live `EdSlice.len` to avoid out-of-bounds reads if a caller
// ever bypasses the verifier.

/// Copy `W` bytes out of `buf` starting at `offset` into a fixed array.
fn read_at<const W: usize>(buf: &EdSlice, offset: u64) -> [u8; W] {
    let mut out = [0u8; W];
    let end = offset.saturating_add(W as u64);
    if buf.ptr.is_null() || end > buf.len {
        return out;
    }
    // SAFETY: `buf.ptr` heads `buf.len` readable bytes and `offset + W <= buf.len`.
    let src = unsafe { std::slice::from_raw_parts(buf.ptr, buf.len as usize) };
    out.copy_from_slice(&src[offset as usize..offset as usize + W]);
    out
}

/// Copy a fixed `W`-byte array into the slot behind `buf` at `offset`.
unsafe fn write_at<const W: usize>(buf: *mut EdSlice, offset: u64, bytes: [u8; W]) {
    if buf.is_null() {
        return;
    }
    // SAFETY: caller asserts `buf` heads the live `mutable [u8]` slot.
    let slice = unsafe { &*buf };
    let end = offset.saturating_add(W as u64);
    if slice.ptr.is_null() || end > slice.len {
        return;
    }
    // SAFETY: `slice.ptr` heads `slice.len` writable bytes and `offset + W <= slice.len`.
    let dst = unsafe { std::slice::from_raw_parts_mut(slice.ptr, slice.len as usize) };
    dst[offset as usize..offset as usize + W].copy_from_slice(&bytes);
}

/// Read a little-endian `u16` at `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_byteorder_read_u16_le(buf: EdSlice, offset: u64) -> u16 {
    u16::from_le_bytes(read_at::<2>(&buf, offset))
}

/// Read a big-endian `u16` at `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_byteorder_read_u16_be(buf: EdSlice, offset: u64) -> u16 {
    u16::from_be_bytes(read_at::<2>(&buf, offset))
}

/// Write `value` as little-endian `u16` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_byteorder_write_u16_le(buf: *mut EdSlice, offset: u64, value: u16) {
    unsafe { write_at::<2>(buf, offset, value.to_le_bytes()) }
}

/// Write `value` as big-endian `u16` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_byteorder_write_u16_be(buf: *mut EdSlice, offset: u64, value: u16) {
    unsafe { write_at::<2>(buf, offset, value.to_be_bytes()) }
}

/// Read a little-endian `u32` at `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_byteorder_read_u32_le(buf: EdSlice, offset: u64) -> u32 {
    u32::from_le_bytes(read_at::<4>(&buf, offset))
}

/// Read a big-endian `u32` at `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_byteorder_read_u32_be(buf: EdSlice, offset: u64) -> u32 {
    u32::from_be_bytes(read_at::<4>(&buf, offset))
}

/// Write `value` as little-endian `u32` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_byteorder_write_u32_le(buf: *mut EdSlice, offset: u64, value: u32) {
    unsafe { write_at::<4>(buf, offset, value.to_le_bytes()) }
}

/// Write `value` as big-endian `u32` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_byteorder_write_u32_be(buf: *mut EdSlice, offset: u64, value: u32) {
    unsafe { write_at::<4>(buf, offset, value.to_be_bytes()) }
}

/// Read a little-endian `u64` at `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_byteorder_read_u64_le(buf: EdSlice, offset: u64) -> u64 {
    u64::from_le_bytes(read_at::<8>(&buf, offset))
}

/// Read a big-endian `u64` at `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_byteorder_read_u64_be(buf: EdSlice, offset: u64) -> u64 {
    u64::from_be_bytes(read_at::<8>(&buf, offset))
}

/// Write `value` as little-endian `u64` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_byteorder_write_u64_le(buf: *mut EdSlice, offset: u64, value: u64) {
    unsafe { write_at::<8>(buf, offset, value.to_le_bytes()) }
}

/// Write `value` as big-endian `u64` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_byteorder_write_u64_be(buf: *mut EdSlice, offset: u64, value: u64) {
    unsafe { write_at::<8>(buf, offset, value.to_be_bytes()) }
}

/// Read a little-endian `i32` at `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_byteorder_read_i32_le(buf: EdSlice, offset: u64) -> i32 {
    i32::from_le_bytes(read_at::<4>(&buf, offset))
}

/// Read a big-endian `i32` at `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_byteorder_read_i32_be(buf: EdSlice, offset: u64) -> i32 {
    i32::from_be_bytes(read_at::<4>(&buf, offset))
}

/// Write `value` as little-endian `i32` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_byteorder_write_i32_le(buf: *mut EdSlice, offset: u64, value: i32) {
    unsafe { write_at::<4>(buf, offset, value.to_le_bytes()) }
}

/// Write `value` as big-endian `i32` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_byteorder_write_i32_be(buf: *mut EdSlice, offset: u64, value: i32) {
    unsafe { write_at::<4>(buf, offset, value.to_be_bytes()) }
}

/// Read a little-endian `i64` at `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_byteorder_read_i64_le(buf: EdSlice, offset: u64) -> i64 {
    i64::from_le_bytes(read_at::<8>(&buf, offset))
}

/// Read a big-endian `i64` at `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_byteorder_read_i64_be(buf: EdSlice, offset: u64) -> i64 {
    i64::from_be_bytes(read_at::<8>(&buf, offset))
}

/// Write `value` as little-endian `i64` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_byteorder_write_i64_le(buf: *mut EdSlice, offset: u64, value: i64) {
    unsafe { write_at::<8>(buf, offset, value.to_le_bytes()) }
}

/// Write `value` as big-endian `i64` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_byteorder_write_i64_be(buf: *mut EdSlice, offset: u64, value: i64) {
    unsafe { write_at::<8>(buf, offset, value.to_be_bytes()) }
}
