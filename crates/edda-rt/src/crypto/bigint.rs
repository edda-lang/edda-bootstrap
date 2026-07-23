//! Arbitrary-precision unsigned big-integer externs (`std.math.bigint`),
//! backed by `num-bigint-dig`. `BigInt` is an opaque heap handle
//! (`linear type BigInt { handle: HeapPtr }`) — a single pointer crossing the
//! ABI as an 8-byte scalar (single-field-struct lowering); a value is a
//! leaked `Box<BigUint>` and `destroy` reclaims it.
//!
//! Result wire shapes (pinned from edda-mir's synthesize_result_adt + the
//! canonical sorted EffectRow shown by the bigint index.toon's effect_cone):
//! variant 0 = Ok; for the dual-err functions variant 1 = BigIntError,
//! variant 2 = AllocError (BigIntError sorts before AllocError). The Allocator
//! capability is honoured as an opaque leading pointer; Rust's global
//! allocator aborts on OOM, so the AllocError arm is never actually minted
//! (leak semantics, consistent with the rest of edda-rt).

use super::out_bytes;
use crate::EdSlice;
use num_bigint_dig::BigUint;
use num_bigint_dig::prime::probably_prime;
use num_integer::Integer;
use num_traits::Zero;

// BigIntError discriminants (declaration order in `std.math.bigint`).
const DIVISION_BY_ZERO: u8 = 0;
const NEGATIVE_RESULT: u8 = 1;
const BUFFER_TOO_SMALL: u8 = 2;

// __Result tags: Ok, then the canonical-sorted err variants.
const TAG_OK: u8 = 0;
const TAG_BIGINT_ERR: u8 = 1;
// (TAG_ALLOC_ERR = 2 is never minted — global-alloc aborts on OOM.)

// `Result<BigInt, ...>` wire shape (16 bytes): tag at 0, payload (8-byte
// HeapPtr, or err discriminant in its low byte) at offset 8.
#[repr(C)]
struct ResultBigInt {
    tag: u8,
    _pad: [u8; 7],
    payload: u64,
}

// `Result<(BigInt, BigInt), ...>` wire shape (24 bytes): tag, then the
// product `(HeapPtr, HeapPtr)` at offsets 8 and 16.
#[repr(C)]
struct ResultPair {
    tag: u8,
    _pad: [u8; 7],
    a: u64,
    b: u64,
}

// `Result<Option<BigInt>, ...>` wire shape (24 bytes): tag, then the
// `Option<BigInt>` payload (its own tag at offset 8, handle at offset 16).
#[repr(C)]
struct ResultOptionBigInt {
    tag: u8,
    _pad: [u8; 7],
    inner: u64,
    ptr: u64,
}

// `Result<(), BigIntError>` wire shape (2 bytes) for `to_bytes_be`.
#[repr(C)]
struct ResultUnitBigIntError {
    tag: u8,
    err: u8,
}

#[inline]
fn alloc_handle(v: BigUint) -> u64 {
    Box::into_raw(Box::new(v)) as u64
}

/// Borrow the `BigUint` behind a `let`-mode `BigInt` handle.
unsafe fn handle_ref<'a>(h: *mut u8) -> Option<&'a BigUint> {
    // SAFETY: a non-null handle was produced by `alloc_handle` (Box::into_raw).
    unsafe { (h as *const BigUint).as_ref() }
}

/// Allocate a zero-valued BigInt.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bigint_zero(_allocator: *const ()) -> ResultBigInt {
    ResultBigInt { tag: TAG_OK, _pad: [0; 7], payload: alloc_handle(BigUint::zero()) }
}

/// Construct a BigInt from big-endian bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_from_bytes_be(
    _allocator: *const (),
    bytes: EdSlice,
) -> ResultBigInt {
    // SAFETY: `bytes` satisfies the crypto-slice-borrow contract.
    let b = unsafe { super::in_bytes(bytes) };
    ResultBigInt { tag: TAG_OK, _pad: [0; 7], payload: alloc_handle(BigUint::from_bytes_be(b)) }
}

