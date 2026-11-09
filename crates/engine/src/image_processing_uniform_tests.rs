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
        // Fields added after the recording sit after its bytes: Vibrance's
        // version and padding (2026-11-08).
        let recorded = case["uniformHex"].as_str().unwrap();
        let (before, added) = actual.split_at(recorded.len().min(actual.len()));
        assert_eq!(
            before, recorded,
            "{}: uniforms changed from main {}",
            case["name"], fixture["baseline"]
        );
        let mut expected_added = String::new();
        for byte in bytemuck::bytes_of(&[uniforms.vibrance_version, 0, 0, 0]) {
            write!(expected_added, "{byte:02x}").unwrap();
        }
        assert_eq!(added, expected_added, "{}: added fields", case["name"]);
    }
}
