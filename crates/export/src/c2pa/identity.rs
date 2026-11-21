//! The signing identity a provenance manifest is signed with: a key the app
//! generated with its own local certificate (verifiers show an unknown
//! signer), or a certificate the user imported from a PKCS#12 file.
//!
//! Stored as one record in a folder the app gives (its data folder):
//! `signing-identity.json`, readable by the user's account only, with which
//! kind it is, the private key (PKCS#8) and the certificate chain, replaced
//! as a whole. The key never goes into recipes, the catalog, logs or
//! reports; only [`remove`] deletes it, on the user's request.

use std::path::{Path, PathBuf};

use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{self, EcdsaKeyPair, Ed25519KeyPair, KeyPair, RsaKeyPair};
use serde::{Deserialize, Serialize};
use x509_parser::prelude::{FromDer, X509Certificate};

use super::der;

const IDENTITY_FILE: &str = "signing-identity.json";

/// EKUs C2PA accepts for a claim signer (C2PA 2.x, "Certificate profile").
const EMAIL_PROTECTION: &str = "1.3.6.1.5.5.7.3.4";
const DOCUMENT_SIGNING: &str = "1.3.6.1.5.5.7.3.36";
const C2PA_CLAIM_SIGNING: &str = "1.3.6.1.4.1.62558.2.1";

/// Where an identity came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdentityKind {
    /// Made on this computer: no certificate authority vouches for it.
    Generated,
    /// The user's own certificate (PKCS#12).
    Imported,
}

/// What the Export dialog shows about the identity (no key material).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityInfo {
    pub kind: IdentityKind,
    /// The signing certificate's common name (or whole subject).
    pub signer: String,
    pub issuer: String,
    /// The certificate's end of validity (RFC 3339).
    pub valid_until: String,
    /// The COSE algorithm the manifests are signed with (`ES256`, …).
    pub algorithm: String,
    /// SHA-256 of the signing certificate (hex), to tell identities apart.
    pub fingerprint: String,
    /// The signing certificate's whole subject (`CN=…, O=…, …`): every
    /// signed file carries it, readable by its recipients.
    pub subject: String,
    /// The certificate is not valid now: exports cannot be signed with it.
    pub expired: bool,
}

/// The stored record (never shown or logged).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    kind: IdentityKind,
    /// The private key, PKCS#8 DER in base64.
    key: String,
    /// The certificates, the signer's first, in PEM.
    chain: String,
}

enum Key {
    Ecdsa(EcdsaKeyPair, Algorithm),
    Rsa(RsaKeyPair),
    Ed25519(Ed25519KeyPair),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Algorithm {
    Es256,
    Es384,
    Ps256,
    EdDsa,
}

impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Algorithm::Es256 => "ES256",
            Algorithm::Es384 => "ES384",
            Algorithm::Ps256 => "PS256",
            Algorithm::EdDsa => "EdDSA",
        }
    }

    /// The COSE algorithm identifier (RFC 9053).
    fn cose(self) -> i64 {
        match self {
            Algorithm::Es256 => -7,
            Algorithm::Es384 => -35,
            Algorithm::Ps256 => -37,
            Algorithm::EdDsa => -8,
        }
    }
}

/// A loaded identity: signs claims.
pub struct SigningIdentity {
    key: Key,
    /// DER certificates, the signer's first.
    chain: Vec<Vec<u8>>,
    kind: IdentityKind,
    rng: SystemRandom,
}

impl std::fmt::Debug for SigningIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key.
        f.debug_struct("SigningIdentity")
            .field("kind", &self.kind)
            .field("algorithm", &self.algorithm().name())
            .finish_non_exhaustive()
    }
}

impl SigningIdentity {
    fn algorithm(&self) -> Algorithm {
        match &self.key {
            Key::Ecdsa(_, alg) => *alg,
            Key::Rsa(_) => Algorithm::Ps256,
            Key::Ed25519(_) => Algorithm::EdDsa,
        }
    }

    pub fn cose_algorithm(&self) -> i64 {
        self.algorithm().cose()
    }

    /// The length of every signature this identity makes.
    pub fn signature_len(&self) -> usize {
        match &self.key {
            Key::Ecdsa(_, Algorithm::Es384) => 96,
            Key::Ecdsa(..) | Key::Ed25519(_) => 64,
            Key::Rsa(rsa) => rsa.public().modulus_len(),
        }
    }

    /// The COSE signature of `message` (ECDSA as r‖s).
    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        let failed = |_| "The manifest could not be signed.".to_string();
        match &self.key {
            Key::Ecdsa(key, _) => key
                .sign(&self.rng, message)
                .map(|s| s.as_ref().to_vec())
                .map_err(failed),
            Key::Ed25519(key) => Ok(key.sign(message).as_ref().to_vec()),
            Key::Rsa(key) => {
                let mut out = vec![0; key.public().modulus_len()];
                key.sign(&signature::RSA_PSS_SHA256, &self.rng, message, &mut out)
                    .map_err(failed)?;
                Ok(out)
            }
        }
    }

    pub fn chain(&self) -> &[Vec<u8>] {
        &self.chain
    }

    pub fn kind(&self) -> IdentityKind {
        self.kind
    }

    pub fn info(&self) -> Result<IdentityInfo, String> {
        let (_, cert) = X509Certificate::from_der(&self.chain[0])
            .map_err(|e| format!("The signing certificate cannot be read: {e}"))?;
        let name = |n: &x509_parser::x509::X509Name<'_>| {
            n.iter_common_name()
                .next()
                .and_then(|cn| cn.as_str().ok())
                .map(str::to_string)
                .unwrap_or_else(|| n.to_string())
        };
        let until = jiff::Timestamp::from_second(cert.validity().not_after.timestamp())
            .map(|t| t.to_string())
            .unwrap_or_default();
        Ok(IdentityInfo {
            kind: self.kind,
            signer: shown(&name(cert.subject())),
            issuer: shown(&name(cert.issuer())),
            valid_until: until,
            algorithm: self.algorithm().name().to_string(),
            fingerprint: self.fingerprint(),
            subject: shown(&cert.subject().to_string()),
            expired: !cert.validity().is_valid(),
        })
    }

    /// SHA-256 of the signing certificate (hex).
    pub fn fingerprint(&self) -> String {
        rawtonic_fs::sha256_hex(&self.chain[0])
    }

    /// Whether manifests can be signed now: the signing certificate is
    /// valid at this moment (verifiers reject a signature made outside it).
    pub fn check_usable(&self) -> Result<(), String> {
        let (_, cert) = X509Certificate::from_der(&self.chain[0])
            .map_err(|e| format!("The signing certificate cannot be read: {e}"))?;
        if cert.validity().is_valid() {
            return Ok(());
        }
        let at = |t: x509_parser::time::ASN1Time| {
            jiff::Timestamp::from_second(t.timestamp())
                .map(|t| t.to_string())
                .unwrap_or_default()
        };
        Err(format!(
            "The signing certificate is valid from {} to {}, not now: import a renewed certificate or make a new one in the Export panel's Provenance section.",
            at(cert.validity().not_before),
            at(cert.validity().not_after)
        ))
    }
}

