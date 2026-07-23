//! Ed448 signature externs (RFC 8032).
//!
//! No audited pure-Rust Ed448 *signature* implementation exists in the
//! RustCrypto ecosystem today (`ed448-goldilocks` ships the curve, not a
//! vetted RFC 8032 signer). Per the "vetted crates only" decision these are
//! fail-closed pending a real backend: verify
//! returns `internal_error`, and sign / key-derivation abort rather than emit
//! an unsound result. Ed448 is not used by TLS 1.3, so this blocks no live
//! handshake.

use crate::EdSlice;

// `Result<bool, Ed448SigError>` wire shape. tag 0 = Ok(`val` as bool);
// tag 1 = Err(`val` as the error discriminant). 2 bytes, mirrors `ResultBool`.
#[repr(C)]
struct ResultBoolSigError {
    tag: u8,
    val: u8,
}

const ERR_INTERNAL: u8 = 2;

#[inline]
fn internal_error() -> ResultBoolSigError {
    ResultBoolSigError { tag: 1, val: ERR_INTERNAL }
}

/// Fail-closed sink for the not-yet-backed Ed448 operations.
fn not_implemented(what: &str) -> ! {
    panic!("edda-rt: {what} is not yet implemented");
}

/// Ed448 public-key derivation — fail-closed stub.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed448_public_from_private(
    _private_seed: EdSlice,
    _public_out: *mut EdSlice,
) {
    not_implemented("ed448 public_from_private");
}

/// Ed448 sign — fail-closed stub.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed448_sign(
    _private_seed: EdSlice,
    _message: EdSlice,
    _sig_out: *mut EdSlice,
) {
    not_implemented("ed448 sign");
}

/// Ed448 verify — fail-closed stub.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed448_verify(
    _public_key: EdSlice,
    _message: EdSlice,
    _signature: EdSlice,
) -> ResultBoolSigError {
    internal_error()
}

/// Ed448ctx sign — fail-closed stub.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed448_sign_ctx(
    _private_seed: EdSlice,
    _message: EdSlice,
    _context: EdSlice,
    _sig_out: *mut EdSlice,
) {
    not_implemented("ed448 sign_with_context");
}

/// Ed448ctx verify — fail-closed stub.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed448_verify_ctx(
    _public_key: EdSlice,
    _message: EdSlice,
    _context: EdSlice,
    _signature: EdSlice,
) -> ResultBoolSigError {
    internal_error()
}

/// Ed448ph sign — fail-closed stub.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed448_sign_ph(
    _private_seed: EdSlice,
    _prehash: EdSlice,
    _context: EdSlice,
    _sig_out: *mut EdSlice,
) {
    not_implemented("ed448 sign_prehashed");
}

/// Ed448ph verify — fail-closed stub.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed448_verify_ph(
    _public_key: EdSlice,
    _prehash: EdSlice,
    _context: EdSlice,
    _signature: EdSlice,
) -> ResultBoolSigError {
    internal_error()
}
