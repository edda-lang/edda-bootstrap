//! X.509 certificate accessors + per-certificate validation. `Certificate` is
//! a leaked `Box<Vec<u8>>` of owned DER, re-parsed by `x509-parser` per call.

use super::super::in_bytes;
use super::verify;
use super::{
    ResultByte, ResultHandle, ResultUnit, err_byte, err_handle, err_unit, ok_byte, ok_handle,
    ok_unit, write_out,
};
use crate::EdSlice;
use x509_parser::prelude::*;

// X509Error discriminants.
const MALFORMED_DER: u8 = 0;
const UNSUPPORTED_ALGORITHM: u8 = 2;
const BUFFER_TOO_SMALL: u8 = 10;
const INDEX_OUT_OF_RANGE: u8 = 11;
const INTERNAL: u8 = 12;

#[inline]
fn alloc_cert(der: Vec<u8>) -> u64 {
    Box::into_raw(Box::new(der)) as u64
}

unsafe fn cert_bytes<'a>(h: u64) -> Option<&'a [u8]> {
    if h == 0 {
        return None;
    }
    // SAFETY: a non-null handle was produced by `alloc_cert`.
    unsafe { (h as *const Vec<u8>).as_ref() }.map(|v| v.as_slice())
}

fn with_cert<R>(h: u64, on_err: R, f: impl FnOnce(&X509Certificate) -> R) -> R {
    // SAFETY: handle-deref contract upheld by the linear/affine type checker.
    let Some(der) = (unsafe { cert_bytes(h) }) else {
        return on_err;
    };
    match X509Certificate::from_der(der) {
        Ok((_, c)) => f(&c),
        Err(_) => on_err,
    }
}

/// Parse a DER X.509 certificate into a handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_parse_der(_allocator: *const (), der: EdSlice) -> ResultHandle {
    // SAFETY: `der` satisfies the crypto-slice-borrow contract.
    let bytes = unsafe { in_bytes(der) };
    match X509Certificate::from_der(bytes) {
        Ok(_) => ok_handle(alloc_cert(bytes.to_vec())),
        Err(_) => err_handle(MALFORMED_DER),
    }
}

/// Free a certificate handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_close(cert: u64) {
    if cert != 0 {
        // SAFETY: `cert` was produced by `alloc_cert` and is consumed (`take`) here.
        drop(unsafe { Box::from_raw(cert as *mut Vec<u8>) });
    }
}

/// Write the first matching attribute string of an X509Name into the buffer.
unsafe fn write_cn(name: &X509Name, out: *mut EdSlice, len: *mut u64) -> ResultUnit {
    let cn = name.iter_common_name().next().and_then(|a| a.as_str().ok()).unwrap_or("");
    // SAFETY: out/len satisfy the crypto-slice-borrow-mut contract.
    if unsafe { write_out(out, len, cn.as_bytes()) } { ok_unit() } else { err_unit(BUFFER_TOO_SMALL) }
}

/// Subject common name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_subject_cn(cert: u64, out: *mut EdSlice, out_len_out: *mut u64) -> ResultUnit {
    with_cert(cert, err_unit(INTERNAL), |c| unsafe { write_cn(c.subject(), out, out_len_out) })
}

/// Issuer common name.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_issuer_cn(cert: u64, out: *mut EdSlice, out_len_out: *mut u64) -> ResultUnit {
    with_cert(cert, err_unit(INTERNAL), |c| unsafe { write_cn(c.issuer(), out, out_len_out) })
}

unsafe fn write_bytes_result(bytes: &[u8], out: *mut EdSlice, len: *mut u64) -> ResultUnit {
    // SAFETY: out/len satisfy the crypto-slice-borrow-mut contract.
    if unsafe { write_out(out, len, bytes) } { ok_unit() } else { err_unit(BUFFER_TOO_SMALL) }
}

/// Subject distinguished name (raw DER).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_subject_dn_der(cert: u64, out: *mut EdSlice, out_len_out: *mut u64) -> ResultUnit {
    with_cert(cert, err_unit(INTERNAL), |c| unsafe { write_bytes_result(c.subject().as_raw(), out, out_len_out) })
}

/// Issuer distinguished name (raw DER).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_issuer_dn_der(cert: u64, out: *mut EdSlice, out_len_out: *mut u64) -> ResultUnit {
    with_cert(cert, err_unit(INTERNAL), |c| unsafe { write_bytes_result(c.issuer().as_raw(), out, out_len_out) })
}

/// Collect the SAN dNSName / IPAddress entries.
fn san_dns<'a>(c: &'a X509Certificate) -> Vec<&'a str> {
    let mut v = Vec::new();
    for ext in c.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
            for gn in &san.general_names {
                if let GeneralName::DNSName(d) = gn {
                    v.push(*d);
                }
            }
        }
    }
    v
}

fn san_ip<'a>(c: &'a X509Certificate) -> Vec<&'a [u8]> {
    let mut v = Vec::new();
    for ext in c.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
            for gn in &san.general_names {
                if let GeneralName::IPAddress(ip) = gn {
                    v.push(*ip);
                }
            }
        }
    }
    v
}

/// Total SAN general-name count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_san_count(cert: u64) -> u64 {
    with_cert(cert, 0, |c| {
        let mut n = 0u64;
        for ext in c.extensions() {
            if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
                n += san.general_names.len() as u64;
            }
        }
        n
    })
}

