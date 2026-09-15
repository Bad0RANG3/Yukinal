//! OpenSSH key revocation list (KRL) parsing and host-certificate matching.
//!
//! The parser follows OpenSSH's `PROTOCOL.krl`: a magic/version header followed by
//! length-prefixed sections. Certificate, explicit-key and fingerprint revocations are
//! enforced. Signed KRLs are accepted only when every signature is valid and at least one
//! signer matches the configured host CA; unsupported critical extensions fail closed.

use std::collections::HashSet;

use sha1::{Digest as _, Sha1};
use sha2::Sha256;
use signature::Verifier;
use ssh_key::encoding::Encode;
use ssh_key::public::KeyData;
use ssh_key::{Certificate, PublicKey};

const KRL_MAGIC: &[u8; 8] = b"SSHKRL\n\0";
const KRL_FORMAT_VERSION: u32 = 1;
const SECTION_CERTIFICATES: u8 = 1;
const SECTION_EXPLICIT_KEY: u8 = 2;
const SECTION_FINGERPRINT_SHA1: u8 = 3;
const SECTION_SIGNATURE: u8 = 4;
const SECTION_FINGERPRINT_SHA256: u8 = 5;
const SECTION_EXTENSION: u8 = 255;

const CERT_SERIAL_LIST: u8 = 0x20;
const CERT_SERIAL_RANGE: u8 = 0x21;
const CERT_SERIAL_BITMAP: u8 = 0x22;
const CERT_KEY_ID: u8 = 0x23;
const CERT_EXTENSION: u8 = 0x39;
const MAX_SIGNATURES: usize = 16;

#[derive(Debug, Clone, Default)]
pub(crate) struct RevocationList {
    certificate_sections: Vec<CertificateRevocations>,
    explicit_keys: HashSet<Vec<u8>>,
    sha1_fingerprints: HashSet<[u8; 20]>,
    sha256_fingerprints: HashSet<[u8; 32]>,
}

#[derive(Debug, Clone, Default)]
struct CertificateRevocations {
    /// Empty means the wildcard CA section.
    ca_key: Vec<u8>,
    serial_ranges: Vec<(u64, u64)>,
    key_ids: HashSet<String>,
}

#[derive(Debug)]
struct KrlSignature {
    /// Byte offset immediately after this signature section's public key string.
    signed_len: usize,
    key: PublicKey,
    signature: ssh_key::Signature,
}

impl RevocationList {
    pub(crate) fn parse(bytes: &[u8], trusted_signers: &[PublicKey]) -> Result<Self, String> {
        let mut reader = Reader::new(bytes);
        if reader.read_exact(KRL_MAGIC.len())? != KRL_MAGIC {
            return Err("KRL magic is missing".to_string());
        }
        if reader.read_u32()? != KRL_FORMAT_VERSION {
            return Err("unsupported KRL format version".to_string());
        }
        let _version = reader.read_u64()?;
        let _generated = reader.read_u64()?;
        if reader.read_u64()? != 0 {
            return Err("KRL carries unsupported flags".to_string());
        }
        let _reserved = reader.read_string()?;
        let _comment = reader.read_string()?;

        let mut list = Self::default();
        let mut signatures = Vec::new();
        let mut signature_keys = HashSet::new();
        while !reader.is_empty() {
            let section_type = reader.read_u8()?;
            if section_type == SECTION_SIGNATURE {
                if signatures.len() >= MAX_SIGNATURES {
                    return Err(format!(
                        "KRL contains more than {MAX_SIGNATURES} signatures"
                    ));
                }
                let key_blob = reader.read_string()?;
                let signed_len = reader.offset;
                let signature_blob = reader.read_string()?;
                let key = PublicKey::from_bytes(key_blob)
                    .map_err(|error| format!("KRL signature public key is invalid: {error}"))?;
                if !signature_keys.insert(key_blob.to_vec()) {
                    return Err("KRL is signed more than once by the same key".to_string());
                }
                let signature = ssh_key::Signature::try_from(signature_blob)
                    .map_err(|error| format!("KRL signature is invalid: {error}"))?;
                let canonical = Vec::<u8>::try_from(signature.clone())
                    .map_err(|error| format!("could not encode KRL signature: {error}"))?;
                if canonical != signature_blob {
                    return Err("KRL signature encoding is not canonical".to_string());
                }
                signatures.push(KrlSignature {
                    signed_len,
                    key,
                    signature,
                });
                continue;
            }
            if !signatures.is_empty() {
                return Err("KRL contains a non-signature section after a signature".to_string());
            }
            let section = reader.read_string()?;
            let mut section_reader = Reader::new(section);
            match section_type {
                SECTION_CERTIFICATES => {
                    list.certificate_sections
                        .push(parse_certificate_section(&mut section_reader)?);
                }
                SECTION_EXPLICIT_KEY => {
                    parse_explicit_keys(&mut section_reader, &mut list.explicit_keys)?;
                }
                SECTION_FINGERPRINT_SHA1 => {
                    parse_fingerprints::<20>(&mut section_reader, &mut list.sha1_fingerprints)?;
                }
                SECTION_FINGERPRINT_SHA256 => {
                    parse_fingerprints::<32>(&mut section_reader, &mut list.sha256_fingerprints)?;
                }
                SECTION_EXTENSION => parse_extension(&mut section_reader)?,
                other => return Err(format!("unsupported KRL section {other}")),
            }
            section_reader.finish()?;
        }
        verify_signatures(bytes, &signatures, trusted_signers)?;
        Ok(list)
    }