/// A certificate's name as the Export panel shows it: control and
/// bidirectional-override characters (which could make one name look like
/// another) become U+FFFD.
fn shown(text: &str) -> String {
    text.chars()
        .map(|c| {
            let bidi = matches!(c, '\u{200e}' | '\u{200f}' | '\u{061c}')
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c);
            if c.is_control() || bidi {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}

/// A private key from PKCS#8 DER, of any type C2PA and ring sign with.
fn key_from_pkcs8(pkcs8: &[u8], rng: &SystemRandom) -> Result<Key, String> {
    if let Ok(key) =
        EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, rng)
    {
        return Ok(Key::Ecdsa(key, Algorithm::Es256));
    }
    if let Ok(key) =
        EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P384_SHA384_FIXED_SIGNING, pkcs8, rng)
    {
        return Ok(Key::Ecdsa(key, Algorithm::Es384));
    }
    if let Ok(key) = RsaKeyPair::from_pkcs8(pkcs8) {
        return Ok(Key::Rsa(key));
    }
    if let Ok(key) = Ed25519KeyPair::from_pkcs8_maybe_unchecked(pkcs8) {
        return Ok(Key::Ed25519(key));
    }
    Err(
        "The private key is not one C2PA signs with (ECDSA P-256 or P-384, RSA of 2048 bits or more, Ed25519)."
            .to_string(),
    )
}

fn public_key(key: &Key) -> &[u8] {
    match key {
        Key::Ecdsa(k, _) => k.public_key().as_ref(),
        Key::Rsa(k) => k.public_key().as_ref(),
        Key::Ed25519(k) => k.public_key().as_ref(),
    }
}

/// Checks that `chain[0]` is a certificate C2PA verifiers accept for
/// signing manifests and that it belongs to `key`.
fn check_signing_certificate(chain: &[Vec<u8>], key: &Key) -> Result<(), String> {
    let first = chain.first().ok_or("The file holds no certificate.")?;
    let (_, cert) = X509Certificate::from_der(first)
        .map_err(|e| format!("The certificate cannot be read: {e}"))?;
    let spki = &cert.public_key().subject_public_key.data;
    // ring gives RSA keys as the PKCS#1 RSAPublicKey, the others raw: both
    // are what the certificate's subjectPublicKey bit string holds.
    if spki.as_ref() != public_key(key) {
        return Err("The certificate does not belong to the private key.".to_string());
    }
    check_public_key_algorithm(&cert, key)?;
    // Verifiers take the chain as given: each certificate issued by the
    // next one.
    for pair in chain.windows(2) {
        let (Ok((_, cert)), Ok((_, issuer))) = (
            X509Certificate::from_der(&pair[0]),
            X509Certificate::from_der(&pair[1]),
        ) else {
            return Err("A certificate of the chain cannot be read.".to_string());
        };
        if cert.issuer().as_raw() != issuer.subject().as_raw() {
            return Err(
                "The certificate chain is not in order (each certificate followed by its issuer's)."
                    .to_string(),
            );
        }
    }
    // Verifiers name the signer by the subject's organisation; c2pa-rs
    // (c2patool 0.27) rejects a signature whose certificate has none.
    if cert.subject().iter_organization().next().is_none() {
        return Err(
            "The certificate names no organisation (O); C2PA verifiers reject signatures made with it."
                .to_string(),
        );
    }
    if cert.is_ca() {
        return Err(
            "This is a certificate authority's certificate; C2PA needs a signing (end-entity) certificate."
                .to_string(),
        );
    }
    if !cert.validity().is_valid() {
        return Err("The certificate is not valid now (expired or not yet valid).".to_string());
    }
    let digital_signature = cert
        .key_usage()
        .ok()
        .flatten()
        .is_some_and(|ku| ku.value.digital_signature());
    if !digital_signature {
        return Err(
            "The certificate is not allowed to make digital signatures (key usage).".to_string(),
        );
    }
    let eku_ok = cert.extended_key_usage().ok().flatten().is_some_and(|eku| {
        let eku = eku.value;
        !eku.any
            && (eku.email_protection
                || eku.other.iter().any(|oid| {
                    let oid = oid.to_id_string();
                    oid == DOCUMENT_SIGNING || oid == C2PA_CLAIM_SIGNING
                }))
    });
    if !eku_ok {
        return Err(
            "The certificate's extended key usage does not allow C2PA signing (it needs email protection, document signing or C2PA claim signing)."
                .to_string(),
        );
    }
    check_certificate_profile(&cert)
}

/// Signature algorithms C2PA accepts for certificates (C2PA 2.x,
/// "Certificate profile"): RSA PKCS#1 v1.5 and PSS with SHA-2, ECDSA with
/// SHA-2, Ed25519.
const CERTIFICATE_ALGORITHMS: [&str; 8] = [
    "1.2.840.113549.1.1.11",
    "1.2.840.113549.1.1.12",
    "1.2.840.113549.1.1.13",
    "1.2.840.113549.1.1.10",
    "1.2.840.10045.4.3.2",
    "1.2.840.10045.4.3.3",
    "1.2.840.10045.4.3.4",
    "1.3.101.112",
];

const RSASSA_PSS: &str = "1.2.840.113549.1.1.10";

const SHA256: &str = "2.16.840.1.101.3.4.2.1";
const SHA2: [&str; 3] = [SHA256, "2.16.840.1.101.3.4.2.2", "2.16.840.1.101.3.4.2.3"];
const MGF1: &str = "1.2.840.113549.1.1.8";
const RSA_ENCRYPTION: &str = "1.2.840.113549.1.1.1";
const EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";
const ED25519: &str = "1.3.101.112";

/// One DER element of `input`: its tag, its contents and what follows.
/// Only single-byte tags and definite lengths (DER).
fn der_element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let len = rest[..count]
            .iter()
            .fold(0usize, |len, b| (len << 8) | *b as usize);
        (len, &rest[count..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// The dotted OID and the parameters' encoding of `identifier`, which must
/// be exactly one AlgorithmIdentifier SEQUENCE.
fn algorithm_oid(identifier: &[u8]) -> Option<(String, &[u8])> {
    let (0x30, content, []) = der_element(identifier)? else {
        return None;
    };
    let (0x06, oid, params) = der_element(content)? else {
        return None;
    };
    let oid = x509_parser::asn1_rs::Oid::new(oid.into()).to_id_string();
    Some((oid, params))
}

/// Whether `value` is the universal type `tag`, constructed (a SEQUENCE)
/// or primitive (NULL) as `constructed` says.
fn universal(
    value: &x509_parser::asn1_rs::Any<'_>,
    tag: x509_parser::asn1_rs::Tag,
    constructed: bool,
) -> bool {
    value.header.class() == x509_parser::asn1_rs::Class::Universal
        && value.header.is_constructed() == constructed
        && value.tag() == tag
}

/// What RSASSA-PSS parameters say.
struct Pss {
    /// The hash (dotted OID).
    hash: String,
    /// The salt length in bytes (RFC 4055: 20 when absent; in a public
    /// key's parameters the least it may be signed with).
    salt: u64,
}

/// RSASSA-PSS parameters, when they are well formed as C2PA requires: a
/// SEQUENCE of `[0]` a SHA-2 hash (absent or NULL parameters), `[1]` MGF1
/// with that same hash, optionally `[2]` a salt length and `[3]` the trailer
/// field 1, in this order and nothing else (the defaults, SHA-1, are not
/// allowed).
fn pss_parameters(params: &x509_parser::asn1_rs::Any<'_>) -> Option<Pss> {
    if !universal(params, x509_parser::asn1_rs::Tag::Sequence, true) {
        return None;
    }
    let hash_params_ok = |params: &[u8]| params.is_empty() || params == [0x05, 0x00];
    let (0xa0, hash, rest) = der_element(params.data)? else {
        return None;
    };
    let (hash, hash_params) = algorithm_oid(hash)?;
    let (0xa1, mgf, mut rest) = der_element(rest)? else {
        return None;
    };
    let (mgf, mgf_params) = algorithm_oid(mgf)?;
    let (mgf_hash, mgf_hash_params) = algorithm_oid(mgf_params)?;
    if !SHA2.contains(&hash.as_str())
        || mgf != MGF1
        || mgf_hash != hash
        || !hash_params_ok(hash_params)
        || !hash_params_ok(mgf_hash_params)
    {
        return None;
    }
    let mut salt = 20;
    if let Some((0xa2, field, after)) = der_element(rest) {
        let (0x02, value, []) = der_element(field)? else {
            return None;
        };
        // A non-negative salt length (its sign bit clear; a leading zero
        // byte keeps it clear for 128 and more) that fits 64 bits.
        if value.first().is_none_or(|first| first & 0x80 != 0) {
            return None;
        }
        let value = match value {
            [0, rest @ ..] if !rest.is_empty() => rest,
            value => value,
        };
        if value.len() > 8 {
            return None;
        }
        salt = value.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b));
        rest = after;
    }
    if let Some((0xa3, trailer, after)) = der_element(rest) {
        let (0x02, [1], []) = der_element(trailer)? else {
            return None;
        };
        rest = after;
    }
    rest.is_empty().then_some(Pss { hash, salt })
}

