//! GF(p) prime-field arithmetic externs for the NIST P-256 and P-384 base
//! fields (`std.crypto.field.gfp.*`), backing the pure-Edda elliptic-curve
//! point math. Field elements cross the ABI as fixed big-endian byte structs
//! (`Element { bytes: [u8; N] }`, by value); `inv`/`from_bytes_be` return the
//! `Option<Element>` wire shape (`tag` 0 = some, 1 = none — matching the
//! `std.core.option.Option` `some`-first/`none`-second variant order).
//!
//! Backed by `crypto-bigint`'s constant-time `DynResidue` modular arithmetic.

pub mod p256;
pub mod p384;
