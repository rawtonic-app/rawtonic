//! Check the engine's actual geometry conversions against the previous reads.
use super::{Crop, SpatialSteps, get_geometry_params_from_json};
use serde_json::{Value, json};

#[test]
fn geometry_params_keep_the_previous_reads_and_f32_conversions() {
    let fields = [
        (
            "transformDistortion",
            0.0,
            (|p: &super::GeometryParams| p.distortion) as fn(&super::GeometryParams) -> f32,
        ),
        ("transformVertical", 0.0, |p| p.vertical),
        ("transformHorizontal", 0.0, |p| p.horizontal),
        ("transformRotate", 0.0, |p| p.rotate),
        ("transformAspect", 0.0, |p| p.aspect),
        ("transformScale", 100.0, |p| p.scale),
        ("transformXOffset", 0.0, |p| p.x_offset),
        ("transformYOffset", 0.0, |p| p.y_offset),
    ];
    let mut recipes = vec![
        Value::Null,
        json!([]),
        json!("bad"),
        json!({}),
        json!({"transformDistortion":12.3,"transformVertical":-23.7,"transformHorizontal":31.5,
            "transformRotate":1.7,"transformAspect":17.3,"transformScale":113.5,
            "transformXOffset":-9.3,"transformYOffset":11.7,"sectionVisibility":{"geometry":false}}),
    ];
    for (key, _, _) in fields {
        for value in [
            Value::Null,
            json!("17"),
            json!(true),
            json!(-377.25),
            json!(999.75),
        ] {
            recipes.push(json!({key:value,"sectionVisibility":{"geometry":false}}));
        }
    }
    for recipe in recipes {
        let params = get_geometry_params_from_json(&recipe);
        for (key, default, field) in fields {
            assert_eq!(
                field(&params).to_bits(),
                (recipe[key].as_f64().unwrap_or(default) as f32).to_bits(),
                "{key}: {recipe}"
            );
        }
    }
}

#[test]
fn spatial_steps_keep_the_previous_unsigned_casts_and_crop_decode() {
    for steps in [
        json!(null),
        json!(-1),
        json!(1.0),
        json!(0),
        json!(1),
        json!(3),
        json!(256),
        json!(u64::MAX),
    ] {
        for rotation in [json!(null), json!("3"), json!(0), json!(-99.5), json!(2.7)] {
            for crop in [
                Value::Null,
                json!({"x":3,"y":7,"width":90,"height":60,"unit":"px","future":null}),
                json!({"x":3,"width":90}),
            ] {
                for flips in [
                    json!([true, false]),
                    json!([false, true]),
                    json!([null, "true"]),
                ] {
                    let recipe = json!({"orientationSteps":steps,"rotation":rotation,"crop":crop,
                        "flipHorizontal":flips[0],"flipVertical":flips[1]});
                    let actual = SpatialSteps::of(&recipe);
                    assert_eq!(
                        actual.orientation_steps,
                        recipe["orientationSteps"].as_u64().unwrap_or(0) as u8,
                        "{recipe}"
                    );
                    assert_eq!(
                        actual.rotation_degrees.to_bits(),
                        (recipe["rotation"].as_f64().unwrap_or(0.0) as f32).to_bits(),
                        "{recipe}"
                    );
                    assert_eq!(
                        actual.flip_horizontal,
                        recipe["flipHorizontal"].as_bool().unwrap_or(false),
                        "{recipe}"
                    );
                    assert_eq!(
                        actual.flip_vertical,
                        recipe["flipVertical"].as_bool().unwrap_or(false),
                        "{recipe}"
                    );
                    let old_crop: Option<Crop> =
                        serde_json::from_value(recipe["crop"].clone()).ok();
                    assert_eq!(
                        actual.crop,
                        serde_json::to_value(old_crop).unwrap_or(Value::Null),
                        "{recipe}"
                    );
                }
            }
        }
    }
}
