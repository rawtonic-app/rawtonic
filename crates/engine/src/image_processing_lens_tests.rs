//! Lens conversions recorded on origin/main 3c010318, before the lens section
//! moved into the recipe crate. No update mode: later changes reuse them.
use super::geometry_params_for;

#[test]
fn geometry_params_match_recorded_main_values() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/lens_geometry_params.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let params = geometry_params_for(&case["recipe"], case["isRaw"].as_bool().unwrap());
        // Fields added later (lensfun's conventions, 2026-11-12) must keep
        // their neutral values for these recipes; the rest as recorded.
        let debug = format!("{params:?}")
            .replace("lens_fill_exact: false, ", "")
            .replace("tca_vr1: 0.0, tca_vb1: 0.0, ", "");
        assert_eq!(
            debug,
            case["paramsDebug"].as_str().unwrap(),
            "{}: geometry parameters changed from main {}",
            case["name"],
            fixture["baseline"]
        );
    }
}