    pub(crate) fn revoked_reason(
        &self,
        certificate: &Certificate,
    ) -> Result<Option<&'static str>, String> {
        let public_key = encode_key_data(certificate.public_key())?;
        if key_is_revoked(self, &public_key) {
            return Ok(Some("certificate public key is revoked"));
        }
        let ca_key = encode_key_data(certificate.signature_key())?;
        if key_is_revoked(self, &ca_key) {
            return Ok(Some("certificate signing CA is revoked"));
        }

        for section in &self.certificate_sections {
            if !section.ca_key.is_empty() && section.ca_key != ca_key {
                continue;
            }
            if section.key_ids.contains(certificate.key_id()) {
                return Ok(Some("certificate key ID is revoked"));
            }
            if certificate.serial() != 0
                && section
                    .serial_ranges
                    .iter()
                    .any(|(lo, hi)| *lo <= certificate.serial() && certificate.serial() <= *hi)
            {
                return Ok(Some("certificate serial is revoked"));
            }
        }
        Ok(None)
    }
}

fn verify_signatures(
    bytes: &[u8],
    signatures: &[KrlSignature],
    trusted_signers: &[PublicKey],
) -> Result<(), String> {
    if signatures.is_empty() {
        return Ok(());
    }
    if trusted_signers.is_empty() {
        return Err(
            "the KRL is signed, but no trusted signer is configured to verify it".to_string(),
        );
    }
    for (index, signature) in signatures.iter().enumerate() {
        Verifier::verify(
            &signature.key,
            &bytes[..signature.signed_len],
            &signature.signature,
        )
        .map_err(|error| format!("KRL signature {} is invalid: {error}", index + 1))?;
    }
    if !signatures.iter().any(|signature| {
        trusted_signers
            .iter()
            .any(|trusted| signature.key.key_data() == trusted.key_data())
    }) {
        return Err(
            "signed KRL is not signed by the configured CA or trusted KRL signer".to_string(),
        );
    }
    Ok(())
}

fn encode_key_data(key: &KeyData) -> Result<Vec<u8>, String> {
    key.encode_vec()
        .map_err(|error| format!("could not encode public key for KRL matching: {error}"))
}

fn key_is_revoked(list: &RevocationList, key_blob: &[u8]) -> bool {
    if list.explicit_keys.contains(key_blob) {
        return true;
    }
    let sha1: [u8; 20] = Sha1::digest(key_blob).into();
    if list.sha1_fingerprints.contains(&sha1) {
        return true;
    }
    let sha256: [u8; 32] = Sha256::digest(key_blob).into();
    list.sha256_fingerprints.contains(&sha256)
}

