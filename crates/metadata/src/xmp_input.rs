//! Resource limits shared by sidecar readers and the preset importer.
use anyhow::{Result, bail, ensure};
use quick_xml::{Reader, events::Event};
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

// The existing sidecar packet limit, including inline recipes and mask data.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
// A compact packet can describe millions of tiny nodes. Leave room for dense
// Lightroom brush arrays, where every dab is a separate XML element.
const MAX_NODES: u32 = 250_000;
// The DOM also stores text and inter-element whitespace. Each structural
// event can separate text at both its opening and closing boundary.
const MAX_DOM_NODES: u32 = 3 * MAX_NODES + 2;
// RDF arrays and structures are shallow. Bound ancestor walks and namespace lookups.
const MAX_DEPTH: usize = 64;
// Bound the XML parser's duplicate checks with ample room for editors that
// store a whole set of develop settings and metadata on one description.
const MAX_ATTRIBUTES: usize = 1_024;
// Bound the DOM allocation for attributes across many individually small tags.
const MAX_PACKET_ATTRIBUTES: usize = 100_000;
const MAX_NAMESPACE_DECLARATIONS: usize = 128;
// Leave room for inline adjustment data while bounding work on a single attribute.
const MAX_ATTRIBUTE_BYTES: usize = 8 * 1024 * 1024;

/// Read at most one byte past the sidecar limit, including if the file grows
/// while being read. The digest readers use the same bound as the XML readers.
pub fn read_xmp_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "XMP exceeds 16 MiB limit",
        ));
    }
    Ok(bytes)
}

pub fn read_xmp_text(path: &Path) -> io::Result<String> {
    String::from_utf8(read_xmp_bytes(path)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.utf8_error()))
}

pub(crate) fn check_packet_size(len: usize) -> Result<()> {
    ensure!(len as u64 <= MAX_BYTES, "XMP exceeds 16 MiB limit");
    Ok(())
}

/// Check bounds before building a DOM. The tokenizer borrows from the input
/// and does not resolve entities. The sidecar DOM checks namespace semantics.
pub fn check_xmp_input(text: &str) -> Result<()> {
    check_packet_size(text.len())?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_comments = true;
    let mut depth = 0;
    let mut nodes = 0;
    let mut attributes = 0;
    let mut namespace_counts = Vec::new();
    let mut namespaces = 0;
    loop {
        let event = reader.read_event()?;
        match event {
            Event::Start(ref tag) | Event::Empty(ref tag) => {
                ensure!(depth < MAX_DEPTH, "XMP nesting exceeds the depth limit");
                let mut declared = 0;
                for (index, attr) in tag.attributes().enumerate() {
                    ensure!(
                        index < MAX_ATTRIBUTES,
                        "XMP element has too many attributes"
                    );
                    attributes += 1;
                    ensure!(
                        attributes <= MAX_PACKET_ATTRIBUTES,
                        "XMP packet has too many attributes"
                    );
                    let attr = attr?;
                    ensure!(
                        attr.value.len() <= MAX_ATTRIBUTE_BYTES,
                        "XMP attribute exceeds the size limit"
                    );
                    let name = attr.key.as_ref();
                    if name == b"xmlns" || name.starts_with(b"xmlns:") {
                        declared += 1;
                    }
                }
                ensure!(
                    namespaces + declared <= MAX_NAMESPACE_DECLARATIONS,
                    "XMP has too many in-scope namespace declarations"
                );
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                    namespaces += declared;
                    namespace_counts.push(declared);
                }
            }
            Event::End(_) => {
                namespaces -= namespace_counts
                    .pop()
                    .ok_or_else(|| anyhow::anyhow!("Unexpected XMP closing tag"))?;
                depth -= 1;
                continue;
            }
            Event::DocType(_) => bail!("XMP document type declarations are unsupported"),
            Event::Eof => {
                ensure!(depth == 0, "Unclosed XMP element");
                return Ok(());
            }
            // A long text node can contain many predefined entity references,
            // e.g. quoted strings in an inline recipe. They share one DOM node.
            Event::Text(_) | Event::GeneralRef(_) => continue,
            _ => {}
        }
        nodes += 1;
        ensure!(nodes <= MAX_NODES, "XMP exceeds the node limit");
    }
}

