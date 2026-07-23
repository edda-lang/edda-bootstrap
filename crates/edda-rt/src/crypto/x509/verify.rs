//! Shared X.509 helpers: algorithm-OID mapping, cross-certificate signature
//! verification (routed through the RustCrypto verifiers), and RFC 6125
//! hostname matching. Used by both the cert and chain externs.

use const_oid::AssociatedOid;
use digest::Digest;
use rsa::RsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier as _;
use sha2::{Sha256, Sha384, Sha512};
use x509_parser::der_parser::Oid;
use x509_parser::prelude::*;

const OID_RSA: &str = "1.2.840.113549.1.1.1";
const OID_EC: &str = "1.2.840.10045.2.1";
const OID_ED25519: &str = "1.3.101.112";
const OID_ED448: &str = "1.3.101.113";
const OID_P256: &str = "1.2.840.10045.3.1.7";
const OID_P384: &str = "1.3.132.0.34";
const OID_P521: &str = "1.3.132.0.35";
const OID_SIG_RSA_SHA256: &str = "1.2.840.113549.1.1.11";
const OID_SIG_RSA_SHA384: &str = "1.2.840.113549.1.1.12";
const OID_SIG_RSA_SHA512: &str = "1.2.840.113549.1.1.13";
const OID_SIG_RSA_PSS: &str = "1.2.840.113549.1.1.10";
const OID_SIG_ECDSA_SHA256: &str = "1.2.840.10045.4.3.2";
const OID_SIG_ECDSA_SHA384: &str = "1.2.840.10045.4.3.3";
const OID_SIG_ECDSA_SHA512: &str = "1.2.840.10045.4.3.4";

// PublicKeyAlgorithm discriminants.
pub(crate) const PK_RSA: u8 = 0;
pub(crate) const PK_ECDSA_P256: u8 = 1;
pub(crate) const PK_ECDSA_P384: u8 = 2;
pub(crate) const PK_ECDSA_P521: u8 = 3;
pub(crate) const PK_ED25519: u8 = 4;
pub(crate) const PK_ED448: u8 = 5;
pub(crate) const PK_UNSUPPORTED: u8 = 6;

// SignatureAlgorithm discriminants.
pub(crate) const SA_RSA_PKCS1_SHA256: u8 = 0;
pub(crate) const SA_RSA_PKCS1_SHA384: u8 = 1;
pub(crate) const SA_RSA_PKCS1_SHA512: u8 = 2;
pub(crate) const SA_RSA_PSS_SHA256: u8 = 3;
pub(crate) const SA_ECDSA_P256_SHA256: u8 = 6;
pub(crate) const SA_ECDSA_P384_SHA384: u8 = 7;
pub(crate) const SA_ECDSA_P521_SHA512: u8 = 8;
pub(crate) const SA_ED25519: u8 = 9;
pub(crate) const SA_ED448: u8 = 10;
pub(crate) const SA_UNSUPPORTED: u8 = 11;

fn oid_str(oid: &Oid) -> String {
    oid.to_id_string()
}

fn ec_curve(spki: &SubjectPublicKeyInfo) -> Option<String> {
    spki.algorithm.parameters.as_ref()?.as_oid().ok().map(|o| oid_str(&o))
}

/// Map a certificate's SPKI to the PublicKeyAlgorithm discriminant.
pub(crate) fn public_key_algorithm(c: &X509Certificate) -> u8 {
    let spki = c.public_key();
    match oid_str(&spki.algorithm.algorithm).as_str() {
        OID_RSA => PK_RSA,
        OID_ED25519 => PK_ED25519,
        OID_ED448 => PK_ED448,
        OID_EC => match ec_curve(spki).as_deref() {
            Some(OID_P256) => PK_ECDSA_P256,
            Some(OID_P384) => PK_ECDSA_P384,
            Some(OID_P521) => PK_ECDSA_P521,
            _ => PK_UNSUPPORTED,
        },
        _ => PK_UNSUPPORTED,
    }
}

/// Map a certificate's signature algorithm to the SignatureAlgorithm discriminant.
pub(crate) fn signature_algorithm(c: &X509Certificate) -> u8 {
    match oid_str(&c.signature_algorithm.algorithm).as_str() {
        OID_SIG_RSA_SHA256 => SA_RSA_PKCS1_SHA256,
        OID_SIG_RSA_SHA384 => SA_RSA_PKCS1_SHA384,
        OID_SIG_RSA_SHA512 => SA_RSA_PKCS1_SHA512,
        OID_SIG_RSA_PSS => SA_RSA_PSS_SHA256,
        OID_SIG_ECDSA_SHA256 => SA_ECDSA_P256_SHA256,
        OID_SIG_ECDSA_SHA384 => SA_ECDSA_P384_SHA384,
        OID_SIG_ECDSA_SHA512 => SA_ECDSA_P521_SHA512,
        OID_ED25519 => SA_ED25519,
        OID_ED448 => SA_ED448,
        _ => SA_UNSUPPORTED,
    }
}

