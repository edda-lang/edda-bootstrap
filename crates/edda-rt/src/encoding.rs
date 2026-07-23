//! std.encoding.* runtime externs: UTF-16, RFC 3986 percent, RFC 4648
//! base32 / base64, lowercase hex.

use crate::EdSlice;

mod base32;
mod base64;
mod hex;
mod percent;
mod utf16;

/// Borrow a `mutable [u8]` out-param slot as a writable byte slice. Shared
/// by the `*_encode_into` / `*_decode_into` externs.
pub(super) unsafe fn out_slice<'a>(s: *mut EdSlice) -> &'a mut [u8] {
    if s.is_null() {
        return &mut [];
    }
    // SAFETY: caller asserts `s` heads the live `mutable [u8]` slot.
    let slot = unsafe { &*s };
    if slot.ptr.is_null() || slot.len == 0 {
        return &mut [];
    }
    // SAFETY: `slot.ptr` heads `slot.len` writable bytes.
    unsafe { std::slice::from_raw_parts_mut(slot.ptr, slot.len as usize) }
}
