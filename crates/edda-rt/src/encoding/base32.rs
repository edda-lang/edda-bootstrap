//! std.encoding.base32 — RFC 4648 base32 encode and decode externs.

use std::mem::ManuallyDrop;

use crate::abi::{EdSlice, EdStr, alloc_edslice, alloc_edstr};

// ============================================================================
// std.encoding.base32 — RFC 4648 base32 encode and decode
// ============================================================================

const BASE32_ERR_BAD_LENGTH: u8 = 0;
const BASE32_ERR_INVALID_CHAR: u8 = 1;
const BASE32_ERR_BAD_PADDING: u8 = 2;

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// `std.encoding.base32.Base32Error` wire shape. Tag-only sum.
#[repr(C)]
struct EdBase32Error {
    tag: u8,
}

/// `Result<[u8], Base32Error>` — return shape for `__edda_base32_decode`.
#[repr(C)]
struct ResultEdSliceBase32Error {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultEdSliceBase32ErrorPayload,
}

#[repr(C)]
union ResultEdSliceBase32ErrorPayload {
    ok: ManuallyDrop<EdSlice>,
    err: ManuallyDrop<EdBase32Error>,
}

/// Reverse-alphabet lookup. Returns the 5-bit value for a valid base32
/// character or `None` for anything else (including `=`, which the
/// padding logic handles separately).
fn base32_char_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a'),
        b'2'..=b'7' => Some(byte - b'2' + 26),
        _ => None,
    }
}

/// Encode `bytes` as a padded RFC 4648 base32 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_base32_encode(_cap_alloc: *const (), bytes: EdSlice) -> EdStr {
    let src = unsafe { std::slice::from_raw_parts(bytes.ptr as *const u8, bytes.len as usize) };
    let mut out = String::with_capacity(((src.len() + 4) / 5) * 8);
    let mut i = 0;
    while i < src.len() {
        // Pack up to 5 bytes (40 bits) into a u64 left-aligned.
        let mut buf: u64 = 0;
        let take = (src.len() - i).min(5);
        for k in 0..take {
            buf |= (src[i + k] as u64) << (32 - k * 8);
        }
        // Emit 8 base32 chars; pad the trailing portion not backed by input.
        let chars_to_emit = match take {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for k in 0..8 {
            if k < chars_to_emit {
                let idx = ((buf >> (35 - k * 5)) & 0x1F) as usize;
                out.push(BASE32_ALPHABET[idx] as char);
            } else {
                out.push('=');
            }
        }
        i += take;
    }
    alloc_edstr(&out)
}

/// Decode a padded RFC 4648 base32 string into a fresh `[u8]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_base32_decode(_cap_alloc: *const (), s: EdStr) -> ResultEdSliceBase32Error {
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    if bytes.len() % 8 != 0 {
        return ResultEdSliceBase32Error {
            tag: 1,
            _pad: [0; 7],
            payload: ResultEdSliceBase32ErrorPayload {
                err: ManuallyDrop::new(EdBase32Error { tag: BASE32_ERR_BAD_LENGTH }),
            },
        };
    }
    let mut out: Vec<u8> = Vec::with_capacity((bytes.len() / 8) * 5);
    let mut i = 0;
    while i < bytes.len() {
        let chunk = &bytes[i..i + 8];
        // Pad bytes only allowed at the tail; sweep left-to-right detecting
        // a transition from non-pad to pad, then validating the tail is all `=`.
        let pad_start = chunk.iter().position(|&b| b == b'=').unwrap_or(8);
        for &b in &chunk[pad_start..] {
            if b != b'=' {
                return ResultEdSliceBase32Error {
                    tag: 1,
                    _pad: [0; 7],
                    payload: ResultEdSliceBase32ErrorPayload {
                        err: ManuallyDrop::new(EdBase32Error { tag: BASE32_ERR_BAD_PADDING }),
                    },
                };
            }
        }
        // Output byte count from pad-start: 8→5, 7→4, 5→3, 4→2, 2→1.
        let out_bytes = match pad_start {
            8 => 5,
            7 => 4,
            5 => 3,
            4 => 2,
            2 => 1,
            _ => {
                return ResultEdSliceBase32Error {
                    tag: 1,
                    _pad: [0; 7],
                    payload: ResultEdSliceBase32ErrorPayload {
                        err: ManuallyDrop::new(EdBase32Error { tag: BASE32_ERR_BAD_PADDING }),
                    },
                };
            }
        };
        let mut buf: u64 = 0;
        for k in 0..pad_start {
            match base32_char_value(chunk[k]) {
                Some(v) => buf |= (v as u64) << (35 - k * 5),
                None => {
                    return ResultEdSliceBase32Error {
                        tag: 1,
                        _pad: [0; 7],
                        payload: ResultEdSliceBase32ErrorPayload {
                            err: ManuallyDrop::new(EdBase32Error { tag: BASE32_ERR_INVALID_CHAR }),
                        },
                    };
                }
            }
        }
        for k in 0..out_bytes {
            out.push(((buf >> (32 - k * 8)) & 0xFF) as u8);
        }
        i += 8;
    }
    ResultEdSliceBase32Error {
        tag: 0,
        _pad: [0; 7],
        payload: ResultEdSliceBase32ErrorPayload {
            ok: ManuallyDrop::new(alloc_edslice(&out)),
        },
    }
}
