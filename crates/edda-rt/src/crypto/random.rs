//! CSPRNG extern: fill a buffer from the operating-system entropy source.

use super::out_bytes;
use crate::EdSlice;

/// Fill `buf` with cryptographically secure random bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_crypto_random_fill(_random: *const (), buf: *mut EdSlice) {
    // SAFETY: `buf` satisfies the crypto-slice-borrow-mut contract.
    let out = unsafe { out_bytes(buf) };
    if out.is_empty() {
        return;
    }
    getrandom::getrandom(out).expect("crypto_random_fill: OS CSPRNG unavailable");
}
