//! X.509 trust store + chain validation. `TrustStore` is a leaked
//! `Box<Vec<Vec<u8>>>` of trust-anchor DER. `validate_chain` is a hand-rolled
//! path validator (no `rustls-webpki` — its crypto providers are C-backed):
//! it checks the end-entity validity + hostname, then walks issuer links to a
//! trust anchor verifying each signature, the CA basicConstraints flag, the
//! pathLenConstraint, and a loop guard. Name-constraints, keyUsage, and
//! extendedKeyUsage are NOT yet enforced (those result variants are never
//! returned) — a documented gap pending a fuller validator.

use super::super::in_bytes;
use super::verify;
use super::{ResultByte, ResultHandle, ResultUnit, err_byte, err_unit, ok_byte, ok_handle, ok_unit};
use crate::EdSlice;
use x509_parser::prelude::*;

// ChainError discriminants.
const C_MALFORMED_DER: u8 = 0;
const C_MALFORMED_PEM: u8 = 1;
const C_INTERNAL: u8 = 5;

// ChainValidationResult discriminants.
const CVR_VALID: u8 = 0;
const CVR_EXPIRED: u8 = 1;
const CVR_NOT_YET_VALID: u8 = 2;
const CVR_HOSTNAME_MISMATCH: u8 = 3;
const CVR_UNTRUSTED_ROOT: u8 = 4;
const CVR_BROKEN_SIGNATURE: u8 = 5;
const CVR_BASIC_CONSTRAINTS_VIOLATION: u8 = 6;
const CVR_PATH_LENGTH_EXCEEDED: u8 = 9;
const CVR_UNSUPPORTED_ALGORITHM: u8 = 11;
const CVR_LOOP_DETECTED: u8 = 13;

const MAX_DEPTH: usize = 16;

#[inline]
fn alloc_store(v: Vec<Vec<u8>>) -> u64 {
    Box::into_raw(Box::new(v)) as u64
}

/// Create an empty trust store.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_trust_store_new(_allocator: *const ()) -> ResultHandle {
    ok_handle(alloc_store(Vec::new()))
}

/// Add a DER trust anchor to the store.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_trust_store_add_der(store: *mut u64, cert_der: EdSlice) -> ResultUnit {
    if store.is_null() {
        return err_unit(C_INTERNAL);
    }
    // SAFETY: `store` is the caller's `mutable TrustStore` handle slot.
    let handle = unsafe { *store };
    let Some(s) = (unsafe { (handle as *mut Vec<Vec<u8>>).as_mut() }) else {
        return err_unit(C_INTERNAL);
    };
    // SAFETY: `cert_der` satisfies the crypto-slice-borrow contract.
    let bytes = unsafe { in_bytes(cert_der) };
    if X509Certificate::from_der(bytes).is_err() {
        return err_unit(C_MALFORMED_DER);
    }
    s.push(bytes.to_vec());
    ok_unit()
}

/// Add PEM trust anchors to the store.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_trust_store_add_pem(store: *mut u64, pem_text: EdSlice) -> ResultUnit {
    if store.is_null() {
        return err_unit(C_INTERNAL);
    }
    // SAFETY: `store` is the caller's `mutable TrustStore` handle slot.
    let handle = unsafe { *store };
    let Some(s) = (unsafe { (handle as *mut Vec<Vec<u8>>).as_mut() }) else {
        return err_unit(C_INTERNAL);
    };
    // SAFETY: `pem_text` satisfies the crypto-slice-borrow contract.
    let bytes = unsafe { in_bytes(pem_text) };
    let mut added = false;
    for pem in Pem::iter_from_buffer(bytes) {
        let Ok(pem) = pem else {
            return err_unit(C_MALFORMED_PEM);
        };
        if pem.label == "CERTIFICATE" {
            if X509Certificate::from_der(&pem.contents).is_err() {
                return err_unit(C_MALFORMED_DER);
            }
            s.push(pem.contents.clone());
            added = true;
        }
    }
    if !added {
        return err_unit(C_MALFORMED_PEM);
    }
    ok_unit()
}

