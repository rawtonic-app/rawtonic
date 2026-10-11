//! Photo merges on scene-linear frames: HDR, panorama and focus stack.
//! Each result is a linear DNG that develops like the source RAWs.
pub mod align;
pub mod dng;
pub mod focus_stack;
pub mod frame;
pub mod hdr;
pub mod hdr_panorama;
pub mod panorama;
pub mod planes;

use std::path::Path;

/// The develop settings a merged DNG starts with. Lens corrections are
/// recipe settings: the reference frame's (the camera's own data or the
/// matching lens profile) when the merge keeps its geometry, none when the
/// merge applied them already (panoramas).
pub fn initial_adjustments(reference: &Path, lens_applied: bool) -> serde_json::Value {
    let mut adjustments = serde_json::json!({});
    if lens_applied {
        adjustments["lensCorrectionMode"] = serde_json::json!("manual");
    } else {
        rawtonic_engine::embedded_lens::apply_default(&mut adjustments, reference);
    }
    adjustments
}

/// The `.rrdata` sidecar of a merged DNG with `adjustments`.
pub fn sidecar_json(adjustments: serde_json::Value) -> anyhow::Result<Vec<u8>> {
    let metadata = rawtonic_engine::image_processing::ImageMetadata {
        adjustments,
        ..Default::default()
    };
    Ok(serde_json::to_vec_pretty(&metadata)?)
}

/// Automatic tone settings for a merged DNG at `dng_path` (already written:
/// its base look gives the baseline), measured on a reduced copy of the
/// merge's linear `image` and merged into `adjustments` (Lightroom's "Auto
/// Settings" of its merge dialogs).
pub fn apply_auto_settings(
    adjustments: &mut serde_json::Value,
    image: &image::Rgb32FImage,
    dng_path: &Path,
) {
    let (w, h) = (image.width() as usize, image.height() as usize);
    let f = w.max(h).div_ceil(1600).max(1);
    let (sw, sh) = ((w / f).max(1), (h / f).max(1));
    let raw: &[f32] = image;
    let mut small = image::Rgb32FImage::new(sw as u32, sh as u32);
    for (i, p) in small.pixels_mut().enumerate() {
        let (sx, sy) = (i % sw, i / sw);
        let mut sum = [0.0f32; 3];
        for y in sy * f..((sy + 1) * f).min(h) {
            for x in sx * f..((sx + 1) * f).min(w) {
                let q = &raw[(y * w + x) * 3..][..3];
                sum[0] += q[0];
                sum[1] += q[1];
                sum[2] += q[2];
            }
        }
        let n = (f * f) as f32;
        *p = image::Rgb(sum.map(|v| v / n));
    }
    let small = image::DynamicImage::ImageRgb32F(small);
    let auto = rawtonic_engine::image_processing::auto_adjustments_json(
        &small,
        &dng_path.to_string_lossy(),
        true,
        None,
        None,
    );
    rawtonic_engine::auto_adjust::merge_auto_adjustments(adjustments, &auto);
}
