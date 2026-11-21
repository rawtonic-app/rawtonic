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

/// The `HDR…`/`SDR…` settings of a Lightroom XMP packet (name → value).
pub fn hdr_settings(xmp: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let mut rest = xmp;
    while let Some(at) = rest.find("crs:") {
        rest = &rest[at + 4..];
        let name_len = rest
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric())
            .count();
        let (name, after) = rest.split_at(name_len);
        if !rawtonic_metadata::is_lightroom_hdr_name(name) {
            continue;
        }
        let Some(value) = after.strip_prefix("=\"") else {
            continue;
        };
        if let Some(end) = value.find('"') {
            found.insert(name.to_string(), unescape(&value[..end]));
        }
    }
    found
}

fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

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
