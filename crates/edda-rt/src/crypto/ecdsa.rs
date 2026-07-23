//! ECDSA signature externs for NIST P-256/P-384/P-521 (`std.crypto.sig.ecdsa`),
//! backed by the `p256`/`p384`/`p521` RustCrypto crates. Each curve signs
//! `hash(message)` with its standard digest (SHA-256/384/512), deterministic
//! per RFC 6979. Keys and signatures cross the ABI as byte slices; every
//! function returns the 2-byte `Result<(),EcdsaSigError>` / `Result<bool,..>`
//! wire shape (single-err sum, by-value).

pub mod p256;
pub mod p384;
pub mod p521;

// `Result<(), EcdsaSigError>` — tag 0 = Ok, 1 = Err(`err` discriminant).
#[repr(C)]
pub(crate) struct ResultUnit {
    pub tag: u8,
    pub err: u8,
}

// `Result<bool, EcdsaSigError>` — tag 0 = Ok(`val`), 1 = Err(`val` discriminant).
#[repr(C)]
pub(crate) struct ResultBool {
    pub tag: u8,
    pub val: u8,
}

// EcdsaSigError / Secp256k1SigError discriminants (shared declaration order).
pub(crate) const ERR_PRIV: u8 = 0;
pub(crate) const ERR_PUB: u8 = 1;
pub(crate) const ERR_NOT_ON_CURVE: u8 = 2;
pub(crate) const ERR_SIG: u8 = 3;
pub(crate) const ERR_DER_BUF: u8 = 4;
pub(crate) const ERR_INTERNAL: u8 = 5;

#[inline]
pub(crate) fn ok_unit() -> ResultUnit {
    ResultUnit { tag: 0, err: 0 }
}

#[inline]
pub(crate) fn err_unit(discriminant: u8) -> ResultUnit {
    ResultUnit { tag: 1, err: discriminant }
}

#[inline]
pub(crate) fn ok_bool(v: bool) -> ResultBool {
    ResultBool { tag: 0, val: v as u8 }
}

#[inline]
pub(crate) fn err_bool(discriminant: u8) -> ResultBool {
    ResultBool { tag: 1, val: discriminant }
}
