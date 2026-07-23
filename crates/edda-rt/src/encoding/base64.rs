//! std.encoding.base64 — RFC 4648 §4 standard base64 (padded) encode and
//! decode externs.
//!
//! Result wire shapes (pinned from edda-mir's synthesize_result_adt + the
//! canonical sorted EffectRow shown by base64/src/index.toon's effect_cone):
//!   - `encode -> String with {allocator, err: alloc.AllocError}`:
//!     variant 0 = Ok(String), variant 1 = AllocError (never minted — the
//!     global allocator aborts on OOM, leak semantics).
//!   - `decode -> [u8] with {allocator, err: alloc.AllocError, err: Base64Error}`:
//!     variant 0 = Ok([u8]), variant 1 = Base64Error, variant 2 = AllocError
//!     (Base64Error sorts before AllocError — local type interned first).

use std::mem::ManuallyDrop;

use crate::abi::{EdSlice, EdStr, alloc_edslice, alloc_edstr};

// Base64Error discriminants (declaration order in `std.encoding.base64`).
const B64_ERR_MALFORMED_LENGTH: u8 = 0;
const B64_ERR_INVALID_CHAR: u8 = 1;
const B64_ERR_INVALID_PADDING: u8 = 2;

// __Result tags: Ok, then the canonical-sorted err variants.
const TAG_OK: u8 = 0;
const TAG_B64_ERR: u8 = 1;
// (TAG_ALLOC_ERR = 2 is never minted — global-alloc aborts on OOM.)

const B64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PAD: u8 = b'=';

// `Result<String, AllocError>` wire shape (24 bytes).
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

// `Result<[u8], Base64Error, AllocError>` wire shape (24 bytes).
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

// `Result<usize, Base64Error>` wire shape (16 bytes) for `decode_into`.
#[repr(C)]
struct ResultUsizeB64Error {
    tag: u8,
    _pad: [u8; 7],
    value: u64,
}

/// Reverse-alphabet lookup. Returns the 6-bit value for a valid standard
/// base64 character, or `None` for anything else (including `=`).
fn b64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Encode `src` as standard padded base64 into `out` (an existing buffer),
/// appending exactly `((src.len()+2)/3)*4` bytes. Shared by `encode` /
/// `encode_into`.
fn b64_encode_into_vec(src: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < src.len() {
        let take = (src.len() - i).min(3);
        let b0 = src[i] as u32;
        let b1 = if take > 1 { src[i + 1] as u32 } else { 0 };
        let b2 = if take > 2 { src[i + 2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ALPHABET[((n >> 18) & 0x3F) as usize]);
        out.push(B64_ALPHABET[((n >> 12) & 0x3F) as usize]);
        out.push(if take > 1 { B64_ALPHABET[((n >> 6) & 0x3F) as usize] } else { PAD });
        out.push(if take > 2 { B64_ALPHABET[(n & 0x3F) as usize] } else { PAD });
        i += 3;
    }
}

/// Encode `bytes` as a padded base64 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_base64_encode(_allocator: *const (), bytes: EdSlice) -> ResultEdStr {
    // SAFETY: `bytes` is a live `let`-mode `[u8]` slice argument.
    let src = unsafe { std::slice::from_raw_parts(bytes.ptr as *const u8, bytes.len as usize) };
    let mut buf: Vec<u8> = Vec::with_capacity(((src.len() + 2) / 3) * 4);
    b64_encode_into_vec(src, &mut buf);
    // SAFETY: the alphabet and `=` are all ASCII, so `buf` is valid UTF-8.
    let s = unsafe { String::from_utf8_unchecked(buf) };
    ResultEdStr {
        tag: TAG_OK,
        _pad: [0; 7],
        payload: ResultEdStrPayload { ok: ManuallyDrop::new(alloc_edstr(&s)) },
    }
}

