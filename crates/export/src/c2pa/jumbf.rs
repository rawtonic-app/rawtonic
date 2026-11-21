//! JUMBF boxes (ISO/IEC 19566-5) as a C2PA manifest store uses them.

use ring::digest;

/// JUMBF content types of C2PA (C2PA 2.x, "C2PA Box details").
pub const MANIFEST_STORE: [u8; 16] = uuid(*b"c2pa");
pub const MANIFEST: [u8; 16] = uuid(*b"c2ma");
pub const ASSERTION_STORE: [u8; 16] = uuid(*b"c2as");
pub const CLAIM: [u8; 16] = uuid(*b"c2cl");
pub const SIGNATURE: [u8; 16] = uuid(*b"c2cs");
/// A CBOR content box.
pub const CBOR: [u8; 16] = uuid(*b"cbor");
/// A JSON content box (JSON-LD assertions such as `stds.schema-org.CreativeWork`).
pub const JSON: [u8; 16] = uuid(*b"json");

/// The JUMBF type UUID `XXXX0011-0010-8000-00AA00389B71` of a four-letter code.
const fn uuid(code: [u8; 4]) -> [u8; 16] {
    [
        code[0], code[1], code[2], code[3], 0x00, 0x11, 0x00, 0x10, 0x80, 0x00, 0x00, 0xAA, 0x00,
        0x38, 0x9B, 0x71,
    ]
}

/// A box of `kind` around `payload` (32-bit length).
pub fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(8 + payload.len()).expect("a manifest box under 4 GiB");
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A JUMBF superbox: a description box (requestable, labelled) and `children`.
pub fn superbox(kind: [u8; 16], label: &str, children: &[Vec<u8>]) -> Vec<u8> {
    let mut description = Vec::with_capacity(18 + label.len());
    description.extend_from_slice(&kind);
    // Toggles: requestable, label present.
    description.push(0x03);
    description.extend_from_slice(label.as_bytes());
    description.push(0);
    let mut payload = boxed(b"jumd", &description);
    children
        .iter()
        .for_each(|child| payload.extend_from_slice(child));
    boxed(b"jumb", &payload)
}

/// An assertion or claim superbox holding one CBOR box.
pub fn cbor_superbox(kind: [u8; 16], label: &str, cbor: &[u8]) -> Vec<u8> {
    superbox(kind, label, &[boxed(b"cbor", cbor)])
}

/// An assertion superbox holding one JSON box.
pub fn json_superbox(label: &str, json: &[u8]) -> Vec<u8> {
    superbox(JSON, label, &[boxed(b"json", json)])
}

/// The hash a hashed URI gives for a superbox: SHA-256 of its contents
/// without its own box header (as c2pa-rs and c2patool compute it).
pub fn superbox_hash(superbox: &[u8]) -> Vec<u8> {
    digest::digest(&digest::SHA256, &superbox[8..])
        .as_ref()
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superbox_layout() {
        let sb = cbor_superbox(CBOR, "c2pa.actions.v2", &[0xa0]);
        // jumb header, jumd (8 + 16 + 1 + 16 label bytes incl. NUL), cbor box (8 + 1).
        assert_eq!(sb.len(), 8 + 41 + 9);
        assert_eq!(&sb[4..8], b"jumb");
        assert_eq!(&sb[12..16], b"jumd");
        assert_eq!(&sb[16..32], &CBOR);
        assert_eq!(sb[32], 0x03);
        assert_eq!(&sb[33..49], b"c2pa.actions.v2\0");
        assert_eq!(&sb[53..57], b"cbor");
        assert_eq!(
            rawtonic_fs::hex(MANIFEST_STORE),
            "6332706100110010800000aa00389b71"
        );
    }
}