/// Whether an RSA public key named `oid` with `params` can verify the
/// manifests' PS256 signatures (SHA-256, MGF1-SHA-256, a 32-byte salt):
/// `rsaEncryption` (parameters absent or NULL), or RSASSA-PSS without
/// parameters or restricted to SHA-256 and a salt of at most 32 bytes.
fn rsa_key_named_ok(oid: &str, params: Option<&x509_parser::asn1_rs::Any<'_>>) -> bool {
    match oid {
        RSA_ENCRYPTION => params.is_none_or(|p| {
            universal(p, x509_parser::asn1_rs::Tag::Null, false) && p.data.is_empty()
        }),
        RSASSA_PSS => params.is_none_or(|p| {
            pss_parameters(p).is_some_and(|pss| pss.hash == SHA256 && pss.salt <= 32)
        }),
        _ => false,
    }
}

/// The certificate's public key is named as C2PA verifiers accept it and as
/// `key` signs (c2pa-rs `check_certificate_profile`): an EC key on its own
/// curve, Ed25519 without parameters, RSA as [`rsa_key_named_ok`] says.
fn check_public_key_algorithm(cert: &X509Certificate<'_>, key: &Key) -> Result<(), String> {
    let algorithm = &cert.public_key().algorithm;
    let oid = algorithm.algorithm.to_id_string();
    let params = algorithm.parameters.as_ref();
    let ok = match key {
        Key::Ecdsa(_, alg) => {
            let curve = match alg {
                Algorithm::Es384 => "1.3.132.0.34",
                _ => "1.2.840.10045.3.1.7",
            };
            oid == EC_PUBLIC_KEY
                && params
                    .and_then(|p| p.as_oid().ok())
                    .is_some_and(|named| named.to_id_string() == curve)
        }
        Key::Ed25519(_) => oid == ED25519 && params.is_none(),
        Key::Rsa(_) => rsa_key_named_ok(&oid, params),
    };
    if ok {
        Ok(())
    } else {
        Err("The certificate names its public key in a way C2PA verifiers reject (algorithm or curve parameters).".to_string())
    }
}

/// The rest of the C2PA certificate profile a signing certificate must
/// follow, as the reference validator checks it (c2pa-rs
/// `check_certificate_profile`): X.509 v3, an accepted signature algorithm
/// named alike inside and outside the signed part, no unique identifiers,
/// an Authority Key Identifier, no certificate-signing key usage, no
/// time-stamping or OCSP signing use (c2pa-rs refuses either beside another
/// purpose, and a claim signer needs one), and no critical extension it does not
/// know.
fn check_certificate_profile(cert: &X509Certificate<'_>) -> Result<(), String> {
    use x509_parser::extensions::ParsedExtension;
    let refuse = |what: &str| {
        Err(format!(
            "The certificate does not follow the C2PA certificate profile ({what}); C2PA verifiers reject signatures made with it."
        ))
    };
    if cert.version() != x509_parser::x509::X509Version::V3 {
        return refuse("not an X.509 version 3 certificate");
    }
    let algorithm = cert.signature_algorithm.algorithm.to_id_string();
    // The whole identifiers, parameters included.
    if cert.tbs_certificate.signature != cert.signature_algorithm
        || !CERTIFICATE_ALGORITHMS.contains(&algorithm.as_str())
    {
        return refuse("its signature algorithm");
    }
    let params = cert.signature_algorithm.parameters.as_ref();
    if algorithm == RSASSA_PSS && !params.is_some_and(|params| pss_parameters(params).is_some()) {
        return refuse("its RSA-PSS parameters");
    }
    if algorithm == ED25519 && params.is_some() {
        return refuse("parameters with its Ed25519 signature algorithm");
    }
    if cert.tbs_certificate.issuer_uid.is_some() || cert.tbs_certificate.subject_uid.is_some() {
        return refuse("unique identifiers");
    }
    let mut authority_key_id = false;
    for extension in cert.extensions() {
        match extension.parsed_extension() {
            ParsedExtension::AuthorityKeyIdentifier(_) => authority_key_id = true,
            ParsedExtension::KeyUsage(usage) if usage.key_cert_sign() => {
                return refuse("it may sign certificates");
            }
            // A claim signer is not a time-stamping or OCSP responder.
            ParsedExtension::ExtendedKeyUsage(eku) if eku.ocsp_signing || eku.time_stamping => {
                return refuse("time-stamping or OCSP signing use");
            }
            ParsedExtension::UnsupportedExtension { .. }
            | ParsedExtension::ParseError { .. }
            | ParsedExtension::Unparsed
                if extension.critical =>
            {
                return refuse("a critical extension verifiers do not know");
            }
            _ => {}
        }
    }
    if !authority_key_id {
        return refuse("no Authority Key Identifier");
    }
    Ok(())
}

/// The identity stored in `dir`, if there is one. A record whose key does
/// not belong to its certificate is refused.
pub fn load(dir: &Path) -> Result<Option<SigningIdentity>, String> {
    let path = dir.join(IDENTITY_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("The signing identity cannot be read: {e}")),
    };
    let record: Record = serde_json::from_str(&text)
        .map_err(|_| "The signing identity cannot be read.".to_string())?;
    use base64::Engine;
    let pkcs8 = base64::engine::general_purpose::STANDARD
        .decode(record.key.as_bytes())
        .map_err(|_| "The signing identity cannot be read.".to_string())?;
    let rng = SystemRandom::new();
    let key = key_from_pkcs8(&pkcs8, &rng)?;
    let chain = pem_certificates(&record.chain)?;
    let first = chain
        .first()
        .ok_or("The signing identity has no certificate.")?;
    let (_, cert) = X509Certificate::from_der(first)
        .map_err(|e| format!("The signing certificate cannot be read: {e}"))?;
    if cert.public_key().subject_public_key.data.as_ref() != public_key(&key) {
        return Err("The signing identity's key does not belong to its certificate.".to_string());
    }
    // An identity saved before a check was added gets it too: what the
    // manifests are signed with must be what its certificate allows.
    check_public_key_algorithm(&cert, &key)?;
    Ok(Some(SigningIdentity {
        key,
        chain,
        kind: record.kind,
        rng,
    }))
}

