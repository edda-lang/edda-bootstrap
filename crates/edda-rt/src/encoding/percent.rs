//! std.encoding.percent — RFC 3986 percent-encoding externs.

use std::mem::ManuallyDrop;

use crate::abi::{EdStr, alloc_edstr};

// ============================================================================
// std.encoding.percent — RFC 3986 percent-encoding for URLs
// ============================================================================

const PERCENT_ERR_INVALID_ESCAPE: u8 = 0;
const PERCENT_ERR_TRUNCATED_ESCAPE: u8 = 1;

/// `std.encoding.percent.PercentError` wire shape. Tag-only sum.
#[repr(C)]
struct EdPercentError {
    tag: u8,
}

/// `Result<String, PercentError>` — return shape for `__edda_percent_decode`.
#[repr(C)]
struct ResultEdStrPercentError {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultEdStrPercentErrorPayload,
}

#[repr(C)]
union ResultEdStrPercentErrorPayload {
    ok: ManuallyDrop<EdStr>,
    err: ManuallyDrop<EdPercentError>,
}

/// RFC 3986 unreserved set: `A-Z a-z 0-9 - _ . ~`.
fn percent_is_unreserved(byte: u8) -> bool {
    matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~')
}

/// Lowercase hex digit. Used to render `%XX` upper-case in the encode output
/// (we still emit upper-case per RFC 3986 §2.1 recommendation).
fn percent_hex_upper(nibble: u8) -> u8 {
    match nibble & 0x0F {
        n @ 0..=9 => b'0' + n,
        n => b'A' + (n - 10),
    }
}

/// Encode `s` according to a per-byte safe-set predicate plus an
/// optional "encode space as +" toggle for form-urlencoded.
fn percent_encode_with(s: &str, is_safe: impl Fn(u8) -> bool, space_is_plus: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &byte in s.as_bytes() {
        if is_safe(byte) {
            out.push(byte as char);
        } else if space_is_plus && byte == b' ' {
            out.push('+');
        } else {
            out.push('%');
            out.push(percent_hex_upper(byte >> 4) as char);
            out.push(percent_hex_upper(byte) as char);
        }
    }
    out
}

/// Hex-digit value (0-15). Returns None on non-hex input.
fn percent_hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Encode `s` as a URI component — unreserved set only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_percent_encode_component(_cap_alloc: *const (), s: EdStr) -> EdStr {
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    let src = std::str::from_utf8(bytes).unwrap_or("");
    alloc_edstr(&percent_encode_with(src, percent_is_unreserved, false))
}

/// Encode `s` as a URI path — unreserved set plus `/`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_percent_encode_path(_cap_alloc: *const (), s: EdStr) -> EdStr {
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    let src = std::str::from_utf8(bytes).unwrap_or("");
    alloc_edstr(&percent_encode_with(src, |b| percent_is_unreserved(b) || b == b'/', false))
}

/// Encode `s` for `application/x-www-form-urlencoded` — space → `+`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_percent_encode_query_form(_cap_alloc: *const (), s: EdStr) -> EdStr {
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    let src = std::str::from_utf8(bytes).unwrap_or("");
    alloc_edstr(&percent_encode_with(src, percent_is_unreserved, true))
}

/// Decode a percent-encoded string. `space_is_plus` converts `+` to space.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_percent_decode(
    _cap_alloc: *const (),
    s: EdStr,
    space_is_plus: bool,
) -> ResultEdStrPercentError {
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'%' {
            if i + 2 >= bytes.len() {
                return ResultEdStrPercentError {
                    tag: 1,
                    _pad: [0; 7],
                    payload: ResultEdStrPercentErrorPayload {
                        err: ManuallyDrop::new(EdPercentError {
                            tag: PERCENT_ERR_TRUNCATED_ESCAPE,
                        }),
                    },
                };
            }
            let hi = percent_hex_value(bytes[i + 1]);
            let lo = percent_hex_value(bytes[i + 2]);
            match (hi, lo) {
                (Some(h), Some(l)) => {
                    out.push((h << 4) | l);
                    i += 3;
                }
                _ => {
                    return ResultEdStrPercentError {
                        tag: 1,
                        _pad: [0; 7],
                        payload: ResultEdStrPercentErrorPayload {
                            err: ManuallyDrop::new(EdPercentError {
                                tag: PERCENT_ERR_INVALID_ESCAPE,
                            }),
                        },
                    };
                }
            }
        } else if space_is_plus && byte == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(byte);
            i += 1;
        }
    }
    let decoded = match String::from_utf8(out) {
        Ok(s) => s,
        Err(_) => {
            // Decoded bytes weren't valid UTF-8. Surface as invalid_escape so
            // the caller knows the result couldn't be a valid String. The
            // alternative — emit a dedicated `invalid_utf8` variant — is left
            // for a future Utf8Error refinement of PercentError.
            return ResultEdStrPercentError {
                tag: 1,
                _pad: [0; 7],
                payload: ResultEdStrPercentErrorPayload {
                    err: ManuallyDrop::new(EdPercentError {
                        tag: PERCENT_ERR_INVALID_ESCAPE,
                    }),
                },
            };
        }
    };
    ResultEdStrPercentError {
        tag: 0,
        _pad: [0; 7],
        payload: ResultEdStrPercentErrorPayload {
            ok: ManuallyDrop::new(alloc_edstr(&decoded)),
        },
    }
}
