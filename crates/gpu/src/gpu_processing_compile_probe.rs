use super::preview_tests::{
    context, get_all_adjustments_from_json, input, prepare_preview_pipeline, shape_layer_stack,
};
use super::*;
use crate::mask_generation::{MaskDefinition, PhotoGeometry, generate_layer_mask};
use serde_json::json;
use std::sync::atomic::Ordering;

/// Compiles all valid preview variants before measuring frame timing.
/// Budget roughly 5 h on RADV (PCP, 2026-11-08: 1919 serial variant compiles
/// at ~9 s each; this covers compilation before the frame-timing section).
/// On Metal, compilation runs in MTLCompilerService, so the test process can show
/// ~0 % CPU while compilation is progressing.
/// Run deliberately with:
/// `cargo test -p rawtonic-gpu -- --ignored gpu_processing::compile_probe::pipeline_compilation_probe --exact --nocapture`
#[test]
#[ignore = "native GPU timing probe; first-use pipeline compilation and the general fallback's frame cost"]
fn pipeline_compilation_probe() {
    let context = context();
    let start = Instant::now();
    let processor = GpuProcessor::new(context.clone(), 1808, 1356).unwrap();
    println!(
        "{}",
        json!({"probe":"compilation","stage":"processor_new","ms":start.elapsed().as_secs_f64()*1000.0})
    );
    let start = Instant::now();
    while processor.detail_pipeline().is_none() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    println!(
        "{}",
        json!({"probe":"compilation","stage":"detail_pipeline","ms":start.elapsed().as_secs_f64()*1000.0})
    );
    // Every preview variant, compiled one after another on this thread.
    for features in (1..PREVIEW_PIPELINE_COUNT).filter(|f| {
        (f & REFINED_NR_PREVIEW_FEATURE == 0 || f & 16 != 0)
            && (f & RELIGHT_RANGE2_FEATURE == 0 || f & RELIGHT_PREVIEW_FEATURE != 0)
            && (f & RELIGHT_RANGE3_FEATURE == 0 || f & RELIGHT_RANGE2_FEATURE != 0)
            && (f & RELIGHT_RANGE4_FEATURE == 0 || f & RELIGHT_RANGE3_FEATURE != 0)
            && (f & WHITES_V12_FEATURE == 0 || f & TONE_PREVIEW_FEATURE != 0)
            && (f & HIGHLIGHTS_V14_FEATURE == 0 || f & 2 != 0)
            && (f & BLACKS_V16_FEATURE == 0 || f & TONE_PREVIEW_FEATURE != 0)
    }) {
        let start = Instant::now();
        create_preview_pipeline(
            &context.device,
            &processor.preview_shader,
            &processor.preview_layout,
            features,
            true,
            true,
            false,
            false,
        )
        .unwrap();
        println!(
            "{}",
            json!({"probe":"compilation","stage":"preview_variant","features":features,
                "ms":start.elapsed().as_secs_f64()*1000.0})
        );
    }
    // The fallback while a variant compiles: the general shader against the
    // twenty-layer RAW-default variant, complete frames, interleaved.
    let (width, height) = (1808, 1356);
    let scale = width as f32 / 4032.0;
    let view = input(&context, width, height, 29);
    let mut profile = shape_layer_stack((328.0, 11.0, 2092.0, 745.0, 50.0));
    profile["sharpness"] = json!(40);
    profile["colorNoiseReduction"] = json!(25);
    let definitions: Vec<MaskDefinition> =
        serde_json::from_value(profile["masks"].clone()).unwrap();
    let masks: Vec<_> = definitions
        .iter()
        .map(|def| {
            Arc::new(
                generate_layer_mask(
                    def,
                    width,
                    height,
                    scale,
                    (0.0, 0.0),
                    &PhotoGeometry::default(),
                    None,
                )
                .unwrap(),
            )
        })
        .collect();
    let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
    prepare_preview_pipeline(&processor, preview_features(&adjustments));
    while processor.preview_pipelines[ALL_PREVIEW_FEATURES]
        .get()
        .is_none()
    {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    for frame in 0..36u32 {
        let mode = ["general", "fallback", "specialized"][frame as usize % 3];
        processor
            .force_general_pipeline
            .store(mode == "general", Ordering::Relaxed);
        processor
            .force_fallback_variant
            .store(mode == "fallback", Ordering::Relaxed);
        let mut profile = profile.clone();
        profile["exposure"] = json!(0.01 * frame as f64);
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        let start = Instant::now();
        processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: &masks,
                    lut: None,
                    roi: None,
                },
                true,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        context
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        if frame >= 6 {
            println!(
                "{}",
                json!({"probe":"compilation","stage":"frame","mode":mode,
                    "ms":start.elapsed().as_secs_f64()*1000.0})
            );
        }
    }
    processor
        .force_general_pipeline
        .store(false, Ordering::Relaxed);
    processor
        .force_fallback_variant
        .store(false, Ordering::Relaxed);
}
