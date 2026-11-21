//! The CBOR (RFC 8949) a C2PA manifest is made of: definite lengths and
//! the shortest argument encoding, maps in the order they are built.

/// A CBOR data item.
#[derive(Debug, Clone, PartialEq)]
pub enum Cbor {
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Tag(u64, Box<Cbor>),
    Null,
}

impl Cbor {
    pub fn text(text: impl Into<String>) -> Self {
        Cbor::Text(text.into())
    }

    /// A map with text keys, in the order given.
    pub fn map<K: Into<String>>(entries: impl IntoIterator<Item = (K, Cbor)>) -> Self {
        Cbor::Map(
            entries
                .into_iter()
                .map(|(k, v)| (Cbor::Text(k.into()), v))
                .collect(),
        )
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Cbor::Int(v) if *v >= 0 => head(out, 0, *v as u64),
            Cbor::Int(v) => head(out, 1, (-1 - *v) as u64),
            Cbor::Bytes(b) => {
                head(out, 2, b.len() as u64);
                out.extend_from_slice(b);
            }
            Cbor::Text(t) => {
                head(out, 3, t.len() as u64);
                out.extend_from_slice(t.as_bytes());
            }
            Cbor::Array(items) => {
                head(out, 4, items.len() as u64);
                items.iter().for_each(|item| item.encode_into(out));
            }
            Cbor::Map(entries) => {
                head(out, 5, entries.len() as u64);
                for (k, v) in entries {
                    k.encode_into(out);
                    v.encode_into(out);
                }
            }
            Cbor::Tag(tag, item) => {
                head(out, 6, *tag);
                item.encode_into(out);
            }
            Cbor::Null => out.push(0xf6),
        }
    }
}

/// A major type and its argument in the shortest form.
fn head(out: &mut Vec<u8>, major: u8, arg: u64) {
    let major = major << 5;
    match arg {
        0..=23 => out.push(major | arg as u8),
        24..=0xff => out.extend_from_slice(&[major | 24, arg as u8]),
        0x100..=0xffff => {
            out.push(major | 25);
            out.extend_from_slice(&(arg as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(major | 26);
            out.extend_from_slice(&(arg as u32).to_be_bytes());
        }
        _ => {
            out.push(major | 27);
            out.extend_from_slice(&arg.to_be_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_rfc_8949_appendix_a_examples() {
        let cases: [(Cbor, &str); 12] = [
            (Cbor::Int(0), "00"),
            (Cbor::Int(23), "17"),
            (Cbor::Int(24), "1818"),
            (Cbor::Int(1000), "1903e8"),
            (Cbor::Int(1_000_000), "1a000f4240"),
            (Cbor::Int(1_000_000_000_000), "1b000000e8d4a51000"),
            (Cbor::Int(-1), "20"),
            (Cbor::Int(-1000), "3903e7"),
            (Cbor::text("IETF"), "6449455446"),
            (Cbor::Bytes(vec![1, 2, 3, 4]), "4401020304"),
            (
                Cbor::map([("a", Cbor::Int(1)), ("b", Cbor::Array(vec![Cbor::Int(2)]))]),
                "a2616101616281 02",
            ),
            (
                Cbor::Tag(18, Box::new(Cbor::Array(vec![Cbor::Null]))),
                "d281f6",
            ),
        ];
        for (item, hex) in cases {
            assert_eq!(
                rawtonic_fs::hex(item.encode()),
                hex.replace(' ', ""),
                "{item:?}"
            );
        }
    }
}
