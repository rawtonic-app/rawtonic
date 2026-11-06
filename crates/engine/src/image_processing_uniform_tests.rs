//! Uniform bytes recorded on origin/main 2cd8c941 before the detail follow-ups
//! and geometry migration. No update mode: later recipe sections reuse these.
use super::get_global_adjustments_from_json;
use std::fmt::Write as _;

#[test]
fn global_adjustments_match_recorded_main_bytes() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/global_adjustments.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let uniforms = get_global_adjustments_from_json(
            &case["recipe"],
            case["isRaw"].as_bool().unwrap(),
            None,
        );
        let mut actual = String::new();
        for byte in bytemuck::bytes_of(&uniforms) {
            write!(actual, "{byte:02x}").unwrap();
        }
        assert_eq!(
            actual,
            case["uniformHex"].as_str().unwrap(),
            "{}: uniforms changed from main {}",
            case["name"],
            fixture["baseline"]
        );
    }
}
