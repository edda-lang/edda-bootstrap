//! Random runtime externs backing `std.math.random`.
//!
//! Two extern families share this module:
//!   - `__edda_random_*` — the ambient `Random` capability. Every draw
//!     pulls fresh bytes from the operating-system entropy source
//!     (`getrandom`), the same source `__edda_crypto_random_fill` uses;
//!     the capability slot is an ignored opaque pointer.
//!   - `__edda_det_random_*` — the `DeterministicRandom` narrowing. The
//!     capability VALUE is a heap handle to a splitmix64 state minted by
//!     `__edda_random_deterministic(seed)`; every draw advances the
//!     state behind the handle, so the sequence is reproducible from the
//!     seed, bit-identical across targets.
//!
//! ABI conventions (mirror the `extern "..."` signatures in
//! `stdlib/lib/math/random/src/random.ea`):
//!   - capability param (`Random` / `mutable Random`) → leading opaque
//!     `*const ()` slot (ignored by the ambient family)
//!   - the `nondet` row entry on the ambient family is a verification-only
//!     pure effect (`PureEffect::Nondet`) —
//!     it threads NO ABI slot, mirroring `divergence` / `cancellation`.
//!     So the ambient externs take a single ignored leading `_random`
//!     pointer, symmetric with the det family (whose row is `{rng}`)
//!   - `mutable DeterministicRandom` → the handle itself (`*mut DetState`),
//!     passed by value — state mutates behind the pointer, the handle
//!     never changes
//!   - `mutable [u8]` out-param → `*mut EdSlice` (pointer to the caller's slot)

use crate::EdSlice;
use crate::crypto::out_bytes;

/// Heap state behind a `DeterministicRandom` capability value.
struct DetState {
    s: u64,
}

/// Advance a splitmix64 state and return the next u64.
fn splitmix64(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Draw 8 fresh bytes from the OS entropy source as a u64.
fn os_u64() -> u64 {
    let mut buf = [0u8; 8];
    getrandom::getrandom(&mut buf).expect("random: OS entropy source unavailable");
    u64::from_le_bytes(buf)
}

/// Reduce a raw u64 draw into `[lo, hi)`.
fn in_range(raw: u64, lo: u64, hi: u64) -> u64 {
    lo + raw % (hi - lo)
}

/// `next_u64(rng: mutable Random) -> u64` — fresh OS entropy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_random_next_u64(_random: *const ()) -> u64 {
    os_u64()
}

/// `next_u32(rng: mutable Random) -> u32` — fresh OS entropy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_random_next_u32(_random: *const ()) -> u32 {
    os_u64() as u32
}

/// `in_range_u64(rng: mutable Random, lo, hi where lo < hi) -> u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_random_in_range_u64(
    _random: *const (),
    lo: u64,
    hi: u64,
) -> u64 {
    in_range(os_u64(), lo, hi)
}

/// `fill_bytes(rng: mutable Random, buf: mutable [u8]) -> ()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_random_fill_bytes(
    _random: *const (),
    buf: *mut EdSlice,
) {
    // SAFETY: `buf` satisfies the crypto-slice-borrow-mut contract.
    let out = unsafe { out_bytes(buf) };
    if out.is_empty() {
        return;
    }
    getrandom::getrandom(out).expect("random_fill_bytes: OS entropy source unavailable");
}

/// `Random.deterministic(seed) -> DeterministicRandom` — mint a seeded
/// splitmix64 state and return its heap handle as the capability value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_random_deterministic(_random: *const (), seed: u64) -> *mut DetState {
    Box::into_raw(Box::new(DetState { s: seed }))
}

/// Borrow the splitmix64 state behind a `DeterministicRandom` handle.
unsafe fn det_state<'a>(rng: *mut DetState) -> &'a mut DetState {
    // SAFETY: capability provenance — the handle came from
    // `__edda_random_deterministic` and Edda's linear-mode discipline
    // prevents concurrent aliasing of a `mutable DeterministicRandom`.
    unsafe { &mut *rng }
}

/// `det_next_u64(rng: mutable DeterministicRandom) -> u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_det_random_next_u64(rng: *mut DetState) -> u64 {
    // SAFETY: live det-random handle (capability provenance).
    splitmix64(&mut unsafe { det_state(rng) }.s)
}

/// `det_next_u32(rng: mutable DeterministicRandom) -> u32`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_det_random_next_u32(rng: *mut DetState) -> u32 {
    // SAFETY: live det-random handle (capability provenance).
    splitmix64(&mut unsafe { det_state(rng) }.s) as u32
}

/// `det_in_range_u64(rng: mutable DeterministicRandom, lo, hi where lo < hi) -> u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_det_random_in_range_u64(
    rng: *mut DetState,
    lo: u64,
    hi: u64,
) -> u64 {
    // SAFETY: live det-random handle (capability provenance).
    in_range(splitmix64(&mut unsafe { det_state(rng) }.s), lo, hi)
}

/// `det_fill_bytes(rng: mutable DeterministicRandom, buf: mutable [u8]) -> ()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_det_random_fill_bytes(rng: *mut DetState, buf: *mut EdSlice) {
    // SAFETY: `buf` satisfies the crypto-slice-borrow-mut contract.
    let out = unsafe { out_bytes(buf) };
    // SAFETY: live det-random handle (capability provenance).
    let state = unsafe { det_state(rng) };
    for chunk in out.chunks_mut(8) {
        let bytes = splitmix64(&mut state.s).to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
}