fn parse_certificate_section(reader: &mut Reader<'_>) -> Result<CertificateRevocations, String> {
    let ca_key = reader.read_string()?.to_vec();
    let _reserved = reader.read_string()?;
    let mut section = CertificateRevocations {
        ca_key,
        ..CertificateRevocations::default()
    };
    while !reader.is_empty() {
        let subsection_type = reader.read_u8()?;
        let data = reader.read_string()?;
        let mut subreader = Reader::new(data);
        match subsection_type {
            CERT_SERIAL_LIST => {
                while !subreader.is_empty() {
                    let serial = subreader.read_u64()?;
                    if serial == 0 {
                        return Err("KRL serial list contains zero".to_string());
                    }
                    section.add_range(serial, serial);
                }
            }
            CERT_SERIAL_RANGE => {
                let lo = subreader.read_u64()?;
                let hi = subreader.read_u64()?;
                if lo == 0 || lo > hi {
                    return Err("KRL serial range is invalid".to_string());
                }
                section.add_range(lo, hi);
            }
            CERT_SERIAL_BITMAP => {
                let offset = subreader.read_u64()?;
                let encoded = subreader.read_string()?;
                let bitmap = positive_mpint(encoded)?;
                for (byte_index, byte) in bitmap.iter().rev().enumerate() {
                    for bit in 0..8_u32 {
                        if byte & (1 << bit) == 0 {
                            continue;
                        }
                        let index = byte_index
                            .checked_mul(8)
                            .and_then(|value| value.checked_add(bit as usize))
                            .ok_or_else(|| "KRL bitmap index overflow".to_string())?;
                        let serial = offset
                            .checked_add(index as u64)
                            .ok_or_else(|| "KRL serial overflow".to_string())?;
                        if serial == 0 {
                            return Err("KRL bitmap revokes serial zero".to_string());
                        }
                        section.add_range(serial, serial);
                    }
                }
            }
            CERT_KEY_ID => {
                while !subreader.is_empty() {
                    let value = std::str::from_utf8(subreader.read_string()?)
                        .map_err(|_| "KRL key ID is not UTF-8".to_string())?;
                    section.key_ids.insert(value.to_string());
                }
            }
            CERT_EXTENSION => parse_extension(&mut subreader)?,
            other => return Err(format!("unsupported KRL certificate subsection {other}")),
        }
        subreader.finish()?;
    }
    Ok(section)
}

fn parse_explicit_keys(
    reader: &mut Reader<'_>,
    target: &mut HashSet<Vec<u8>>,
) -> Result<(), String> {
    while !reader.is_empty() {
        let key = reader.read_string()?;
        if key.is_empty() {
            return Err("KRL contains an empty explicit key".to_string());
        }
        target.insert(key.to_vec());
    }
    Ok(())
}

fn parse_fingerprints<const N: usize>(
    reader: &mut Reader<'_>,
    target: &mut HashSet<[u8; N]>,
) -> Result<(), String> {
    while !reader.is_empty() {
        let raw = reader.read_string()?;
        let hash: [u8; N] = raw
            .try_into()
            .map_err(|_| format!("KRL fingerprint must be {N} bytes"))?;
        target.insert(hash);
    }
    Ok(())
}

fn parse_extension(reader: &mut Reader<'_>) -> Result<(), String> {
    let name = std::str::from_utf8(reader.read_string()?)
        .map_err(|_| "KRL extension name is not UTF-8".to_string())?;
    let critical = reader.read_u8()? != 0;
    let _contents = reader.read_string()?;
    if critical {
        return Err(format!("unsupported critical KRL extension \"{name}\""));
    }
    Ok(())
}

fn positive_mpint(encoded: &[u8]) -> Result<&[u8], String> {
    if encoded.first().is_some_and(|byte| byte & 0x80 != 0) {
        return Err("KRL bitmap mpint is negative".to_string());
    }
    if encoded.len() > 1 && encoded[0] == 0 {
        if encoded[1] & 0x80 == 0 {
            return Err("KRL bitmap mpint has a non-canonical leading zero".to_string());
        }
        return Ok(&encoded[1..]);
    }
    Ok(if encoded == [0] { &[] } else { encoded })
}

