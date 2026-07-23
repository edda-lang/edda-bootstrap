//! String construction, equality, formatting, and float<->bits reinterpret externs.

use crate::abi::{EdStr, alloc_edstr};
use crate::EdSlice;

/// Concatenate two EdStr values into a new heap-allocated EdStr.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_string_concat(a: EdStr, b: EdStr) -> EdStr {
    let (a_bytes, b_bytes) = unsafe {
        (
            std::slice::from_raw_parts(a.ptr as *const u8, a.len as usize),
            std::slice::from_raw_parts(b.ptr as *const u8, b.len as usize),
        )
    };
    let mut combined = Vec::with_capacity(a.len as usize + b.len as usize);
    combined.extend_from_slice(a_bytes);
    combined.extend_from_slice(b_bytes);
    let boxed = combined.into_boxed_slice();
    let len = boxed.len() as u64;
    let ptr = Box::into_raw(boxed) as *mut u8;
    crate::stats::record(crate::stats::K_CONCAT, len);
    EdStr { ptr, len }
}

/// Construct a heap-owned `String` from an owned `[u8]` fat pointer.
///
/// Backs `std.text.string.from_owned_utf8(bytes: take [u8]) -> String`.
/// The native compiler lowers this symbol inline;
/// the bootstrap treats it as an ordinary extern, so the runtime provides
/// it as a real symbol. Because the input is `take`-mode, the bytes are
/// already owned by the caller and transferred wholesale — no copy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_string_from_raw(bytes: EdSlice) -> EdStr {
    EdStr { ptr: bytes.ptr, len: bytes.len }
}

/// Format an f64 as a decimal string.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_format_f64(val: f64) -> EdStr {
    alloc_edstr(&format!("{val}"))
}

/// Format an i64 as a decimal string.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_format_i64(val: i64) -> EdStr {
    alloc_edstr(&format!("{val}"))
}

/// Format a u64 as a decimal string.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_format_u64(val: u64) -> EdStr {
    alloc_edstr(&format!("{val}"))
}

/// Format an i128 (passed as two u64 halves to sidestep the win64
/// i128-by-value FFI ABI gap) as a decimal string.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_format_i128_lohi(lo: u64, hi: u64) -> EdStr {
    let val = (((hi as u128) << 64) | lo as u128) as i128;
    alloc_edstr(&format!("{val}"))
}

/// Format a u128 (passed as two u64 halves to sidestep the win64
/// i128-by-value FFI ABI gap) as a decimal string.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_format_u128_lohi(lo: u64, hi: u64) -> EdStr {
    let val = ((hi as u128) << 64) | lo as u128;
    alloc_edstr(&format!("{val}"))
}

/// Format a bool (0 or 1) as "false" or "true".
#[unsafe(no_mangle)]
pub extern "C" fn __edda_format_bool(val: u8) -> EdStr {
    alloc_edstr(if val != 0 { "true" } else { "false" })
}

/// Reinterpret an `f64`'s IEEE-754 storage as `u64`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_f64_to_bits(value: f64) -> u64 {
    value.to_bits()
}

/// Inverse of `__edda_f64_to_bits` — interpret a `u64` bit pattern as `f64`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_f64_from_bits(bits: u64) -> f64 {
    f64::from_bits(bits)
}

/// Reinterpret an `f32`'s IEEE-754 storage as `u32`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_f32_to_bits(value: f32) -> u32 {
    value.to_bits()
}

/// Inverse of `__edda_f32_to_bits` — interpret a `u32` bit pattern as `f32`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_f32_from_bits(bits: u32) -> f32 {
    f32::from_bits(bits)
}

/// Format a String value as a String (identity — clones the bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_format_str(val: EdStr) -> EdStr {
    let bytes = unsafe { std::slice::from_raw_parts(val.ptr as *const u8, val.len as usize) };
    let boxed: Box<[u8]> = bytes.to_vec().into_boxed_slice();
    let len = boxed.len() as u64;
    let ptr = Box::into_raw(boxed) as *mut u8;
    EdStr { ptr, len }
}
