//! The DER (X.690) a locally generated X.509 certificate needs.

pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(content);
    out
}

pub fn seq(items: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &items.concat())
}

pub fn set(items: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x31, &items.concat())
}

/// A non-negative INTEGER from big-endian bytes.
pub fn uint(bytes: &[u8]) -> Vec<u8> {
    let skip = bytes
        .iter()
        .take_while(|b| **b == 0)
        .count()
        .min(bytes.len().saturating_sub(1));
    let mut content = Vec::with_capacity(bytes.len() + 1);
    if bytes.get(skip).is_some_and(|b| b & 0x80 != 0) {
        content.push(0);
    }
    content.extend_from_slice(&bytes[skip..]);
    tlv(0x02, &content)
}

/// An OBJECT IDENTIFIER from its dotted form.
pub fn oid(dotted: &str) -> Vec<u8> {
    let arcs: Vec<u64> = dotted
        .split('.')
        .map(|arc| arc.parse().expect("a numeric OID arc"))
        .collect();
    let mut content = vec![(arcs[0] * 40 + arcs[1]) as u8];
    for &arc in &arcs[2..] {
        let mut chunk = vec![(arc & 0x7f) as u8];
        let mut rest = arc >> 7;
        while rest > 0 {
            chunk.push(0x80 | (rest & 0x7f) as u8);
            rest >>= 7;
        }
        content.extend(chunk.iter().rev());
    }
    tlv(0x06, &content)
}

pub fn bit_string(bytes: &[u8]) -> Vec<u8> {
    let mut content = vec![0];
    content.extend_from_slice(bytes);
    tlv(0x03, &content)
}

pub fn octets(bytes: &[u8]) -> Vec<u8> {
    tlv(0x04, bytes)
}

pub fn utf8(text: &str) -> Vec<u8> {
    tlv(0x0c, text.as_bytes())
}

pub fn boolean(value: bool) -> Vec<u8> {
    tlv(0x01, &[if value { 0xff } else { 0 }])
}

/// A context-specific constructed (EXPLICIT) tag.
pub fn explicit(number: u8, content: &[u8]) -> Vec<u8> {
    tlv(0xa0 | number, content)
}

/// UTCTime (years 1950–2049) or GeneralizedTime, to the second, in UTC.
pub fn time(at: jiff::Timestamp) -> Vec<u8> {
    let civil = jiff::tz::TimeZone::UTC.to_datetime(at);
    let year = civil.year();
    let rest = format!(
        "{:02}{:02}{:02}{:02}{:02}Z",
        civil.month(),
        civil.day(),
        civil.hour(),
        civil.minute(),
        civil.second()
    );
    if (1950..2050).contains(&year) {
        tlv(0x17, format!("{:02}{rest}", year % 100).as_bytes())
    } else {
        tlv(0x18, format!("{year:04}{rest}").as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_known_values() {
        assert_eq!(
            rawtonic_fs::hex(oid("1.2.840.10045.2.1")),
            "06072a8648ce3d0201"
        );
        assert_eq!(rawtonic_fs::hex(oid("2.5.29.37")), "0603551d25");
        assert_eq!(rawtonic_fs::hex(uint(&[0x00, 0x80])), "02020080");
        assert_eq!(rawtonic_fs::hex(uint(&[0x00, 0x00, 0x01])), "020101");
        assert_eq!(rawtonic_fs::hex(uint(&[0x00])), "020100");
        assert_eq!(tlv(0x04, &[0; 200])[..3], [0x04, 0x81, 200]);
        assert_eq!(tlv(0x04, &[0; 300])[..4], [0x04, 0x82, 0x01, 0x2c]);
        let at: jiff::Timestamp = "2026-11-21T09:05:07Z".parse().unwrap();
        assert_eq!(time(at), tlv(0x17, b"261121090507Z"));
        let later: jiff::Timestamp = "2051-01-02T03:04:05Z".parse().unwrap();
        assert_eq!(time(later), tlv(0x18, b"20510102030405Z"));
    }
}