/// What [`load`] would load, without the key in memory longer than needed.
pub fn info(dir: &Path) -> Result<Option<IdentityInfo>, String> {
    load(dir)?.map(|identity| identity.info()).transpose()
}

/// Deletes the identity in `dir` (the user's request). Whether there was one.
pub fn remove(dir: &Path) -> Result<bool, String> {
    match std::fs::remove_file(dir.join(IDENTITY_FILE)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("The signing identity could not be removed: {e}")),
    }
}

/// Generates a key and a local certificate for it, naming `signer`, and
/// stores them in `dir` (replacing an identity there). The certificate is
/// issued by a local authority made for it alone, whose key is not kept:
/// C2PA does not accept self-signed signing certificates, and no one can
/// issue another certificate in its name later.
pub fn generate(dir: &Path, signer: &str) -> Result<IdentityInfo, String> {
    let rng = SystemRandom::new();
    fn failed<E>(_: E) -> String {
        "A signing key could not be generated.".to_string()
    }
    let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
    let ca_pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).map_err(failed)?;
    let ca = EcdsaKeyPair::from_pkcs8(alg, ca_pkcs8.as_ref(), &rng).map_err(failed)?;
    let ee_pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).map_err(failed)?;
    let ee = EcdsaKeyPair::from_pkcs8(alg, ee_pkcs8.as_ref(), &rng).map_err(failed)?;
    let signer = signer.trim();
    let signer = if signer.is_empty() {
        "Rawtonic local signer"
    } else {
        signer
    };
    let now = jiff::Timestamp::now();
    let start = now - jiff::SignedDuration::from_hours(24);
    let ca_name = name(&format!("{signer} (local certificate authority)"));
    let ca_ski = key_id(ca.public_key().as_ref());
    let ca_cert = certificate(
        &rng,
        &ca,
        &ca_name,
        &ca_name,
        ca.public_key().as_ref(),
        (
            start,
            now + jiff::SignedDuration::from_hours(24 * 365 * 10 + 48),
        ),
        &[
            extension("2.5.29.19", true, &der::seq(&[der::boolean(true)])),
            // keyCertSign, cRLSign.
            extension("2.5.29.15", true, &der::tlv(0x03, &[0x01, 0x06])),
            extension("2.5.29.14", false, &der::octets(&ca_ski)),
        ],
    )?;
    let ee_cert = certificate(
        &rng,
        &ca,
        &ca_name,
        &name(signer),
        ee.public_key().as_ref(),
        (
            start,
            now + jiff::SignedDuration::from_hours(24 * 365 * 5 + 24),
        ),
        &[
            extension("2.5.29.19", true, &der::seq(&[])),
            // digitalSignature.
            extension("2.5.29.15", true, &der::tlv(0x03, &[0x07, 0x80])),
            extension(
                "2.5.29.37",
                false,
                &der::seq(&[der::oid(EMAIL_PROTECTION), der::oid(DOCUMENT_SIGNING)]),
            ),
            extension(
                "2.5.29.14",
                false,
                &der::octets(&key_id(ee.public_key().as_ref())),
            ),
            extension("2.5.29.35", false, &der::seq(&[der::tlv(0x80, &ca_ski)])),
        ],
    )?;
    // `ca` and its PKCS#8 go out of scope here: never stored.
    store(
        dir,
        ee_pkcs8.as_ref(),
        &[ee_cert, ca_cert],
        IdentityKind::Generated,
    )
}

/// Imports the key and certificate chain of a PKCS#12 file into `dir`
/// (replacing an identity there), after checking that C2PA verifiers accept
/// the certificate for signing.
pub fn import_pkcs12(dir: &Path, file: &Path, password: &str) -> Result<IdentityInfo, String> {
    let data =
        std::fs::read(file).map_err(|e| format!("{} cannot be read: {e}", file.display()))?;
    let store_ = p12_keystore::KeyStore::from_pkcs12(
        &data,
        password,
        p12_keystore::Pkcs12ImportPolicy::Strict,
    )
    .map_err(|_| {
        "The file could not be opened: wrong password, or not a PKCS#12 (.p12/.pfx) file."
            .to_string()
    })?;
    let (_, entry) = store_
        .private_key_chain()
        .ok_or("The file holds no private key with its certificate.")?;
    let pkcs8 = entry.key().as_der().to_vec();
    let chain: Vec<Vec<u8>> = entry.certs().iter().map(|c| c.as_der().to_vec()).collect();
    let rng = SystemRandom::new();
    let key = key_from_pkcs8(&pkcs8, &rng)?;
    check_signing_certificate(&chain, &key)?;
    store(dir, &pkcs8, &chain, IdentityKind::Imported)
}

fn store(
    dir: &Path,
    pkcs8: &[u8],
    chain: &[Vec<u8>],
    kind: IdentityKind,
) -> Result<IdentityInfo, String> {
    let failed = |e: anyhow::Error| format!("The signing identity could not be saved: {e:#}");
    private_dir(dir).map_err(|e| failed(e.into()))?;
    use base64::Engine;
    let record = Record {
        kind,
        key: base64::engine::general_purpose::STANDARD.encode(pkcs8),
        chain: pem(chain),
    };
    let bytes = serde_json::to_vec(&record).expect("an identity record");
    // One file, replaced whole: `atomic_write`'s temporary file is readable
    // by the user only and a new file keeps that; a replaced one keeps its
    // permissions, so a record others can read is removed first.
    let path = dir.join(IDENTITY_FILE);
    remove_if_shared(&path).map_err(|e| failed(e.into()))?;
    rawtonic_fs::atomic_write(&path, bytes).map_err(failed)?;
    load(dir)?
        .ok_or("The signing identity was not saved.")?
        .info()
}

