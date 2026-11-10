//! Uniform bytes recorded on origin/main 2cd8c941 before the detail follow-ups
//! and geometry migration. No update mode: later recipe sections reuse these.
//! Fields added since are spliced into the recorded bytes at their offset
//! (`temperature_version`, 0x101 = Temperature and Tint version 1 for these
//! keyless recipes; `vibrance_version`, `structure_version` (1 for them)
//! and the end padding),
//! so every recorded field must still match.
use super::{GlobalAdjustments, get_global_adjustments_from_json};
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
        // Fields added after the recording: Temperature's and Tint's
        // versions at their offset, Vibrance's version and the padding at
        // the end (2026-11-08), then Structure's version (2026-11-09).
        let recorded = case["uniformHex"].as_str().unwrap();
        let at = 2 * std::mem::offset_of!(GlobalAdjustments, temperature_version);
        let mut expected_added = String::new();
        for byte in bytemuck::bytes_of(&[uniforms.vibrance_version, 1, 0]) {
            write!(expected_added, "{byte:02x}").unwrap();
        }
        let expected = format!(
            "{}{}{}{}",
            &recorded[..at],
            "01010000",
            &recorded[at..],
            expected_added
        );
        assert_eq!(
            actual, expected,
            "{}: uniforms changed from main {}",
            case["name"], fixture["baseline"]
        );
    }
}