/// Clone a BigInt into a fresh handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_clone(_allocator: *const (), b: *mut u8) -> ResultBigInt {
    // SAFETY: `b` is a live borrowed handle.
    let v = unsafe { handle_ref(b) }.cloned().unwrap_or_else(BigUint::zero);
    ResultBigInt { tag: TAG_OK, _pad: [0; 7], payload: alloc_handle(v) }
}

/// Destroy a BigInt, freeing its heap slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_destroy(b: *mut u8) {
    if !b.is_null() {
        // SAFETY: `b` was produced by `alloc_handle` and is consumed (`take`) here.
        drop(unsafe { Box::from_raw(b as *mut BigUint) });
    }
}

/// Bit length of a BigInt.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_bit_length(b: *mut u8) -> u64 {
    // SAFETY: `b` is a live borrowed handle.
    unsafe { handle_ref(b) }.map_or(0, |v| v.bits() as u64)
}

/// Byte length of a BigInt.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_byte_length(b: *mut u8) -> u64 {
    // SAFETY: `b` is a live borrowed handle.
    unsafe { handle_ref(b) }.map_or(0, |v| ((v.bits() + 7) / 8) as u64)
}

/// Test whether a BigInt is zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_is_zero(b: *mut u8) -> u8 {
    // SAFETY: `b` is a live borrowed handle.
    unsafe { handle_ref(b) }.map_or(1, |v| v.is_zero() as u8)
}

/// Equality of two BigInts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_eq(a: *mut u8, b: *mut u8) -> u8 {
    // SAFETY: `a`/`b` are live borrowed handles.
    match unsafe { (handle_ref(a), handle_ref(b)) } {
        (Some(x), Some(y)) => (x == y) as u8,
        _ => 0,
    }
}

/// Strict less-than of two BigInts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_lt(a: *mut u8, b: *mut u8) -> u8 {
    // SAFETY: `a`/`b` are live borrowed handles.
    match unsafe { (handle_ref(a), handle_ref(b)) } {
        (Some(x), Some(y)) => (x < y) as u8,
        _ => 0,
    }
}

/// Encode a BigInt big-endian into a caller buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_to_bytes_be(b: *mut u8, out: *mut EdSlice) -> ResultUnitBigIntError {
    // SAFETY: `b` is a live borrowed handle; `out` satisfies crypto-slice-borrow-mut.
    let Some(v) = (unsafe { handle_ref(b) }) else {
        return ResultUnitBigIntError { tag: TAG_OK, err: 0 };
    };
    let bytes = v.to_bytes_be();
    let o = unsafe { out_bytes(out) };
    if bytes.len() > o.len() {
        return ResultUnitBigIntError { tag: 1, err: BUFFER_TOO_SMALL };
    }
    o.fill(0);
    let start = o.len() - bytes.len();
    o[start..].copy_from_slice(&bytes);
    ResultUnitBigIntError { tag: TAG_OK, err: 0 }
}

/// Add two BigInts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_add(_allocator: *const (), a: *mut u8, b: *mut u8) -> ResultBigInt {
    // SAFETY: `a`/`b` are live borrowed handles.
    let (Some(x), Some(y)) = (unsafe { (handle_ref(a), handle_ref(b)) }) else {
        return ResultBigInt { tag: TAG_OK, _pad: [0; 7], payload: alloc_handle(BigUint::zero()) };
    };
    ResultBigInt { tag: TAG_OK, _pad: [0; 7], payload: alloc_handle(x + y) }
}

/// Subtract two BigInts (unsigned).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_sub(_allocator: *const (), a: *mut u8, b: *mut u8) -> ResultBigInt {
    // SAFETY: `a`/`b` are live borrowed handles.
    let (Some(x), Some(y)) = (unsafe { (handle_ref(a), handle_ref(b)) }) else {
        return ResultBigInt { tag: TAG_BIGINT_ERR, _pad: [0; 7], payload: NEGATIVE_RESULT as u64 };
    };
    if x < y {
        return ResultBigInt { tag: TAG_BIGINT_ERR, _pad: [0; 7], payload: NEGATIVE_RESULT as u64 };
    }
    ResultBigInt { tag: TAG_OK, _pad: [0; 7], payload: alloc_handle(x - y) }
}

