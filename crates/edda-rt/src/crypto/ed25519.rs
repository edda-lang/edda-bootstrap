//! Ed25519 signature externs (RFC 8032). The pure variant
//! (`public_from_private` / `sign` / `verify`) uses `ed25519-dalek`. The
//! Ed25519ctx and Ed25519ph variants take an externally-supplied context /
//! precomputed prehash, which `ed25519-dalek`'s API cannot accept (it always
//! hashes the message itself), so they are assembled here per RFC 8032 §5.1
//! over `curve25519-dalek`'s audited scalar/point arithmetic — verified
//! against the RFC 8032 §7.2/§7.3 test vectors.

use super::{in_bytes, out_bytes};
use crate::EdSlice;
use curve25519_dalek::constants::ED25519_BASEPOINT_TABLE;
use curve25519_dalek::edwards::CompressedEdwardsY;
use curve25519_dalek::{EdwardsPoint, Scalar};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha512};

// `Result<bool, Ed25519SigError>` wire shape. tag 0 = Ok(`val` as bool);
// tag 1 = Err(`val` as the error discriminant). 2 bytes, mirrors `ResultBool`.
#[repr(C)]
struct ResultBoolSigError {
    tag: u8,
    val: u8,
}

const ERR_SIG_ENCODING: u8 = 0;
const ERR_PUBKEY_ENCODING: u8 = 1;
const ERR_INTERNAL: u8 = 2;

#[inline]
fn ok_bool(v: bool) -> ResultBoolSigError {
    ResultBoolSigError { tag: 0, val: v as u8 }
}

#[inline]
fn sig_err(discriminant: u8) -> ResultBoolSigError {
    ResultBoolSigError { tag: 1, val: discriminant }
}

// RFC 8032 §5.1 dom2 domain-separation string.
const DOM2_PREFIX: &[u8] = b"SigEd25519 no Ed25519 collisions";

/// `dom2(phflag, ctx)` per RFC 8032 §5.1.
fn build_dom2(phflag: u8, ctx: &[u8]) -> Vec<u8> {
    let mut d = Vec::with_capacity(DOM2_PREFIX.len() + 2 + ctx.len());
    d.extend_from_slice(DOM2_PREFIX);
    d.push(phflag);
    d.push(ctx.len() as u8);
    d.extend_from_slice(ctx);
    d
}

/// SHA-512 over the concatenated parts, reduced to a scalar mod L.
fn sha512_scalar(parts: &[&[u8]]) -> Scalar {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    let mut wide = [0u8; 64];
    wide.copy_from_slice(&h.finalize());
    Scalar::from_bytes_mod_order_wide(&wide)
}

/// Expand a 32-byte Ed25519 seed per RFC 8032 §5.1.5.
fn expand_seed(seed: &[u8; 32]) -> (Scalar, [u8; 32], [u8; 32]) {
    let h = Sha512::digest(seed);
    let mut sb = [0u8; 32];
    sb.copy_from_slice(&h[0..32]);
    sb[0] &= 248;
    sb[31] &= 127;
    sb[31] |= 64;
    let s = Scalar::from_bytes_mod_order(sb);
    let mut prefix = [0u8; 32];
    prefix.copy_from_slice(&h[32..64]);
    let a = (ED25519_BASEPOINT_TABLE * &s).compress().to_bytes();
    (s, prefix, a)
}

/// Ed25519 dom2 sign (`phflag` 0 = ctx, 1 = ph; `m` is the message or prehash).
fn dom_sign(seed: &[u8; 32], phflag: u8, ctx: &[u8], m: &[u8], out: &mut [u8]) {
    let (s, prefix, a) = expand_seed(seed);
    let dom2 = build_dom2(phflag, ctx);
    let r = sha512_scalar(&[&dom2, &prefix, m]);
    let r_point = (ED25519_BASEPOINT_TABLE * &r).compress().to_bytes();
    let k = sha512_scalar(&[&dom2, &r_point, &a, m]);
    let sig_s = r + k * s;
    out[0..32].copy_from_slice(&r_point);
    out[32..64].copy_from_slice(&sig_s.to_bytes());
}

