//! TPX5 cost ceiling (not shipped): the export's GPU colour NR at full
//! resolution = render(C50) - render(C0), fresh source upload each time.
//! tpx5cost RAW REPEATS
use std::sync::{Arc, Mutex};
use rawtonic_engine::image_loader::{self, BaseImageOptions};
use rawtonic_engine::image_processing::get_all_adjustments_from_json;
use rawtonic_gpu::gpu_processing::{
    GpuContext, GpuProcessorSlot, ReadbackShader, RenderOutputPrecision, RenderRequest,
    RenderSource, process_and_get_dynamic_image_inner,
};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let raw = &args[1];
    let reps: usize = args[2].parse().unwrap();
    let base: serde_json::Value = serde_json::from_str(include_str!("../../engine/tests/fixtures/new-edit-recipe.raw.json")).unwrap();
    let bytes = std::fs::read(raw).unwrap();
    let context = GpuContext::headless().unwrap();
    let processor = GpuProcessorSlot::background(None);
    let cache = Mutex::new(None);
    let mut recipe = base.clone();
    for key in ["lensDistortionEnabled", "lensTcaEnabled", "lensVignetteEnabled"] { recipe[key] = false.into(); }
    recipe["sharpness"] = 0.into(); recipe["lumaNoiseReduction"] = 0.into();
    let (img, measures) = image_loader::load_base_image_measured(&bytes, raw, false, &BaseImageOptions::default().for_recipe(&recipe, raw), None).unwrap();
    let full = Arc::new(img);
    let mut hash = 1000u64;
    for rep in 0..reps {
        for c in [0, 50] {
            let mut r = recipe.clone();
            r["colorNoiseReduction"] = c.into();
            let mut a = get_all_adjustments_from_json(&r, true, None);
            rawtonic_engine::baseline_exposure::apply_base_look(&mut a.global, raw, measures.as_ref(), None);
            a.global.show_clipping = 0;
            hash += 1;
            let t = std::time::Instant::now();
            let out = process_and_get_dynamic_image_inner(&context, &processor, &cache, RenderSource::Image { image: &full, transform_hash: hash }, RenderRequest { adjustments: a, mask_bitmaps: &[], lut: None, roi: None }, "tpx5", RenderOutputPrecision::EightBit, false, None, None, ReadbackShader::Export).unwrap();
            eprintln!("TIME rep {rep} color {c} {:?} {:?}", t.elapsed(), (out.width(), out.height()));
        }
    }
}