/// Free a trust store.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_trust_store_close(store: u64) {
    if store != 0 {
        // SAFETY: `store` was produced by `alloc_store` and is consumed (`take`) here.
        drop(unsafe { Box::from_raw(store as *mut Vec<Vec<u8>>) });
    }
}

/// Validate an end-entity certificate chain against a trust store.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_validate_chain(
    end_entity_der: EdSlice,
    chain_intermediates_der: EdSlice,
    trust_store: u64,
    now_unix_seconds: i64,
    hostname: EdSlice,
) -> ResultByte {
    // SAFETY: slices satisfy the crypto-slice-borrow contracts.
    let (ee, inters_raw, host_b) =
        unsafe { (in_bytes(end_entity_der), in_bytes(chain_intermediates_der), in_bytes(hostname)) };
    let Some(store) = (unsafe { (trust_store as *const Vec<Vec<u8>>).as_ref() }) else {
        return err_byte(C_INTERNAL);
    };
    let Ok((_, ee_cert)) = X509Certificate::from_der(ee) else {
        return err_byte(C_MALFORMED_DER);
    };
    let v = ee_cert.validity();
    if now_unix_seconds < v.not_before.timestamp() {
        return ok_byte(CVR_NOT_YET_VALID);
    }
    if now_unix_seconds > v.not_after.timestamp() {
        return ok_byte(CVR_EXPIRED);
    }
    let Ok(host) = core::str::from_utf8(host_b) else {
        return err_byte(C_INTERNAL);
    };
    if !verify::hostname_matches(&ee_cert, host) {
        return ok_byte(CVR_HOSTNAME_MISMATCH);
    }

    // Parse intermediates (concatenated DER) and trust anchors.
    let mut intermediates = Vec::new();
    let mut rest = inters_raw;
    while !rest.is_empty() {
        match X509Certificate::from_der(rest) {
            Ok((next, c)) => {
                intermediates.push(c);
                rest = next;
            }
            Err(_) => break,
        }
    }
    let anchors: Vec<_> =
        store.iter().filter_map(|d| X509Certificate::from_der(d).ok().map(|(_, c)| c)).collect();

    // Walk issuer links toward a trust anchor.
    let mut current = &ee_cert;
    let mut seen: Vec<&[u8]> = vec![current.subject().as_raw()];
    for depth in 0..MAX_DEPTH {
        let issuer_dn = current.issuer().as_raw();

        // Reached a trust anchor?
        for anchor in &anchors {
            if anchor.subject().as_raw() == issuer_dn {
                match verify::verify_cert_signature(current, anchor) {
                    Some(true) => return ok_byte(CVR_VALID),
                    Some(false) => {}
                    None => return ok_byte(CVR_UNSUPPORTED_ALGORITHM),
                }
            }
        }

        // Find a signing intermediate.
        let mut next = None;
        for inter in &intermediates {
            if inter.subject().as_raw() == issuer_dn {
                match verify::verify_cert_signature(current, inter) {
                    Some(true) => {
                        next = Some(inter);
                        break;
                    }
                    Some(false) => return ok_byte(CVR_BROKEN_SIGNATURE),
                    None => return ok_byte(CVR_UNSUPPORTED_ALGORITHM),
                }
            }
        }
        let Some(issuer) = next else {
            return ok_byte(CVR_UNTRUSTED_ROOT);
        };

        let bc = issuer.basic_constraints().ok().flatten();
        let (is_ca, plen) = match &bc {
            Some(b) => (b.value.ca, b.value.path_len_constraint),
            None => (false, None),
        };
        if !is_ca {
            return ok_byte(CVR_BASIC_CONSTRAINTS_VIOLATION);
        }
        if let Some(p) = plen
            && depth as u64 > p as u64
        {
            return ok_byte(CVR_PATH_LENGTH_EXCEEDED);
        }
        let isub = issuer.subject().as_raw();
        if seen.contains(&isub) {
            return ok_byte(CVR_LOOP_DETECTED);
        }
        seen.push(isub);
        current = issuer;
    }
    ok_byte(CVR_UNTRUSTED_ROOT)
}