/// Ed25519 dom2 verify.
fn dom_verify(pubkey: &[u8; 32], phflag: u8, ctx: &[u8], m: &[u8], sig: &[u8]) -> bool {
    let Some(a) = CompressedEdwardsY(*pubkey).decompress() else {
        return false;
    };
    let mut s_bytes = [0u8; 32];
    s_bytes.copy_from_slice(&sig[32..64]);
    let s_opt: Option<Scalar> = Scalar::from_canonical_bytes(s_bytes).into();
    let Some(s) = s_opt else {
        return false;
    };
    let dom2 = build_dom2(phflag, ctx);
    let k = sha512_scalar(&[&dom2, &sig[0..32], pubkey, m]);
    let r_check = EdwardsPoint::vartime_double_scalar_mul_basepoint(&k, &(-a), &s);
    r_check.compress().to_bytes()[..] == sig[0..32]
}

/// Derive the Ed25519 public key from a 32-byte private seed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed25519_public_from_private(
    private_seed: EdSlice,
    public_out: *mut EdSlice,
) {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (seed, out) = unsafe { (in_bytes(private_seed), out_bytes(public_out)) };
    if seed.len() != 32 || out.len() != 32 {
        return;
    }
    let sk = SigningKey::from_bytes(<&[u8; 32]>::try_from(seed).unwrap());
    out.copy_from_slice(sk.verifying_key().as_bytes());
}

/// Sign `message` with pure Ed25519.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed25519_sign(
    private_seed: EdSlice,
    message: EdSlice,
    sig_out: *mut EdSlice,
) {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (seed, msg, out) = unsafe { (in_bytes(private_seed), in_bytes(message), out_bytes(sig_out)) };
    if seed.len() != 32 || out.len() != 64 {
        return;
    }
    let sk = SigningKey::from_bytes(<&[u8; 32]>::try_from(seed).unwrap());
    out.copy_from_slice(&sk.sign(msg).to_bytes());
}

/// Verify a pure Ed25519 signature.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed25519_verify(
    public_key: EdSlice,
    message: EdSlice,
    signature: EdSlice,
) -> ResultBoolSigError {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (pk, msg, sig) = unsafe { (in_bytes(public_key), in_bytes(message), in_bytes(signature)) };
    if pk.len() != 32 || sig.len() != 64 {
        return sig_err(ERR_SIG_ENCODING);
    }
    let Ok(vk) = VerifyingKey::from_bytes(<&[u8; 32]>::try_from(pk).unwrap()) else {
        return sig_err(ERR_PUBKEY_ENCODING);
    };
    let sig = Signature::from_bytes(<&[u8; 64]>::try_from(sig).unwrap());
    ok_bool(vk.verify_strict(msg, &sig).is_ok())
}

/// Ed25519ctx sign.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed25519_sign_ctx(
    private_seed: EdSlice,
    message: EdSlice,
    context: EdSlice,
    sig_out: *mut EdSlice,
) {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (seed, msg, ctx, out) =
        unsafe { (in_bytes(private_seed), in_bytes(message), in_bytes(context), out_bytes(sig_out)) };
    if seed.len() != 32 || out.len() != 64 || ctx.is_empty() || ctx.len() > 255 {
        return;
    }
    dom_sign(seed.try_into().unwrap(), 0, ctx, msg, out);
}

/// Ed25519ctx verify.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed25519_verify_ctx(
    public_key: EdSlice,
    message: EdSlice,
    context: EdSlice,
    signature: EdSlice,
) -> ResultBoolSigError {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (pk, msg, ctx, sig) =
        unsafe { (in_bytes(public_key), in_bytes(message), in_bytes(context), in_bytes(signature)) };
    if pk.len() != 32 {
        return sig_err(ERR_PUBKEY_ENCODING);
    }
    if sig.len() != 64 {
        return sig_err(ERR_SIG_ENCODING);
    }
    if ctx.is_empty() || ctx.len() > 255 {
        return sig_err(ERR_INTERNAL);
    }
    ok_bool(dom_verify(pk.try_into().unwrap(), 0, ctx, msg, sig))
}

