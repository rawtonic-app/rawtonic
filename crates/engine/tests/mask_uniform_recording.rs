//! Synthetic mask-heavy recipes recorded on main before the masks extraction.
//! No update mode: later refactors must match the recorded bytes.
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
        assert_eq!(
            actual,
            case["maskUniformHex"].as_str().unwrap(),
            "{} differs from main {}",
            case["name"],
            fixture["baseline"]
        );
        assert_eq!(
            serde_json::to_value(mask_definitions_from_json(&case["recipe"])).unwrap(),
            case["definitions"],
            "{} containers differ",
            case["name"]
        );
    }
}