impl CertificateRevocations {
    fn add_range(&mut self, lo: u64, hi: u64) {
        self.serial_ranges.push((lo, hi));
        self.serial_ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.serial_ranges.len());
        for &(next_lo, next_hi) in &self.serial_ranges {
            match merged.last_mut() {
                Some((_, current_hi)) if next_lo <= current_hi.saturating_add(1) => {
                    *current_hi = (*current_hi).max(next_hi);
                }
                _ => merged.push((next_lo, next_hi)),
            }
        }
        self.serial_ranges = merged;
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn read_exact(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "KRL offset overflow".to_string())?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| "KRL is truncated".to_string())?;
        self.offset = end;
        Ok(value)
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(
            self.read_exact(4)?.try_into().expect("four bytes"),
        ))
    }

    fn read_u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(
            self.read_exact(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn read_string(&mut self) -> Result<&'a [u8], String> {
        let length = self.read_u32()? as usize;
        self.read_exact(length)
    }

    fn finish(&self) -> Result<(), String> {
        if self.is_empty() {
            Ok(())
        } else {
            Err("KRL section contains unparsed data".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::test_support::{generated_key, test_host_certificate};
    use signature::Signer as _;

    fn string(value: &[u8]) -> Vec<u8> {
        let mut out = (value.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(value);
        out
    }

    fn krl(sections: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = KRL_MAGIC.to_vec();
        out.extend_from_slice(&KRL_FORMAT_VERSION.to_be_bytes());
        out.extend_from_slice(&1_u64.to_be_bytes());
        out.extend_from_slice(&0_u64.to_be_bytes());
        out.extend_from_slice(&0_u64.to_be_bytes());
        out.extend_from_slice(&string(b""));
        out.extend_from_slice(&string(b"fixture"));
        for (kind, body) in sections {
            out.push(*kind);
            out.extend_from_slice(&string(body));
        }
        out
    }

    fn signed_krl(sections: &[(u8, Vec<u8>)], signers: &[&ssh_key::PrivateKey]) -> Vec<u8> {
        let mut out = krl(sections);
        for signer in signers {
            out.push(SECTION_SIGNATURE);
            out.extend_from_slice(&string(
                &signer
                    .public_key()
                    .to_bytes()
                    .expect("encode signature public key"),
            ));
            let signed_len = out.len();
            let signature: ssh_key::Signature = signer
                .try_sign(&out[..signed_len])
                .expect("sign KRL fixture");
            let encoded = Vec::<u8>::try_from(signature).expect("encode KRL signature");
            out.extend_from_slice(&string(&encoded));
        }
        out
    }

    #[test]
    fn parses_serial_lists_ranges_bitmaps_and_key_ids() {
        let ca = b"ca-key";
        let mut serials = Vec::new();
        serials.extend_from_slice(&7_u64.to_be_bytes());
        serials.extend_from_slice(&9_u64.to_be_bytes());
        let mut range = Vec::new();
        range.extend_from_slice(&20_u64.to_be_bytes());
        range.extend_from_slice(&22_u64.to_be_bytes());
        let mut bitmap = 100_u64.to_be_bytes().to_vec();
        bitmap.extend_from_slice(&string(&[0b0000_0001, 0]));
        let mut ids = Vec::new();
        ids.extend_from_slice(&string(b"host.example"));

        let mut cert = Vec::new();
        cert.extend_from_slice(&string(ca));
        cert.extend_from_slice(&string(b""));
        cert.push(CERT_SERIAL_LIST);
        cert.extend_from_slice(&string(&serials));
        cert.push(CERT_SERIAL_RANGE);
        cert.extend_from_slice(&string(&range));
        cert.push(CERT_SERIAL_BITMAP);
        cert.extend_from_slice(&string(&bitmap));
        cert.push(CERT_KEY_ID);
        cert.extend_from_slice(&string(&ids));

        let parsed =
            RevocationList::parse(&krl(&[(SECTION_CERTIFICATES, cert)]), &[]).expect("parse");
        let section = &parsed.certificate_sections[0];
        assert_eq!(section.ca_key, ca);
        assert_eq!(
            section.serial_ranges,
            vec![(7, 7), (9, 9), (20, 22), (108, 108)]
        );
        assert!(section.key_ids.contains("host.example"));
    }

    #[test]
    fn parses_explicit_keys_and_fingerprints() {
        let mut explicit = Vec::new();
        explicit.extend_from_slice(&string(b"key-blob"));
        let mut sha1 = Vec::new();
        sha1.extend_from_slice(&string(&[1_u8; 20]));
        let mut sha256 = Vec::new();
        sha256.extend_from_slice(&string(&[2_u8; 32]));

        let parsed = RevocationList::parse(
            &krl(&[
                (SECTION_EXPLICIT_KEY, explicit),
                (SECTION_FINGERPRINT_SHA1, sha1),
                (SECTION_FINGERPRINT_SHA256, sha256),
            ]),
            &[],
        )
        .expect("parse");
        assert!(parsed.explicit_keys.contains(b"key-blob".as_slice()));
        assert!(parsed.sha1_fingerprints.contains(&[1_u8; 20]));
        assert!(parsed.sha256_fingerprints.contains(&[2_u8; 32]));
    }

    #[test]
    fn rejects_malformed_signed_and_critical_krls() {
        assert!(RevocationList::parse(b"not a krl", &[]).is_err());
        assert!(RevocationList::parse(
            &krl(&[(SECTION_SIGNATURE, Vec::new())]),
            &[generated_key().public_key().clone()]
        )
        .is_err());

        let mut extension = Vec::new();
        extension.extend_from_slice(&string(b"critical@example"));
        extension.push(1);
        extension.extend_from_slice(&string(b"body"));
        assert!(
            RevocationList::parse(&krl(&[(SECTION_EXTENSION, extension)]), &[])
                .expect_err("critical extension")
                .contains("critical")
        );
    }

    #[test]
    fn verifies_signed_krls_against_the_configured_ca_and_rejects_tampering() {
        let ca = generated_key();
        let other = generated_key();
        let host = generated_key();
        let certificate = test_host_certificate(&ca, &host, &["api.example.test"]);

        let mut serial_section = Vec::new();
        serial_section.extend_from_slice(&string(&ca.public_key().to_bytes().expect("CA blob")));
        serial_section.extend_from_slice(&string(b""));
        serial_section.push(CERT_SERIAL_LIST);
        serial_section.extend_from_slice(&string(&certificate.serial().to_be_bytes()));

        let signed = signed_krl(&[(SECTION_CERTIFICATES, serial_section)], &[&ca]);
        let parsed =
            RevocationList::parse(&signed, &[ca.public_key().clone()]).expect("valid signature");
        assert_eq!(
            parsed.revoked_reason(&certificate).expect("match"),
            Some("certificate serial is revoked")
        );
        let commented_ca =
            PublicKey::new(ca.public_key().key_data().clone(), "configured CA comment");
        RevocationList::parse(&signed, &[commented_ca])
            .expect("a public-key comment is not part of the key identity");

        let error = RevocationList::parse(&signed, &[other.public_key().clone()])
            .expect_err("a signature from another CA is not trusted");
        assert!(error.contains("trusted"), "{error}");
        let error = RevocationList::parse(&signed, &[]).expect_err("no signer");
        assert!(error.contains("no trusted signer"), "{error}");

        let mut tampered = signed.clone();
        let needle = certificate.serial().to_be_bytes();
        let serial_offset = tampered
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("serial in KRL fixture");
        tampered[serial_offset] ^= 1;
        let error = RevocationList::parse(&tampered, &[ca.public_key().clone()])
            .expect_err("tampering invalidates the signature");
        assert!(error.contains("signature 1 is invalid"), "{error}");

        let mut trailing = signed;
        trailing.push(SECTION_EXTENSION);
        trailing.extend_from_slice(&string(&string(b"unused")));
        let error = RevocationList::parse(&trailing, &[ca.public_key().clone()])
            .expect_err("signatures must remain last");
        assert!(error.contains("after a signature"), "{error}");
    }

    #[test]
    fn accepts_independent_and_rotated_signing_keys() {
        let ca = generated_key();
        let old_signer = generated_key();
        let new_signer = generated_key();
        let host = generated_key();
        let certificate = test_host_certificate(&ca, &host, &["api.example.test"]);

        let mut serial_section = Vec::new();
        serial_section.extend_from_slice(&string(&ca.public_key().to_bytes().expect("CA blob")));
        serial_section.extend_from_slice(&string(b""));
        serial_section.push(CERT_SERIAL_LIST);
        serial_section.extend_from_slice(&string(&certificate.serial().to_be_bytes()));

        let independently_signed = signed_krl(
            &[(SECTION_CERTIFICATES, serial_section.clone())],
            &[&old_signer],
        );
        RevocationList::parse(
            &independently_signed,
            &[ca.public_key().clone(), old_signer.public_key().clone()],
        )
        .expect("an independent KRL signer is trusted without signing the host CA key");

        let rotated = signed_krl(
            &[(SECTION_CERTIFICATES, serial_section)],
            &[&old_signer, &new_signer],
        );
        RevocationList::parse(
            &rotated,
            &[
                ca.public_key().clone(),
                old_signer.public_key().clone(),
                new_signer.public_key().clone(),
            ],
        )
        .expect("both sides of a rotation can remain trusted");
        RevocationList::parse(
            &rotated,
            &[ca.public_key().clone(), new_signer.public_key().clone()],
        )
        .expect("the replacement signer remains valid after the old signer is removed");
        assert!(RevocationList::parse(&rotated, &[ca.public_key().clone()]).is_err());
    }

    #[test]
    fn matches_host_certificate_serial_and_key_id_revocations() {
        let ca = generated_key();
        let host = generated_key();
        let certificate = test_host_certificate(&ca, &host, &["api.example.test"]);

        let mut serial_section = Vec::new();
        serial_section.extend_from_slice(&string(&ca.public_key().to_bytes().expect("CA blob")));
        serial_section.extend_from_slice(&string(b""));
        serial_section.push(CERT_SERIAL_LIST);
        serial_section.extend_from_slice(&string(&certificate.serial().to_be_bytes()));
        let list = RevocationList::parse(&krl(&[(SECTION_CERTIFICATES, serial_section)]), &[])
            .expect("serial KRL");
        assert_eq!(
            list.revoked_reason(&certificate).expect("match"),
            Some("certificate serial is revoked")
        );

        let mut id_section = Vec::new();
        id_section.extend_from_slice(&string(b""));
        id_section.extend_from_slice(&string(b""));
        id_section.push(CERT_KEY_ID);
        id_section.extend_from_slice(&string(&string(certificate.key_id().as_bytes())));
        let list = RevocationList::parse(&krl(&[(SECTION_CERTIFICATES, id_section)]), &[])
            .expect("ID KRL");
        assert_eq!(
            list.revoked_reason(&certificate).expect("match"),
            Some("certificate key ID is revoked")
        );
    }

    #[test]
    fn a_certificate_from_another_ca_is_not_matched_by_a_scoped_serial() {
        let ca = generated_key();
        let other_ca = generated_key();
        let host = generated_key();
        let certificate = test_host_certificate(&ca, &host, &["api.example.test"]);

        let mut section = Vec::new();
        section.extend_from_slice(&string(&other_ca.public_key().to_bytes().expect("CA blob")));
        section.extend_from_slice(&string(b""));
        section.push(CERT_SERIAL_LIST);
        section.extend_from_slice(&string(&certificate.serial().to_be_bytes()));
        let list = RevocationList::parse(&krl(&[(SECTION_CERTIFICATES, section)]), &[])
            .expect("scoped KRL");
        assert_eq!(list.revoked_reason(&certificate).expect("match"), None);
    }
}