/// Ed25519ph sign.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed25519_sign_ph(
    private_seed: EdSlice,
    prehash: EdSlice,
    context: EdSlice,
    sig_out: *mut EdSlice,
) {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (seed, ph, ctx, out) =
        unsafe { (in_bytes(private_seed), in_bytes(prehash), in_bytes(context), out_bytes(sig_out)) };
    if seed.len() != 32 || ph.len() != 64 || out.len() != 64 || ctx.len() > 255 {
        return;
    }
    dom_sign(seed.try_into().unwrap(), 1, ctx, ph, out);
}

/// Ed25519ph verify.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ed25519_verify_ph(
    public_key: EdSlice,
    prehash: EdSlice,
    context: EdSlice,
    signature: EdSlice,
) -> ResultBoolSigError {
    // SAFETY: args satisfy the crypto-slice-borrow contracts.
    let (pk, ph, ctx, sig) =
        unsafe { (in_bytes(public_key), in_bytes(prehash), in_bytes(context), in_bytes(signature)) };
    if pk.len() != 32 {
        return sig_err(ERR_PUBKEY_ENCODING);
    }
    if sig.len() != 64 || ph.len() != 64 {
        return sig_err(ERR_SIG_ENCODING);
    }
    if ctx.len() > 255 {
        return sig_err(ERR_INTERNAL);
    }
    ok_bool(dom_verify(pk.try_into().unwrap(), 1, ctx, ph, sig))
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 8032 §7.2 Ed25519ctx test vector.
    #[test]
    fn rfc8032_ed25519ctx() {
        let seed = hex("0305334e381af78f141cb666f6199f57bc3495335a256a95bd2a55bf546663f6");
        let pubkey = hex("dfc9425e4f968f7f0c29f0259cf5f9aed6851c2bb4ad8bfb860cfee0ab248292");
        let msg = hex("f726936d19c800494e3fdaff20b276a8");
        let ctx = hex("666f6f");
        let expect = hex(
            "55a4cc2f70a54e04288c5f4cd1e45a7bb520b36292911876cada7323198dd87a8b36950b95130022907a7fb7c4e9b2d5f6cca685a587b4b21f4b888e4e7edb0d",
        );
        let mut sig = [0u8; 64];
        dom_sign(seed.as_slice().try_into().unwrap(), 0, &ctx, &msg, &mut sig);
        assert_eq!(sig.as_slice(), expect.as_slice(), "ctx sign");
        assert!(dom_verify(pubkey.as_slice().try_into().unwrap(), 0, &ctx, &msg, &sig), "ctx verify");
        assert!(!dom_verify(pubkey.as_slice().try_into().unwrap(), 0, b"bar", &msg, &sig), "ctx wrong-context rejects");
    }

    // RFC 8032 §7.3 Ed25519ph test vector.
    #[test]
    fn rfc8032_ed25519ph() {
        let seed = hex("833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42");
        let pubkey = hex("ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf");
        // PH(M) = SHA-512("abc")
        let prehash = {
            use sha2::Digest;
            sha2::Sha512::digest(b"abc").to_vec()
        };
        let expect = hex(
            "98a70222f0b8121aa9d30f813d683f809e462b469c7ff87639499bb94e6dae4131f85042463c2a355a2003d062adf5aaa10b8c61e636062aaad11c2a26083406",
        );
        let mut sig = [0u8; 64];
        dom_sign(seed.as_slice().try_into().unwrap(), 1, &[], &prehash, &mut sig);
        assert_eq!(sig.as_slice(), expect.as_slice(), "ph sign");
        assert!(dom_verify(pubkey.as_slice().try_into().unwrap(), 1, &[], &prehash, &sig), "ph verify");
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
}