/// Creates `dir` readable by the user only (Unix).
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// An existing record others can read is removed before the new one is
/// written (an atomic replace keeps the replaced file's permissions).
fn remove_if_shared(path: &PathBuf) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::symlink_metadata(path)
            && meta.permissions().mode() & 0o077 != 0
        {
            std::fs::remove_file(path)?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn name(common_name: &str) -> Vec<u8> {
    der::seq(&[
        der::set(&[der::seq(&[der::oid("2.5.4.3"), der::utf8(common_name)])]),
        der::set(&[der::seq(&[
            der::oid("2.5.4.10"),
            der::utf8("Rawtonic local identity (unverified)"),
        ])]),
    ])
}

/// A key identifier: the first 20 bytes of SHA-256 of the public key
/// (RFC 7093 method 1).
fn key_id(public_key: &[u8]) -> Vec<u8> {
    ring::digest::digest(&ring::digest::SHA256, public_key).as_ref()[..20].to_vec()
}

fn extension(oid: &str, critical: bool, value: &[u8]) -> Vec<u8> {
    let mut parts = vec![der::oid(oid)];
    if critical {
        parts.push(der::boolean(true));
    }
    parts.push(der::octets(value));
    der::seq(&parts)
}

/// An X.509 v3 certificate for a P-256 `public_key`, signed by `issuer_key`.
fn certificate(
    rng: &SystemRandom,
    issuer_key: &EcdsaKeyPair,
    issuer: &[u8],
    subject: &[u8],
    public_key: &[u8],
    validity: (jiff::Timestamp, jiff::Timestamp),
    extensions: &[Vec<u8>],
) -> Result<Vec<u8>, String> {
    let ecdsa_sha256 = der::seq(&[der::oid("1.2.840.10045.4.3.2")]);
    certificate_named(
        rng,
        issuer_key,
        &ecdsa_sha256,
        (issuer, subject),
        &p256_public_key_info(public_key),
        validity,
        extensions,
    )
}

/// The SubjectPublicKeyInfo of a P-256 `public_key`.
fn p256_public_key_info(public_key: &[u8]) -> Vec<u8> {
    der::seq(&[
        der::seq(&[der::oid(EC_PUBLIC_KEY), der::oid("1.2.840.10045.3.1.7")]),
        der::bit_string(public_key),
    ])
}

/// [`certificate`] whose signature algorithm is named `algorithm` and whose
/// SubjectPublicKeyInfo is `public_key_info` (tests name others than the
/// ECDSA it is signed with).
fn certificate_named(
    rng: &SystemRandom,
    issuer_key: &EcdsaKeyPair,
    algorithm: &[u8],
    (issuer, subject): (&[u8], &[u8]),
    public_key_info: &[u8],
    (not_before, not_after): (jiff::Timestamp, jiff::Timestamp),
    extensions: &[Vec<u8>],
) -> Result<Vec<u8>, String> {
    let ecdsa_sha256 = algorithm.to_vec();
    let mut serial = [0u8; 16];
    rng.fill(&mut serial)
        .map_err(|_| "A certificate serial number could not be made.".to_string())?;
    serial[0] &= 0x7f;
    let tbs = der::seq(&[
        der::explicit(0, &der::uint(&[2])),
        der::uint(&serial),
        ecdsa_sha256.clone(),
        issuer.to_vec(),
        der::seq(&[der::time(not_before), der::time(not_after)]),
        subject.to_vec(),
        public_key_info.to_vec(),
        der::explicit(3, &der::seq(extensions)),
    ]);
    // ring signs r‖s; X.509 takes the same signature in DER.
    let fixed = issuer_key
        .sign(rng, &tbs)
        .map_err(|_| "A certificate could not be signed.".to_string())?;
    Ok(der::seq(&[
        tbs,
        ecdsa_sha256,
        der::bit_string(&ecdsa_der(fixed.as_ref())),
    ]))
}

/// An ECDSA r‖s signature as the DER `Ecdsa-Sig-Value` X.509 uses.
fn ecdsa_der(fixed: &[u8]) -> Vec<u8> {
    let (r, s) = fixed.split_at(fixed.len() / 2);
    der::seq(&[der::uint(r), der::uint(s)])
}

fn pem(chain: &[Vec<u8>]) -> String {
    use base64::Engine;
    let mut out = String::new();
    for cert in chain {
        out.push_str("-----BEGIN CERTIFICATE-----\n");
        let b64 = base64::engine::general_purpose::STANDARD.encode(cert);
        for line in b64.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
            out.push('\n');
        }
        out.push_str("-----END CERTIFICATE-----\n");
    }
    out
}