fn rsa_pkcs1_verify<D>(spki_der: &[u8], tbs: &[u8], sig: &[u8]) -> Option<bool>
where
    D: Digest + AssociatedOid,
{
    let pk = RsaPublicKey::from_public_key_der(spki_der).ok()?;
    let vk = rsa::pkcs1v15::VerifyingKey::<D>::new(pk);
    let s = rsa::pkcs1v15::Signature::try_from(sig).ok()?;
    Some(vk.verify(tbs, &s).is_ok())
}

fn ecdsa_p256_verify(point: &[u8], tbs: &[u8], sig: &[u8]) -> Option<bool> {
    let vk = p256::ecdsa::VerifyingKey::from_sec1_bytes(point).ok()?;
    let s = p256::ecdsa::Signature::from_der(sig).ok()?;
    Some(p256::ecdsa::signature::Verifier::verify(&vk, tbs, &s).is_ok())
}

fn ecdsa_p384_verify(point: &[u8], tbs: &[u8], sig: &[u8]) -> Option<bool> {
    let vk = p384::ecdsa::VerifyingKey::from_sec1_bytes(point).ok()?;
    let s = p384::ecdsa::Signature::from_der(sig).ok()?;
    Some(p384::ecdsa::signature::Verifier::verify(&vk, tbs, &s).is_ok())
}

fn ecdsa_p521_verify(point: &[u8], tbs: &[u8], sig: &[u8]) -> Option<bool> {
    let vk = p521::ecdsa::VerifyingKey::from_sec1_bytes(point).ok()?;
    let s = p521::ecdsa::Signature::from_der(sig).ok()?;
    Some(p521::ecdsa::signature::Verifier::verify(&vk, tbs, &s).is_ok())
}

fn ed25519_verify(key: &[u8], tbs: &[u8], sig: &[u8]) -> Option<bool> {
    let vk = ed25519_dalek::VerifyingKey::from_bytes(<&[u8; 32]>::try_from(key).ok()?).ok()?;
    let s = ed25519_dalek::Signature::from_slice(sig).ok()?;
    Some(ed25519_dalek::Verifier::verify(&vk, tbs, &s).is_ok())
}

/// Verify that `cert`'s signature was produced by `issuer`'s key.
pub(crate) fn verify_cert_signature(cert: &X509Certificate, issuer: &X509Certificate) -> Option<bool> {
    let tbs = cert.tbs_certificate.as_ref();
    let sig = cert.signature_value.as_ref();
    let issuer_spki_der = issuer.public_key().raw;
    let issuer_point = issuer.public_key().subject_public_key.data.as_ref();
    match oid_str(&cert.signature_algorithm.algorithm).as_str() {
        OID_SIG_RSA_SHA256 => rsa_pkcs1_verify::<Sha256>(issuer_spki_der, tbs, sig),
        OID_SIG_RSA_SHA384 => rsa_pkcs1_verify::<Sha384>(issuer_spki_der, tbs, sig),
        OID_SIG_RSA_SHA512 => rsa_pkcs1_verify::<Sha512>(issuer_spki_der, tbs, sig),
        OID_SIG_ECDSA_SHA256 => ecdsa_p256_verify(issuer_point, tbs, sig),
        OID_SIG_ECDSA_SHA384 => ecdsa_p384_verify(issuer_point, tbs, sig),
        OID_SIG_ECDSA_SHA512 => ecdsa_p521_verify(issuer_point, tbs, sig),
        OID_ED25519 => ed25519_verify(issuer_point, tbs, sig),
        _ => None,
    }
}

fn dns_label_matches(pattern: &str, host: &str) -> bool {
    if pattern.eq_ignore_ascii_case(host) {
        return true;
    }
    // Single leftmost-label wildcard (RFC 6125 §6.4.3).
    if let Some(suffix) = pattern.strip_prefix("*.")
        && let Some((_, rest)) = host.split_once('.')
    {
        return !suffix.is_empty() && suffix.eq_ignore_ascii_case(rest);
    }
    false
}

/// RFC 6125 hostname verification against the SAN dNSName set.
pub(crate) fn hostname_matches(c: &X509Certificate, hostname: &str) -> bool {
    for ext in c.extensions() {
        if let ParsedExtension::SubjectAlternativeName(san) = ext.parsed_extension() {
            for gn in &san.general_names {
                if let GeneralName::DNSName(dns) = gn
                    && dns_label_matches(dns, hostname)
                {
                    return true;
                }
            }
        }
    }
    false
}
