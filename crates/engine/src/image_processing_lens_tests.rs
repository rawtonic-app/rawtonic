//! Lens conversions recorded on origin/main 3c010318, before the lens section
//! moved into the recipe crate. No update mode: later changes reuse them.
use super::geometry_params_for;

#[test]
fn geometry_params_match_recorded_main_values() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/lens_geometry_params.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let params = geometry_params_for(&case["recipe"], case["isRaw"].as_bool().unwrap());
        assert_eq!(
            format!("{params:?}"),
            case["paramsDebug"].as_str().unwrap(),
            "{}: geometry parameters changed from main {}",
            case["name"],
            fixture["baseline"]
        );
    }
}
