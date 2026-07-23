//! std.encoding.utf16 — UTF-16 LE/BE encode and decode externs.

use std::mem::ManuallyDrop;

use crate::abi::{EdSlice, EdStr, alloc_edslice, alloc_edstr};

// ============================================================================
// std.encoding.utf16 — UTF-16 LE/BE encode and decode
// ============================================================================

const UTF16_ERR_ODD_BYTE_LENGTH: u8 = 0;
const UTF16_ERR_UNPAIRED_HIGH_SURROGATE: u8 = 1;
const UTF16_ERR_UNPAIRED_LOW_SURROGATE: u8 = 2;

/// `std.encoding.utf16.Utf16Error` wire shape. Tag-only sum.
#[repr(C)]
struct EdUtf16Error {
    tag: u8,
}

/// `Result<String, Utf16Error>` — wire shape for `__edda_utf16_decode_le` /
/// `__edda_utf16_decode_be`. Payload union picks `EdStr` (16 bytes) over
/// `EdUtf16Error` (1 byte).
#[repr(C)]
struct ResultEdStrUtf16Error {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultEdStrUtf16ErrorPayload,
}

#[repr(C)]
union ResultEdStrUtf16ErrorPayload {
    ok: ManuallyDrop<EdStr>,
    err: ManuallyDrop<EdUtf16Error>,
}

/// Encode a `&str` as UTF-16 code units into a fresh byte vector. `little_endian`
/// chooses the byte order of each emitted u16. Result length is exactly
/// `2 * code_unit_count(s)`.
fn utf16_encode_bytes(s: &str, little_endian: bool) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(s.len() * 2);
    for unit in s.encode_utf16() {
        let bytes = if little_endian { unit.to_le_bytes() } else { unit.to_be_bytes() };
        out.extend_from_slice(&bytes);
    }
    out
}

/// Decode a UTF-16 byte stream into a UTF-8 `String`. Returns the discriminant
/// of an `EdUtf16Error` on failure (odd length, unpaired surrogate).
fn utf16_decode_bytes(bytes: &[u8], little_endian: bool) -> Result<String, u8> {
    if bytes.len() % 2 != 0 {
        return Err(UTF16_ERR_ODD_BYTE_LENGTH);
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if little_endian {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    let mut decoded = String::with_capacity(units.len());
    let mut iter = std::char::decode_utf16(units);
    while let Some(item) = iter.next() {
        match item {
            Ok(c) => decoded.push(c),
            Err(e) => {
                // `unpaired_surrogate()` returns the offending u16. Classify as
                // high vs low by the surrogate range it falls in.
                let u = e.unpaired_surrogate();
                let tag = if (0xD800..=0xDBFF).contains(&u) {
                    UTF16_ERR_UNPAIRED_HIGH_SURROGATE
                } else {
                    UTF16_ERR_UNPAIRED_LOW_SURROGATE
                };
                return Err(tag);
            }
        }
    }
    Ok(decoded)
}

/// Encode `s` as UTF-16 little-endian bytes; leaks an owning `EdSlice`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_utf16_encode_le(_cap_alloc: *const (), s: EdStr) -> EdSlice {
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    let s = std::str::from_utf8(bytes).unwrap_or("");
    alloc_edslice(&utf16_encode_bytes(s, true))
}

/// Encode `s` as UTF-16 big-endian bytes; leaks an owning `EdSlice`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_utf16_encode_be(_cap_alloc: *const (), s: EdStr) -> EdSlice {
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    let s = std::str::from_utf8(bytes).unwrap_or("");
    alloc_edslice(&utf16_encode_bytes(s, false))
}

/// Decode a UTF-16 little-endian byte stream into a `String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_utf16_decode_le(_cap_alloc: *const (), bytes: EdSlice) -> ResultEdStrUtf16Error {
    let slice = unsafe { std::slice::from_raw_parts(bytes.ptr as *const u8, bytes.len as usize) };
    match utf16_decode_bytes(slice, true) {
        Ok(s) => ResultEdStrUtf16Error {
            tag: 0,
            _pad: [0; 7],
            payload: ResultEdStrUtf16ErrorPayload {
                ok: ManuallyDrop::new(alloc_edstr(&s)),
            },
        },
        Err(tag) => ResultEdStrUtf16Error {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdStrUtf16ErrorPayload {
                err: ManuallyDrop::new(EdUtf16Error { tag }),
            },
        },
    }
}

/// Decode a UTF-16 big-endian byte stream into a `String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_utf16_decode_be(_cap_alloc: *const (), bytes: EdSlice) -> ResultEdStrUtf16Error {
    let slice = unsafe { std::slice::from_raw_parts(bytes.ptr as *const u8, bytes.len as usize) };
    match utf16_decode_bytes(slice, false) {
        Ok(s) => ResultEdStrUtf16Error {
            tag: 0,
            _pad: [0; 7],
            payload: ResultEdStrUtf16ErrorPayload {
                ok: ManuallyDrop::new(alloc_edstr(&s)),
            },
        },
        Err(tag) => ResultEdStrUtf16Error {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdStrUtf16ErrorPayload {
                err: ManuallyDrop::new(EdUtf16Error { tag }),
            },
        },
    }
}
