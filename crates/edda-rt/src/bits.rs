//! std.mem.bits primitive bit-twiddling intrinsics (pure scalar externs).

// ============================================================================
// std.mem.bits — primitive bit-twiddling intrinsics
// ============================================================================
//
// Each symbol backs a `stable function` in `stdlib/mem/bits/src/bits.ea`
// declared `@abi("__edda_bits_*")`. All are pure scalar ops with no
// capability, raise, or allocation — the C ABI passes the operand(s) by
// value and returns the scalar result. The zero-counting family returns
// `u32` for both widths (the bit position never exceeds 64), matching the
// source declarations (`count_ones_u64(x: u64) -> u32`). Rotate `n` is a
// `u32` and the source `where n < {32,64}` refinement keeps it in range —
// `rotate_{left,right}` is well-defined for any `n` regardless.

/// `x.count_ones()` — population count of the 32-bit operand.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u32_count_ones(x: u32) -> u32 { x.count_ones() }

/// `x.count_zeros()` — count of clear bits in the 32-bit operand.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u32_count_zeros(x: u32) -> u32 { x.count_zeros() }

/// `x.leading_zeros()` — clear high bits before the first set bit.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u32_leading_zeros(x: u32) -> u32 { x.leading_zeros() }

/// `x.trailing_zeros()` — clear low bits before the first set bit.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u32_trailing_zeros(x: u32) -> u32 { x.trailing_zeros() }

/// `x.rotate_left(n)` — cyclic left shift by `n` bits.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u32_rotate_left(x: u32, n: u32) -> u32 { x.rotate_left(n) }

/// `x.rotate_right(n)` — cyclic right shift by `n` bits.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u32_rotate_right(x: u32, n: u32) -> u32 { x.rotate_right(n) }

/// `x.swap_bytes()` — reverse the byte order of the 32-bit operand.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u32_swap_bytes(x: u32) -> u32 { x.swap_bytes() }

/// `x.reverse_bits()` — reverse the bit order of the 32-bit operand.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u32_reverse_bits(x: u32) -> u32 { x.reverse_bits() }

/// `x.count_ones()` — population count of the 64-bit operand.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u64_count_ones(x: u64) -> u32 { x.count_ones() }

/// `x.count_zeros()` — count of clear bits in the 64-bit operand.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u64_count_zeros(x: u64) -> u32 { x.count_zeros() }

/// `x.leading_zeros()` — clear high bits before the first set bit.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u64_leading_zeros(x: u64) -> u32 { x.leading_zeros() }

/// `x.trailing_zeros()` — clear low bits before the first set bit.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u64_trailing_zeros(x: u64) -> u32 { x.trailing_zeros() }

/// `x.rotate_left(n)` — cyclic left shift by `n` bits.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u64_rotate_left(x: u64, n: u32) -> u64 { x.rotate_left(n) }

/// `x.rotate_right(n)` — cyclic right shift by `n` bits.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u64_rotate_right(x: u64, n: u32) -> u64 { x.rotate_right(n) }

/// `x.swap_bytes()` — reverse the byte order of the 64-bit operand.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u64_swap_bytes(x: u64) -> u64 { x.swap_bytes() }

/// `x.reverse_bits()` — reverse the bit order of the 64-bit operand.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_bits_u64_reverse_bits(x: u64) -> u64 { x.reverse_bits() }
