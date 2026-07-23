//! ECDH externs: X25519, NIST P-256, and NIST P-384 key agreement.
//!
//! Keys cross the ABI as raw byte slices: X25519 uses 32-byte scalars and
//! 32-byte u-coordinates; the NIST curves use big-endian scalars and
//! uncompressed SEC1 points (`0x04 || X || Y`). Backed by `x25519-dalek`,
//! `p256`, and `p384`.

use super::{in_bytes, out_bytes};
use crate::EdSlice;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use rand_core::OsRng;

// `Result<(), X25519Error>` wire shape (1-variant error → 2-byte by-value sum).
// err discriminant: 0 = invalid_peer_public.
#[repr(C)]
struct ResultUnitX25519Error {
    tag: u8,
    err: u8,
}

// `Result<(), P256Error>` / `Result<(), P384Error>` wire shape.
// err discriminant: 0 = invalid_private_key, 1 = invalid_peer_public.
#[repr(C)]
struct ResultUnitNistError {
    tag: u8,
    err: u8,
}

const ERR_INVALID_PRIVATE: u8 = 0;
const ERR_INVALID_PEER: u8 = 1;

// ---------------------------------------------------------------------------
// X25519
// ---------------------------------------------------------------------------

/// Generate an X25519 keypair into the caller's 32-byte slots.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdh_x25519_generate_keypair(
    _random: *const (),
    private_key: *mut EdSlice,
    public_key: *mut EdSlice,
) {
    // SAFETY: each slot satisfies the crypto-slice-borrow-mut contract.
    let (priv_out, pub_out) = unsafe { (out_bytes(private_key), out_bytes(public_key)) };
    if priv_out.len() != 32 || pub_out.len() != 32 {
        return;
    }
    getrandom::getrandom(priv_out).expect("ecdh x25519 keygen: OS CSPRNG unavailable");
    let sk = x25519_dalek::StaticSecret::from(<[u8; 32]>::try_from(&*priv_out).unwrap());
    pub_out.copy_from_slice(x25519_dalek::PublicKey::from(&sk).as_bytes());
}

/// Derive the X25519 public key for `private_key`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdh_x25519_derive_public(
    private_key: EdSlice,
    public_key: *mut EdSlice,
) {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (priv_in, pub_out) = unsafe { (in_bytes(private_key), out_bytes(public_key)) };
    if priv_in.len() != 32 || pub_out.len() != 32 {
        return;
    }
    let sk = x25519_dalek::StaticSecret::from(<[u8; 32]>::try_from(priv_in).unwrap());
    pub_out.copy_from_slice(x25519_dalek::PublicKey::from(&sk).as_bytes());
}

/// X25519 shared-secret agreement.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdh_x25519_derive_shared(
    private_key: EdSlice,
    peer_public_key: EdSlice,
    shared_secret: *mut EdSlice,
) -> ResultUnitX25519Error {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (priv_in, peer_in, out) =
        unsafe { (in_bytes(private_key), in_bytes(peer_public_key), out_bytes(shared_secret)) };
    if priv_in.len() != 32 || peer_in.len() != 32 || out.len() != 32 {
        return ResultUnitX25519Error { tag: 1, err: 0 };
    }
    let sk = x25519_dalek::StaticSecret::from(<[u8; 32]>::try_from(priv_in).unwrap());
    let peer = x25519_dalek::PublicKey::from(<[u8; 32]>::try_from(peer_in).unwrap());
    let shared = sk.diffie_hellman(&peer);
    if !shared.was_contributory() {
        return ResultUnitX25519Error { tag: 1, err: 0 };
    }
    out.copy_from_slice(shared.as_bytes());
    ResultUnitX25519Error { tag: 0, err: 0 }
}

// ---------------------------------------------------------------------------
// NIST P-256
// ---------------------------------------------------------------------------

/// Generate a P-256 keypair (32-byte scalar, 65-byte SEC1 point).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdh_p256_generate_keypair(
    _random: *const (),
    private_key: *mut EdSlice,
    public_key: *mut EdSlice,
) {
    // SAFETY: each slot satisfies the crypto-slice-borrow-mut contract.
    let (priv_out, pub_out) = unsafe { (out_bytes(private_key), out_bytes(public_key)) };
    if priv_out.len() != 32 || pub_out.len() != 65 {
        return;
    }
    let sk = p256::SecretKey::random(&mut OsRng);
    priv_out.copy_from_slice(&sk.to_bytes());
    pub_out.copy_from_slice(sk.public_key().to_encoded_point(false).as_bytes());
}