fn pem_certificates(pem: &str) -> Result<Vec<Vec<u8>>, String> {
    use base64::Engine;
    let mut certs = Vec::new();
    let mut current: Option<String> = None;
    for line in pem.lines().map(str::trim) {
        match (line, current.as_mut()) {
            ("-----BEGIN CERTIFICATE-----", _) => current = Some(String::new()),
            ("-----END CERTIFICATE-----", Some(b64)) => {
                certs.push(
                    base64::engine::general_purpose::STANDARD
                        .decode(b64.as_bytes())
                        .map_err(|e| format!("The certificate chain cannot be read: {e}"))?,
                );
                current = None;
            }
            (text, Some(b64)) => b64.push_str(text),
            _ => {}
        }
    }
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key (PKCS#8) and chain of the record in `dir`.
    fn stored(dir: &Path) -> (Vec<u8>, Vec<Vec<u8>>) {
        use base64::Engine;
        let record: Record =
            serde_json::from_slice(&std::fs::read(dir.join(IDENTITY_FILE)).unwrap()).unwrap();
        let key = base64::engine::general_purpose::STANDARD
            .decode(record.key.as_bytes())
            .unwrap();
        (key, pem_certificates(&record.chain).unwrap())
    }

    #[test]
    fn generated_identity_is_private_and_valid_for_c2pa() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join("provenance");
        let info = generate(&store_dir, "Jane Example").unwrap();
        assert_eq!(info.kind, IdentityKind::Generated);
        assert_eq!(info.signer, "Jane Example");
        assert_eq!(info.issuer, "Jane Example (local certificate authority)");
        assert_eq!(info.algorithm, "ES256");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&store_dir.join(IDENTITY_FILE)), 0o600);
            assert_eq!(mode(&store_dir), 0o700);
        }
        let identity = load(&store_dir).unwrap().unwrap();
        assert_eq!(identity.chain().len(), 2);
        // The certificate passes the checks an imported one must pass.
        check_signing_certificate(identity.chain(), &identity.key).unwrap();
        let (_, ca) = X509Certificate::from_der(&identity.chain()[1]).unwrap();
        assert!(ca.is_ca());
        let (_, ee) = X509Certificate::from_der(&identity.chain()[0]).unwrap();
        let signed_by = |cert: &X509Certificate<'_>, issuer: &X509Certificate<'_>| {
            signature::UnparsedPublicKey::new(
                &signature::ECDSA_P256_SHA256_ASN1,
                &issuer.public_key().subject_public_key.data,
            )
            .verify(cert.tbs_certificate.as_ref(), &cert.signature_value.data)
            .is_ok()
        };
        assert!(signed_by(&ee, &ca));
        assert!(signed_by(&ca, &ca));
        assert!(!signed_by(&ee, &ee));
        // The issuing authority's key is not kept: only the signer's key is.
        let files: Vec<_> = std::fs::read_dir(&store_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(files, [IDENTITY_FILE], "{files:?}");
        assert!(!info.expired);
        identity.check_usable().unwrap();
        let message = b"claim";
        let sig = identity.sign(message).unwrap();
        assert_eq!(sig.len(), identity.signature_len());
        signature::UnparsedPublicKey::new(
            &signature::ECDSA_P256_SHA256_FIXED,
            public_key(&identity.key),
        )
        .verify(message, &sig)
        .unwrap();
        // The key does not show in debug output.
        assert!(!format!("{identity:?}").contains("key:"));
        assert!(remove(&store_dir).unwrap());
        assert!(load(&store_dir).unwrap().is_none());
        assert!(!remove(&store_dir).unwrap());
    }

    #[test]
    fn a_shared_key_file_is_replaced_by_a_private_one() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(IDENTITY_FILE), b"old").unwrap();
            std::fs::set_permissions(
                dir.path().join(IDENTITY_FILE),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            generate(dir.path(), "").unwrap();
            let mode = std::fs::metadata(dir.path().join(IDENTITY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// A PKCS#12 of `chain` and `key`, encrypted with `algorithm`.
    fn pkcs12(
        key: &[u8],
        chain: &[Vec<u8>],
        algorithm: p12_keystore::EncryptionAlgorithm,
    ) -> Vec<u8> {
        let certs = chain
            .iter()
            .map(|c| p12_keystore::Certificate::from_der(c).unwrap());
        let entry = p12_keystore::PrivateKeyChain::new(
            vec![1u8, 2, 3],
            p12_keystore::PrivateKey::from_der(key).unwrap(),
            certs,
        );
        let mut store = p12_keystore::KeyStore::new();
        store.add_entry(
            "signer",
            p12_keystore::KeyStoreEntry::PrivateKeyChain(entry),
        );
        store
            .writer("secret")
            .encryption_algorithm(algorithm)
            .write()
            .unwrap()
    }

    #[test]
    fn imports_a_pkcs12_certificate_and_refuses_wrong_ones() {
        let made = tempfile::tempdir().unwrap();
        let generated = generate(made.path(), "Studio Example").unwrap();
        let (key, chain) = stored(made.path());
        use p12_keystore::EncryptionAlgorithm::*;
        for algorithm in [PbeWithHmacSha256AndAes256, PbeWithShaAnd3KeyTripleDesCbc] {
            let file = made.path().join("signer.p12");
            std::fs::write(&file, pkcs12(&key, &chain, algorithm)).unwrap();
            let target = tempfile::tempdir().unwrap();
            let info = import_pkcs12(target.path(), &file, "secret").unwrap();
            assert_eq!(info.kind, IdentityKind::Imported);
            assert_eq!(info.fingerprint, generated.fingerprint);
            assert_eq!(info.signer, "Studio Example");
            assert_eq!(
                load(target.path()).unwrap().unwrap().chain(),
                chain.as_slice()
            );
            let wrong = import_pkcs12(target.path(), &file, "guess").unwrap_err();
            assert!(wrong.contains("wrong password"), "{wrong}");
            // The identity imported before stays.
            assert!(info_matches(target.path(), &info));
        }
        // A certificate of another key is refused, and nothing is stored.
        let other = tempfile::tempdir().unwrap();
        generate(other.path(), "Other").unwrap();
        let (other_key, _) = stored(other.path());
        let file = made.path().join("mismatch.p12");
        std::fs::write(
            &file,
            pkcs12(&other_key, &chain, PbeWithHmacSha256AndAes256),
        )
        .unwrap();
        let target = tempfile::tempdir().unwrap();
        assert!(import_pkcs12(target.path(), &file, "secret").is_err());
        assert!(load(target.path()).unwrap().is_none());
        // A chain that does not start with the key's certificate (here the
        // local authority's) is refused.
        let ca_only = vec![chain[1].clone()];
        let rng = SystemRandom::new();
        let key = key_from_pkcs8(&key, &rng).unwrap();
        assert!(check_signing_certificate(&ca_only, &key).is_err());
    }

    #[test]
    fn a_record_whose_key_is_not_its_certificates_is_refused() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        generate(one.path(), "One").unwrap();
        generate(two.path(), "Two").unwrap();
        let (key_two, _) = stored(two.path());
        let (_, chain_one) = stored(one.path());
        let mixed = tempfile::tempdir().unwrap();
        // As if a replacement had been cut short between key and chain.
        let record = Record {
            kind: IdentityKind::Generated,
            key: {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(&key_two)
            },
            chain: pem(&chain_one),
        };
        std::fs::write(
            mixed.path().join(IDENTITY_FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let error = load(mixed.path()).unwrap_err();
        assert!(error.contains("does not belong"), "{error}");
    }

    /// A record saved before the public-key check (here its certificate
    /// names the P-256 key on another curve) is refused when it is loaded,
    /// so it never signs.
    #[test]
    fn a_saved_identity_its_certificate_does_not_allow_is_refused_on_load() {
        let rng = SystemRandom::new();
        let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap();
        let now = jiff::Timestamp::now();
        let day = jiff::SignedDuration::from_hours(24);
        let on_p384 = der::seq(&[
            der::seq(&[der::oid(EC_PUBLIC_KEY), der::oid("1.3.132.0.34")]),
            der::bit_string(key.public_key().as_ref()),
        ]);
        let cert = certificate_named(
            &rng,
            &key,
            &der::seq(&[der::oid("1.2.840.10045.4.3.2")]),
            (&name("Old"), &name("Old")),
            &on_p384,
            (now - day, now + day),
            &[],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let record = Record {
            kind: IdentityKind::Imported,
            key: {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref())
            },
            chain: pem(&[cert]),
        };
        std::fs::write(
            dir.path().join(IDENTITY_FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let Err(error) = load(dir.path()) else {
            panic!("loaded");
        };
        assert!(error.contains("public key"), "{error}");
    }

    #[test]
    fn an_expired_certificate_cannot_sign() {
        let rng = SystemRandom::new();
        let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap();
        let name = name("Expired");
        let day = jiff::SignedDuration::from_hours(24);
        let now = jiff::Timestamp::now();
        let cert = certificate(
            &rng,
            &key,
            &name,
            &name,
            key.public_key().as_ref(),
            (now - day * 10, now - day),
            &[],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let info = store(dir.path(), pkcs8.as_ref(), &[cert], IdentityKind::Imported).unwrap();
        assert!(info.expired);
        let error = load(dir.path())
            .unwrap()
            .unwrap()
            .check_usable()
            .unwrap_err();
        assert!(error.contains("not now"), "{error}");
    }

    #[test]
    fn a_certificate_outside_the_c2pa_profile_is_refused_at_import() {
        let rng = SystemRandom::new();
        let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let ca_pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
        let ca = EcdsaKeyPair::from_pkcs8(alg, ca_pkcs8.as_ref(), &rng).unwrap();
        let ee_pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
        let ee = EcdsaKeyPair::from_pkcs8(alg, ee_pkcs8.as_ref(), &rng).unwrap();
        let now = jiff::Timestamp::now();
        let day = jiff::SignedDuration::from_hours(24);
        let leaf = |extensions: &[Vec<u8>]| {
            certificate(
                &rng,
                &ca,
                &name("A Studio CA"),
                &name("Studio"),
                ee.public_key().as_ref(),
                (now - day, now + day * 30),
                extensions,
            )
            .unwrap()
        };
        let base = || {
            vec![
                extension("2.5.29.19", true, &der::seq(&[])),
                extension("2.5.29.15", true, &der::tlv(0x03, &[0x07, 0x80])),
                extension("2.5.29.37", false, &der::seq(&[der::oid(EMAIL_PROTECTION)])),
            ]
        };
        let key = key_from_pkcs8(ee_pkcs8.as_ref(), &rng).unwrap();
        // Everything C2PA asks for, with an Authority Key Identifier: accepted.
        let mut with_aki = base();
        with_aki.push(extension(
            "2.5.29.35",
            false,
            &der::seq(&[der::tlv(0x80, &[1; 20])]),
        ));
        check_signing_certificate(&[leaf(&with_aki)], &key).unwrap();
        // The same without it, as a CA might issue it: refused.
        let error = check_signing_certificate(&[leaf(&base())], &key).unwrap_err();
        assert!(error.contains("Authority Key Identifier"), "{error}");
        // Time-stamping or OCSP signing beside document signing: refused, each.
        for purpose in ["1.3.6.1.5.5.7.3.8", "1.3.6.1.5.5.7.3.9"] {
            let mut mixed = base();
            mixed[2] = extension(
                "2.5.29.37",
                false,
                &der::seq(&[der::oid(DOCUMENT_SIGNING), der::oid(purpose)]),
            );
            mixed.push(extension(
                "2.5.29.35",
                false,
                &der::seq(&[der::tlv(0x80, &[1; 20])]),
            ));
            let error = check_signing_certificate(&[leaf(&mixed)], &key).unwrap_err();
            assert!(
                error.contains("time-stamping or OCSP"),
                "{purpose}: {error}"
            );
        }
        // RSA-PSS named with SHA-256 but MGF1 with SHA-384: refused; matching: accepted.
        let pss = |mgf_hash: &str| {
            let hash = |dotted: &str| der::seq(&[der::oid(dotted)]);
            der::seq(&[
                der::oid(RSASSA_PSS),
                der::seq(&[
                    der::explicit(0, &hash("2.16.840.1.101.3.4.2.1")),
                    der::explicit(
                        1,
                        &der::seq(&[der::oid("1.2.840.113549.1.1.8"), hash(mgf_hash)]),
                    ),
                    der::explicit(2, &der::uint(&[32])),
                ]),
            ])
        };
        for (mgf_hash, ok) in [
            ("2.16.840.1.101.3.4.2.2", false),
            ("2.16.840.1.101.3.4.2.1", true),
        ] {
            let cert = certificate_named(
                &rng,
                &ca,
                &pss(mgf_hash),
                (&name("A Studio CA"), &name("Studio")),
                &p256_public_key_info(ee.public_key().as_ref()),
                (now - day, now + day * 30),
                &with_aki,
            )
            .unwrap();
            let result = check_signing_certificate(&[cert], &key);
            assert_eq!(result.is_ok(), ok, "{mgf_hash}: {result:?}");
        }
        // A critical extension verifiers do not know: refused.
        let mut unknown = with_aki.clone();
        unknown.push(extension("1.3.6.1.4.1.99999.1", true, &der::octets(&[1])));
        let error = check_signing_certificate(&[leaf(&unknown)], &key).unwrap_err();
        assert!(error.contains("critical extension"), "{error}");
    }

    /// The checks of review round 3 (PSS parameters, Ed25519 parameters,
    /// the public key's algorithm), and the key usage, validity and chain
    /// order a signing certificate needs: each malformed or refused case
    /// beside one that passes.
    #[test]
    fn malformed_or_unusable_certificates_are_refused_at_import() {
        let rng = SystemRandom::new();
        let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let ca_pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
        let ca = EcdsaKeyPair::from_pkcs8(alg, ca_pkcs8.as_ref(), &rng).unwrap();
        let ee_pkcs8 = EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
        let ee = EcdsaKeyPair::from_pkcs8(alg, ee_pkcs8.as_ref(), &rng).unwrap();
        let key = key_from_pkcs8(ee_pkcs8.as_ref(), &rng).unwrap();
        let now = jiff::Timestamp::now();
        let day = jiff::SignedDuration::from_hours(24);
        let ee_info = p256_public_key_info(ee.public_key().as_ref());
        let ecdsa = der::seq(&[der::oid("1.2.840.10045.4.3.2")]);
        let extensions = |key_usage: Option<&[u8]>| {
            let mut all = vec![extension("2.5.29.19", true, &der::seq(&[]))];
            if let Some(bits) = key_usage {
                all.push(extension("2.5.29.15", true, &der::tlv(0x03, bits)));
            }
            all.push(extension(
                "2.5.29.37",
                false,
                &der::seq(&[der::oid(DOCUMENT_SIGNING)]),
            ));
            all.push(extension(
                "2.5.29.35",
                false,
                &der::seq(&[der::tlv(0x80, &[1; 20])]),
            ));
            all
        };
        let signing = extensions(Some(&[0x07, 0x80]));
        let cert = |algorithm: &[u8], info: &[u8], validity, extensions: &[Vec<u8>]| {
            certificate_named(
                &rng,
                &ca,
                algorithm,
                (&name("A Studio CA"), &name("Studio")),
                info,
                validity,
                extensions,
            )
            .unwrap()
        };
        let valid = (now - day, now + day * 30);
        let check = |cert: Vec<u8>| check_signing_certificate(&[cert], &key);
        check(cert(&ecdsa, &ee_info, valid, &signing)).unwrap();

        // RSASSA-PSS parameters: [0] hash, [1] MGF1 with that hash,
        // optional [2] salt length and [3] trailer field 1, nothing else.
        let hash = |dotted: &str| der::seq(&[der::oid(dotted)]);
        let sha256 = "2.16.840.1.101.3.4.2.1";
        let mgf1 = |hash_id: Vec<u8>| der::seq(&[der::oid(MGF1), hash_id]);
        let pss = |fields: &[Vec<u8>]| der::seq(&[der::oid(RSASSA_PSS), der::seq(fields)]);
        let h0 = || der::explicit(0, &hash(sha256));
        let m1 = || der::explicit(1, &mgf1(hash(sha256)));
        let salt = |value: &[u8]| der::explicit(2, &der::tlv(0x02, value));
        let cases: Vec<(&str, Vec<u8>, bool)> = vec![
            (
                "SHA-256, MGF1 SHA-256, salt 32",
                pss(&[h0(), m1(), salt(&[32])]),
                true,
            ),
            (
                "with trailer field 1",
                pss(&[h0(), m1(), salt(&[32]), der::explicit(3, &der::uint(&[1]))]),
                true,
            ),
            (
                "hash with NULL parameters",
                pss(&[
                    der::explicit(0, &der::seq(&[der::oid(sha256), vec![0x05, 0x00]])),
                    der::explicit(1, &mgf1(der::seq(&[der::oid(sha256), vec![0x05, 0x00]]))),
                ]),
                true,
            ),
            ("no parameters", der::seq(&[der::oid(RSASSA_PSS)]), false),
            ("empty parameters (SHA-1 defaults)", pss(&[]), false),
            ("hash only", pss(&[h0()]), false),
            ("MGF before hash", pss(&[m1(), h0()]), false),
            (
                "SHA-1",
                pss(&[
                    der::explicit(0, &hash("1.3.14.3.2.26")),
                    der::explicit(1, &mgf1(hash("1.3.14.3.2.26"))),
                ]),
                false,
            ),
            (
                "a mask function other than MGF1",
                pss(&[
                    h0(),
                    der::explicit(
                        1,
                        &der::seq(&[der::oid("1.2.840.113549.1.1.9"), hash(sha256)]),
                    ),
                ]),
                false,
            ),
            (
                "MGF1 with SHA-512",
                pss(&[
                    h0(),
                    der::explicit(1, &mgf1(hash("2.16.840.1.101.3.4.2.3"))),
                ]),
                false,
            ),
            (
                "trailer field 2",
                pss(&[h0(), m1(), der::explicit(3, &der::uint(&[2]))]),
                false,
            ),
            (
                "an unknown field",
                pss(&[h0(), m1(), der::explicit(4, &der::uint(&[1]))]),
                false,
            ),
            (
                "a negative salt length",
                pss(&[h0(), m1(), salt(&[0xe0])]),
                false,
            ),
            (
                "bytes after the hash identifier",
                pss(&[
                    der::explicit(0, &[hash(sha256), vec![0x05, 0x00]].concat()),
                    m1(),
                ]),
                false,
            ),
            (
                "hash parameters other than NULL",
                pss(&[
                    der::explicit(0, &der::seq(&[der::oid(sha256), der::uint(&[1])])),
                    m1(),
                ]),
                false,
            ),
            (
                "a length past the end",
                {
                    let mut p = pss(&[h0(), m1()]);
                    let n = p.len();
                    p[n - 1] = 0x7f;
                    p[n - 3] = 0x7f;
                    p
                },
                false,
            ),
        ];
        for (case, algorithm, ok) in cases {
            let result = check(cert(&algorithm, &ee_info, valid, &signing));
            assert_eq!(result.is_ok(), ok, "{case}: {result:?}");
            if !ok {
                let error = result.unwrap_err();
                assert!(
                    error.contains("RSA-PSS") || error.contains("signature algorithm"),
                    "{case}: {error}"
                );
            }
        }

        // Ed25519 as the certificate's signature algorithm takes no parameters.
        check(cert(
            &der::seq(&[der::oid(ED25519)]),
            &ee_info,
            valid,
            &signing,
        ))
        .unwrap();
        let error = check(cert(
            &der::seq(&[der::oid(ED25519), vec![0x05, 0x00]]),
            &ee_info,
            valid,
            &signing,
        ))
        .unwrap_err();
        assert!(error.contains("Ed25519"), "{error}");

        // The public key named on another curve, or without its curve.
        let point = ee.public_key().as_ref();
        for info in [
            der::seq(&[
                der::seq(&[der::oid(EC_PUBLIC_KEY), der::oid("1.3.132.0.34")]),
                der::bit_string(point),
            ]),
            der::seq(&[der::seq(&[der::oid(EC_PUBLIC_KEY)]), der::bit_string(point)]),
        ] {
            let error = check(cert(&ecdsa, &info, valid, &signing)).unwrap_err();
            assert!(error.contains("public key"), "{error}");
        }

        // Key usage without digitalSignature (key encipherment, or
        // non-repudiation only), or none at all.
        for key_usage in [Some(&[0x05, 0x20][..]), Some(&[0x06, 0x40][..]), None] {
            let error = check(cert(&ecdsa, &ee_info, valid, &extensions(key_usage))).unwrap_err();
            assert!(
                error.contains("digital signatures"),
                "{key_usage:?}: {error}"
            );
        }

        // Expired, or not valid yet.
        for validity in [(now - day * 10, now - day), (now + day, now + day * 30)] {
            let error = check(cert(&ecdsa, &ee_info, validity, &signing)).unwrap_err();
            assert!(error.contains("not valid now"), "{error}");
        }

        // The chain: the signer's certificate, then its issuer's.
        let authority = |subject: &str| {
            certificate(
                &rng,
                &ca,
                &name(subject),
                &name(subject),
                ca.public_key().as_ref(),
                valid,
                &[extension(
                    "2.5.29.19",
                    true,
                    &der::seq(&[der::boolean(true)]),
                )],
            )
            .unwrap()
        };
        let leaf = cert(&ecdsa, &ee_info, valid, &signing);
        check_signing_certificate(&[leaf.clone(), authority("A Studio CA")], &key).unwrap();
        for chain in [
            vec![leaf.clone(), authority("Another CA")],
            vec![
                leaf.clone(),
                authority("A Studio CA"),
                authority("Another CA"),
            ],
        ] {
            let error = check_signing_certificate(&chain, &key).unwrap_err();
            assert!(error.contains("not in order"), "{error}");
        }
        // The authority's certificate first: not the key's certificate.
        let error = check_signing_certificate(&[authority("A Studio CA"), leaf], &key).unwrap_err();
        assert!(error.contains("does not belong"), "{error}");
    }

    /// An RSA public key restricted to RSASSA-PSS must allow what the
    /// manifests are signed with: SHA-256, MGF1-SHA-256, a 32-byte salt.
    #[test]
    fn an_rsa_key_restricted_to_other_pss_parameters_is_refused() {
        fn any(der: &[u8]) -> x509_parser::asn1_rs::Any<'_> {
            x509_parser::asn1_rs::Any::from_der(der).unwrap().1
        }
        let hash = |dotted: &str| der::seq(&[der::oid(dotted)]);
        let pss = |hash_oid: &str, salt: Option<&[u8]>| {
            let mut fields = vec![
                der::explicit(0, &hash(hash_oid)),
                der::explicit(1, &der::seq(&[der::oid(MGF1), hash(hash_oid)])),
            ];
            if let Some(salt) = salt {
                fields.push(der::explicit(2, &der::tlv(0x02, salt)));
            }
            der::seq(&fields)
        };
        let null = [0x05, 0x00];
        assert!(rsa_key_named_ok(RSA_ENCRYPTION, None));
        assert!(rsa_key_named_ok(RSA_ENCRYPTION, Some(&any(&null))));
        assert!(!rsa_key_named_ok(
            RSA_ENCRYPTION,
            Some(&any(&pss(SHA256, None)))
        ));
        assert!(rsa_key_named_ok(RSASSA_PSS, None));
        for (params, ok) in [
            (pss(SHA256, Some(&[32])), true),
            (pss(SHA256, None), true),
            (pss(SHA256, Some(&[0, 0x20])), true),
            (pss(SHA256, Some(&[0, 0xde])), false),
            (pss(SHA256, Some(&[0xde])), false),
            (pss(SHA256, Some(&[33])), false),
            (pss(SHA256, Some(&[64])), false),
            (pss(SHA256, Some(&[1, 0, 0, 0, 0, 0, 0, 0, 0])), false),
            (pss("2.16.840.1.101.3.4.2.2", Some(&[48])), false),
            (null.to_vec(), false),
        ] {
            assert_eq!(
                rsa_key_named_ok(RSASSA_PSS, Some(&any(&params))),
                ok,
                "{params:02x?}"
            );
        }
        assert!(!rsa_key_named_ok(EC_PUBLIC_KEY, None));
        // Tag 16 as a primitive or a context-specific value is no SEQUENCE.
        let valid = pss(SHA256, Some(&[32]));
        for first in [0x10, 0xb0] {
            let mut other = valid.clone();
            other[0] = first;
            assert!(
                !rsa_key_named_ok(RSASSA_PSS, Some(&any(&other))),
                "{first:02x}"
            );
        }
        assert!(!rsa_key_named_ok(RSA_ENCRYPTION, Some(&any(&[0x25, 0x00]))));
        // The salt of 222 bytes an issuer may use with 2048-bit keys is read
        // as 222, not as a negative number: allowed in a certificate's own
        // signature parameters, beyond what a signing key may require.
        let params = any(&valid);
        assert_eq!(pss_parameters(&params).unwrap().salt, 32);
        let big = pss(SHA256, Some(&[0, 0xde]));
        assert_eq!(pss_parameters(&any(&big)).unwrap().salt, 222);
    }

    /// A PKCS#12 file whose key is tied to the authority's certificate
    /// (listed first, so the writer gives it the key's local id) is refused
    /// and nothing is stored: p12-keystore orders the chain from the
    /// certificate tied to the key, by issuer, and that one is not the key's.
    #[test]
    fn a_pkcs12_whose_key_is_tied_to_another_certificate_is_refused() {
        let made = tempfile::tempdir().unwrap();
        generate(made.path(), "Studio Example").unwrap();
        let (key, chain) = stored(made.path());
        let file = made.path().join("reversed.p12");
        let reversed = vec![chain[1].clone(), chain[0].clone()];
        let algorithm = p12_keystore::EncryptionAlgorithm::PbeWithHmacSha256AndAes256;
        std::fs::write(&file, pkcs12(&key, &reversed, algorithm)).unwrap();
        let target = tempfile::tempdir().unwrap();
        let error = import_pkcs12(target.path(), &file, "secret").unwrap_err();
        assert!(error.contains("does not belong"), "{error}");
        assert!(load(target.path()).unwrap().is_none());
    }

    #[test]
    fn names_are_shown_without_control_or_direction_characters() {
        assert_eq!(shown("Studio Example"), "Studio Example");
        assert_eq!(shown("Ann\u{202e}gpj.exe\n"), "Ann\u{fffd}gpj.exe\u{fffd}");
        assert_eq!(shown("a\u{2066}b\u{200f}c"), "a\u{fffd}b\u{fffd}c");
    }

    fn info_matches(dir: &Path, want: &IdentityInfo) -> bool {
        info(dir).unwrap().as_ref() == Some(want)
    }

    #[test]
    fn pem_round_trip() {
        let chain = vec![vec![1u8; 100], vec![2u8; 3]];
        assert_eq!(pem_certificates(&pem(&chain)).unwrap(), chain);
    }
}
