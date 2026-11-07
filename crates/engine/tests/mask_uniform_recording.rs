//! Synthetic mask-heavy recipes recorded on main before the masks extraction.
//! No update mode: later refactors must match the recorded bytes.
//!
//! The uniform bytes are recorded per platform (`<os>-<arch>`): a few
//! colour values go through the platform's maths library and differ in the
//! last bits between macOS and Linux. Each platform's bytes were recorded with
//! the unchanged baseline renderer (`baseline`, test commit 444a77f2):
//! macos-aarch64 on the Mac mini, linux-x86_64 on the Linux PC (2026-11-07).
//! A platform without a recording fails rather than passing unchecked.
use rawtonic_engine::image_processing::{
    get_all_adjustments_from_json, mask_definitions_from_json,
};
use std::fmt::Write as _;

#[test]
fn mask_adjustments_and_containers_match_recorded_main() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../src/fixtures/mask_adjustments.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let prepared =
            get_all_adjustments_from_json(&case["recipe"], case["isRaw"].as_bool().unwrap(), None);
        assert_eq!(
            prepared.uniforms.mask_count,
            case["maskCount"].as_u64().unwrap() as u32,
            "{}",
            case["name"]
        );
        let mut actual = String::new();
        for slot in prepared
            .uniforms
            .mask_adjustments
            .iter()
            .take(prepared.uniforms.mask_count as usize)
        {
            for byte in bytemuck::bytes_of(slot) {
                write!(actual, "{byte:02x}").unwrap();
            }
        }
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let recorded = case["maskUniformHex"][&platform]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "no recording for {platform}: record it with the baseline renderer {}",
                    fixture["baseline"]
                )
            });
        assert_eq!(
            actual, recorded,
            "{} differs from main {}",
            case["name"], fixture["baseline"]
        );
        assert_eq!(
            serde_json::to_value(mask_definitions_from_json(&case["recipe"])).unwrap(),
            case["definitions"],
            "{} containers differ",
            case["name"]
        );
    }
}