/// SAN dNSName count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_san_dns_count(cert: u64) -> u64 {
    with_cert(cert, 0, |c| san_dns(c).len() as u64)
}

/// The n-th SAN dNSName.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_san_dns_at(cert: u64, n: u64, out: *mut EdSlice, out_len_out: *mut u64) -> ResultUnit {
    with_cert(cert, err_unit(INTERNAL), |c| match san_dns(c).get(n as usize) {
        Some(d) => unsafe { write_bytes_result(d.as_bytes(), out, out_len_out) },
        None => err_unit(INDEX_OUT_OF_RANGE),
    })
}

/// SAN iPAddress count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_san_ip_count(cert: u64) -> u64 {
    with_cert(cert, 0, |c| san_ip(c).len() as u64)
}

/// The n-th SAN iPAddress (raw 4- or 16-byte address).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_san_ip_at(cert: u64, n: u64, out: *mut EdSlice, out_len_out: *mut u64) -> ResultUnit {
    with_cert(cert, err_unit(INTERNAL), |c| match san_ip(c).get(n as usize) {
        Some(ip) => unsafe { write_bytes_result(ip, out, out_len_out) },
        None => err_unit(INDEX_OUT_OF_RANGE),
    })
}

/// notBefore as a Unix timestamp (seconds).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_not_before_unix(cert: u64) -> i64 {
    with_cert(cert, 0, |c| c.validity().not_before.timestamp())
}

/// notAfter as a Unix timestamp (seconds).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_not_after_unix(cert: u64) -> i64 {
    with_cert(cert, 0, |c| c.validity().not_after.timestamp())
}

/// Serial number (raw big-endian bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_serial(cert: u64, out: *mut EdSlice, out_len_out: *mut u64) -> ResultUnit {
    with_cert(cert, err_unit(INTERNAL), |c| unsafe { write_bytes_result(c.raw_serial(), out, out_len_out) })
}

/// Public-key algorithm discriminant.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_public_key_algorithm(cert: u64) -> u8 {
    with_cert(cert, verify::PK_UNSUPPORTED, |c| verify::public_key_algorithm(c))
}

/// SubjectPublicKeyInfo (raw DER).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_spki_der(cert: u64, out: *mut EdSlice, out_len_out: *mut u64) -> ResultUnit {
    with_cert(cert, err_unit(INTERNAL), |c| unsafe { write_bytes_result(c.public_key().raw, out, out_len_out) })
}

/// Signature algorithm discriminant.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_signature_algorithm(cert: u64) -> u8 {
    with_cert(cert, verify::SA_UNSUPPORTED, |c| verify::signature_algorithm(c))
}

/// BasicConstraints cA flag.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_bc_is_ca(cert: u64) -> u8 {
    with_cert(cert, 0, |c| {
        c.basic_constraints().ok().flatten().map(|bc| bc.value.ca as u8).unwrap_or(0)
    })
}

/// BasicConstraints pathLenConstraint (-1 if absent).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_bc_path_length(cert: u64) -> i32 {
    with_cert(cert, -1, |c| {
        c.basic_constraints()
            .ok()
            .flatten()
            .and_then(|bc| bc.value.path_len_constraint)
            .and_then(|p| i32::try_from(p).ok())
            .unwrap_or(-1)
    })
}

/// RFC 6125 hostname verification against the SAN dNSName set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_verify_hostname(cert: u64, hostname: EdSlice) -> ResultByte {
    // SAFETY: `hostname` (a `String` = EdStr, layout-identical to EdSlice) satisfies the borrow contract.
    let h = unsafe { in_bytes(hostname) };
    let Ok(host) = core::str::from_utf8(h) else {
        return err_byte(INTERNAL);
    };
    with_cert(cert, err_byte(INTERNAL), |c| ok_byte(verify::hostname_matches(c, host) as u8))
}

/// True iff `now_unix_seconds` is within [notBefore, notAfter].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_verify_validity(cert: u64, now_unix_seconds: i64) -> u8 {
    with_cert(cert, 0, |c| {
        let v = c.validity();
        (now_unix_seconds >= v.not_before.timestamp() && now_unix_seconds <= v.not_after.timestamp()) as u8
    })
}

/// True iff `cert`'s signature verifies under `issuer`'s key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_verify_signed_by(cert: u64, issuer: u64) -> ResultByte {
    // SAFETY: handle-deref contract upheld by the affine type checker.
    let (Some(cd), Some(id)) = (unsafe { (cert_bytes(cert), cert_bytes(issuer)) }) else {
        return err_byte(INTERNAL);
    };
    let (Ok((_, c)), Ok((_, i))) =
        (X509Certificate::from_der(cd), X509Certificate::from_der(id))
    else {
        return err_byte(MALFORMED_DER);
    };
    match verify::verify_cert_signature(&c, &i) {
        Some(b) => ok_byte(b as u8),
        None => err_byte(UNSUPPORTED_ALGORITHM),
    }
}

/// True iff the certificate is self-issued and self-signed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_x509_is_self_signed(cert: u64) -> ResultByte {
    with_cert(cert, err_byte(INTERNAL), |c| {
        if c.subject().as_raw() != c.issuer().as_raw() {
            return ok_byte(0);
        }
        match verify::verify_cert_signature(c, c) {
            Some(b) => ok_byte(b as u8),
            None => err_byte(UNSUPPORTED_ALGORITHM),
        }
    })
}