/// Encode `bytes` as padded base64 directly into `out` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_base64_encode_into(
    bytes: EdSlice,
    out: *mut EdSlice,
    offset: u64,
) -> u64 {
    // SAFETY: `bytes` is a live `[u8]`; `out` heads the caller's `mutable [u8]` slot.
    let src = unsafe { std::slice::from_raw_parts(bytes.ptr as *const u8, bytes.len as usize) };
    let dst = unsafe { super::out_slice(out) };
    let mut buf: Vec<u8> = Vec::with_capacity(((src.len() + 2) / 3) * 4);
    b64_encode_into_vec(src, &mut buf);
    let start = offset as usize;
    let n = buf.len().min(dst.len().saturating_sub(start));
    dst[start..start + n].copy_from_slice(&buf[..n]);
    buf.len() as u64
}

/// Decode a padded base64 string into a fresh `[u8]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_base64_decode(_allocator: *const (), s: EdStr) -> ResultEdSlice {
    // SAFETY: `s` is a live `let`-mode `String` argument.
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    let mut out: Vec<u8> = Vec::new();
    match b64_decode_into_vec(bytes, &mut out) {
        Ok(()) => ResultEdSlice {
            tag: TAG_OK,
            _pad: [0; 7],
            payload: ResultEdSlicePayload { ok: ManuallyDrop::new(alloc_edslice(&out)) },
        },
        Err(discriminant) => ResultEdSlice {
            tag: TAG_B64_ERR,
            _pad: [0; 7],
            payload: ResultEdSlicePayload { err_discriminant: discriminant as u64 },
        },
    }
}

/// Decode a padded base64 string directly into `out` at `offset`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_base64_decode_into(
    s: EdStr,
    out: *mut EdSlice,
    offset: u64,
) -> ResultUsizeB64Error {
    // SAFETY: `s` is a live `String`; `out` heads the caller's `mutable [u8]` slot.
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) };
    let mut decoded: Vec<u8> = Vec::new();
    match b64_decode_into_vec(bytes, &mut decoded) {
        Ok(()) => {
            let dst = unsafe { super::out_slice(out) };
            let start = offset as usize;
            let n = decoded.len().min(dst.len().saturating_sub(start));
            dst[start..start + n].copy_from_slice(&decoded[..n]);
            ResultUsizeB64Error { tag: TAG_OK, _pad: [0; 7], value: decoded.len() as u64 }
        }
        Err(discriminant) => {
            ResultUsizeB64Error { tag: TAG_B64_ERR, _pad: [0; 7], value: discriminant as u64 }
        }
    }
}

/// Decode `bytes` (standard padded base64) into `out`, or return a
/// Base64Error discriminant.
fn b64_decode_into_vec(bytes: &[u8], out: &mut Vec<u8>) -> Result<(), u8> {
    if bytes.len() % 4 != 0 {
        return Err(B64_ERR_MALFORMED_LENGTH);
    }
    out.reserve((bytes.len() / 4) * 3);
    let mut i = 0;
    while i < bytes.len() {
        let quad = &bytes[i..i + 4];
        let pad = pad_count(quad)?;
        // Padding is admitted only in the final quad.
        if pad > 0 && i + 4 != bytes.len() {
            return Err(B64_ERR_INVALID_PADDING);
        }
        let mut n: u32 = 0;
        for (k, &b) in quad.iter().enumerate() {
            let v = if b == PAD { 0 } else { b64_value(b).ok_or(B64_ERR_INVALID_CHAR)? };
            n |= (v as u32) << (18 - k * 6);
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
        i += 4;
    }
    Ok(())
}

/// Count and validate the trailing padding of a 4-byte base64 quad.
fn pad_count(quad: &[u8]) -> Result<usize, u8> {
    let pad = quad.iter().filter(|&&b| b == PAD).count();
    if pad == 0 {
        return Ok(0);
    }
    // All padding must be contiguous at the tail (positions 4-pad .. 4).
    if pad > 2 || quad[4 - pad..].iter().any(|&b| b != PAD) {
        return Err(B64_ERR_INVALID_PADDING);
    }
    Ok(pad)
}
