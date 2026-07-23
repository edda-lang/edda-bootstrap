//! std.encoding.hex — RFC 4648 §8 lowercase hex encode and decode externs.
//!
//! Result wire shapes (pinned from edda-mir's synthesize_result_adt + the
//! canonical sorted EffectRow shown by hex/src/index.toon's effect_cone):
//!   - `encode -> String with {allocator, err: alloc.AllocError}`:
//!     variant 0 = Ok(String), variant 1 = AllocError (never minted — the
//!     global allocator aborts on OOM, leak semantics).
//!   - `decode -> [u8] with {allocator, err: alloc.AllocError, err: HexError}`:
//!     variant 0 = Ok([u8]), variant 1 = HexError, variant 2 = AllocError
//!     (HexError sorts before AllocError — local type interned first).
//! The `allocator` capability lowers to a **leading** opaque `*const ()`
//! arg (the compiler hoists all capabilities ahead of user params,
//! regardless of source position), ignored at runtime.

use std::mem::ManuallyDrop;

use crate::abi::{EdSlice, EdStr, alloc_edslice, alloc_edstr};

// HexError discriminants (declaration order in `std.encoding.hex`).
const HEX_ERR_ODD_LENGTH: u8 = 0;
const HEX_ERR_INVALID_CHAR: u8 = 1;

// __Result tags: Ok, then the canonical-sorted err variants.
const TAG_OK: u8 = 0;
const TAG_HEX_ERR: u8 = 1;
// (TAG_ALLOC_ERR = 2 is never minted — global-alloc aborts on OOM.)

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

// `Result<String, AllocError>` wire shape (24 bytes): tag, then the
// `EdStr` ok payload at offset 8.
#[repr(C)]
struct ResultEdStr {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultEdStrPayload,
}

#[repr(C)]
union ResultEdStrPayload {
    ok: ManuallyDrop<EdStr>,
    err_discriminant: u64,
}

// `Result<[u8], HexError, AllocError>` wire shape (24 bytes): tag, then
// the `EdSlice` ok payload (or err discriminant low byte) at offset 8.
#[repr(C)]
struct ResultEdSlice {
    tag: u8,
    _pad: [u8; 7],
    payload: ResultEdSlicePayload,
}

#[repr(C)]
union ResultEdSlicePayload {
    ok: ManuallyDrop<EdSlice>,
    err_discriminant: u64,
}

// `Result<usize, HexError>` wire shape (16 bytes) for `decode_into`.
#[repr(C)]
struct ResultUsizeHexError {
    tag: u8,
    _pad: [u8; 7],
    value: u64,
}

/// Hex-digit value (0-15) for an ASCII byte, accepting upper or lower case.
/// Returns `None` on any non-hex byte.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Encode `src` as lowercase hex into a fresh `String`.
fn hex_encode_string(src: &[u8]) -> String {
    let mut out = String::with_capacity(src.len() * 2);
    for &b in src {
        out.push(HEX_DIGITS[(b >> 4) as usize] as char);
        out.push(HEX_DIGITS[(b & 0x0F) as usize] as char);
    }
    out
}

/// Encode `bytes` as a lowercase hex string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hex_encode(_allocator: *const (), bytes: EdSlice) -> ResultEdStr {
    // SAFETY: `bytes` is a live `let`-mode `[u8]` slice argument.
    let src = unsafe { std::slice::from_raw_parts(bytes.ptr as *const u8, bytes.len as usize) };
    let s = hex_encode_string(src);
    ResultEdStr {
        tag: TAG_OK,
        _pad: [0; 7],
        payload: ResultEdStrPayload { ok: ManuallyDrop::new(alloc_edstr(&s)) },
    }
}

/// Encode `bytes` as lowercase hex directly into `out` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hex_encode_into(
    bytes: EdSlice,
    out: *mut EdSlice,
    offset: u64,
) -> u64 {
    // SAFETY: `bytes` is a live `[u8]`; `out` heads the caller's `mutable [u8]` slot.
    let src = unsafe { std::slice::from_raw_parts(bytes.ptr as *const u8, bytes.len as usize) };
    let dst = unsafe { super::out_slice(out) };
    let mut w = offset as usize;
    for &b in src {
        if w + 1 >= dst.len() {
            break;
        }
        dst[w] = HEX_DIGITS[(b >> 4) as usize];
        dst[w + 1] = HEX_DIGITS[(b & 0x0F) as usize];
        w += 2;
    }
    (src.len() * 2) as u64
}

/// Decode a hex string into a fresh `[u8]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hex_decode(_allocator: *const (), s: EdStr) -> ResultEdSlice {
    // SAFETY: `s` is a live `let`-mode `String` argument.
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    if bytes.len() % 2 != 0 {
        return hex_decode_err(HEX_ERR_ODD_LENGTH);
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let (Some(hi), Some(lo)) = (hex_value(bytes[i]), hex_value(bytes[i + 1])) else {
            return hex_decode_err(HEX_ERR_INVALID_CHAR);
        };
        out.push((hi << 4) | lo);
        i += 2;
    }
    ResultEdSlice {
        tag: TAG_OK,
        _pad: [0; 7],
        payload: ResultEdSlicePayload { ok: ManuallyDrop::new(alloc_edslice(&out)) },
    }
}

/// Decode a hex string directly into `out` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hex_decode_into(
    s: EdStr,
    out: *mut EdSlice,
    offset: u64,
) -> ResultUsizeHexError {
    // SAFETY: `s` is a live `String`; `out` heads the caller's `mutable [u8]` slot.
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    let dst = unsafe { super::out_slice(out) };
    let mut r = 0;
    let mut w = offset as usize;
    while r < bytes.len() {
        let (Some(hi), Some(lo)) = (hex_value(bytes[r]), hex_value(bytes[r + 1])) else {
            return ResultUsizeHexError { tag: TAG_HEX_ERR, _pad: [0; 7], value: HEX_ERR_INVALID_CHAR as u64 };
        };
        if w < dst.len() {
            dst[w] = (hi << 4) | lo;
        }
        w += 1;
        r += 2;
    }
    ResultUsizeHexError { tag: TAG_OK, _pad: [0; 7], value: (bytes.len() / 2) as u64 }
}

/// Build a tag-1 `Err(HexError)` decode result with `discriminant` in the payload.
fn hex_decode_err(discriminant: u8) -> ResultEdSlice {
    ResultEdSlice {
        tag: TAG_HEX_ERR,
        _pad: [0; 7],
        payload: ResultEdSlicePayload { err_discriminant: discriminant as u64 },
    }
}