/// Derive the uncompressed P-256 public point for `private_key`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdh_p256_derive_public(
    private_key: EdSlice,
    public_key: *mut EdSlice,
) -> ResultUnitNistError {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (priv_in, pub_out) = unsafe { (in_bytes(private_key), out_bytes(public_key)) };
    if priv_in.len() != 32 || pub_out.len() != 65 {
        return ResultUnitNistError { tag: 1, err: ERR_INVALID_PRIVATE };
    }
    let Ok(sk) = p256::SecretKey::from_bytes(p256::FieldBytes::from_slice(priv_in)) else {
        return ResultUnitNistError { tag: 1, err: ERR_INVALID_PRIVATE };
    };
    pub_out.copy_from_slice(sk.public_key().to_encoded_point(false).as_bytes());
    ResultUnitNistError { tag: 0, err: 0 }
}

/// P-256 ECDH shared-secret agreement.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdh_p256_derive_shared(
    private_key: EdSlice,
    peer_public_key: EdSlice,
    shared_secret: *mut EdSlice,
) -> ResultUnitNistError {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (priv_in, peer_in, out) =
        unsafe { (in_bytes(private_key), in_bytes(peer_public_key), out_bytes(shared_secret)) };
    if priv_in.len() != 32 || peer_in.len() != 65 || out.len() != 32 {
        return ResultUnitNistError { tag: 1, err: ERR_INVALID_PEER };
    }
    let Ok(sk) = p256::SecretKey::from_bytes(p256::FieldBytes::from_slice(priv_in)) else {
        return ResultUnitNistError { tag: 1, err: ERR_INVALID_PRIVATE };
    };
    let Ok(peer) = p256::PublicKey::from_sec1_bytes(peer_in) else {
        return ResultUnitNistError { tag: 1, err: ERR_INVALID_PEER };
    };
    let shared = p256::ecdh::diffie_hellman(sk.to_nonzero_scalar(), peer.as_affine());
    out.copy_from_slice(shared.raw_secret_bytes());
    ResultUnitNistError { tag: 0, err: 0 }
}

// ---------------------------------------------------------------------------
// NIST P-384
// ---------------------------------------------------------------------------

/// Generate a P-384 keypair (48-byte scalar, 97-byte SEC1 point).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdh_p384_generate_keypair(
    _random: *const (),
    private_key: *mut EdSlice,
    public_key: *mut EdSlice,
) {
    // SAFETY: each slot satisfies the crypto-slice-borrow-mut contract.
    let (priv_out, pub_out) = unsafe { (out_bytes(private_key), out_bytes(public_key)) };
    if priv_out.len() != 48 || pub_out.len() != 97 {
        return;
    }
    let sk = p384::SecretKey::random(&mut OsRng);
    priv_out.copy_from_slice(&sk.to_bytes());
    pub_out.copy_from_slice(sk.public_key().to_encoded_point(false).as_bytes());
}

/// Derive the uncompressed P-384 public point for `private_key`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdh_p384_derive_public(
    private_key: EdSlice,
    public_key: *mut EdSlice,
) -> ResultUnitNistError {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (priv_in, pub_out) = unsafe { (in_bytes(private_key), out_bytes(public_key)) };
    if priv_in.len() != 48 || pub_out.len() != 97 {
        return ResultUnitNistError { tag: 1, err: ERR_INVALID_PRIVATE };
    }
    let Ok(sk) = p384::SecretKey::from_bytes(p384::FieldBytes::from_slice(priv_in)) else {
        return ResultUnitNistError { tag: 1, err: ERR_INVALID_PRIVATE };
    };
    pub_out.copy_from_slice(sk.public_key().to_encoded_point(false).as_bytes());
    ResultUnitNistError { tag: 0, err: 0 }
}

/// P-384 ECDH shared-secret agreement.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdh_p384_derive_shared(
    private_key: EdSlice,
    peer_public_key: EdSlice,
    shared_secret: *mut EdSlice,
) -> ResultUnitNistError {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (priv_in, peer_in, out) =
        unsafe { (in_bytes(private_key), in_bytes(peer_public_key), out_bytes(shared_secret)) };
    if priv_in.len() != 48 || peer_in.len() != 97 || out.len() != 48 {
        return ResultUnitNistError { tag: 1, err: ERR_INVALID_PEER };
    }
    let Ok(sk) = p384::SecretKey::from_bytes(p384::FieldBytes::from_slice(priv_in)) else {
        return ResultUnitNistError { tag: 1, err: ERR_INVALID_PRIVATE };
    };
    let Ok(peer) = p384::PublicKey::from_sec1_bytes(peer_in) else {
        return ResultUnitNistError { tag: 1, err: ERR_INVALID_PEER };
    };
    let shared = p384::ecdh::diffie_hellman(sk.to_nonzero_scalar(), peer.as_affine());
    out.copy_from_slice(shared.raw_secret_bytes());
    ResultUnitNistError { tag: 0, err: 0 }
}
