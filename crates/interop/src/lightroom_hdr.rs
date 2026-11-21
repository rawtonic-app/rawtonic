//! Lightroom's and Camera Raw's HDR edit settings, kept without loss.
//!
//! Rawtonic renders every photo as SDR. A photo edited in HDR mode in
//! Lightroom carries `crs:HDREditMode="1"`, the headroom `crs:HDRMaxValue`
//! and the sliders of the SDR rendition (`crs:SDRBrightness`,
//! `crs:SDRContrast`, `crs:SDRClarity`, `crs:SDRHighlights`,
//! `crs:SDRShadows`, `crs:SDRWhites`, `crs:SDRBlend`; names from Adobe's
//! public XMP documentation and exiftool's Camera Raw tag table). They are
//! not edit tools here, but a Lightroom user must not lose them: the import
//! keeps every `crs:` setting whose name starts with `HDR` or `SDR` (so
//! later additions of the same family survive too) beside the recipe as
//! `lightroomHdr`, and the XMP sidecar writer
//! (`rawtonic_metadata::merge_xmp`) writes them back as `crs:` attributes.
use std::collections::BTreeMap;

/// The recipe key (in `adjustments`) that holds the settings, name → value.
pub use rawtonic_metadata::LIGHTROOM_HDR_KEY;

/// The `HDR…`/`SDR…` settings of a Lightroom XMP packet (name → value):
/// properties of the Camera Raw namespace under any prefix, as attributes or
/// as simple elements, of the packet's own descriptions (not of the
/// descriptions nested in masks or other lists); the first one found wins.
/// A fragment without its root (the XMP Rawtonic builds from a catalog's
/// settings) is read inside one that declares the usual prefixes.
pub fn hdr_settings(xmp: &str) -> BTreeMap<String, String> {
    let wrapped;
    let doc = match roxmltree::Document::parse(xmp) {
        Ok(doc) => doc,
        Err(_) => {
            wrapped = format!("<x xmlns:rdf=\"{RDF}\" xmlns:crs=\"{CRS}\">{xmp}</x>");
            match roxmltree::Document::parse(&wrapped) {
                Ok(doc) => doc,
                Err(_) => return BTreeMap::new(),
            }
        }
    };
    let mut found = BTreeMap::new();
    for node in doc
        .descendants()
        .filter(|n| n.has_tag_name((RDF, "Description")))
    {
        if node
            .ancestors()
            .skip(1)
            .any(|a| a.has_tag_name((RDF, "Description")))
        {
            continue;
        }
        for attr in node.attributes() {
            if attr.namespace() == Some(CRS)
                && rawtonic_metadata::is_lightroom_hdr_name(attr.name())
            {
                found
                    .entry(attr.name().to_string())
                    .or_insert_with(|| attr.value().to_string());
            }
        }
        for child in node.children().filter(|c| c.is_element()) {
            let tag = child.tag_name();
            if tag.namespace() == Some(CRS)
                && rawtonic_metadata::is_lightroom_hdr_name(tag.name())
                && !child.children().any(|c| c.is_element())
            {
                found
                    .entry(tag.name().to_string())
                    .or_insert_with(|| child.text().unwrap_or_default().to_string());
            }
        }
    }
    found
}

const RDF: &str = rawtonic_metadata::RDF;
const CRS: &str = rawtonic_metadata::CRS;

/// Whether the settings describe an edit made in HDR mode
/// (`crs:HDREditMode` = 1).
pub fn is_hdr_edit(settings: &BTreeMap<String, String>) -> bool {
    settings
        .get("HDREditMode")
        .is_some_and(|v| v.trim().trim_start_matches('+').parse::<f64>() == Ok(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XMP: &str = r#"<rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+0.50" crs:HDREditMode="1" crs:HDRMaxValue="4.000000" crs:SDRBrightness="+12" crs:SDRContrast="-5" crs:SDRClarity="0" crs:SDRHighlights="-30" crs:SDRShadows="+20" crs:SDRWhites="0" crs:SDRBlend="0.5"/>"#;

    #[test]
    fn every_hdr_and_sdr_setting_is_found_and_nothing_else() {
        let found = hdr_settings(XMP);
        assert_eq!(found.len(), 9);
        assert_eq!(found["HDRMaxValue"], "4.000000");
        assert_eq!(found["SDRHighlights"], "-30");
        assert!(!found.contains_key("Exposure2012"));
    }

    #[test]
    fn other_prefixes_quotes_elements_and_nesting_are_read_as_xml() {
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><r:RDF xmlns:r="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><r:Description r:about="" xmlns:c="http://ns.adobe.com/camera-raw-settings/1.0/" c:HDREditMode='1'><c:SDRBlend>0.25</c:SDRBlend><c:MaskGroupBasedCorrections><r:Seq><r:li><r:Description c:HDREditMode="0" c:SDRContrast="9"/></r:li></r:Seq></c:MaskGroupBasedCorrections></r:Description></r:RDF></x:xmpmeta>"#;
        let found = hdr_settings(xmp);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found["HDREditMode"], "1");
        assert_eq!(found["SDRBlend"], "0.25");
    }

    #[test]
    fn the_notice_condition_is_hdr_edit_mode_one() {
        let mode = |v: &str| {
            is_hdr_edit(&hdr_settings(&format!(
                r#"<rdf:Description crs:HDREditMode="{v}"/>"#
            )))
        };
        assert!(mode("1"));
        assert!(!mode("0"));
        assert!(!is_hdr_edit(&hdr_settings(
            XMP.replace("HDREditMode=\"1\"", "").as_str()
        )));
        assert!(!is_hdr_edit(&BTreeMap::new()));
    }
}
