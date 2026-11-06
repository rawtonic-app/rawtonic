//! Independent old effect-stage reads from origin/main bd31cdd9.
//! Keep the oracle literal: registry/default/visibility changes must show up.
use super::*;
use serde_json::{Value, json};

fn old_may_apply(adjustments: &serde_json::Value) -> bool {
    let effects_visible = adjustments
        .get("sectionVisibility")
        .and_then(|v| v.get("effects"))
        .and_then(|s| s.as_bool())
        .unwrap_or(true);
    adjustments["lensBlurEnabled"].as_bool().unwrap_or(false)
        && effects_visible
        && !adjustments["lensBlurDepthMap"]
            .as_str()
            .unwrap_or("")
            .is_empty()
        && adjustments["lensBlurAmount"].as_f64().unwrap_or(50.0) as f32 > 0.0
}

fn old_apply_lens_blur<'a>(
    image: Cow<'a, DynamicImage>,
    adjustments: &serde_json::Value,
) -> Cow<'a, DynamicImage> {
    if !old_may_apply(adjustments) {
        return image;
    }
    let depth_b64 = adjustments["lensBlurDepthMap"].as_str().unwrap_or("");
    let amount = adjustments["lensBlurAmount"].as_f64().unwrap_or(50.0) as f32;

    let b64_data = match depth_b64.find(',') {
        Some(idx) => &depth_b64[idx + 1..],
        None => depth_b64,
    };
    let decoded = match BASE64.decode(b64_data) {
        Ok(b) => b,
        Err(_) => return image,
    };
    let depth_map = match image::load_from_memory(&decoded) {
        Ok(img) => img.into_luma8(),
        Err(_) => return image,
    };
    if depth_map.width() < 2 || depth_map.height() < 2 {
        return image;
    }

    let (w, h) = image.dimensions();
    if w < 8 || h < 8 {
        return image;
    }

    let max_radius = (amount / 100.0) * (w.max(h) as f32) * 0.012;
    if max_radius < 0.35 {
        return image;
    }

    let shape = adjustments["lensBlurShape"].as_str().unwrap_or("circle");
    let min_depth = adjustments["lensBlurMinDepth"].as_f64().unwrap_or(20.0) as f32 / 100.0;
    let max_depth = adjustments["lensBlurMaxDepth"].as_f64().unwrap_or(100.0) as f32 / 100.0;
    let min_fade = adjustments["lensBlurMinFade"].as_f64().unwrap_or(15.0) as f32 / 100.0;
    let max_fade = adjustments["lensBlurMaxFade"].as_f64().unwrap_or(15.0) as f32 / 100.0;
    let diffusion =
        (adjustments["lensBlurDiffusion"].as_f64().unwrap_or(0.0) as f32 / 100.0).clamp(0.0, 1.0);

    let start = std::time::Instant::now();
    let src = image.as_ref().to_rgb32f();

    let coc = build_coc_field(
        &src, &depth_map, min_depth, max_depth, min_fade, max_fade, max_radius,
    );

    let blurred_px = coc.par_iter().filter(|c| c.abs() > 0.4).count();
    if blurred_px * 400 < coc.len() && diffusion <= 0.0 {
        return image;
    }

    let out = render_depth_of_field(&src, &coc, max_radius, shape, diffusion);

    log::info!(
        "lens blur ({}x{}, r_max {:.1}px, diffusion {:.2}) took {:.2?}",
        w,
        h,
        max_radius,
        diffusion,
        start.elapsed()
    );

    Cow::Owned(DynamicImage::ImageRgb32F(out))
}

#[test]
fn lens_blur_stage_matches_old_reads_per_field() {
    let source = DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(64, 48, |x, y| {
        image::Rgb([
            ((x * 17 + y * 31) % 101) as f32 / 100.0,
            x as f32 / 64.0,
            y as f32 / 48.0,
        ])
    }));
    let depth = DynamicImage::ImageLuma8(image::ImageBuffer::from_fn(8, 8, |x, y| {
        image::Luma([((x * 32 + y * 5) % 256) as u8])
    }));
    let mut encoded = std::io::Cursor::new(Vec::new());
    depth
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let data = format!(
        "data:image/png;base64,{}",
        BASE64.encode(encoded.into_inner())
    );
    let base = json!({"lensBlurEnabled":true,"lensBlurDepthMap":data,"lensBlurAmount":90,"lensBlurDiffusion":21,"lensBlurMinDepth":30,"lensBlurMaxDepth":65,"lensBlurMinFade":15,"lensBlurMaxFade":18,"lensBlurShape":"hexagon"});
    for key in [
        "lensBlurEnabled",
        "lensBlurDepthMap",
        "lensBlurAmount",
        "lensBlurDiffusion",
        "lensBlurMinDepth",
        "lensBlurMaxDepth",
        "lensBlurMinFade",
        "lensBlurMaxFade",
        "lensBlurShape",
    ] {
        for value in [
            Value::Null,
            json!(false),
            json!(true),
            json!(""),
            json!("unknown"),
            json!("circle"),
            json!("ring"),
            json!("hexagon"),
            json!("octagon"),
            json!(-1),
            json!(0),
            json!(42.75),
            json!(110),
        ] {
            for visible in [true, false] {
                let mut recipe = base.clone();
                recipe[key] = value.clone();
                recipe["sectionVisibility"] = json!({"effects":visible});
                assert_eq!(may_apply(&recipe), old_may_apply(&recipe), "{key}: {value}");
                let actual = apply_lens_blur(Cow::Borrowed(&source), &recipe).to_rgb32f();
                let expected = old_apply_lens_blur(Cow::Borrowed(&source), &recipe).to_rgb32f();
                assert_eq!(
                    actual.as_raw(),
                    expected.as_raw(),
                    "{key}: {value}, effects visible: {visible}"
                );
            }
        }
    }
    let mut partial = base;
    partial
        .as_object_mut()
        .unwrap()
        .retain(|k, _| k == "lensBlurEnabled" || k == "lensBlurDepthMap");
    assert_eq!(
        apply_lens_blur(Cow::Borrowed(&source), &partial)
            .to_rgb32f()
            .as_raw(),
        old_apply_lens_blur(Cow::Borrowed(&source), &partial)
            .to_rgb32f()
            .as_raw()
    );
}