pub(crate) fn parse_packet(text: &str) -> Result<roxmltree::Document<'_>> {
    check_xmp_input(text)?;
    Ok(roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: MAX_DOM_NODES,
            ..Default::default()
        },
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Metadata, carries_flag, merge_xmp, read_capture_time, read_xmp, write_xmp};
    use rawtonic_fs::atomic_write;
    use std::fmt::Write;

    fn assert_packet_error(body: &str, expected: &str) {
        let packet = format!(r#"<rdf:RDF xmlns:rdf="{}">{body}</rdf:RDF>"#, crate::RDF);
        for error in [
            read_xmp(&packet).unwrap_err(),
            read_capture_time(&packet).unwrap_err(),
            carries_flag(&packet).unwrap_err(),
            merge_xmp(Some(&packet), &Metadata::default(), None).unwrap_err(),
        ] {
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn rejects_packets_with_excessive_size_depth_nodes_or_attributes() {
        assert_packet_error(&"x".repeat(MAX_BYTES as usize), "16 MiB");
        let deep = format!("{}{}", "<x>".repeat(MAX_DEPTH), "</x>".repeat(MAX_DEPTH));
        assert_packet_error(&deep, "depth limit");
        assert_packet_error(&"<x/>".repeat(MAX_NODES as usize), "node limit");

        let long_attribute = format!(r#"<x a="{}"/>"#, "x".repeat(MAX_ATTRIBUTE_BYTES + 1));
        assert_packet_error(&long_attribute, "attribute exceeds");
        let mut wide = "<x".to_owned();
        for i in 0..=MAX_ATTRIBUTES {
            write!(wide, r#" a{i}="a""#).unwrap();
        }
        wide.push_str("/>");
        assert_packet_error(&wide, "element has too many attributes");
        assert_packet_error(
            &r#"<x a="a" b="b"/>"#.repeat(MAX_PACKET_ATTRIBUTES / 2),
            "packet has too many attributes",
        );
    }

    #[test]
    fn predefined_entity_references_do_not_use_up_the_node_limit() {
        let text = format!("<x>{}</x>", "&amp;".repeat(MAX_NODES as usize));
        let parsed = parse_packet(&text).unwrap();
        assert_eq!(
            parsed.root_element().text().unwrap().len(),
            MAX_NODES as usize
        );
    }

    #[test]
    fn namespace_declarations_are_bounded_per_scope() {
        let mut declarations = String::new();
        for i in 0..MAX_NAMESPACE_DECLARATIONS {
            write!(declarations, r#" xmlns:p{i}="urn:{i}""#).unwrap();
        }
        let one_scope = format!("<x{declarations}/>");
        assert!(read_xmp(&one_scope).is_ok());

        let inherited = format!(r#"<x{declarations}><y xmlns:extra="urn:extra"/></x>"#);
        assert!(
            read_xmp(&inherited)
                .unwrap_err()
                .to_string()
                .contains("namespace declarations")
        );
        let siblings = format!("<root>{one_scope}{one_scope}</root>");
        assert!(read_xmp(&siblings).is_ok());
    }

    #[test]
    fn merge_bounds_growth_when_replacing_repeated_language_items() {
        let items = r#"<rdf:li xml:lang="fr">old</rdf:li>"#.repeat(50);
        let source = format!(
            r#"<rdf:RDF xmlns:rdf="{}" xmlns:dc="{}"><rdf:Description><dc:title><rdf:Alt><rdf:li xml:lang="x-default">old</rdf:li>{items}</rdf:Alt></dc:title></rdf:Description></rdf:RDF>"#,
            crate::RDF,
            crate::DC
        );
        let metadata = Metadata {
            title: "x".repeat(400_000),
            ..Metadata::default()
        };
        assert!(
            merge_xmp(Some(&source), &metadata, None)
                .unwrap_err()
                .to_string()
                .contains("16 MiB")
        );
    }

    #[test]
    fn rejects_doctypes_and_malformed_xml() {
        // Entity expansion requires DTD processing. Even a single harmless
        // declaration fails at this boundary before any entity can be resolved.
        let dtd = r#"<!DOCTYPE a [<!ENTITY sample "text">]><a>&sample;</a>"#;
        assert!(
            read_xmp(dtd)
                .unwrap_err()
                .to_string()
                .contains("document type")
        );
        assert!(merge_xmp(Some(dtd), &Metadata::default(), None).is_err());
        assert!(
            read_xmp("<x")
                .unwrap_err()
                .to_string()
                .contains("not closed")
        );
        assert_packet_error(r#"<x xmlns:p="one" xmlns:p="two"/>"#, "duplicated");
        assert_packet_error(
            r#"<x xmlns:p="urn:same" xmlns:q="urn:same" p:a="1" q:a="2"/>"#,
            "already defined",
        );
    }

    #[test]
    fn rejects_invalid_utf8_and_oversized_files_without_replacing_them() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid.xmp");
        atomic_write(&path, b"<x>\xff</x>").unwrap();
        assert_eq!(
            read_xmp_text(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(write_xmp(&path, &Metadata::default(), None, None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"<x>\xff</x>");

        let oversized = directory.path().join("oversized.xmp");
        File::create_new(&oversized)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(
            read_xmp_text(&oversized)
                .unwrap_err()
                .to_string()
                .contains("16 MiB")
        );
        assert!(write_xmp(&oversized, &Metadata::default(), None, None).is_err());
        assert_eq!(std::fs::metadata(&oversized).unwrap().len(), MAX_BYTES + 1);
    }
}