/// Multiply two BigInts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_mul(_allocator: *const (), a: *mut u8, b: *mut u8) -> ResultBigInt {
    // SAFETY: `a`/`b` are live borrowed handles.
    let (Some(x), Some(y)) = (unsafe { (handle_ref(a), handle_ref(b)) }) else {
        return ResultBigInt { tag: TAG_OK, _pad: [0; 7], payload: alloc_handle(BigUint::zero()) };
    };
    ResultBigInt { tag: TAG_OK, _pad: [0; 7], payload: alloc_handle(x * y) }
}

/// Euclidean division of two BigInts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_divmod(_allocator: *const (), a: *mut u8, b: *mut u8) -> ResultPair {
    // SAFETY: `a`/`b` are live borrowed handles.
    let (Some(x), Some(y)) = (unsafe { (handle_ref(a), handle_ref(b)) }) else {
        return ResultPair { tag: TAG_BIGINT_ERR, _pad: [0; 7], a: DIVISION_BY_ZERO as u64, b: 0 };
    };
    if y.is_zero() {
        return ResultPair { tag: TAG_BIGINT_ERR, _pad: [0; 7], a: DIVISION_BY_ZERO as u64, b: 0 };
    }
    let (q, r) = x.div_rem(y);
    ResultPair { tag: TAG_OK, _pad: [0; 7], a: alloc_handle(q), b: alloc_handle(r) }
}

/// Modular exponentiation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_mod_pow(
    _allocator: *const (),
    base: *mut u8,
    exp: *mut u8,
    m: *mut u8,
) -> ResultBigInt {
    // SAFETY: handles are live borrows.
    let (Some(b), Some(e), Some(md)) = (unsafe { (handle_ref(base), handle_ref(exp), handle_ref(m)) })
    else {
        return ResultBigInt { tag: TAG_BIGINT_ERR, _pad: [0; 7], payload: DIVISION_BY_ZERO as u64 };
    };
    if md.is_zero() {
        return ResultBigInt { tag: TAG_BIGINT_ERR, _pad: [0; 7], payload: DIVISION_BY_ZERO as u64 };
    }
    ResultBigInt { tag: TAG_OK, _pad: [0; 7], payload: alloc_handle(b.modpow(e, md)) }
}

/// Modular inverse.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_mod_inv(
    _allocator: *const (),
    a: *mut u8,
    m: *mut u8,
) -> ResultOptionBigInt {
    // SAFETY: handles are live borrows.
    let (Some(x), Some(md)) = (unsafe { (handle_ref(a), handle_ref(m)) }) else {
        return ResultOptionBigInt { tag: TAG_BIGINT_ERR, _pad: [0; 7], inner: DIVISION_BY_ZERO as u64, ptr: 0 };
    };
    if md.is_zero() {
        return ResultOptionBigInt { tag: TAG_BIGINT_ERR, _pad: [0; 7], inner: DIVISION_BY_ZERO as u64, ptr: 0 };
    }
    match num_bigint_dig::ModInverse::mod_inverse(x.clone(), md.clone()).and_then(|i| i.to_biguint()) {
        Some(inv) => ResultOptionBigInt { tag: TAG_OK, _pad: [0; 7], inner: 0, ptr: alloc_handle(inv) },
        None => ResultOptionBigInt { tag: TAG_OK, _pad: [0; 7], inner: 1, ptr: 0 },
    }
}

/// Probabilistic primality test.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_bigint_is_probable_prime(b: *mut u8, rounds: u64) -> u8 {
    // SAFETY: `b` is a live borrowed handle.
    unsafe { handle_ref(b) }.map_or(0, |v| probably_prime(v, rounds as usize) as u8)
}
