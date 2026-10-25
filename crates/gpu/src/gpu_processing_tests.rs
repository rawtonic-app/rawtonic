use super::*;
use crate::mask_generation::{MaskBitmap, MaskDefinition, PhotoGeometry, generate_layer_mask};
use serde_json::json;
use std::sync::atomic::Ordering;

/// Recipes in these tests describe exactly the controls they set: RAW
/// detail defaults (Sharpening 40, Color 25, Luminance 15 for unset keys) are pinned to 0
/// unless a profile sets them.
fn get_all_adjustments_from_json(
    profile: &serde_json::Value,
    is_raw: bool,
    tonemapper_override: Option<u32>,
) -> crate::image_processing::PreparedAdjustments {
    let mut profile = profile.clone();
    if let Some(object) = profile.as_object_mut() {
        object.entry("sharpness").or_insert(json!(0));
        object.entry("colorNoiseReduction").or_insert(json!(0));
        object.entry("lumaNoiseReduction").or_insert(json!(0));
    }
    crate::image_processing::get_all_adjustments_from_json(&profile, is_raw, tonemapper_override)
}

#[test]
fn direct_float_upload_preserves_all_channels_and_alpha() {
    for width in [1, 7, 4097] {
        let rgb = DynamicImage::ImageRgb32F(ImageBuffer::from_fn(width, 3, |x, y| {
            image::Rgb([x as f32 / 255.0, y as f32 - 1.0, 0.00001])
        }));
        let rgba = DynamicImage::ImageRgba32F(ImageBuffer::from_fn(width, 3, |x, y| {
            image::Rgba([
                x as f32 / 255.0,
                y as f32 - 1.0,
                0.00001,
                (x % 5) as f32 / 4.0,
            ])
        }));
        let rgb8 = DynamicImage::ImageRgb8(ImageBuffer::from_fn(width, 3, |x, y| {
            image::Rgb([x as u8, (y * 80) as u8, 255])
        }));
        for image in [rgb, rgba, rgb8] {
            let expected: Vec<_> = image
                .to_rgba32f()
                .into_raw()
                .into_iter()
                .map(|value| f16::from_f32(value).to_bits())
                .collect();
            let actual: Vec<_> = to_rgba_f16(&image).into_iter().map(f16::to_bits).collect();
            assert_eq!(actual, expected);
        }
    }
}

#[test]
fn banded_input_upload_matches_the_full_conversion() {
    let context = context();
    let mut checked = 0;
    for (width, height) in [(1, 1), (7, 5), (300, 257), (65, 600)] {
        let value = |x: u32, y: u32, channel: u32| {
            ((x * 31 + y * 17 + channel * 7) % 509) as f32 / 101.0 - 0.5
        };
        let images = [
            DynamicImage::ImageRgb32F(ImageBuffer::from_fn(width, height, |x, y| {
                image::Rgb([value(x, y, 0), value(x, y, 1), value(x, y, 2)])
            })),
            DynamicImage::ImageRgba32F(ImageBuffer::from_fn(width, height, |x, y| {
                image::Rgba([
                    value(x, y, 0),
                    value(x, y, 1),
                    value(x, y, 2),
                    value(x, y, 3),
                ])
            })),
            DynamicImage::ImageRgb8(ImageBuffer::from_fn(width, height, |x, y| {
                image::Rgb([(x * 3) as u8, (y * 5) as u8, (x ^ y) as u8])
            })),
            DynamicImage::ImageRgba16(ImageBuffer::from_fn(width, height, |x, y| {
                image::Rgba([(x * 997) as u16, (y * 641) as u16, 40_000, (x * y) as u16])
            })),
        ];
        for image in images {
            let expected = to_rgba_f16(&image);
            for band_rows in [1, 3, 256, 1000] {
                let texture =
                    create_input_texture(&context.device, &context.queue, &image, band_rows)
                        .unwrap();
                let texels = read_texture_data_roi(
                    &context.device,
                    &context.queue,
                    &texture,
                    wgpu::Origin3d::ZERO,
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    8,
                )
                .unwrap();
                assert_eq!(
                    texels,
                    bytemuck::cast_slice::<f16, u8>(&expected),
                    "{width}x{height} {:?} in {band_rows}-row bands",
                    image.color()
                );
                checked += 1;
            }
        }
    }
    println!("{checked} banded input uploads match the full conversion");
}

/// The tests that occupy every thread of rayon's global pool run one at a
/// time: two at once would each wait for threads the other holds.
static GLOBAL_POOL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Thumbnail batches run on rayon's global pool and wait there for the GPU
/// processor lock; a caller holding that lock must not need the pool for its
/// input upload (an Edit-all write in the multi view froze every render).
#[test]
fn input_uploads_do_not_wait_for_a_busy_rayon_pool() {
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;
    let _pool = GLOBAL_POOL.lock().unwrap_or_else(|e| e.into_inner());
    let context = context();
    let image = DynamicImage::ImageRgb32F(ImageBuffer::from_fn(64, 600, |x, y| {
        image::Rgb([x as f32 / 64.0, y as f32 / 600.0, 0.5])
    }));
    let lock = Arc::new(Mutex::new(()));
    let guard = lock.lock().unwrap();
    // Every global pool thread waits for the lock.
    let threads = rayon::current_num_threads();
    let (started_tx, started_rx) = mpsc::channel();
    for _ in 0..threads {
        let (lock, started) = (lock.clone(), started_tx.clone());
        rayon::spawn(move || {
            started.send(()).unwrap();
            drop(lock.lock().unwrap());
        });
    }
    for _ in 0..threads {
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    // The lock holder uploads (outside rayon, as the preview worker does).
    let (done_tx, done_rx) = mpsc::channel();
    let uploader = context.clone();
    let source = image.clone();
    std::thread::spawn(move || {
        let texture = create_input_texture(&uploader.device, &uploader.queue, &source, 16);
        done_tx.send(texture.is_ok()).unwrap();
    });
    let outside = done_rx.recv_timeout(Duration::from_secs(20));
    // On a pool thread that holds the lock while the others wait for it.
    let (done_tx, done_rx) = mpsc::channel();
    let uploader = context.clone();
    rayon::spawn(move || {
        let texture = create_input_texture(&uploader.device, &uploader.queue, &image, 16);
        done_tx.send(texture.is_ok()).unwrap();
    });
    let inside = done_rx.recv_timeout(Duration::from_secs(20));
    drop(guard);
    assert_eq!(outside, Ok(true), "upload outside rayon");
    // The pool is busy until the lock is released; then it uploads too.
    assert!(
        inside == Ok(true) || done_rx.recv_timeout(Duration::from_secs(20)) == Ok(true),
        "upload on a pool thread"
    );
}

/// The colour noise pyramid is built under the GPU processor lock with
/// rayon: a caller outside rayon holding the lock must not need the global
/// pool while thumbnail batches wait there for it (Edit all selected's live
/// cells froze every render after the recipes were written).
#[test]
fn parallel_work_under_the_processor_lock_does_not_wait_for_a_busy_pool() {
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;
    let _pool = GLOBAL_POOL.lock().unwrap_or_else(|e| e.into_inner());
    let lock = Arc::new(Mutex::new(()));
    let guard = lock.lock().unwrap();
    let threads = rayon::current_num_threads();
    let (started_tx, started_rx) = mpsc::channel();
    for _ in 0..threads {
        let (lock, started) = (lock.clone(), started_tx.clone());
        rayon::spawn(move || {
            started.send(()).unwrap();
            drop(lock.lock().unwrap());
        });
    }
    for _ in 0..threads {
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let sum: u64 = off_global_pool(|| (0..100_000u64).into_par_iter().map(|v| v % 7).sum());
        done_tx.send(sum).unwrap();
    });
    let done = done_rx.recv_timeout(Duration::from_secs(20));
    drop(guard);
    assert_eq!(done, Ok((0..100_000u64).map(|v| v % 7).sum()));
}

fn all_sub_mask() -> serde_json::Value {
    json!({"id":"all","type":"all","visible":true,"mode":"additive","parameters":{}})
}

fn local_mask(id: &str, adjustments: serde_json::Value) -> serde_json::Value {
    json!({"id":id,"name":id,"visible":true,"invert":false,"opacity":100,
        "subMasks":[all_sub_mask()],"adjustments":adjustments})
}

fn generated_profile_masks(
    profile: &serde_json::Value,
    width: u32,
    height: u32,
) -> Vec<Arc<MaskBitmap>> {
    let definitions: Vec<MaskDefinition> = profile
        .get("masks")
        .and_then(|masks| serde_json::from_value(masks.clone()).ok())
        .unwrap_or_default();
    definitions
        .iter()
        .filter_map(|definition| {
            generate_layer_mask(
                definition,
                width,
                height,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::default(),
                None,
            )
            .map(Arc::new)
        })
        .take(MAX_MASKS)
        .collect()
}

#[test]
fn mask_adjustments_skip_empty_layers_without_shifting_bitmaps() {
    let mut empty = local_mask("empty", json!({"exposure":-4}));
    empty["subMasks"] = json!([]);
    let mut inverted_empty = empty.clone();
    inverted_empty["invert"] = json!(true);
    let mut hidden = local_mask("hidden", json!({"exposure":-3}));
    hidden["visible"] = json!(false);
    let mut first = local_mask("first", json!({"exposure":0.25}));
    first["opacity"] = json!(25);
    // Nonempty definitions still produce a bitmap even when a component is
    // hidden, unsupported or invalid. Inversion can turn that black mask white.
    let mut hidden_component = local_mask("hidden-component", json!({"exposure":0.5}));
    hidden_component["subMasks"][0]["visible"] = json!(false);
    let mut unsupported = local_mask("unsupported", json!({"exposure":0.75}));
    unsupported["subMasks"][0]["type"] = json!("future-mask-type");
    unsupported["invert"] = json!(true);
    let mut invalid_parameters = local_mask("invalid-parameters", json!({"exposure":1.0}));
    invalid_parameters["subMasks"][0]["type"] = json!("brush");
    invalid_parameters["subMasks"][0]["parameters"] = json!({"lines":"invalid"});
    let mut mixed = local_mask("mixed", json!({"exposure":1.25}));
    mixed["subMasks"] = json!([
        all_sub_mask(),
        hidden_component["subMasks"][0],
        unsupported["subMasks"][0]
    ]);
    let active = [
        first,
        hidden_component,
        unsupported,
        invalid_parameters,
        mixed,
    ];
    let profile = json!({"masks":[empty, hidden, active[0], empty, active[1],
        inverted_empty, active[2], active[3], active[4], empty]});
    let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
    let bitmaps = generated_profile_masks(&profile, 3, 2);
    assert_eq!(adjustments.mask_count as usize, bitmaps.len());
    assert_eq!(adjustments.mask_count, 5);
    for (index, (definition, expected_pixel)) in active.iter().zip([63, 0, 255, 0, 255]).enumerate()
    {
        let reference =
            get_all_adjustments_from_json(&json!({"masks":[definition]}), true, Some(1));
        assert_eq!(
            adjustments.mask_adjustments[index].exposure,
            reference.mask_adjustments[0].exposure
        );
        assert!(
            bitmaps[index]
                .pixels()
                .all(|pixel| pixel[0] == expected_pixel)
        );
    }
}

#[test]
fn mask_adjustments_apply_the_limit_after_bitmap_selection() {
    let mut definitions = Vec::new();
    for index in 0..MAX_MASKS + 3 {
        let mut empty = local_mask("empty", json!({"exposure":-5}));
        empty["subMasks"] = json!([]);
        let mut hidden = local_mask("hidden", json!({"exposure":-4}));
        hidden["visible"] = json!(false);
        let mut active = local_mask(
            &format!("active-{index}"),
            json!({"exposure": index as f32 / 10.0}),
        );
        active["opacity"] = json!(index + 1);
        definitions.extend([empty, hidden, active]);
    }
    let profile = json!({"masks":definitions});
    let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
    let bitmaps = generated_profile_masks(&profile, 3, 2);
    assert_eq!(adjustments.mask_count as usize, MAX_MASKS);
    assert_eq!(bitmaps.len(), MAX_MASKS);
    for (index, bitmap) in bitmaps.iter().enumerate() {
        let reference = get_all_adjustments_from_json(
            &local_profile(json!({"exposure":index as f32 / 10.0})),
            true,
            Some(1),
        );
        assert_eq!(
            adjustments.mask_adjustments[index].exposure,
            reference.mask_adjustments[0].exposure
        );
        let expected_pixel = (255.0 * (index + 1) as f32 / 100.0) as u8;
        assert!(bitmap.pixels().all(|pixel| pixel[0] == expected_pixel));
    }
}

#[test]
fn malformed_mask_definitions_keep_adjustments_and_bitmaps_consistent() {
    let valid = local_mask("valid", json!({"exposure":1}));
    let mut missing_field = valid.clone();
    missing_field.as_object_mut().unwrap().remove("id");
    let mut invalid_submask = valid.clone();
    invalid_submask["subMasks"][0]["mode"] = json!("unknown-mode");
    // The existing strict array parser rejects the whole list if any definition
    // is malformed. Both consumers must keep that same policy.
    for masks in [
        json!(null),
        json!({}),
        json!([valid, missing_field, valid]),
        json!([valid, invalid_submask, valid]),
    ] {
        let profile = json!({"masks":masks});
        assert_eq!(
            get_all_adjustments_from_json(&profile, true, Some(1)).mask_count,
            0
        );
        assert!(generated_profile_masks(&profile, 3, 2).is_empty());
    }
}

fn local_profile(adjustments: serde_json::Value) -> serde_json::Value {
    json!({"masks":[local_mask("local", adjustments)]})
}

fn custom_curve() -> serde_json::Value {
    json!({"luma":[{"x":0,"y":0},{"x":120,"y":140},{"x":255,"y":255}]})
}

#[test]
fn preview_features_preserve_global_and_local_effects() {
    for expected in 0..PREVIEW_PIPELINE_COUNT {
        // Refinements matter only with noise reduction.
        if expected & REFINED_NR_PREVIEW_FEATURE != 0 && expected & 16 == 0 {
            continue;
        }
        let mut effects = json!({});
        if expected & 1 != 0 {
            effects["sharpness"] = json!(-25);
        }
        if expected & 2 != 0 {
            effects["highlights"] = json!(-40);
        }
        if expected & 4 != 0 {
            effects["curves"] = custom_curve();
        }
        if expected & 16 != 0 {
            effects["colorNoiseReduction"] = json!(30);
        }
        if expected & TONE_PREVIEW_FEATURE != 0 {
            effects["whites"] = json!(-20);
        }
        let mut profile = if expected & 8 != 0 {
            local_profile(effects)
        } else {
            effects
        };
        // Relight and the noise refinements are photo-wide only.
        if expected & RELIGHT_PREVIEW_FEATURE != 0 {
            profile["relightLights"] = json!([{"intensity":65}]);
        }
        if expected & REFINED_NR_PREVIEW_FEATURE != 0 {
            profile["lumaNoiseDetail"] = json!(70);
        }
        let mut adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        assert_eq!(preview_features(&adjustments), expected, "{profile}");
        if expected & 8 != 0 {
            // Stale entries beyond mask_count must not affect the current image.
            adjustments.mask_count = 0;
            assert_eq!(
                preview_features(&adjustments),
                expected & RELIGHT_PREVIEW_FEATURE
            );
            continue;
        }
        if expected & REFINED_NR_PREVIEW_FEATURE != 0 {
            // Without noise reduction the refinements change nothing.
            adjustments.global.color_noise_reduction = 0.0;
            assert_eq!(
                preview_features(&adjustments),
                expected & !(16 | REFINED_NR_PREVIEW_FEATURE)
            );
        }
    }
    // Opposing layers do not cancel everywhere: their bitmap coverage differs.
    let mut profile = local_profile(json!({"sharpness":-50,"lumaNoiseReduction":20}));
    profile["sharpness"] = json!(50);
    profile["highlights"] = json!(-10);
    profile["curves"] = custom_curve();
    let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
    assert_eq!(preview_features(&adjustments), 31);
}

#[test]
fn preview_curve_bypass_matches_shader_identity_contract() {
    let mut adjustments = get_all_adjustments_from_json(&local_profile(json!({})), true, Some(1));
    assert_eq!(preview_features(&adjustments), 8);
    // Zero/one point curves are disabled in the shader, regardless of their data.
    adjustments.mask_adjustments[0].luma_curve_count = 1;
    adjustments.mask_adjustments[0].luma_curve[0].y = 42.0;
    assert_eq!(preview_features(&adjustments), 8);
    adjustments.mask_adjustments[0].luma_curve_count = 2;
    assert_eq!(preview_features(&adjustments), 12);
    // Do not treat approximate endpoints as identity: luma interpolation uses them.
    adjustments.mask_count = 0;
    adjustments.global.luma_curve[1].y = 254.99;
    assert_eq!(preview_features(&adjustments), 4);
}

#[test]
fn blur_dependencies_include_local_adjustments_and_indirect_effects() {
    let mut adjustments = AllAdjustments::default();
    adjustments.global.exposure = 2.0;
    adjustments.global.temperature = 0.5;
    assert_eq!(required_blurs(&adjustments), [false; 5]);
    adjustments.global.centré = 0.5;
    assert_eq!(
        required_blurs(&adjustments),
        [false, false, true, false, false]
    );
    adjustments.mask_count = 2;
    adjustments.mask_adjustments[0].sharpness = -0.5;
    adjustments.mask_adjustments[1].glow_amount = 0.5;
    assert_eq!(
        required_blurs(&adjustments),
        [true, true, true, true, false]
    );
    adjustments.mask_count = 0;
    assert_eq!(
        required_blurs(&adjustments),
        [false, false, true, false, false]
    );
    // Luminance noise reduction: its bands' blurs.
    adjustments.global.centré = 0.0;
    adjustments.global.luma_noise_reduction = 0.15;
    assert_eq!(
        required_blurs(&adjustments),
        [true, true, false, false, true]
    );
}

#[test]
fn curve_upload_counts_cannot_exceed_the_uniform_arrays() {
    let points: Vec<_> = (0..25).map(|i| json!({"x":i * 10,"y":i * 10})).collect();
    let curves = json!({"luma":points,"red":points,"green":points,"blue":points});
    let profile = json!({"curves":curves,"masks":[{"id":"local","name":"Local","visible":true,"invert":false,"opacity":100,"subMasks": [all_sub_mask()],"adjustments":{"curves":curves}}]});
    let a = get_all_adjustments_from_json(&profile, true, None);
    assert_eq!(a.mask_count, 1);
    assert_eq!(
        [
            a.global.luma_curve_count,
            a.global.red_curve_count,
            a.global.green_curve_count,
            a.global.blue_curve_count
        ],
        [16; 4]
    );
    let m = a.mask_adjustments[0];
    assert_eq!(
        [
            m.luma_curve_count,
            m.red_curve_count,
            m.green_curve_count,
            m.blue_curve_count
        ],
        [16; 4]
    );
}

#[test]
#[ignore = "requires a native GPU"]
fn export_processors_compile_no_display_variant_ahead() {
    let context = context();
    let started = |processor: &GpuProcessor| {
        (0..PREVIEW_PIPELINE_COUNT)
            .filter(|&features| processor.preview_compilation[features].get().is_some())
            .collect::<Vec<_>>()
    };
    let gate_open = |processor: &GpuProcessor| *processor.start_gate.open.lock().unwrap();

    let develop = create_processor(&ProcessorSlot::new(None), &context, 64, 64).unwrap();
    assert!(started(&develop).contains(&RAW_DEFAULT_PREVIEW_FEATURES));

    let exports = create_processor(&ProcessorSlot::background(None), &context, 64, 64).unwrap();
    assert_eq!(started(&exports), Vec::<usize>::new());
    // No display variant to wait for: the general pipelines compile at once.
    assert!(gate_open(&exports));
    exports.main_pipeline.wait().unwrap();
}

#[test]
#[ignore = "requires a native GPU; invalid WGSL must fail instead of rendering black"]
fn invalid_shader_source_fails_processor_construction() {
    let context = context();
    let broken = format!(
        "{}\nconst BROKEN_CONSTANT: f32 = ;\n",
        include_str!("shaders/shader.wgsl")
    );
    let error = match GpuProcessor::with_shader_source(context.clone(), 64, 64, &broken, true) {
        Ok(_) => panic!("an invalid image-processing shader must not build a processor"),
        Err(error) => error,
    };
    assert!(error.contains("Image Processing Shader"), "{error}");

    // A failed pipeline is reported too, and the scopes do not swallow later work.
    let module = create_shader(
        &context.device,
        "Blur Shader",
        include_str!("shaders/blur.wgsl"),
    )
    .unwrap();
    let error = create_compute_pipeline(
        &context.device,
        &wgpu::ComputePipelineDescriptor {
            label: Some("Missing entry point"),
            layout: None,
            module: &module,
            entry_point: Some("does_not_exist"),
            compilation_options: Default::default(),
            cache: None,
        },
    )
    .unwrap_err();
    assert!(error.contains("Missing entry point"), "{error}");
    GpuProcessor::new(context, 64, 64).unwrap();
}

#[test]
#[ignore = "requires a native GPU; a failed export variant must use the general pipeline"]
fn failed_export_variant_uses_the_general_pipeline() {
    let context = context();
    let processor = GpuProcessor::new(context, 64, 64).unwrap();
    let adjustments = get_all_adjustments_from_json(&json!({}), false, None);
    let features = export_features(&adjustments);
    assert_eq!(
        features & 0b1_0011,
        0,
        "the neutral recipe uses a specialized variant"
    );
    processor.export_pipelines[features].set(None).unwrap();
    processor.high_precision_export_pipelines[features]
        .set(None)
        .unwrap();
    assert!(std::ptr::eq(
        processor
            .export_pipeline(&adjustments, MainOutput::EightBit)
            .unwrap(),
        processor.main_pipeline.wait().unwrap()
    ));
    assert!(std::ptr::eq(
        processor
            .export_pipeline(&adjustments, MainOutput::SixteenBit)
            .unwrap(),
        processor.high_precision_pipeline.wait().unwrap()
    ));
}

#[test]
#[ignore = "requires a native GPU; compares masked pixels with and without skipped layers"]
fn empty_mask_layers_preserve_adjustment_pixels() {
    let context = context();
    let (width, height) = (97, 65);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 3);
    let mut empty = local_mask("empty", json!({"exposure":-4, "temperature":100}));
    empty["subMasks"] = json!([]);
    let mut hidden = local_mask("hidden", json!({"exposure":4}));
    hidden["visible"] = json!(false);
    let mut first = local_mask("first", json!({"exposure":1.25, "temperature":20}));
    first["subMasks"][0]["type"] = json!("linear");
    first["subMasks"][0]["parameters"] = json!({"startX":0,"startY":0,
        "endX":96,"endY":64,"range":60});
    let mut last = local_mask("last", json!({"exposure":-0.5,"curves":custom_curve()}));
    last["subMasks"][0]["type"] = json!("radial");
    last["subMasks"][0]["parameters"] = json!({"centerX":55,"centerY":25,
        "radiusX":27,"radiusY":19,"rotation":20,"feather":0.5});
    let mut black = local_mask("black", json!({"exposure":-3}));
    black["subMasks"][0]["visible"] = json!(false);
    let mut inverted = black.clone();
    inverted["invert"] = json!(true);
    inverted["adjustments"] = json!({"saturation":-15});
    let clean = json!({"masks":[first, black, inverted, last]});
    let decorated = json!({"masks":[empty, hidden, first, empty, black,
        empty, inverted, last, empty]});
    let mut limit_clean = Vec::new();
    let mut limit_decorated = Vec::new();
    for index in 0..MAX_MASKS + 3 {
        let mut definition = local_mask(
            &format!("layer-{index}"),
            json!({"exposure":if index.is_multiple_of(2) {0.04} else {-0.03},
                "temperature":index as f32 - 16.0}),
        );
        definition["opacity"] = json!(35);
        limit_clean.push(definition.clone());
        limit_decorated.extend([empty.clone(), hidden.clone(), definition]);
    }
    let cases = [
        ("mixed", decorated, clean),
        (
            "only-skipped",
            json!({"masks":[empty, hidden, empty]}),
            json!({}),
        ),
        (
            "limit",
            json!({"masks":limit_decorated}),
            json!({"masks":limit_clean}),
        ),
    ];
    let render = |profile: &serde_json::Value, for_display, precision, roi| {
        let adjustments = get_all_adjustments_from_json(profile, true, Some(1));
        let bitmaps = generated_profile_masks(profile, width, height);
        prepare_preview_pipeline(&processor, preview_features(&adjustments));
        let (pixels, w, h, x, y) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: &bitmaps,
                    lut: None,
                    roi,
                },
                for_display,
                precision,
            )
            .unwrap();
        if for_display {
            read_texture_data_roi(
                &context.device,
                &context.queue,
                &processor.test_working_texture(),
                wgpu::Origin3d { x, y, z: 0 },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                4,
            )
            .unwrap()
        } else {
            match pixels {
                RenderedPixels::U8(pixels) => pixels,
                RenderedPixels::U16(pixels) => bytemuck::cast_slice(&pixels).to_vec(),
            }
        }
    };
    let mut comparisons = 0;
    for (label, decorated, clean) in cases {
        for roi in [
            None,
            Some(Roi {
                x: 13,
                y: 9,
                width: 67,
                height: 43,
            }),
        ] {
            for (for_display, precision) in [
                (false, RenderOutputPrecision::EightBit),
                (false, RenderOutputPrecision::SixteenBit),
                (true, RenderOutputPrecision::EightBit),
            ] {
                let reference = render(&clean, for_display, precision, roi);
                let actual = render(&decorated, for_display, precision, roi);
                assert_eq!(
                    actual, reference,
                    "{label}, display={for_display}, {precision:?}, {roi:?}"
                );
                if label != "only-skipped" {
                    let unadjusted = render(&json!({}), for_display, precision, roi);
                    assert_ne!(reference, unadjusted, "masked fixture must change pixels");
                }
                comparisons += 1;
            }
        }
    }
    println!("empty-mask alignment pixel comparisons: {comparisons}");
}

#[test]
#[ignore = "requires a native GPU; diffraction correction"]
fn diffraction_correction_sharpens_without_shifting_the_level() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 256, 128).unwrap();
    let view = input(&context, 256, 128, 7);
    let render = |radius: f32, amount: f32| {
        // Darker, so the synthetic texture's peaks do not clip.
        let mut adjustments =
            get_all_adjustments_from_json(&json!({"exposure": -1.5}), true, Some(3));
        adjustments.global.sharpen_falloff[1] = radius;
        adjustments.global.sharpen_falloff[2] = amount;
        let (pixels, ..) = processor
            .run(
                &view,
                256,
                128,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U8(pixels) => pixels,
            _ => unreachable!(),
        }
    };
    let off = render(0.0, 0.0);
    let on = render(1.2, 0.6);
    let changed = off.iter().zip(&on).filter(|(a, b)| a != b).count();
    assert!(
        changed > off.len() / 10,
        "diffraction correction had no effect"
    );
    let mean = |p: &[u8]| p.iter().map(|v| *v as f64).sum::<f64>() / p.len() as f64;
    let (m_on, m_off) = (mean(&on), mean(&off));
    assert!(
        (m_on - m_off).abs() < 0.02 * m_off,
        "level shifted: {m_off} -> {m_on}"
    );
    // Below the radius the shader acts on, nothing changes.
    assert_eq!(render(0.2, 0.6), off);
}

#[test]
#[ignore = "requires a native GPU; LUT amount"]
fn lut_amount_mixes_to_100_percent_and_extrapolates_above() {
    let context = context();
    let (width, height) = (256, 128);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 11);
    // A look: more contrast, warmer, a little less saturated.
    let size = 17u32;
    let mut data = Vec::new();
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let [r, g, b] = [r, g, b].map(|v| v as f32 / (size - 1) as f32);
                let s = |x: f32| 0.6 * x * x * (3.0 - 2.0 * x) + 0.4 * x;
                let rgb = [s(r) + 0.03, s(g), s(b) - 0.03];
                let mean = (rgb[0] + rgb[1] + rgb[2]) / 3.0;
                data.extend(rgb.map(|c| (mean + (c - mean) * 0.85).clamp(0.0, 1.0)));
            }
        }
    }
    let lut = Arc::new(Lut {
        size,
        data,
        shaper: None,
    });
    for scene in [false, true] {
        let render = |amount: Option<f64>| {
            let recipe = match amount {
                Some(amount) => json!({
                    "exposure": -1.5, "lutPath": "/look.cube",
                    "lutIntensity": amount, "lutIsSceneReferred": scene,
                }),
                None => json!({"exposure": -1.5}),
            };
            let adjustments = get_all_adjustments_from_json(&recipe, true, Some(3));
            let (pixels, ..) = processor
                .run(
                    &view,
                    width,
                    height,
                    RenderRequest {
                        adjustments,
                        mask_bitmaps: &[],
                        lut: amount.map(|_| lut.clone()),
                        roi: None,
                    },
                    false,
                    RenderOutputPrecision::SixteenBit,
                )
                .unwrap();
            let RenderedPixels::U16(pixels) = pixels else {
                unreachable!()
            };
            let channels = pixels.len() / (width * height) as usize;
            pixels
                .chunks(channels)
                .map(|p| [0, 1, 2].map(|c| p[c] as f32 / 65535.0))
                .collect::<Vec<[f32; 3]>>()
        };
        let base = render(None);
        let full = render(Some(100.0));
        assert_eq!(render(Some(0.0)), base, "scene {scene}: 0 %");
        let changed = base.iter().zip(&full).filter(|(a, b)| a != b).count();
        assert!(
            changed > base.len() / 2,
            "scene {scene}: the LUT had no effect"
        );
        for amount in [40.0, 150.0, 200.0, 300.0] {
            let got = render(Some(amount));
            let a = (amount as f32 / 100.0).min(2.0);
            // Colours beyond the cube are clipped by the output, after
            // the amount; compare where the colour without the LUT and
            // with it at 100 % are inside.
            let inside = |p: &[f32; 3]| p.iter().all(|v| (0.002..0.998).contains(v));
            let mut worst = 0.0f32;
            let mut compared = 0;
            for ((got, base), full) in got.iter().zip(&base).zip(&full) {
                assert!(got.iter().all(|v| (0.0..=1.0).contains(v)));
                if !inside(base) || !inside(full) {
                    continue;
                }
                compared += 1;
                let want = crate::lut::apply_amount(*base, *full, a);
                for c in 0..3 {
                    worst = worst.max((got[c] - want[c]).abs());
                }
            }
            assert!(
                compared > base.len() / 2,
                "scene {scene}: {compared} compared"
            );
            // Outputs are 16-bit and the inputs to the amount are those
            // of the shader, not the rounded outputs.
            assert!(
                worst < 2.0 / 255.0,
                "scene {scene} {amount} %: off by {worst}"
            );
        }
        let far = render(Some(200.0));
        let moved = |x: &[[f32; 3]]| -> f32 {
            x.iter()
                .zip(&base)
                .map(|(x, b)| (0..3).map(|c| (x[c] - b[c]).abs()).sum::<f32>())
                .sum::<f32>()
        };
        let ratio = moved(&far) / moved(&full);
        assert!(
            ratio > 1.5 && ratio < 2.1,
            "scene {scene}: 200 % moved {ratio}x"
        );
    }
}

#[test]
#[ignore = "requires a native GPU; LUT shaper stage"]
fn a_lut_shaper_reads_on_the_gpu_as_the_engine_reads_it() {
    let context = context();
    let (width, height) = (256, 128);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 11);
    let render = |lut: Option<&Arc<Lut>>, scene: bool| {
        let recipe = match lut {
            Some(_) => json!({
                "exposure": -1.0, "lutPath": "/look.cube",
                "lutIntensity": 100.0, "lutIsSceneReferred": scene,
            }),
            None => json!({"exposure": -1.0}),
        };
        let adjustments = get_all_adjustments_from_json(&recipe, true, Some(3));
        let (pixels, ..) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: lut.cloned(),
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        let RenderedPixels::U16(pixels) = pixels else {
            unreachable!()
        };
        let channels = pixels.len() / (width * height) as usize;
        pixels
            .chunks(channels)
            .map(|p| [0, 1, 2].map(|c| p[c] as f32 / 65535.0))
            .collect::<Vec<[f32; 3]>>()
    };
    // A 17³ look (contrast, warmer, less saturated) read through a
    // log-style shaper of 1024 entries over -0.05..1.1, whose output the
    // table reads over 0.05..0.95.
    let size = 17u32;
    let mut data = Vec::new();
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let [r, g, b] = [r, g, b].map(|v| v as f32 / (size - 1) as f32);
                let s = |x: f32| 0.6 * x * x * (3.0 - 2.0 * x) + 0.4 * x;
                let rgb = [s(r) + 0.03, s(g), s(b) - 0.03];
                let mean = (rgb[0] + rgb[1] + rgb[2]) / 3.0;
                data.extend(rgb.map(|c| (mean + (c - mean) * 0.85).clamp(0.0, 1.0)));
            }
        }
    }
    let log = |x: f32| 0.05 + 0.9 * (1.0 + 63.0 * x.max(0.0)).log2() / 6.0;
    let entries = 1024;
    let shaper = crate::lut::Shaper {
        size: entries,
        data: (0..entries)
            .flat_map(|i| {
                let x = -0.05 + 1.15 * i as f32 / (entries - 1) as f32;
                [log(x), log(x * 0.95), log(x * 1.05)]
            })
            .collect(),
        domain: ([-0.05; 3], [1.1; 3]),
        table_domain: ([0.05; 3], [0.95; 3]),
    };
    let lut = Arc::new(Lut {
        size,
        data: data.clone(),
        shaper: Some(shaper),
    });
    // Display LUT: where the colour without it is inside the cube (no
    // colour carried beyond it), the render is the engine's reading of
    // that colour.
    let base = render(None, false);
    let full = render(Some(&lut), false);
    let inside = |p: &[f32; 3]| p.iter().all(|v| (0.002..0.998).contains(v));
    let (mut worst, mut compared) = (0.0f32, 0);
    for (base, got) in base.iter().zip(&full) {
        if !inside(base) {
            continue;
        }
        compared += 1;
        let want = lut.sample(*base);
        for c in 0..3 {
            worst = worst.max((got[c] - want[c]).abs());
        }
    }
    assert!(compared > base.len() / 2, "{compared} compared");
    assert!(worst < 2.0 / 255.0, "display: off by {worst}");
    // Without the shaper the render differs: the stage is applied.
    let plain = Arc::new(Lut {
        size,
        data: data.clone(),
        shaper: None,
    });
    assert!(render(Some(&plain), false) != full);
    // Scene LUT: a straight shaper over the table's own domain reads the
    // table as the plain one does.
    for scene in [false, true] {
        let straight = Arc::new(Lut {
            size,
            data: data.clone(),
            shaper: Some(crate::lut::Shaper {
                size: 2,
                data: vec![0.2, 0.1, 0.3, 0.8, 1.6, 0.9],
                domain: ([0.0; 3], [1.0; 3]),
                table_domain: ([0.2, 0.1, 0.3], [0.8, 1.6, 0.9]),
            }),
        });
        let (a, b) = (render(Some(&straight), scene), render(Some(&plain), scene));
        let worst = a
            .iter()
            .zip(&b)
            .flat_map(|(a, b)| (0..3).map(move |c| (a[c] - b[c]).abs()))
            .fold(0.0f32, f32::max);
        assert!(
            worst < 1.0 / 255.0,
            "scene {scene}: straight shaper off by {worst}"
        );
    }
}

#[test]
#[ignore = "requires a native GPU; camera profile matrix and HSV tables"]
fn camera_profile_matrix_and_tables_act_on_the_gpu() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 256, 128).unwrap();
    let view = input(&context, 256, 128, 5);
    // Matrix, table entries, HueSatMap and LookTable dimensions, key.
    type Profile = ([[f32; 3]; 3], Vec<[f32; 4]>, [u32; 4], [u32; 4], u32);
    let render = |profile: Option<Profile>| {
        let mut adjustments = get_all_adjustments_from_json(&json!({}), true, Some(3));
        if let Some((matrix, entries, hue_sat, look, key)) = profile {
            let hue_sat_len = hue_sat[0] * hue_sat[1] * hue_sat[2];
            crate::camera_profile::publish_tables(key, entries);
            let global = &mut adjustments.global;
            global.camera_profile = [1, key, hue_sat_len, 1];
            global.camera_profile_matrix = crate::image_processing::gpu_mat3_from_rows(matrix);
            global.camera_profile_hue_sat = hue_sat;
            global.camera_profile_look = look;
        }
        let (pixels, ..) = processor
            .run(
                &view,
                256,
                128,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U8(pixels) => pixels,
            _ => unreachable!(),
        }
    };
    let max_diff = |a: &[u8], b: &[u8]| {
        a.iter()
            .zip(b)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0)
    };
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let neutral = render(None);
    // Identity matrix and identity HueSatMap + sRGB-encoded LookTable.
    let hue_sat = vec![[0.0, 1.0, 1.0, 0.0]; 90 * 30];
    let look = vec![[0.0, 1.0, 1.0, 0.0]; 36 * 8 * 16];
    let entries: Vec<[f32; 4]> = hue_sat.iter().chain(&look).copied().collect();
    let same = render(Some((
        identity,
        entries.clone(),
        [90, 30, 1, 0],
        [36, 8, 16, 1],
        0xC0FFEE,
    )));
    assert!(
        max_diff(&neutral, &same) <= 1,
        "identity profile changed pixels"
    );
    // A HueSatMap that halves saturation leaves the image less saturated.
    let desaturate: Vec<[f32; 4]> = vec![[0.0, 0.5, 1.0, 0.0]; 6 * 2];
    let flat = render(Some((identity, desaturate, [6, 2, 1, 0], [0; 4], 0xC0FFEF)));
    assert!(max_diff(&neutral, &flat) > 8, "HueSatMap had no effect");
    let spread = |p: &[u8]| {
        p.as_chunks::<4>()
            .0
            .iter()
            .map(|c| (c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])) as u64)
            .sum::<u64>()
    };
    assert!(spread(&flat) < spread(&neutral) * 3 / 4);
    // A matrix moves colours.
    let swap = [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
    let swapped = render(Some((
        swap,
        vec![[0.0, 1.0, 1.0, 0.0]],
        [0; 4],
        [0; 4],
        0xC0FFF0,
    )));
    assert!(
        max_diff(&neutral, &swapped) > 8,
        "profile matrix had no effect"
    );
    // The CPU form of that table (camera_profile.rs) halves HSV saturation.
    let table = crate::camera_profile::HsvTable {
        hue_divisions: 6,
        sat_divisions: 2,
        val_divisions: 1,
        srgb_value: false,
        data: vec![[0.0, 0.5, 1.0]; 12],
    };
    let out = table.apply([0.8, 0.4, 0.2]);
    assert!((out[0] - 0.8).abs() < 1e-5 && (out[2] - 0.5).abs() < 1e-5);
}

#[test]
#[ignore = "requires a native GPU; exercises individual Develop controls and brightness response"]
fn develop_controls_change_pixels_and_reset_exactly() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 256, 128).unwrap();
    let view = input(&context, 256, 128, 3);
    let render = |profile: &serde_json::Value, raw: bool, mapper: u32| {
        let adjustments = get_all_adjustments_from_json(profile, raw, Some(mapper));
        let (pixels, ..) = processor
            .run(
                &view,
                256,
                128,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U8(pixels) => pixels,
            _ => unreachable!(),
        }
    };
    let mut profiles = Vec::new();
    for (key, lo, hi, neutral, dependency) in [
        ("exposure", -5., 5., 0., json!({})),
        ("brightness", -5., 5., 0., json!({})),
        ("contrast", -100., 100., 0., json!({})),
        ("highlights", -100., 100., 0., json!({})),
        ("shadows", -100., 100., 0., json!({})),
        ("whites", -100., 100., 0., json!({})),
        ("blacks", -100., 100., 0., json!({})),
        ("temperature", -100., 100., 0., json!({})),
        ("tint", -100., 100., 0., json!({})),
        ("vibrance", -100., 100., 0., json!({})),
        ("saturation", -100., 100., 0., json!({})),
        ("clarity", -100., 100., 0., json!({})),
        ("structure", -100., 100., 0., json!({})),
        ("dehaze", -100., 100., 0., json!({})),
        ("sharpness", -100., 100., 0., json!({})),
        ("sharpnessThreshold", 0., 80., 15., json!({"sharpness":70})),
        ("sharpenRadius", 0.5, 3., 1., json!({"sharpness":70})),
        ("sharpenDetail", 0., 100., 25., json!({"sharpness":70})),
        ("sharpenMasking", 100., 100., 0., json!({"sharpness":70})),
        ("sharpnessFalloff", 50., 100., 0., json!({"sharpness":70})),
        // Sharpening response 2 (`sharpenVersion`).
        ("sharpness", 60., 150., 0., json!({"sharpenVersion":2})),
        (
            "sharpenRadius",
            0.5,
            3.,
            1.,
            json!({"sharpness":120,"sharpenVersion":2}),
        ),
        (
            "sharpenDetail",
            0.,
            100.,
            25.,
            json!({"sharpness":120,"sharpenVersion":2}),
        ),
        (
            "sharpenMasking",
            100.,
            100.,
            0.,
            json!({"sharpness":120,"sharpenVersion":2}),
        ),
        ("moireAmount", 50., 100., 0., json!({})),
        ("moirePattern", 0., 100., 30., json!({"moireAmount":100})),
        ("lumaNoiseReduction", 30., 100., 0., json!({})),
        ("colorNoiseReduction", 30., 100., 0., json!({})),
        ("vignetteAmount", -100., 100., 0., json!({})),
        (
            "vignetteMidpoint",
            0.,
            100.,
            50.,
            json!({"vignetteAmount":-60}),
        ),
        (
            "vignetteFeather",
            0.,
            100.,
            50.,
            json!({"vignetteAmount":-60}),
        ),
        ("grainAmount", 30., 100., 0., json!({})),
        ("grainSize", 0., 100., 25., json!({"grainAmount":60})),
        ("centré", -100., 100., 0., json!({})),
        ("hue", -180., 180., 0., json!({})),
        ("chromaticAberrationRedCyan", -100., 100., 0., json!({})),
        ("chromaticAberrationBlueYellow", -100., 100., 0., json!({})),
        (
            "vignetteRoundness",
            -100.,
            100.,
            0.,
            json!({"vignetteAmount":-60}),
        ),
        ("grainRoughness", 0., 100., 50., json!({"grainAmount":60})),
        ("glowAmount", 30., 100., 0., json!({})),
        ("halationAmount", 30., 100., 0., json!({})),
        ("flareAmount", 30., 100., 0., json!({})),
        (
            "vignetteHighlights",
            50.,
            100.,
            0.,
            json!({"vignetteAmount":-60}),
        ),
        ("defringePurpleAmount", 10., 20., 0., json!({})),
        ("defringeGreenAmount", 10., 20., 0., json!({})),
        (
            "defringePurpleAmount",
            10.,
            20.,
            0.,
            json!({"defringeVersion": 2}),
        ),
    ] {
        for value in [lo, hi] {
            let mut profile = dependency.clone();
            profile[key] = json!(value);
            let mut reset = dependency.clone();
            reset[key] = json!(neutral);
            profiles.push((format!("{key}={value}"), profile, reset, dependency.clone()));
        }
    }
    for color in [
        "reds", "oranges", "yellows", "greens", "aquas", "blues", "purples", "magentas",
    ] {
        for key in ["hue", "saturation", "luminance"] {
            for value in [-100, 100] {
                profiles.push((
                    format!("hsl.{color}.{key}={value}"),
                    json!({"hsl":{color:{key:value}}}),
                    json!({"hsl":{color:{key:0}}}),
                    json!({}),
                ));
            }
        }
    }
    for key in [
        "shadowsTint",
        "redHue",
        "redSaturation",
        "greenHue",
        "greenSaturation",
        "blueHue",
        "blueSaturation",
    ] {
        for value in [-100, 100] {
            profiles.push((
                format!("calibration.{key}={value}"),
                json!({"colorCalibration":{key:value}}),
                json!({"colorCalibration":{key:0}}),
                json!({}),
            ));
        }
    }
    for band in ["global", "shadows", "midtones", "highlights"] {
        profiles.push((
            format!("grading.{band}.saturation"),
            json!({"colorGrading":{band:{"hue":45,"saturation":100,"luminance":0}}}),
            json!({"colorGrading":{band:{"hue":45,"saturation":0,"luminance":0}}}),
            json!({}),
        ));
        for value in [-100, 100] {
            profiles.push((
                format!("grading.{band}.luminance={value}"),
                json!({"colorGrading":{band:{"hue":0,"saturation":0,"luminance":value}}}),
                json!({"colorGrading":{band:{"hue":0,"saturation":0,"luminance":0}}}),
                json!({}),
            ));
        }
    }
    for (key, neutral) in [("balance", 0), ("blending", 50)] {
        for value in [if key == "balance" { -100 } else { 0 }, 100] {
            let base = json!({"colorGrading":{"shadows":{"hue":210,"saturation":80,"luminance":0},"highlights":{"hue":45,"saturation":80,"luminance":0}}});
            let mut changed = base.clone();
            changed["colorGrading"][key] = json!(value);
            let mut reset = base.clone();
            reset["colorGrading"][key] = json!(neutral);
            profiles.push((format!("grading.{key}={value}"), changed, reset, base));
        }
    }
    for structure in ["soft", "clumped", "speckled", "gritty"] {
        profiles.push((
            format!("grainStructure={structure}"),
            json!({"grainAmount": 60, "grainStructure": structure}),
            json!({"grainAmount": 60, "grainStructure": "even"}),
            json!({"grainAmount": 60}),
        ));
    }
    for style in ["colorPriority", "paintOverlay"] {
        for amount in [-60, 60] {
            profiles.push((
                format!("vignetteStyle={style},{amount}"),
                json!({"vignetteAmount": amount, "vignetteStyle": style}),
                json!({"vignetteAmount": amount, "vignetteStyle": "highlightPriority"}),
                json!({"vignetteAmount": amount}),
            ));
        }
    }
    profiles.push((
        "blackAndWhite".to_string(),
        json!({"blackAndWhite": true}),
        json!({"blackAndWhite": false}),
        json!({}),
    ));
    for channel in [
        "red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta",
    ] {
        for value in [-100, 100] {
            profiles.push((
                format!("grayMixer.{channel}={value}"),
                json!({"blackAndWhite": true, "grayMixer": {channel: value}}),
                json!({"blackAndWhite": true, "grayMixer": {channel: 0}}),
                json!({"blackAndWhite": true}),
            ));
        }
    }
    for channel in ["rgb", "red", "green", "blue"] {
        for (key, value, neutral) in [
            ("inBlack", 40.0, 0.0),
            ("inWhite", 200.0, 255.0),
            ("gamma", 2.0, 1.0),
            ("gamma", 0.5, 1.0),
            ("outBlack", 40.0, 0.0),
            ("outWhite", 200.0, 255.0),
        ] {
            profiles.push((
                format!("levels.{channel}.{key}={value}"),
                json!({"levels": {channel: {key: value}}}),
                json!({"levels": {channel: {key: neutral}}}),
                json!({}),
            ));
        }
    }
    let range = |patch: serde_json::Value| {
        let mut r = json!({"id": "r1", "hue": 40.0, "chroma": 0.12, "lightness": 0.6,
            "hueRange": 60, "chromaRange": 100, "lightnessRange": 100, "smoothness": 50});
        for (k, v) in patch.as_object().unwrap() {
            r[k] = v.clone();
        }
        json!({"colorRanges": [r]})
    };
    for (key, value) in [
        ("hueShift", 80.0),
        ("saturation", -80.0),
        ("saturation", 80.0),
        ("lightnessShift", 80.0),
        ("hueUniformity", 100.0),
        ("saturationUniformity", 100.0),
        ("lightnessUniformity", 100.0),
    ] {
        profiles.push((
            format!("colorRange.{key}={value}"),
            range(json!({key: value})),
            range(json!({key: 0.0})),
            range(json!({})),
        ));
    }
    let mut shown = range(json!({}));
    shown["colorRangeShow"] = json!("r1");
    profiles.push((
        "colorRange.show".to_string(),
        shown,
        range(json!({})),
        range(json!({})),
    ));
    for region in ["shadows", "darks", "lights", "highlights"] {
        for value in [-100, 100] {
            profiles.push((
                format!("regionCurve.{region}={value}"),
                json!({"regionCurve": {region: value}}),
                json!({"regionCurve": {region: 0}}),
                json!({}),
            ));
        }
    }
    for (split, value, region) in [
        ("shadowSplit", 10, "shadows"),
        ("midtoneSplit", 35, "darks"),
        ("highlightSplit", 90, "highlights"),
    ] {
        profiles.push((
            format!("regionCurve.{split}={value}"),
            json!({"regionCurve": {region: 80, split: value}}),
            json!({"regionCurve": {region: 80}}),
            json!({"regionCurve": {region: 80}}),
        ));
    }
    for channel in ["luma", "red", "green", "blue"] {
        profiles.push((
            format!("curve.{channel}"),
            json!({"curves":{channel:[{"x":0,"y":0},{"x":128,"y":160},{"x":255,"y":255}]}}),
            json!({"curves":{channel:[{"x":0,"y":0},{"x":255,"y":255}]}}),
            json!({}),
        ));
    }
    let mut checked = 0;
    for (raw, mapper) in [(true, 1), (true, 0), (false, 0), (true, 2), (false, 2)] {
        for (name, profile, reset, base) in &profiles {
            // Vignette style and Highlights exist for the Standard look only.
            if mapper < 2
                && name.starts_with("vignette")
                && (name.contains("Style") || name.contains("Highlights"))
            {
                continue;
            }
            let baseline = render(base, raw, mapper);
            let changed = render(profile, raw, mapper);
            assert_ne!(
                changed, baseline,
                "no visible effect: {name}, raw={raw}, mapper={mapper}"
            );
            assert_eq!(
                render(reset, raw, mapper),
                baseline,
                "reset mismatch: {name}"
            );
            checked += 1;
        }
        let mean = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| (p[0] as f64 + p[1] as f64 + p[2] as f64) / 3.)
                .sum::<f64>()
                / (pixels.len() / 4) as f64
        };
        let neutral = mean(&render(&json!({}), raw, mapper));
        let mut previous = f64::NEG_INFINITY;
        for brightness in [-5_f64, -1., -0.1, -0.01, 0., 0.01, 0.1, 1., 5.] {
            let value = mean(&render(&json!({"brightness":brightness}), raw, mapper));
            assert!(
                value >= previous,
                "brightness response reversed at {brightness}"
            );
            if brightness.abs() <= 0.01 {
                assert!((value - neutral).abs() < 1., "fine step too strong");
            }
            println!("brightness={brightness}, raw={raw}, mapper={mapper}, meanRGB={value:.4}");
            previous = value;
        }
    }
    println!("{checked} individual control endpoint/reset GPU probes passed");
}

#[test]
#[ignore = "requires a native GPU; compares sharpening responses 1 and 2"]
fn sharpen_version_2_keeps_amount_40_and_caps_halos() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 256, 128).unwrap();
    let view = input(&context, 256, 128, 5);
    let render = |profile: serde_json::Value| {
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        let (pixels, ..) = processor
            .run(
                &view,
                256,
                128,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U16(pixels) => pixels,
            _ => unreachable!(),
        }
    };
    let recipe = |amount: f64, version: u64| {
        json!({"sharpness": amount, "lumaNoiseReduction": 0, "colorNoiseReduction": 0,
            "sharpenVersion": version})
    };
    // At Amount 40 with the other controls at their defaults both responses
    // are the same path, bit for bit.
    assert_eq!(render(recipe(40., 2)), render(recipe(40., 1)));
    // Above it they differ, and response 2 keeps the strongest change (the
    // halo) below response 1's, whose ceiling grew with the amount.
    let flat = render(recipe(0., 1));
    let strongest = |pixels: &[u16]| {
        let mut change: Vec<i32> = pixels
            .iter()
            .zip(&flat)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .collect();
        change.sort_unstable();
        change[change.len() * 99 / 100]
    };
    let v1 = render(recipe(150., 1));
    let v2 = render(recipe(150., 2));
    assert_ne!(v1, v2);
    assert!(
        strongest(&v2) < strongest(&v1),
        "halo {} not below response 1's {}",
        strongest(&v2),
        strongest(&v1)
    );
}

#[test]
#[ignore = "requires a native GPU; compares sharpening responses 2 and 3"]
fn sharpen_version_3_spares_the_shadows() {
    // Grey texture (±25 %) around a shadow level on the left half and a
    // mid-tone level on the right half.
    let (width, height) = (256u32, 128u32);
    let data: Vec<f16> = (0..width * height)
        .flat_map(|i| {
            let base = if i % width < width / 2 { 0.004 } else { 0.15 };
            let v = f16::from_f32(base * (0.75 + 0.5 * ((i * 7919 + 13) % 101) as f32 / 100.0));
            [v, v, v, f16::ONE]
        })
        .collect();
    let context = context();
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input_from(&context, width, height, &data);
    let render = |amount: f64, version: u64| {
        let profile = json!({"sharpness": amount, "lumaNoiseReduction": 0, "colorNoiseReduction": 0,
            "sharpenVersion": version});
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        let (pixels, ..) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U16(pixels) => pixels,
            _ => unreachable!(),
        }
    };
    let flat = render(0., 3);
    // Mean absolute change of the first channel per half, away from the
    // border between them and the image edges.
    let change = |pixels: &[u16], left: bool| {
        let (mut sum, mut n) = (0.0f64, 0.0f64);
        for y in 8..height - 8 {
            let xs = if left {
                8..width / 2 - 8
            } else {
                width / 2 + 8..width - 8
            };
            for x in xs {
                let i = ((y * width + x) * 4) as usize;
                sum += (pixels[i] as f64 - flat[i] as f64).abs();
                n += 1.0;
            }
        }
        sum / n
    };
    for amount in [40., 80.] {
        let (v2, v3) = (render(amount, 2), render(amount, 3));
        let (dark2, dark3) = (change(&v2, true), change(&v3, true));
        let (mid2, mid3) = (change(&v2, false), change(&v3, false));
        assert!(dark2 > 0.0 && mid2 > 0.0, "response 2 sharpens both halves");
        assert!(
            dark3 < 0.2 * dark2,
            "Amount {amount}: shadows {dark3} not below a fifth of response 2's {dark2}"
        );
        assert!(
            (mid3 / mid2 - 1.0).abs() < 0.15,
            "Amount {amount}: mid-tones {mid3} against response 2's {mid2}"
        );
    }
}

#[test]
#[ignore = "requires a native GPU; compares sharpening responses 3, 4 and 5"]
fn sharpen_version_4_keeps_low_contrast_texture_at_radius_and_masking() {
    // Low-contrast grey texture at a mid-tone level, where response 3's
    // Threshold gate shut Radius 0.5 out (fine grain, ±3 %) and Masking 50
    // removed all sharpening (a 6 px pattern, ±8 %).
    let (width, height) = (256u32, 128u32);
    let grain: Vec<f16> = (0..width * height)
        .flat_map(|i| {
            let v = f16::from_f32(0.15 * (0.97 + 0.06 * ((i * 7919 + 13) % 101) as f32 / 100.0));
            [v, v, v, f16::ONE]
        })
        .collect();
    let pattern: Vec<f16> = (0..width * height)
        .flat_map(|i| {
            let phase = |c: u32| (c as f32 * std::f32::consts::TAU / 6.0).sin();
            let v = f16::from_f32(0.15 * (1.0 + 0.08 * phase(i % width) * phase(i / width)));
            [v, v, v, f16::ONE]
        })
        .collect();
    // Strong fine patterns at the 2 and 4 px periods of a sparse sample grid
    // (diagonal and vertical stripes, ±60 %): Masking must see their spread.
    let stripes = |period: f32, diagonal: bool| -> Vec<f16> {
        (0..width * height)
            .flat_map(|i| {
                let (x, y) = ((i % width) as f32, (i / width) as f32);
                let t = if diagonal { x + y } else { x };
                let v =
                    f16::from_f32(0.15 * (1.0 + 0.6 * (t * std::f32::consts::TAU / period).cos()));
                [v, v, v, f16::ONE]
            })
            .collect()
    };
    let context = context();
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let render = |data: &[f16], extra: serde_json::Value, version: u64| {
        let view = input_from(&context, width, height, data);
        let mut profile = json!({"sharpness": 40, "lumaNoiseReduction": 0, "colorNoiseReduction": 0,
            "sharpenVersion": version});
        for (key, value) in extra.as_object().unwrap() {
            profile[key] = value.clone();
        }
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        let (pixels, ..) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U16(pixels) => pixels,
            _ => unreachable!(),
        }
    };
    // Mean absolute change of the first channel against the unsharpened
    // render, away from the image edges.
    let change = |data: &[f16], extra: serde_json::Value, version: u64| {
        let flat = render(data, json!({"sharpness": 0}), version);
        let pixels = render(data, extra, version);
        let (mut sum, mut n) = (0.0f64, 0.0f64);
        for y in 8..height - 8 {
            for x in 8..width - 8 {
                let i = ((y * width + x) * 4) as usize;
                sum += (pixels[i] as f64 - flat[i] as f64).abs();
                n += 1.0;
            }
        }
        sum / n
    };
    // From Radius 1 up without Masking: the same path, bit for bit.
    for (amount, radius) in [(40, 1.0), (80, 1.0), (80, 2.0)] {
        let recipe = json!({"sharpness": amount, "sharpenDetail": 50, "sharpenRadius": radius});
        assert_eq!(render(&grain, recipe.clone(), 4), render(&grain, recipe, 3));
    }
    let radius_1 = change(&grain, json!({}), 4);
    let (half_3, half_4) = (
        change(&grain, json!({"sharpenRadius": 0.5}), 3),
        change(&grain, json!({"sharpenRadius": 0.5}), 4),
    );
    assert!(
        half_4 > 1.5 * half_3 && half_4 > 0.5 * radius_1,
        "Radius 0.5: {half_4} against response 3's {half_3} and Radius 1's {radius_1}"
    );
    let unmasked = change(&pattern, json!({}), 4);
    let (masked_3, masked_4) = (
        change(&pattern, json!({"sharpenMasking": 50}), 3),
        change(&pattern, json!({"sharpenMasking": 50}), 4),
    );
    assert!(
        masked_4 > 0.03 * unmasked && masked_4 > 2.0 * masked_3 && masked_4 < unmasked,
        "Masking 50: {masked_4} against response 3's {masked_3} and no masking's {unmasked}"
    );
    for (period, diagonal) in [(2.0, false), (4.0, false), (4.0, true)] {
        let data = stripes(period, diagonal);
        let (masked, unmasked) = (
            change(&data, json!({"sharpenMasking": 50}), 4),
            change(&data, json!({}), 4),
        );
        assert!(
            masked > 0.8 * unmasked,
            "Masking 50 on {period} px stripes (diagonal {diagonal}): {masked} of {unmasked}"
        );
    }
    // Response 5: Masking's ramp keeps more of moderate texture, not more
    // of the fine grain (noise) than response 4, and the stripes; without
    // Masking it is response 4, bit for bit.
    for recipe in [
        json!({"sharpenRadius": 0.5}),
        json!({"sharpness": 80, "sharpenDetail": 50}),
    ] {
        assert_eq!(render(&grain, recipe.clone(), 5), render(&grain, recipe, 4));
    }
    // A stronger 6 px pattern (±20 %), half way up response 4's ramp.
    let texture: Vec<f16> = (0..width * height)
        .flat_map(|i| {
            let phase = |c: u32| (c as f32 * std::f32::consts::TAU / 6.0).sin();
            let v = f16::from_f32(0.15 * (1.0 + 0.2 * phase(i % width) * phase(i / width)));
            [v, v, v, f16::ONE]
        })
        .collect();
    let (texture_4, texture_5, texture_all) = (
        change(&texture, json!({"sharpenMasking": 50}), 4),
        change(&texture, json!({"sharpenMasking": 50}), 5),
        change(&texture, json!({}), 5),
    );
    assert!(
        texture_5 > 1.2 * texture_4 && texture_5 < texture_all,
        "Masking 50: {texture_5} against response 4's {texture_4} and no masking's {texture_all}"
    );
    // Masking near 0 (fractional values from imports) keeps all of it.
    let barely = change(&texture, json!({"sharpenMasking": 1e-7}), 5);
    assert!(
        (barely / texture_all - 1.0).abs() < 0.01,
        "Masking 1e-7: {barely} against no masking's {texture_all}"
    );
    let (grain_4, grain_5) = (
        change(&grain, json!({"sharpenMasking": 50}), 4),
        change(&grain, json!({"sharpenMasking": 50}), 5),
    );
    assert!(
        grain_5 <= grain_4 && grain_5 < 0.1 * radius_1,
        "Masking 50 on grain: {grain_5} against response 4's {grain_4} and no masking's {radius_1}"
    );
    for (period, diagonal) in [(2.0, false), (4.0, false), (4.0, true)] {
        let data = stripes(period, diagonal);
        let (masked, unmasked) = (
            change(&data, json!({"sharpenMasking": 50}), 5),
            change(&data, json!({}), 5),
        );
        assert!(
            masked > 0.8 * unmasked,
            "Response 5, Masking 50 on {period} px stripes (diagonal {diagonal}): {masked} of {unmasked}"
        );
    }
}

#[test]
#[ignore = "requires a native GPU; compares sharpening responses 5 and 6"]
fn sharpen_version_6_reaches_further_on_non_raw_photos() {
    // A textured mid-grey (a 6 px pattern, ±20 %) as a JPEG and as a RAW.
    let (width, height) = (128u32, 128u32);
    let texture: Vec<f16> = (0..width * height)
        .flat_map(|i| {
            let phase = |c: u32| (c as f32 * std::f32::consts::TAU / 6.0).sin();
            let v = f16::from_f32(0.4 * (1.0 + 0.2 * phase(i % width) * phase(i / width)));
            [v, v, v, f16::ONE]
        })
        .collect();
    let context = context();
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input_from(&context, width, height, &texture);
    let render = |amount: f64, version: u64, is_raw: bool| {
        let profile = json!({"sharpness": amount, "lumaNoiseReduction": 0, "colorNoiseReduction": 0,
            "sharpenVersion": version});
        let (pixels, ..) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: get_all_adjustments_from_json(&profile, is_raw, Some(1)),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U16(pixels) => pixels,
            _ => unreachable!(),
        }
    };
    let change = |amount: f64, version: u64| {
        let (flat, pixels) = (render(0.0, version, false), render(amount, version, false));
        let mut sum = 0.0f64;
        for y in 8..height - 8 {
            for x in 8..width - 8 {
                let i = ((y * width + x) * 4) as usize;
                sum += (pixels[i] as f64 - flat[i] as f64).abs();
            }
        }
        sum
    };
    // RAW photos and amounts up to 40: response 5, bit for bit.
    assert_eq!(render(80.0, 6, true), render(80.0, 5, true));
    for amount in [20.0, 40.0] {
        assert_eq!(render(amount, 6, false), render(amount, 5, false));
    }
    // Above 40 the change grows by (amount / 40)^0.15.
    for amount in [80.0, 150.0] {
        let ratio = change(amount, 6) / change(amount, 5);
        let expected = (amount / 40.0f64).powf(0.15);
        assert!(
            (ratio / expected - 1.0).abs() < 0.03,
            "Amount {amount}: change ×{ratio} against response 5, expected ×{expected}"
        );
    }
}

fn context() -> GpuContext {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .unwrap();
    let limits = adapter.limits();
    let features = adapter.features() & wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: features,
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .unwrap();
    GpuContext {
        device: Arc::new(device),
        queue: Arc::new(queue),
        limits,
        display: Arc::new(Mutex::new(None)),
        display_space: Default::default(),
    }
}

fn input(context: &GpuContext, width: u32, height: u32, seed: u32) -> wgpu::TextureView {
    let data: Vec<f16> = (0..width * height)
        .flat_map(|i| {
            [
                f16::from_f32(((i * 13 + seed) % 257) as f32 / 128.0),
                f16::from_f32(((i * 7 + seed) % 193) as f32 / 192.0),
                f16::from_f32(((i * 3 + seed) % 127) as f32 / 126.0),
                f16::ONE,
            ]
        })
        .collect();
    input_from(context, width, height, &data)
}

/// An RGBA16F input texture holding `data`.
fn input_from(context: &GpuContext, width: u32, height: u32, data: &[f16]) -> wgpu::TextureView {
    context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Synthetic preview regression input"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            bytemuck::cast_slice(data),
        )
        .create_view(&Default::default())
}

#[test]
#[ignore = "requires a native GPU; compares actual 8/16-bit pixels against full preparation"]
fn cached_preview_pixels_match_full_preparation() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2304, 512).unwrap();
    let profiles = [
        json!({"exposure":1.1,"temperature":30,"tint":-10}),
        json!({"shadows":65,"blacks":-15,"sharpness":45}),
        json!({"clarity":30,"structure":-25,"centré":20}),
        json!({"glowAmount":35,"halationAmount":25,"dehaze":40}),
        json!({"highlights":-40}),
        json!({"highlights":30,"sharpness":45}),
        json!({"curves":{"luma":[{"x":0,"y":0},{"x":120,"y":140},{"x":255,"y":255}]}}),
        json!({"lumaNoiseReduction":20,"colorNoiseReduction":15}),
        json!({"grainAmount":60,"grainStructure":"soft"}),
        json!({"grainAmount":45,"grainSize":60,"grainRoughness":80,"grainStructure":"clumped"}),
        json!({"grainAmount":70,"grainRoughness":100,"grainStructure":"speckled","curves":custom_curve()}),
        json!({"grainAmount":50,"grainSize":10,"grainStructure":"gritty","shadows":40}),
        json!({"masks":[{"id":"local","name":"Local","visible":true,"invert":false,"opacity":100,"subMasks": [all_sub_mask()],"adjustments":{"sharpness":25,"shadows":35,"clarity":20,"structure":30,"dehaze":15}}]}),
        json!({}),
        local_profile(json!({"exposure":0.75,"brightness":0.1})),
        local_profile(json!({"highlights":-45})),
        local_profile(json!({"curves":custom_curve()})),
        local_profile(json!({"lumaNoiseReduction":30,"colorNoiseReduction":45})),
        local_profile(
            json!({"sharpness":-40,"highlights":35,"curves":custom_curve(),"lumaNoiseReduction":20}),
        ),
        json!({"lumaNoiseReduction":40,"curves":custom_curve()}),
        json!({"highlights":-30,"sharpness":35,"curves":custom_curve(),"colorNoiseReduction":20}),
        json!({"sharpness":50,"highlights":-20,"masks":[
            {"id":"a","name":"A","visible":true,"invert":false,"opacity":100,"subMasks": [all_sub_mask()],"adjustments":{"sharpness":-50,"highlights":20}},
            {"id":"b","name":"B","visible":true,"invert":false,"opacity":100,"subMasks": [all_sub_mask()],"adjustments":{"curves":custom_curve(),"colorNoiseReduction":30}}
        ]}),
    ];
    // Prepare the exact variants used below so asynchronous compilation cannot
    // make a specialized-pixel regression silently exercise only the fallback.
    for profile in &profiles {
        let features = preview_features(&get_all_adjustments_from_json(profile, true, Some(1)));
        prepare_preview_pipeline(&processor, features);
    }
    let mut comparisons = 0;
    for (width, height) in [(640, 400), (2176, 64)] {
        let views = [
            input(&context, width, height, 3),
            input(&context, width, height, 47),
        ];
        let mask_bitmaps = [
            masked(ImageBuffer::from_fn(width, height, |x, _| {
                Luma([((x % 127) * 2) as u8])
            })),
            masked(ImageBuffer::from_fn(width, height, |_, y| {
                Luma([255 - ((y % 127) * 2) as u8])
            })),
        ];
        for precision in [
            RenderOutputPrecision::EightBit,
            RenderOutputPrecision::SixteenBit,
        ] {
            for roi in [
                None,
                Some(Roi {
                    x: 17,
                    y: 9,
                    width: width - 33,
                    height: height - 18,
                }),
            ] {
                for view in [&views[0], &views[1], &views[0]] {
                    for (is_raw, tonemapper) in
                        [(true, 1), (true, 0), (true, 2), (true, 3), (false, 0)]
                    {
                        for profile in &profiles {
                            let adjustments =
                                get_all_adjustments_from_json(profile, is_raw, Some(tonemapper));
                            let masks = &mask_bitmaps[..adjustments.mask_count as usize];
                            let render = || {
                                let (pixels, ..) = processor
                                    .run(
                                        view,
                                        width,
                                        height,
                                        RenderRequest {
                                            adjustments: adjustments.clone(),
                                            mask_bitmaps: masks,
                                            lut: None,
                                            roi,
                                        },
                                        false,
                                        precision,
                                    )
                                    .unwrap();
                                match pixels {
                                    RenderedPixels::U8(pixels) => pixels,
                                    RenderedPixels::U16(pixels) => {
                                        bytemuck::cast_slice(&pixels).to_vec()
                                    }
                                }
                            };
                            processor
                                .force_full_preparation
                                .store(false, Ordering::Relaxed);
                            let first = render();
                            let cached = render();
                            processor
                                .force_full_preparation
                                .store(true, Ordering::Relaxed);
                            let reference = render();
                            assert_eq!(
                                first, reference,
                                "cold/source/tile invalidation: {profile}"
                            );
                            assert_eq!(cached, reference, "cached: {profile}");
                            comparisons += 2;
                            if precision == RenderOutputPrecision::EightBit {
                                processor
                                    .force_full_preparation
                                    .store(false, Ordering::Relaxed);
                                let (_, w, h, x, y) = processor
                                    .run(
                                        view,
                                        width,
                                        height,
                                        RenderRequest {
                                            adjustments: adjustments.clone(),
                                            mask_bitmaps: masks,
                                            lut: None,
                                            roi,
                                        },
                                        true,
                                        precision,
                                    )
                                    .unwrap();
                                let preview = read_texture_data_roi(
                                    &context.device,
                                    &context.queue,
                                    &processor.test_working_texture(),
                                    wgpu::Origin3d { x, y, z: 0 },
                                    wgpu::Extent3d {
                                        width: w,
                                        height: h,
                                        depth_or_array_layers: 1,
                                    },
                                    4,
                                )
                                .unwrap();
                                assert_eq!(preview.len(), reference.len());
                                let max_delta = preview
                                    .iter()
                                    .zip(&reference)
                                    .map(|(a, b)| a.abs_diff(*b))
                                    .max()
                                    .unwrap();
                                // Specializing Metal's arithmetic may round an 8-bit display
                                // channel differently by one unit. Export stays byte-exact.
                                assert!(
                                    max_delta <= 1,
                                    "preview max channel delta {max_delta}: {profile}"
                                );
                                comparisons += 1;
                            }
                            assert!(processor.blur_cache.lock().unwrap().bytes <= BLUR_CACHE_BYTES);
                        }
                    }
                }
            }
        }
    }
    println!(
        "{comparisons} GPU comparisons passed: exact 8/16-bit exports, preview delta <= 1/255, masks, ROI, tiles and source changes"
    );
}

/// Exports upload their masks layer by layer (`MaskUploader`, any order)
/// and free the bitmaps before rendering: the pixels must be those of a
/// render from the bitmaps, 8- and 16-bit, byte and half-float arrays.
#[test]
#[ignore = "requires a native GPU; streamed export masks against bitmaps"]
fn uploaded_masks_render_like_bitmaps() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2304, 512).unwrap();
    let (width, height) = (640u32, 400u32);
    let view = input(&context, width, height, 17);
    let coverage: Vec<f32> = (0..width * height)
        .map(|i| 0.1 + 0.7 * (i % width) as f32 / width as f32)
        .collect();
    let wide = Arc::new(MaskBitmap::from_coverage(width, height, &coverage));
    let bytes = upload_test_masks(width, height, 3);
    let profile = layer_sums_profile(0.2, 0.5);
    let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
    let mut checked = 0;
    for masks in [
        bytes.clone(),
        vec![
            Arc::clone(&bytes[0]),
            Arc::clone(&wide),
            Arc::clone(&bytes[2]),
        ],
        vec![Arc::clone(&bytes[1])],
    ] {
        for precision in [
            RenderOutputPrecision::EightBit,
            RenderOutputPrecision::SixteenBit,
        ] {
            let render = |masks: &[Arc<MaskBitmap>], uploaded: Option<&UploadedMasks>| {
                let (pixels, ..) = processor
                    .run_in_window(
                        &view,
                        width,
                        height,
                        Roi {
                            x: 0,
                            y: 0,
                            width,
                            height,
                        },
                        RenderRequest {
                            adjustments: adjustments.clone(),
                            mask_bitmaps: masks,
                            lut: None,
                            roi: None,
                        },
                        false,
                        precision,
                        ReadbackShader::Export,
                        None,
                        None,
                        uploaded,
                    )
                    .unwrap();
                match pixels {
                    RenderedPixels::U8(pixels) => pixels.iter().map(|v| *v as u16).collect(),
                    RenderedPixels::U16(pixels) => pixels,
                }
            };
            let expected = render(&masks, None);
            let wide_array = masks.iter().any(|mask| mask.wide().is_some());
            let mut uploader = MaskUploader::new(&context, width, height, masks.len(), wide_array);
            // Last layer first, as parallel generators may finish.
            for index in (0..masks.len()).rev() {
                uploader.push_at(&context, index, &masks[index]).unwrap();
            }
            let uploaded = uploader.finish(&context).unwrap();
            assert!(
                render(&[], Some(&uploaded)) == expected,
                "{} layers",
                masks.len()
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 6);
    // A layer missing or the array's precision wrong is an error, not a render.
    let mut uploader = MaskUploader::new(&context, width, height, 2, false);
    uploader.push_at(&context, 0, &bytes[0]).unwrap();
    assert!(uploader.push_at(&context, 1, &wide).is_err());
    assert!(uploader.finish(&context).is_err());
    let mut uploader = MaskUploader::new(&context, width, height, 1, true);
    uploader.push_at(&context, 0, &bytes[0]).unwrap();
    assert!(uploader.finish(&context).is_err());
}

/// 16-bit masks (`maskVersion` 2) in the half-float array: cached and fully
/// prepared renders, export and display paths agree as for byte masks, a
/// byte layer beside a 16-bit one keeps its pixels, and the 16-bit gradient
/// reaches the shader finer than its bytes.
#[test]
#[ignore = "requires a native GPU; compares 16-bit mask renders across paths"]
fn wide_mask_renders_match_across_paths() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2304, 512).unwrap();
    let (width, height) = (640u32, 400u32);
    let view = input(&context, width, height, 5);
    // A gentle ramp over the whole width: 8-bit steps every 2.5 px.
    let coverage: Vec<f32> = (0..width * height)
        .map(|i| 0.2 + 0.4 * (i % width) as f32 / width as f32)
        .collect();
    let wide = Arc::new(MaskBitmap::from_coverage(width, height, &coverage));
    let bytes = masked((*wide).clone().into_bytes());
    let byte_layer = masked(ImageBuffer::from_fn(width, height, |_, y| {
        Luma([255 - ((y % 127) * 2) as u8])
    }));
    let profile = json!({"masks":[
        {"id":"a","name":"A","visible":true,"invert":false,"opacity":100,"subMasks":[all_sub_mask()],"adjustments":{"exposure":3.0,"saturation":40}},
        {"id":"b","name":"B","visible":true,"invert":false,"opacity":100,"subMasks":[all_sub_mask()],"adjustments":{"highlights":-35,"curves":custom_curve()}}
    ]});
    let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
    prepare_preview_pipeline(&processor, preview_features(&adjustments));
    let render = |masks: &[Arc<MaskBitmap>], display: bool, precision| {
        processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: masks,
                    lut: None,
                    roi: None,
                },
                display,
                precision,
            )
            .unwrap()
    };
    let u16_pixels = |pixels: RenderedPixels| match pixels {
        RenderedPixels::U16(pixels) => pixels,
        RenderedPixels::U8(_) => unreachable!(),
    };
    for masks in [
        vec![Arc::clone(&wide), Arc::clone(&byte_layer)],
        vec![Arc::clone(&bytes), Arc::clone(&byte_layer)],
    ] {
        processor
            .force_full_preparation
            .store(false, Ordering::Relaxed);
        let first = u16_pixels(render(&masks, false, RenderOutputPrecision::SixteenBit).0);
        let cached = u16_pixels(render(&masks, false, RenderOutputPrecision::SixteenBit).0);
        processor
            .force_full_preparation
            .store(true, Ordering::Relaxed);
        let reference = u16_pixels(render(&masks, false, RenderOutputPrecision::SixteenBit).0);
        processor
            .force_full_preparation
            .store(false, Ordering::Relaxed);
        assert_eq!(first, reference);
        assert_eq!(cached, reference);
        let (pixels, ..) = render(&masks, false, RenderOutputPrecision::EightBit);
        let RenderedPixels::U8(export) = pixels else {
            unreachable!()
        };
        let (_, w, h, x, y) = render(&masks, true, RenderOutputPrecision::EightBit);
        let preview = read_texture_data_roi(
            &context.device,
            &context.queue,
            &processor.test_working_texture(),
            wgpu::Origin3d { x, y, z: 0 },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            4,
        )
        .unwrap();
        let max_delta = preview
            .iter()
            .zip(&export)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(max_delta <= 1, "preview max channel delta {max_delta}");
    }
    // On a flat grey photo, along a row the 16-bit ramp gives more distinct
    // output levels than its bytes (where every 2.5 px share one level).
    let grey = grey_texture(
        &context,
        width,
        height,
        &vec![0.18; (width * height) as usize],
    );
    let row_levels = |masks: &[Arc<MaskBitmap>]| {
        let (pixels, ..) = processor
            .run(
                &grey,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: masks,
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        let pixels = u16_pixels(pixels);
        let row = &pixels[200 * width as usize * 4..201 * width as usize * 4];
        row.chunks(4)
            .map(|p| p[1])
            .collect::<std::collections::BTreeSet<u16>>()
            .len()
    };
    let fine = row_levels(&[Arc::clone(&wide), Arc::clone(&byte_layer)]);
    let coarse = row_levels(&[Arc::clone(&bytes), Arc::clone(&byte_layer)]);
    assert!(fine > coarse * 2, "16-bit {fine} levels vs bytes {coarse}");
    println!("wide masks: {fine} output levels per row vs {coarse} from bytes");
}

fn prepare_preview_pipeline(processor: &GpuProcessor, features: usize) {
    processor.preview_pipelines[features].get_or_init(|| {
        create_preview_pipeline(
            &processor.context.device,
            &processor.preview_shader,
            &processor.preview_layout,
            features,
            true,
            true,
            false,
            false,
        )
        .unwrap()
    });
}

fn prepare_layer_sum_pipeline(processor: &GpuProcessor, features: usize) {
    processor.layer_sum_pipelines[features].get_or_init(|| {
        create_preview_pipeline(
            &processor.context.device,
            &processor.preview_shader,
            &processor.preview_layout,
            features,
            true,
            true,
            true,
            false,
        )
        .unwrap()
    });
}

fn prepare_legacy_curve_pipeline(processor: &GpuProcessor, features: usize) {
    processor.legacy_curve_pipelines[features].get_or_init(|| {
        create_preview_pipeline(
            &processor.context.device,
            &processor.preview_shader,
            &processor.preview_layout,
            features,
            false,
            false,
            false,
            false,
        )
        .unwrap()
    });
}

fn prepare_unprepared_layer_pipeline(processor: &GpuProcessor, features: usize) {
    processor.unprepared_layer_pipelines[features].get_or_init(|| {
        create_preview_pipeline(
            &processor.context.device,
            &processor.preview_shader,
            &processor.preview_layout,
            features,
            true,
            false,
            false,
            false,
        )
        .unwrap()
    });
}

#[test]
#[ignore = "native GPU timing probe; not a portable acceptance benchmark"]
fn preview_render_timing_probe() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2304, 1792).unwrap();
    let view = input(&context, 2304, 1728, 3);
    for (name, profile) in [
        ("exposure", json!({"exposure":1})),
        ("shadows", json!({"shadows":50})),
        ("structure", json!({"structure":50})),
    ] {
        for full in [true, false] {
            processor
                .force_full_preparation
                .store(full, Ordering::Relaxed);
            for frame in 0..8 {
                let start = Instant::now();
                processor
                    .run(
                        &view,
                        2304,
                        1728,
                        RenderRequest {
                            adjustments: get_all_adjustments_from_json(&profile, true, Some(1)),
                            mask_bitmaps: &[],
                            lut: None,
                            roi: None,
                        },
                        true,
                        RenderOutputPrecision::EightBit,
                    )
                    .unwrap();
                let submit_ms = start.elapsed().as_secs_f64() * 1000.0;
                context
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                println!(
                    "{}",
                    json!({"profile":name,"full_preparation":full,"frame":frame,"submit_ms":submit_ms,"complete_ms":start.elapsed().as_secs_f64()*1000.0})
                );
            }
        }
    }
}

#[test]
#[ignore = "native GPU timing probe; compares luminance noise reductions 1 and 2"]
fn luma_noise_version_timing_probe() {
    let context = context();
    // Warm display frames at preview size and full-resolution 16-bit
    // readbacks of a noisy photo (the speck step active) at the RAW detail
    // defaults; the luminance amount moves a little every frame so the
    // detail stage is computed again, as during a slider drag. Versions 1
    // and 2 alternate within one process.
    for (width, height, display, frames) in [(2304, 1728, true, 24), (6000, 4000, false, 6)] {
        let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
        let view = input(&context, width, height, 3);
        let mut times = [Vec::new(), Vec::new()];
        for frame in 0..frames + 2 {
            for version in [1, 2] {
                let profile = json!({"sharpness": 40, "colorNoiseReduction": 25,
                    "lumaNoiseReduction": 15.0 + 0.01 * frame as f64, "lumaNoiseVersion": version});
                let mut adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
                adjustments.global.noise_level = 0.02;
                let start = Instant::now();
                processor
                    .run(
                        &view,
                        width,
                        height,
                        RenderRequest {
                            adjustments,
                            mask_bitmaps: &[],
                            lut: None,
                            roi: None,
                        },
                        display,
                        if display {
                            RenderOutputPrecision::EightBit
                        } else {
                            RenderOutputPrecision::SixteenBit
                        },
                    )
                    .unwrap();
                context
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                // The first two frames warm shaders and caches.
                if frame >= 2 {
                    times[version - 1].push(start.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
        let median = |t: &mut Vec<f64>| {
            t.sort_by(f64::total_cmp);
            t[t.len() / 2]
        };
        let (v1, v2) = (median(&mut times[0]), median(&mut times[1]));
        println!(
            "{}",
            json!({"size":format!("{width}x{height}"),"v1_ms":v1,"v2_ms":v2,
                "change_pct":(v2 / v1 - 1.0) * 100.0,"frames":frames})
        );
    }
}

#[test]
#[ignore = "native GPU timing probe; compares sharpening responses 1 and 2"]
fn sharpen_version_timing_probe() {
    let context = context();
    // Warm display frames at preview size and full-resolution 16-bit
    // readbacks; the amount moves a little every frame so the detail stage
    // is computed again, as during a slider drag. The previous and the
    // newest response alternate within one process.
    for (width, height, display, frames) in [(2304, 1728, true, 24), (6000, 4000, false, 6)] {
        let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
        let view = input(&context, width, height, 3);
        for (name, base) in [
            ("a40d25", json!({"sharpness":40})),
            ("a80d25", json!({"sharpness":80})),
            ("a150d25", json!({"sharpness":150})),
            ("a80d100", json!({"sharpness":80,"sharpenDetail":100})),
            (
                "a120d100r2",
                json!({"sharpness":120,"sharpenDetail":100,"sharpenRadius":2.0}),
            ),
            ("a120d25r0.5", json!({"sharpness":120,"sharpenRadius":0.5})),
            ("a40d25r0.5", json!({"sharpness":40,"sharpenRadius":0.5})),
            ("a80d25r2", json!({"sharpness":80,"sharpenRadius":2.0})),
            ("a40d25m50", json!({"sharpness":40,"sharpenMasking":50})),
            (
                "a80d100m80",
                json!({"sharpness":80,"sharpenDetail":100,"sharpenMasking":80}),
            ),
        ] {
            let mut times = [Vec::new(), Vec::new()];
            for frame in 0..frames + 2 {
                let newest = rawtonic_engine::image_processing::SHARPEN_VERSION;
                for (slot, version) in [newest - 1, newest].into_iter().enumerate() {
                    let mut profile = base.clone();
                    let amount = profile["sharpness"].as_f64().unwrap();
                    // Amount 40 stays exact (responses 1 and 2 share its path).
                    if amount != 40.0 {
                        profile["sharpness"] = json!(amount + 0.01 * frame as f64);
                    } else {
                        profile["sharpnessThreshold"] = json!(15.0 + 0.01 * frame as f64);
                    }
                    profile["sharpenVersion"] = json!(version);
                    let start = Instant::now();
                    processor
                        .run(
                            &view,
                            width,
                            height,
                            RenderRequest {
                                adjustments: get_all_adjustments_from_json(&profile, true, Some(1)),
                                mask_bitmaps: &[],
                                lut: None,
                                roi: None,
                            },
                            display,
                            if display {
                                RenderOutputPrecision::EightBit
                            } else {
                                RenderOutputPrecision::SixteenBit
                            },
                        )
                        .unwrap();
                    context
                        .device
                        .poll(wgpu::PollType::wait_indefinitely())
                        .unwrap();
                    // The first two frames warm shaders and caches.
                    if frame >= 2 {
                        times[slot].push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                }
            }
            let median = |t: &mut Vec<f64>| {
                t.sort_by(f64::total_cmp);
                t[t.len() / 2]
            };
            let (previous, newest) = (median(&mut times[0]), median(&mut times[1]));
            println!(
                "{}",
                json!({"size":format!("{width}x{height}"),"setting":name,"previous_ms":previous,
                    "newest_ms":newest,"change_pct":(newest / previous - 1.0) * 100.0,"frames":frames})
            );
        }
    }
}

#[test]
#[ignore = "native GPU timing probe; synthetic input, not a portable acceptance benchmark"]
fn editing_tools_timing_probe() {
    let context = context();
    let (width, height) = (2304, 1728);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 3);
    let ranges: Vec<_> = (0..8)
        .map(|i| {
            json!({"id": format!("r{i}"), "hue": i as f64 * 45.0, "chroma": 0.1,
            "lightness": 0.6, "hueShift": 30, "saturation": 20, "hueUniformity": 40})
        })
        .collect();
    let wheel = |h: i32| json!({"hue": h, "saturation": 40, "luminance": 10});
    let tools = json!({
        "colorGrading": {"shadows": wheel(220), "midtones": wheel(30), "highlights": wheel(45),
            "global": wheel(200), "blending": 60, "balance": 10},
        "blackAndWhite": true, "grayMixer": {"red": -40, "yellow": 30, "blue": -20},
        "levels": {"rgb": {"inBlack": 10, "gamma": 1.2}, "blue": {"outWhite": 240}},
        "regionCurve": {"shadows": 20, "darks": -10, "lights": 15, "highlights": -20},
        "colorRanges": ranges,
        "colorCalibration": {"redHue": 20, "blueSaturation": 30, "shadowsTint": 10},
        "defringePurpleAmount": 10, "defringeGreenAmount": 5,
        "moireAmount": 60,
        "vignetteAmount": -40
    });
    let mut profiles = vec![("neutral".to_string(), json!({}))];
    for key in tools.as_object().unwrap().keys() {
        if key == "grayMixer" {
            continue;
        }
        let mut profile = json!({key: tools[key].clone()});
        if key == "blackAndWhite" {
            profile["grayMixer"] = tools["grayMixer"].clone();
        }
        profiles.push((key.clone(), profile));
    }
    profiles.push(("all".to_string(), tools.clone()));
    for (name, profile) in &profiles {
        let mut times = Vec::new();
        for frame in 0..10 {
            // A different exposure per frame defeats the unchanged-frame reuse.
            let mut profile = profile.clone();
            profile["exposure"] = json!(frame as f64 * 0.001);
            let start = Instant::now();
            processor
                .run(
                    &view,
                    width,
                    height,
                    RenderRequest {
                        adjustments: get_all_adjustments_from_json(&profile, true, Some(2)),
                        mask_bitmaps: &[],
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
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        times.drain(..2);
        times.sort_by(f64::total_cmp);
        println!(
            "{}",
            json!({"profile": name, "median_complete_ms": times[times.len() / 2]})
        );
    }
}

#[test]
#[ignore = "native GPU timing probe; synthetic input, not pointer-to-photon or target-hardware acceptance"]
fn complex_preview_timing_probe() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 4096, 3072).unwrap();
    let profiles = [
        ("local_exposure", local_profile(json!({"exposure":0.75}))),
        (
            "local_tone",
            local_profile(json!({"shadows":35,"highlights":-25,"sharpness":30})),
        ),
        (
            "noise_reduction",
            json!({"lumaNoiseReduction":30,"colorNoiseReduction":25}),
        ),
        (
            "local_curve",
            local_profile(json!({"curves":custom_curve()})),
        ),
        (
            "local_curve_nr",
            local_profile(json!({"curves":custom_curve(),"lumaNoiseReduction":30})),
        ),
    ];
    for (name, width, height, roi) in [
        ("fit", 2304, 1728, None),
        (
            "large_source_roi",
            4096,
            3072,
            Some(Roi {
                x: 1900,
                y: 1300,
                width: 2000,
                height: 1400,
            }),
        ),
    ] {
        let view = input(&context, width, height, 3);
        let mask = masked(ImageBuffer::from_fn(width, height, |x, _| {
            Luma([((x % 127) * 2) as u8])
        }));
        for (profile_name, profile) in &profiles {
            let adjustments = get_all_adjustments_from_json(profile, true, Some(1));
            prepare_preview_pipeline(&processor, preview_features(&adjustments));
            let masks = if adjustments.mask_count > 0 {
                std::slice::from_ref(&mask)
            } else {
                &[]
            };
            // Alternate both orders, with two warm-up pairs outside the samples.
            // The previous masked/NR path used the general shader but already
            // reused selective blur preparation; retain that exact preparation.
            for frame in 0..14 {
                for generic in if frame % 2 == 0 {
                    [true, false]
                } else {
                    [false, true]
                } {
                    processor
                        .force_general_pipeline
                        .store(generic, Ordering::Relaxed);
                    let start = Instant::now();
                    processor
                        .run(
                            &view,
                            width,
                            height,
                            RenderRequest {
                                adjustments: adjustments.clone(),
                                mask_bitmaps: masks,
                                lut: None,
                                roi,
                            },
                            true,
                            RenderOutputPrecision::EightBit,
                        )
                        .unwrap();
                    let submit_ms = start.elapsed().as_secs_f64() * 1000.0;
                    context
                        .device
                        .poll(wgpu::PollType::wait_indefinitely())
                        .unwrap();
                    if frame >= 2 {
                        println!(
                            "{}",
                            json!({"scenario":name,"profile":profile_name,"pipeline":if generic {"previous"} else {"specialized"},"frame":frame-2,"source_width":width,"source_height":height,"submit_ms":submit_ms,"complete_ms":start.elapsed().as_secs_f64()*1000.0})
                        );
                    }
                }
            }
        }
    }
}

fn curve_mask_stack(count: usize, only_last: bool) -> serde_json::Value {
    let masks: Vec<_> = (0..count)
        .map(|i| {
            let curves = if only_last && i + 1 != count {
                json!({})
            } else if i % 2 == 0 {
                json!({"luma":[{"x":0,"y":12},{"x":112,"y":160},{"x":255,"y":245}],
                    "red":[{"x":0,"y":0},{"x":144,"y":104},{"x":255,"y":255}]})
            } else {
                json!({"luma":[{"x":0,"y":0},{"x":144,"y":92},{"x":255,"y":255}],
                    "blue":[{"x":0,"y":16},{"x":96,"y":140},{"x":255,"y":240}]})
            };
            json!({"id":format!("curve-{i}"),"name":format!("Curve {i}"),
                "visible":true,"invert":false,"opacity":100,"subMasks": [all_sub_mask()],
                "adjustments":{"curves":curves}})
        })
        .collect();
    json!({"masks":masks})
}

fn curve_regression_profiles() -> Vec<(&'static str, serde_json::Value)> {
    let sixteen_points: Vec<_> = (0..16)
        .map(|i| json!({"x":i * 17,"y":if i % 2 == 0 {i * 17} else {255 - i * 13}}))
        .collect();
    let channels = json!({
        "luma":[{"x":0,"y":8},{"x":64,"y":90},{"x":180,"y":145},{"x":255,"y":249}],
        "red":[{"x":0,"y":16},{"x":112,"y":75},{"x":255,"y":239}],
        "green":[{"x":0,"y":0},{"x":150,"y":200},{"x":255,"y":255}],
        "blue":[{"x":0,"y":255},{"x":128,"y":160},{"x":255,"y":0}]
    });
    let order_ab = curve_mask_stack(2, false);
    let mut order_ba = order_ab.clone();
    order_ba["masks"].as_array_mut().unwrap().reverse();
    let mut combined = local_profile(json!({"curves":channels,"sharpness":-25,
        "highlights":-30,"lumaNoiseReduction":20,"colorNoiseReduction":15}));
    combined["curves"] = custom_curve();
    vec![
        (
            "zero_points",
            json!({"curves":{"luma":[],"red":[],"green":[],"blue":[]}}),
        ),
        (
            "one_point",
            json!({"curves":{"luma":[{"x":33,"y":241}],
            "red":[{"x":0,"y":255}],"green":[{"x":200,"y":-10}],
            "blue":[{"x":255,"y":20}]}}),
        ),
        (
            "bypass_with_active_channel",
            json!({"curves":{"luma":[],
            "red":[{"x":0,"y":30},{"x":255,"y":220}],
            "green":[{"x":90,"y":210}],"blue":[]}}),
        ),
        (
            "moved_endpoints",
            json!({"curves":{"luma":[{"x":20,"y":30},{"x":235,"y":210}],
            "red":[{"x":40,"y":210},{"x":220,"y":20}]}}),
        ),
        // Endpoint returns are deliberately not clamped until apply_all_curves;
        // the channel curve must receive the out-of-range luma result first.
        (
            "unclamped_endpoints",
            json!({"curves":{"luma":[{"x":16,"y":-32},{"x":240,"y":300}],
            "red":[{"x":-64,"y":16},{"x":320,"y":239}],
            "blue":[{"x":-32,"y":220},{"x":300,"y":20}]}}),
        ),
        (
            "duplicate_knots",
            json!({"curves":{"luma":[{"x":0,"y":0},{"x":128,"y":30},
            {"x":128,"y":220},{"x":128.0001,"y":80},{"x":255,"y":255}]}}),
        ),
        (
            "unsorted_knots",
            json!({"curves":{"luma":[{"x":0,"y":20},{"x":160,"y":190},
            {"x":96,"y":35},{"x":255,"y":240}],
            "green":[{"x":200,"y":30},{"x":40,"y":220}]}}),
        ),
        (
            "sixteen_points",
            json!({"curves":{"luma":sixteen_points,"red":sixteen_points,
            "green":sixteen_points,"blue":sixteen_points}}),
        ),
        (
            "rgb_identity_tolerance",
            json!({"curves":{
            "luma":[{"x":0,"y":0},{"x":128,"y":160},{"x":255,"y":255}],
            "red":[{"x":0.09,"y":0.09},{"x":128,"y":128.49},{"x":254.91,"y":254.91}],
            "green":[{"x":0.1,"y":0.1},{"x":128,"y":128.49},{"x":255,"y":255}],
            "blue":[{"x":0,"y":0},{"x":128,"y":128.51},{"x":255,"y":255}]}}),
        ),
        (
            "near_identity_luma",
            json!({"curves":{"luma":[{"x":0,"y":0.09},{"x":255,"y":254.99}]}}),
        ),
        (
            "flat_and_turning_segments",
            json!({"curves":{"luma":[{"x":0,"y":80},{"x":32,"y":80},
            {"x":64,"y":220},{"x":128,"y":20},{"x":192,"y":20},{"x":255,"y":180}]}}),
        ),
        ("global_channels", json!({"curves":channels})),
        ("local_channels", local_profile(json!({"curves":channels}))),
        (
            "local_one_point",
            local_profile(json!({"curves":{"luma":[{"x":10,"y":200}],
            "red":[{"x":0,"y":20},{"x":255,"y":210}],"green":[],"blue":[]}})),
        ),
        ("all_features", combined),
        ("mask_order_ab", order_ab),
        ("mask_order_ba", order_ba),
        ("twenty_masks", curve_mask_stack(20, false)),
        ("last_of_32_masks", curve_mask_stack(32, true)),
    ]
}

fn curve_regression_input(
    context: &GpuContext,
    width: u32,
    height: u32,
    reverse: bool,
) -> wgpu::TextureView {
    let values = [
        0.0, 0.00001, 0.09, 0.1, 15.99, 16.0, 16.01, 19.99, 20.0, 20.01, 31.99, 32.0, 32.01, 63.99,
        64.0, 64.01, 95.99, 96.0, 96.01, 111.99, 112.0, 112.01, 127.9999, 128.0, 128.00005,
        128.0001, 128.0002, 143.99, 144.0, 144.01, 159.99, 160.0, 160.01, 179.99, 180.0, 180.01,
        234.99, 235.0, 235.01, 239.99, 240.0, 240.01, 254.9, 254.91, 254.99, 255.0, 255.01, 300.0,
        510.0,
    ];
    let data: Vec<f32> = (0..width * height)
        .flat_map(|index| {
            let index = if reverse {
                width * height - index - 1
            } else {
                index
            };
            let sample = |offset: u32| {
                if (index / width).is_multiple_of(2) {
                    values[((index + offset) as usize) % values.len()] / 255.0
                } else {
                    ((index + offset) % 2049) as f32 / 2048.0
                }
            };
            [sample(0), sample(17), sample(31), 1.0]
        })
        .collect();
    context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Curve knots and endpoint regression input"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&data),
        )
        .create_view(&Default::default())
}

#[test]
#[ignore = "requires a native GPU; compares direct curve reads with the previous specialized shader"]
fn direct_curve_preview_matches_legacy_specialization() {
    let context = context();
    let (width, height) = (2176, 40);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let profiles = curve_regression_profiles();
    for (_, profile) in &profiles {
        let features = preview_features(&get_all_adjustments_from_json(profile, true, Some(1)));
        prepare_preview_pipeline(&processor, features);
        prepare_legacy_curve_pipeline(&processor, features);
    }
    // Channel ramps include black, white, HDR values and many values on either
    // side of the tightly spaced knots. A second source also tests source reuse.
    let views = [
        curve_regression_input(&context, width, height, false),
        curve_regression_input(&context, width, height, true),
    ];
    let masks: Vec<_> = (0..MAX_MASKS)
        .map(|i| {
            masked(ImageBuffer::from_fn(width, height, |x, y| {
                let coverage = match (x + y + i as u32) % 5 {
                    0 => 0,
                    1 => 1,
                    2 => 127,
                    3 => 254,
                    _ => 255,
                };
                Luma([coverage])
            }))
        })
        .collect();
    let mut comparisons = 0;
    let mut ordered_reference = None;
    for (source_index, view) in views.iter().enumerate() {
        for (raw, mapper) in [(true, 1), (true, 0), (false, 0)] {
            for roi in [
                None,
                Some(Roi {
                    x: 17,
                    y: 3,
                    width: width - 33,
                    height: height - 6,
                }),
            ] {
                for (name, profile) in &profiles {
                    let adjustments = get_all_adjustments_from_json(profile, raw, Some(mapper));
                    let render = |legacy| {
                        processor
                            .force_legacy_curves
                            .store(legacy, Ordering::Relaxed);
                        let (_, w, h, x, y) = processor
                            .run(
                                view,
                                width,
                                height,
                                RenderRequest {
                                    adjustments: adjustments.clone(),
                                    mask_bitmaps: &masks[..adjustments.mask_count as usize],
                                    lut: None,
                                    roi,
                                },
                                true,
                                RenderOutputPrecision::EightBit,
                            )
                            .unwrap();
                        read_texture_data_roi(
                            &context.device,
                            &context.queue,
                            &processor.test_working_texture(),
                            wgpu::Origin3d { x, y, z: 0 },
                            wgpu::Extent3d {
                                width: w,
                                height: h,
                                depth_or_array_layers: 1,
                            },
                            4,
                        )
                        .unwrap()
                    };
                    let reference = render(true);
                    let direct = render(false);
                    assert_eq!(direct.len(), reference.len());
                    let max_delta = direct
                        .iter()
                        .zip(&reference)
                        .map(|(a, b)| a.abs_diff(*b))
                        .max()
                        .unwrap();
                    assert!(
                        max_delta <= 1,
                        "direct curve delta {max_delta}: {name}, source {source_index}, raw {raw}, mapper {mapper}, roi {}",
                        roi.is_some()
                    );
                    assert!(
                        direct
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .all(|pixel| pixel[3] == 255),
                        "alpha: {name}"
                    );
                    if source_index == 0 && raw && mapper == 1 && roi.is_none() {
                        if *name == "mask_order_ab" {
                            ordered_reference = Some(reference.clone());
                        } else if *name == "mask_order_ba" {
                            assert_ne!(
                                ordered_reference.as_ref().unwrap(),
                                &reference,
                                "mask order fixture must exercise sequential composition"
                            );
                        }
                    }
                    comparisons += 1;
                }
            }
        }
    }
    processor
        .force_legacy_curves
        .store(false, Ordering::Relaxed);
    println!(
        "{comparisons} direct/legacy specialized curve comparisons passed: endpoints, knots, bypasses, 16 points, RGB identities, 2/20/32 masks, order, ROI and tiles"
    );
}

#[test]
#[ignore = "native GPU timing probe; synthetic curves compared with the previous specialized pipeline"]
fn curve_preview_timing_probe() {
    let context = context();
    let (width, height) = (2304, 1728);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 3);
    let masks: Vec<_> = (0..20)
        .map(|i| {
            masked(ImageBuffer::from_fn(width, height, |x, y| {
                Luma([(((x + y * 3 + i * 7) % 127) * 2) as u8])
            }))
        })
        .collect();
    for (name, profile) in [
        ("global_curve", json!({"curves":custom_curve()})),
        (
            "local_curve",
            local_profile(json!({"curves":custom_curve()})),
        ),
        (
            "local_curve_nr",
            local_profile(json!({"curves":custom_curve(),"lumaNoiseReduction":30})),
        ),
        ("twenty_local_curves", curve_mask_stack(20, false)),
    ] {
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        let features = preview_features(&adjustments);
        prepare_preview_pipeline(&processor, features);
        prepare_legacy_curve_pipeline(&processor, features);
        // Hold blur preparation, dimensions and every other specialization
        // constant. Compilation and two alternating warm-up pairs are excluded.
        for frame in 0..14 {
            for legacy in if frame % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                processor
                    .force_legacy_curves
                    .store(legacy, Ordering::Relaxed);
                let start = Instant::now();
                processor
                    .run(
                        &view,
                        width,
                        height,
                        RenderRequest {
                            adjustments: adjustments.clone(),
                            mask_bitmaps: &masks[..adjustments.mask_count as usize],
                            lut: None,
                            roi: None,
                        },
                        true,
                        RenderOutputPrecision::EightBit,
                    )
                    .unwrap();
                let submit_ms = start.elapsed().as_secs_f64() * 1000.0;
                context
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                if frame >= 2 {
                    println!(
                        "{}",
                        json!({"scenario":"fit","profile":name,
                        "pipeline":if legacy {"legacy_specialized"} else {"direct_curves"},
                        "frame":frame-2,"source_width":width,"source_height":height,
                        "mask_count":adjustments.mask_count,"submit_ms":submit_ms,
                        "complete_ms":start.elapsed().as_secs_f64()*1000.0})
                    );
                }
            }
        }
    }
    processor
        .force_legacy_curves
        .store(false, Ordering::Relaxed);
}

fn mask_upload_profile(count: usize) -> serde_json::Value {
    let masks: Vec<_> = (0..count)
        .map(|i| {
            json!({
                "id":format!("upload-{i}"),"name":format!("Upload {i}"),
                "visible":true,"invert":false,"opacity":100,"subMasks": [all_sub_mask()],
                "adjustments":{"exposure":if i % 2 == 0 {0.65} else {-0.35},
                    "temperature":if i % 2 == 0 {12} else {-20}}
            })
        })
        .collect();
    json!({"masks":masks})
}

fn upload_test_masks(width: u32, height: u32, count: usize) -> Vec<Arc<MaskBitmap>> {
    (0..count)
        .map(|i| {
            masked(ImageBuffer::from_fn(width, height, |x, y| {
                Luma([((x * (i as u32 + 1) + y * 3 + i as u32 * 41) % 256) as u8])
            }))
        })
        .collect()
}

// The upload oracle holds shader specialization and blur preparation constant.
// force_mask_upload selects only the previous concatenate-and-upload path.
fn assert_mask_upload_frame(
    processor: &GpuProcessor,
    view: &wgpu::TextureView,
    dimensions: (u32, u32),
    masks: &[Arc<MaskBitmap>],
    roi: Option<Roi>,
    output_expectation: (bool, usize),
    label: &str,
) -> Vec<u8> {
    let (width, height) = dimensions;
    let (for_display, expected_layers) = output_expectation;
    let adjustments =
        get_all_adjustments_from_json(&mask_upload_profile(masks.len()), true, Some(1));
    prepare_preview_pipeline(processor, preview_features(&adjustments));
    let render = || {
        let (pixels, w, h, x, y) = processor
            .run(
                view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: masks,
                    lut: None,
                    roi,
                },
                for_display,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        if for_display {
            read_texture_data_roi(
                &processor.context.device,
                &processor.context.queue,
                &processor.test_working_texture(),
                wgpu::Origin3d { x, y, z: 0 },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                4,
            )
            .unwrap()
        } else {
            match pixels {
                RenderedPixels::U8(pixels) => pixels,
                _ => unreachable!(),
            }
        }
    };
    processor.force_mask_upload.store(false, Ordering::Relaxed);
    let layers_before = processor.mask_upload_layers.load(Ordering::Relaxed);
    let bytes_before = processor.mask_upload_bytes.load(Ordering::Relaxed);
    let actual = render();
    assert_eq!(
        processor.mask_upload_layers.load(Ordering::Relaxed) - layers_before,
        expected_layers,
        "uploaded layers: {label}"
    );
    assert_eq!(
        processor.mask_upload_bytes.load(Ordering::Relaxed) - bytes_before,
        width as usize * height as usize * expected_layers,
        "uploaded bytes: {label}"
    );
    processor.force_mask_upload.store(true, Ordering::Relaxed);
    let reference = render();
    processor.force_mask_upload.store(false, Ordering::Relaxed);
    assert_eq!(actual, reference, "cached mask pixels: {label}");
    actual
}

#[test]
#[ignore = "requires a native GPU; compares mask cache pixels and exact upload counts"]
fn mask_texture_cache_tracks_content_and_preserves_foreground() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 512, 256).unwrap();
    // Width deliberately does not align to 256-byte rows.
    let (width, height) = (321, 181);
    let view = input(&context, width, height, 3);
    let second_view = input(&context, width, height, 47);
    let mut masks = upload_test_masks(width, height, 2);
    let frame = |masks: &[Arc<MaskBitmap>], writes, label| {
        assert_mask_upload_frame(
            &processor,
            &view,
            (width, height),
            masks,
            None,
            (true, writes),
            label,
        )
    };
    let original = frame(&masks, 2, "cold odd rows");
    assert!(processor.mask_cache_retained_bytes() > 0);
    assert_eq!(original, frame(&masks, 0, "warm"));
    let cloned_handles = masks.clone();
    assert_eq!(original, frame(&cloned_handles, 0, "Arc clones"));
    Arc::make_mut(&mut masks[0]).bytes_mut().fill(255);
    let edited = frame(&masks, 1, "one changed layer");
    assert_ne!(original, edited, "edit fixture must change pixels");
    masks.swap(0, 1);
    let reordered = frame(&masks, 2, "reordered layers");
    assert_ne!(edited, reordered, "ordering fixture must change pixels");
    frame(&masks[..1], 1, "two layers to one");
    frame(&masks, 2, "one layer to two");
    frame(&[], 0, "remove all masks");
    assert_eq!(
        processor.mask_cache_retained_bytes(),
        0,
        "empty foreground releases cache"
    );
    frame(&masks[..1], 1, "add after empty");
    frame(&masks, 2, "add second layer");
    assert_mask_upload_frame(
        &processor,
        &view,
        (width, height),
        &masks,
        Some(Roi {
            x: 19,
            y: 11,
            width: 253,
            height: 149,
        }),
        (true, 0),
        "ROI change",
    );
    assert_mask_upload_frame(
        &processor,
        &second_view,
        (width, height),
        &masks,
        None,
        (true, 0),
        "source switch with same masks",
    );
    let retained = processor.mask_cache_retained_bytes();
    let background_masks = upload_test_masks(257, 129, 2);
    let background_view = input(&context, 257, 129, 29);
    assert_mask_upload_frame(
        &processor,
        &background_view,
        (257, 129),
        &background_masks,
        None,
        (false, 2),
        "background with other dimensions",
    );
    assert_mask_upload_frame(
        &processor,
        &background_view,
        (257, 129),
        &[],
        None,
        (false, 0),
        "background without masks",
    );
    assert_eq!(
        processor.mask_cache_retained_bytes(),
        retained,
        "background preserves foreground"
    );
    frame(&masks, 0, "foreground after background");
    assert_mask_upload_frame(
        &processor,
        &background_view,
        (257, 129),
        &background_masks,
        None,
        (true, 2),
        "foreground dimensions change",
    );
    assert_mask_upload_frame(
        &processor,
        &background_view,
        (257, 129),
        &background_masks,
        None,
        (true, 0),
        "new dimensions warm",
    );
    frame(&masks, 2, "restore dimensions");

    // Exercise the production oversized path without allocating hundreds of MiB.
    let budget = processor.mask_cache_budget.swap(1, Ordering::Relaxed);
    frame(&masks, 2, "oversized transient");
    assert_eq!(
        processor.mask_cache_retained_bytes(),
        0,
        "oversized request releases old entry"
    );
    processor.mask_cache_budget.store(budget, Ordering::Relaxed);
    frame(&masks, 2, "cache after oversized request");
    assert!(processor.mask_cache_retained_bytes() <= budget);

    let invalid = upload_test_masks(width - 1, height, 1);
    let result = processor.run(
        &view,
        width,
        height,
        RenderRequest {
            adjustments: get_all_adjustments_from_json(&mask_upload_profile(1), true, Some(1)),
            mask_bitmaps: &invalid,
            lut: None,
            roi: None,
        },
        true,
        RenderOutputPrecision::EightBit,
    );
    assert!(
        result.is_err(),
        "mismatched mask dimensions must return an error"
    );
    println!(
        "20 mask-cache pixel comparisons passed: cold/warm, Arc identity, edit, reorder, counts, odd rows, ROI, source, background, dimensions and memory budget; invalid dimensions rejected"
    );
}

#[test]
#[ignore = "native GPU timing probe; synthetic CPU cache-hit preparation through GPU completion"]
fn mask_upload_timing_probe() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 3072, 2048).unwrap();
    let adjustments = get_all_adjustments_from_json(&mask_upload_profile(20), true, Some(1));
    prepare_preview_pipeline(&processor, preview_features(&adjustments));
    for (scenario, width, height, roi) in [
        ("fit", 2304, 1728, None),
        (
            "large_source_roi",
            3072,
            2048,
            Some(Roi {
                x: 900,
                y: 500,
                width: 2000,
                height: 1400,
            }),
        ),
    ] {
        let view = input(&context, width, height, 3);
        let masks = upload_test_masks(width, height, 20);
        // Both scenarios fit the combined retained CPU + GPU budget. The old
        // timed branch reproduces GrayImage clones on CPU-cache hits, then the
        // previous concatenation/upload; the new branch clones Arc handles.
        // JSON hydration, mask generation, IPC and display presentation are
        // outside this measurement. Shader and blur paths are identical.
        for frame in 0..14 {
            for previous in if frame % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                processor
                    .force_mask_upload
                    .store(previous, Ordering::Relaxed);
                let layers_before = processor.mask_upload_layers.load(Ordering::Relaxed);
                let bytes_before = processor.mask_upload_bytes.load(Ordering::Relaxed);
                let start = Instant::now();
                let request_masks: Vec<_> = if previous {
                    masks
                        .iter()
                        .map(|mask| Arc::new((**mask).clone()))
                        .collect()
                } else {
                    masks.clone()
                };
                processor
                    .run(
                        &view,
                        width,
                        height,
                        RenderRequest {
                            adjustments: adjustments.clone(),
                            mask_bitmaps: &request_masks,
                            lut: None,
                            roi,
                        },
                        true,
                        RenderOutputPrecision::EightBit,
                    )
                    .unwrap();
                let submit_ms = start.elapsed().as_secs_f64() * 1000.0;
                context
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                let complete_ms = start.elapsed().as_secs_f64() * 1000.0;
                let uploaded_layers =
                    processor.mask_upload_layers.load(Ordering::Relaxed) - layers_before;
                let uploaded_bytes =
                    processor.mask_upload_bytes.load(Ordering::Relaxed) - bytes_before;
                if frame >= 2 {
                    assert_eq!(uploaded_layers, if previous { 20 } else { 0 });
                    println!(
                        "{}",
                        json!({"scenario":scenario,"profile":"twenty_local_masks",
                        "pipeline":if previous {"previous_mask_preparation"} else {"cached_masks"},
                        "measurement":"synthetic_cpu_cache_hit_to_gpu_completion",
                        "frame":frame-2,"source_width":width,"source_height":height,
                        "mask_count":20,"uploaded_layers":uploaded_layers,"uploaded_bytes":uploaded_bytes,
                        "retained_mask_bytes":processor.mask_cache_retained_bytes(),
                        "submit_ms":submit_ms,"complete_ms":complete_ms})
                    );
                }
            }
        }
    }
    processor.force_mask_upload.store(false, Ordering::Relaxed);
}

#[test]
#[ignore = "requires a native GPU; compares prepared layer processing with the previous direct-curve preview"]
fn prepared_layers_match_previous_preview() {
    let context = context();
    let (width, height) = (2176, 40);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let mut profiles = curve_regression_profiles();
    let mixed = json!({"exposure":3.0,"brightness":1.3,"masks":[
        local_mask("neutral-first",json!({})),
        local_mask("grade",json!({"colorGrading":{"shadows":{"hue":200,"saturation":80,"luminance":-20},"global":{"hue":30,"saturation":50,"luminance":10},"blending":65,"balance":25}})),
        local_mask("curve",json!({"curves":custom_curve()})),
        local_mask("moire",json!({"moireAmount":100,"moirePattern":60})),
        local_mask("hsl",json!({"hsl":{"red":{"hue":20,"saturation":-30,"luminance":15}},"temperature":-20})),
        local_mask("ranges",json!({"colorRanges":[
            {"id":"a","hue":40.0,"chroma":0.1,"lightness":0.6,"hueRange":60,"chromaRange":100,"hueShift":60,"saturation":-40},
            {"id":"b","hue":250.0,"chroma":0.08,"lightness":0.4,"hueRange":45,"lightnessUniformity":80}
        ]})),
        local_mask("neutral-last",json!({}))
    ]});
    let mut reversed = mixed.clone();
    reversed["masks"].as_array_mut().unwrap().reverse();
    profiles.extend([
        ("mixed_neutral_curve_grade_hsl", mixed),
        ("mixed_reversed", reversed),
        // A layer whose only slider is Defringe (the general shader's loop
        // applies it; the prepared one must not skip the layer).
        ("defringe_only", json!({"masks":[local_mask("defringe",json!({"defringe":80}))]})),
        ("grading_threshold", json!({"masks":[
            local_mask("below",json!({"colorGrading":{"global":{"hue":200,"saturation":0.49}}})),
            local_mask("at",json!({"colorGrading":{"global":{"hue":200,"saturation":0.5}}})),
            local_mask("above",json!({"colorGrading":{"global":{"hue":200,"saturation":0.51}}})),
            local_mask("negative",json!({"colorGrading":{"global":{"hue":200,"saturation":-100}}}))
        ]})),
    ]);
    for (_, profile) in &profiles {
        let features = preview_features(&get_all_adjustments_from_json(profile, true, Some(1)));
        prepare_preview_pipeline(&processor, features);
        prepare_unprepared_layer_pipeline(&processor, features);
    }
    // Channel ramps include black, white, HDR values and many values on either
    // side of the tightly spaced knots. A second source also tests source reuse.
    let views = [
        curve_regression_input(&context, width, height, false),
        curve_regression_input(&context, width, height, true),
    ];
    let masks: Vec<_> = (0..MAX_MASKS)
        .map(|i| {
            masked(ImageBuffer::from_fn(width, height, |x, y| {
                let coverage = match (x + y + i as u32) % 5 {
                    0 => 0,
                    1 => 1,
                    2 => 127,
                    3 => 254,
                    _ => 255,
                };
                Luma([coverage])
            }))
        })
        .collect();
    let mut comparisons = 0;
    let mut ordered_reference = None;
    for (source_index, view) in views.iter().enumerate() {
        for (raw, mapper) in [(true, 1), (true, 0), (false, 0), (true, 2), (false, 2)] {
            for roi in [
                None,
                Some(Roi {
                    x: 17,
                    y: 3,
                    width: width - 33,
                    height: height - 6,
                }),
            ] {
                for (name, profile) in &profiles {
                    let adjustments = get_all_adjustments_from_json(profile, raw, Some(mapper));
                    let render = |legacy| {
                        processor
                            .force_unprepared_layers
                            .store(legacy, Ordering::Relaxed);
                        let (_, w, h, x, y) = processor
                            .run(
                                view,
                                width,
                                height,
                                RenderRequest {
                                    adjustments: adjustments.clone(),
                                    mask_bitmaps: &masks[..adjustments.mask_count as usize],
                                    lut: None,
                                    roi,
                                },
                                true,
                                RenderOutputPrecision::EightBit,
                            )
                            .unwrap();
                        read_texture_data_roi(
                            &context.device,
                            &context.queue,
                            &processor.test_working_texture(),
                            wgpu::Origin3d { x, y, z: 0 },
                            wgpu::Extent3d {
                                width: w,
                                height: h,
                                depth_or_array_layers: 1,
                            },
                            4,
                        )
                        .unwrap()
                    };
                    let reference = render(true);
                    let direct = render(false);
                    assert_eq!(direct.len(), reference.len());
                    let max_delta = direct
                        .iter()
                        .zip(&reference)
                        .map(|(a, b)| a.abs_diff(*b))
                        .max()
                        .unwrap();
                    assert!(
                        max_delta <= 1,
                        "direct curve delta {max_delta}: {name}, source {source_index}, raw {raw}, mapper {mapper}, roi {}",
                        roi.is_some()
                    );
                    assert!(
                        direct
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .all(|pixel| pixel[3] == 255),
                        "alpha: {name}"
                    );
                    if source_index == 0 && raw && mapper == 1 && roi.is_none() {
                        if *name == "mask_order_ab" {
                            ordered_reference = Some(reference.clone());
                        } else if *name == "mask_order_ba" {
                            assert_ne!(
                                ordered_reference.as_ref().unwrap(),
                                &reference,
                                "mask order fixture must exercise sequential composition"
                            );
                        }
                    }
                    comparisons += 1;
                }
            }
        }
    }
    processor
        .force_unprepared_layers
        .store(false, Ordering::Relaxed);
    println!(
        "{comparisons} prepared/previous layer comparisons passed: endpoints, knots, bypasses, 16 points, RGB identities, 2/20/32 masks, order, ROI and tiles"
    );
}

#[test]
#[ignore = "native GPU timing probe; synthetic layers compared with the previous direct-curve pipeline"]
fn layer_preview_timing_probe() {
    let context = context();
    let (width, height) = (2304, 1728);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 3);
    let masks: Vec<_> = (0..20)
        .map(|i| {
            masked(ImageBuffer::from_fn(width, height, |x, y| {
                Luma([(((x + y * 3 + i * 7) % 127) * 2) as u8])
            }))
        })
        .collect();
    for (name, profile) in [
        ("global_curve", json!({"curves":custom_curve()})),
        (
            "local_curve",
            local_profile(json!({"curves":custom_curve()})),
        ),
        (
            "local_curve_nr",
            local_profile(json!({"curves":custom_curve(),"lumaNoiseReduction":30})),
        ),
        ("twenty_local_curves", curve_mask_stack(20, false)),
    ] {
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        let features = preview_features(&adjustments);
        prepare_preview_pipeline(&processor, features);
        prepare_unprepared_layer_pipeline(&processor, features);
        // Hold blur preparation, dimensions and every other specialization
        // constant. Compilation and two alternating warm-up pairs are excluded.
        for frame in 0..14 {
            for legacy in if frame % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                processor
                    .force_unprepared_layers
                    .store(legacy, Ordering::Relaxed);
                let start = Instant::now();
                processor
                    .run(
                        &view,
                        width,
                        height,
                        RenderRequest {
                            adjustments: adjustments.clone(),
                            mask_bitmaps: &masks[..adjustments.mask_count as usize],
                            lut: None,
                            roi: None,
                        },
                        true,
                        RenderOutputPrecision::EightBit,
                    )
                    .unwrap();
                let submit_ms = start.elapsed().as_secs_f64() * 1000.0;
                context
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                if frame >= 2 {
                    println!(
                        "{}",
                        json!({"scenario":"fit","profile":name,
                        "pipeline":if legacy {"previous_direct_curves"} else {"prepared_layers"},
                        "frame":frame-2,"source_width":width,"source_height":height,
                        "mask_count":adjustments.mask_count,"submit_ms":submit_ms,
                        "complete_ms":start.elapsed().as_secs_f64()*1000.0})
                    );
                }
            }
        }
    }
    processor
        .force_unprepared_layers
        .store(false, Ordering::Relaxed);
}

/// A Lightroom-style curve with 20 points (zig-zag of ±10 levels).
fn twenty_point_curve() -> Vec<serde_json::Value> {
    let xs = [
        0, 13, 27, 40, 54, 67, 81, 94, 107, 121, 134, 148, 161, 174, 188, 201, 215, 228, 242, 255,
    ];
    xs.iter()
        .enumerate()
        .map(|(i, &x)| {
            let y = match i {
                0 | 19 => x,
                _ if i % 2 == 1 => x + 10,
                _ => x - 10,
            };
            json!({"x":x,"y":y})
        })
        .collect()
}

#[test]
fn display_samples_come_back_as_srgb_values() {
    use rawtonic_engine::output_color_space::OutputColorSpace;
    let p3 = OutputColorSpace::DisplayP3;
    // sRGB red and a mid grey encoded for a Display P3 layer.
    for srgb in [
        [255.0, 0.0, 0.0],
        [128.0, 128.0, 128.0],
        [30.0, 160.0, 90.0],
    ] {
        let linear = srgb.map(|v: f64| OutputColorSpace::Srgb.transfer().to_linear(v / 255.0));
        let m = p3.from_linear_srgb();
        let encoded: [f64; 3] = std::array::from_fn(|r| {
            p3.transfer()
                .from_linear((0..3).map(|c| m[r][c] * linear[c]).sum::<f64>())
                * 255.0
        });
        // Greys encode alike; colours differ.
        assert_eq!(
            encoded.map(f64::round) == srgb,
            srgb[0] == srgb[1],
            "{srgb:?}"
        );
        let back = display_to_srgb(p3, encoded);
        for c in 0..3 {
            assert!((back[c] - srgb[c]).abs() < 0.01, "{srgb:?}: {back:?}");
        }
    }
    assert_eq!(
        display_to_srgb(OutputColorSpace::Srgb, [1.0, 2.0, 3.0]),
        [1.0, 2.0, 3.0]
    );
}

#[test]
fn a_frame_of_the_photograph_left_never_covers_the_next_ones_still() {
    // B's still is shown; A's frame, rendered before the step, finishes now.
    assert!(!frame_shows(Some("B"), Some("A")));
    // B's own developed frame replaces its still; without a still any frame
    // shows, as before.
    assert!(frame_shows(Some("B"), Some("B")));
    assert!(frame_shows(None, Some("A")));
    assert!(frame_shows(Some("B"), None));
    // Asking for B keeps its still holding; asking for A (back) releases it.
    assert_eq!(
        still_after_asking(Some("B".into()), "B").as_deref(),
        Some("B")
    );
    assert_eq!(still_after_asking(Some("B".into()), "A"), None);
}

#[test]
fn a_camera_jpeg_in_adobe_rgb_is_shown_in_the_display_space() {
    use rawtonic_engine::output_color_space::OutputColorSpace;
    // Adobe RGB's own primaries: pure green lies outside sRGB and Display P3
    // (clipped there), white and grey stay neutral, alpha is kept.
    let pixels = [[255, 255, 255, 255], [128, 128, 128, 255], [0, 255, 0, 90]];
    let make = || image::RgbaImage::from_fn(3, 1, |x, _| image::Rgba(pixels[x as usize]));
    let mut same = make();
    encoded_to_display(
        OutputColorSpace::AdobeRgb,
        OutputColorSpace::AdobeRgb,
        &mut same,
    );
    assert_eq!(same, make());
    let mut srgb = make();
    encoded_to_display(
        OutputColorSpace::AdobeRgb,
        OutputColorSpace::Srgb,
        &mut srgb,
    );
    assert_eq!(srgb.get_pixel(0, 0).0, [255, 255, 255, 255]);
    let grey = srgb.get_pixel(1, 0).0;
    // Adobe RGB 128 (gamma 2.2) is linear 0.2195, sRGB-encoded 128.6 → 129.
    assert!(
        grey[..3].iter().all(|&v| (128..=130).contains(&v)),
        "{grey:?}"
    );
    assert_eq!(srgb.get_pixel(2, 0).0, [0, 255, 0, 90]);
    // In sRGB the camera's green is not the sRGB-encoded value 0/255/0 left
    // unconverted, which a Display P3 screen would show far too saturated.
    let mut p3 = make();
    encoded_to_display(
        OutputColorSpace::AdobeRgb,
        OutputColorSpace::DisplayP3,
        &mut p3,
    );
    let green = p3.get_pixel(2, 0).0;
    assert!(
        green[0] < 8 && green[1] > 240 && green[3] == 90,
        "{green:?}"
    );
    let mut from_srgb = make();
    srgb_to_display(OutputColorSpace::DisplayP3, &mut from_srgb);
    assert!(
        from_srgb.get_pixel(2, 0).0[0] > 50,
        "sRGB green is inside P3"
    );
}

#[test]
fn a_still_is_encoded_for_the_display_as_its_developed_frame_is() {
    use rawtonic_engine::output_color_space::OutputColorSpace;
    let pixels = [[255, 0, 0, 255], [128, 128, 128, 255], [30, 160, 90, 200]];
    let mut still = image::RgbaImage::from_fn(3, 1, |x, _| image::Rgba(pixels[x as usize]));
    let unchanged = still.clone();
    srgb_to_display(OutputColorSpace::Srgb, &mut still);
    assert_eq!(still, unchanged);
    srgb_to_display(OutputColorSpace::DisplayP3, &mut still);
    let p3 = OutputColorSpace::DisplayP3;
    for (x, srgb) in pixels.iter().enumerate() {
        let shown = still.get_pixel(x as u32, 0).0;
        // The Display P3 encoding of the sRGB colour (as the display shader
        // encodes a developed frame), within a code; alpha is kept.
        let linear = [0, 1, 2].map(|c| {
            OutputColorSpace::Srgb
                .transfer()
                .to_linear(srgb[c] as f64 / 255.0)
        });
        let m = p3.from_linear_srgb();
        for (r, row) in m.iter().enumerate() {
            let expected = p3.transfer().from_linear(
                (0..3)
                    .map(|c| row[c] * linear[c])
                    .sum::<f64>()
                    .clamp(0.0, 1.0),
            ) * 255.0;
            assert!(
                (shown[r] as f64 - expected).abs() <= 1.0,
                "{srgb:?}: {shown:?}, channel {r} {expected}"
            );
        }
        assert_eq!(shown[3], srgb[3]);
    }
    assert_eq!(still.get_pixel(1, 0).0, [128, 128, 128, 255]);
    assert_ne!(still.get_pixel(0, 0).0, [255, 0, 0, 255]);
}

#[test]
fn long_curves_use_the_curve_table() {
    let long = twenty_point_curve();
    let a =
        get_all_adjustments_from_json(&json!({"curves":{"red":long,"version":2}}), true, Some(1));
    assert_eq!(a.global.region_curve_active[1], 1);
    assert_eq!(a.curve_table.as_ref().unwrap().len(), CURVE_LUT_SAMPLES);
    // Even when the first 16 points alone would be identity.
    let identity_start: Vec<_> = (0..20).map(|i| json!({"x":i * 10,"y":i * 10})).collect();
    let mut tail = identity_start.clone();
    tail[18] = json!({"x":180,"y":200});
    let a =
        get_all_adjustments_from_json(&json!({"curves":{"luma":tail,"version":2}}), true, Some(1));
    assert_ne!(preview_features(&a) & 4, 0);
    // Hidden curves and short ones keep the uniforms' evaluator.
    for profile in [
        json!({"curves":{"luma":long,"version":2},"sectionVisibility":{"curves":false}}),
        // Curves saved before version 2 keep their first 16 points.
        json!({"curves":{"luma":long}}),
        json!({"curves":custom_curve()}),
    ] {
        let a = get_all_adjustments_from_json(&profile, true, Some(1));
        assert_eq!(a.global.region_curve_active[1], 0, "{profile}");
        assert!(a.curve_table.is_none(), "{profile}");
    }
}

#[test]
#[ignore = "requires a native GPU; curves with more than 16 points against their exact values"]
fn long_curves_render_every_point() {
    let context = context();
    let (width, height) = (257, 64);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 5);
    let long = twenty_point_curve();
    let channels: [Vec<(f32, f32)>; 4] = [
        rawtonic_engine::point_curve::curve_points(&long),
        vec![(0.0, 0.0), (128.0, 150.0), (255.0, 255.0)],
        vec![(0.0, 0.0), (255.0, 255.0)],
        vec![(0.0, 0.0), (255.0, 255.0)],
    ];
    let curved = json!({"curves":{"version":2,"luma":long,
        "red":[{"x":0,"y":0},{"x":128,"y":150},{"x":255,"y":255}]}});
    let render = |profile: &serde_json::Value, display: bool, precision| -> Vec<f32> {
        let adjustments = get_all_adjustments_from_json(profile, false, Some(0));
        if display {
            prepare_preview_pipeline(&processor, preview_features(&adjustments));
        }
        let (pixels, w, h, x, y) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                display,
                precision,
            )
            .unwrap();
        if display {
            return read_texture_data_roi(
                &context.device,
                &context.queue,
                &processor.test_working_texture(),
                wgpu::Origin3d { x, y, z: 0 },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                4,
            )
            .unwrap()
            .iter()
            .map(|&v| v as f32 / 255.0)
            .collect();
        }
        match pixels {
            RenderedPixels::U16(words) => words.iter().map(|&v| v as f32 / 65535.0).collect(),
            RenderedPixels::U8(bytes) => bytes.iter().map(|&v| v as f32 / 255.0).collect(),
        }
    };
    // Export (16-bit) against the exact curves of the uncurved render; pixels
    // clipped there carry their excess past the curve (`carry_beyond_srgb`).
    let before = render(&json!({}), false, RenderOutputPrecision::SixteenBit);
    let after = render(&curved, false, RenderOutputPrecision::SixteenBit);
    let mut worst = 0.0f32;
    let mut compared = 0;
    for (pixel, out) in before.chunks(4).zip(after.chunks(4)) {
        if pixel[..3].iter().any(|&v| v >= 0.999) {
            continue;
        }
        let expected: [f32; 3] = std::array::from_fn(|c| {
            rawtonic_engine::point_curve::curves_value(&channels, pixel[c])[c]
        });
        for c in 0..3 {
            worst = worst.max((out[c] - expected[c]).abs() * 255.0);
        }
        compared += 1;
    }
    assert!(compared > 5_000, "{compared} pixels compared");
    assert!(worst <= 0.3, "export: worst {worst} levels");
    // The display variants read the same table as the export.
    let export = render(&curved, false, RenderOutputPrecision::EightBit);
    let display = render(&curved, true, RenderOutputPrecision::EightBit);
    let display_delta = export
        .iter()
        .zip(&display)
        .map(|(a, b)| ((a - b).abs() * 255.0).round() as u32)
        .max()
        .unwrap();
    assert!(
        display_delta <= 1,
        "display against export: {display_delta} levels"
    );
    // A change after the 16th point alone leaves the uniforms equal: the
    // next display frame must still render it.
    let mut moved = curved.clone();
    moved["curves"]["luma"][18]["y"] = json!(200);
    let changed = render(&moved, true, RenderOutputPrecision::EightBit);
    assert_ne!(display, changed, "a tail-only curve change must re-render");
    println!("export worst {worst:.3} levels over {compared} pixels; display {display_delta}");
}

/// The shader's `wide_curve_input` / `wide_curve_output` around the exact
/// curves: ProPhoto primaries with the sRGB tone curve.
fn wide_curves_value(channels: &[Vec<(f32, f32)>; 4], display: [f32; 3]) -> [f32; 3] {
    const TO_PROPHOTO: [[f32; 3]; 3] = [
        [0.5293458, 0.3300728, 0.1405812],
        [0.0983744, 0.8734611, 0.0281647],
        [0.0168832, 0.1176725, 0.8654443],
    ];
    const TO_SRGB: [[f32; 3]; 3] = [
        [2.0340763, -0.7273343, -0.3067417],
        [-0.2288135, 1.2317302, -0.0029169],
        [-0.0085698, -0.1532866, 1.1618565],
    ];
    let decode = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    let apply = |m: &[[f32; 3]; 3], c: [f32; 3]| -> [f32; 3] {
        std::array::from_fn(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2])
    };
    let wide = apply(&TO_PROPHOTO, display.map(decode)).map(encode);
    let curved: [f32; 3] =
        std::array::from_fn(|c| rawtonic_engine::point_curve::curves_value(channels, wide[c])[c]);
    apply(&TO_SRGB, curved.map(decode)).map(|v| v.signum() * encode(v.abs()))
}

#[test]
fn wide_curves_reproduce_camera_raws_red_curve_on_grey() {
    // Measured in Camera Raw (notes: reference/point-curve.md): a red curve
    // 128 -> 160 shows neutral 128 as R 186 / G 119.
    let identity = vec![(0.0, 0.0), (255.0, 255.0)];
    let red = vec![(0.0, 0.0), (128.0, 160.0), (255.0, 255.0)];
    let out = wide_curves_value(
        &[identity.clone(), red, identity.clone(), identity],
        [128.0 / 255.0; 3],
    )
    .map(|v| v * 255.0);
    assert!(
        (out[0] - 186.0).abs() < 1.0 && (out[1] - 119.0).abs() < 1.0,
        "{out:?}"
    );
}

#[test]
#[ignore = "requires a native GPU; curves.version 3 acts in the wide working space"]
fn wide_curves_act_in_the_working_space() {
    let context = context();
    let (width, height) = (257, 64);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 5);
    let luma = json!([{"x":0,"y":0},{"x":64,"y":48},{"x":128,"y":128},{"x":192,"y":208},{"x":255,"y":255}]);
    let red = json!([{"x":0,"y":0},{"x":128,"y":160},{"x":255,"y":255}]);
    let channels: [Vec<(f32, f32)>; 4] = [
        rawtonic_engine::point_curve::curve_points(luma.as_array().unwrap()),
        rawtonic_engine::point_curve::curve_points(red.as_array().unwrap()),
        vec![(0.0, 0.0), (255.0, 255.0)],
        vec![(0.0, 0.0), (255.0, 255.0)],
    ];
    let curves = json!({"version":3,"luma":luma,"red":red});
    let all = masked(ImageBuffer::from_pixel(width, height, Luma([255u8])));
    let render = |profile: &serde_json::Value, display: bool, precision| -> Vec<f32> {
        let adjustments = get_all_adjustments_from_json(profile, false, Some(0));
        let bitmaps = vec![all.clone(); adjustments.mask_count as usize];
        if display {
            prepare_preview_pipeline(&processor, preview_features(&adjustments));
        }
        let (pixels, w, h, x, y) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &bitmaps,
                    lut: None,
                    roi: None,
                },
                display,
                precision,
            )
            .unwrap();
        if display {
            return read_texture_data_roi(
                &context.device,
                &context.queue,
                &processor.test_working_texture(),
                wgpu::Origin3d { x, y, z: 0 },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                4,
            )
            .unwrap()
            .iter()
            .map(|&v| v as f32 / 255.0)
            .collect();
        }
        match pixels {
            RenderedPixels::U16(words) => words.iter().map(|&v| v as f32 / 65535.0).collect(),
            RenderedPixels::U8(bytes) => bytes.iter().map(|&v| v as f32 / 255.0).collect(),
        }
    };
    let before = render(&json!({}), false, RenderOutputPrecision::SixteenBit);
    // The photo's curves, and the same curves in a layer over the whole photo.
    for profile in [
        json!({"curves":curves}),
        json!({"masks":[local_mask("wide", json!({"curves":curves}))]}),
    ] {
        let after = render(&profile, false, RenderOutputPrecision::SixteenBit);
        let (mut worst, mut compared, mut moved) = (0.0f32, 0, 0.0f32);
        for (pixel, out) in before.chunks(4).zip(after.chunks(4)) {
            if pixel[..3].iter().any(|&v| !(0.001..=0.999).contains(&v)) {
                continue;
            }
            let expected = wide_curves_value(&channels, [pixel[0], pixel[1], pixel[2]]);
            if expected.iter().any(|&v| !(0.001..=0.999).contains(&v)) {
                continue;
            }
            let display = rawtonic_engine::point_curve::curves_value(&channels, pixel[0]);
            moved = moved.max((expected[1] - display[1]).abs() * 255.0);
            for c in 0..3 {
                worst = worst.max((out[c] - expected[c]).abs() * 255.0);
            }
            compared += 1;
        }
        assert!(compared > 3_000, "{profile}: {compared} pixels compared");
        assert!(worst <= 0.3, "{profile}: worst {worst} levels");
        // Not the display-value curves: a red curve moves green there.
        assert!(moved > 5.0, "{profile}: {moved}");
        let export = render(&profile, false, RenderOutputPrecision::EightBit);
        let display = render(&profile, true, RenderOutputPrecision::EightBit);
        let delta = export
            .iter()
            .zip(&display)
            .map(|(a, b)| ((a - b).abs() * 255.0).round() as u32)
            .max()
            .unwrap();
        assert!(
            delta <= 1,
            "{profile}: display against export {delta} levels"
        );
        println!("{profile}: worst {worst:.3} levels over {compared} pixels; display {delta}");
    }
}

#[test]
#[ignore = "requires a native GPU; the Luminance curve keeps chromaticity"]
fn luminance_curve_changes_tone_and_keeps_colour() {
    let context = context();
    let (width, height) = (257, 64);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 5);
    let s_curve = json!([{"x":0,"y":0},{"x":64,"y":40},{"x":128,"y":128},{"x":192,"y":215},{"x":255,"y":255}]);
    let points = rawtonic_engine::point_curve::curve_points(s_curve.as_array().unwrap());
    let decode = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    // The shader's `apply_luminance_curve` on an sRGB colour inside the cube.
    let luminance = |points: &[(f32, f32)], display: [f32; 3]| -> [f32; 3] {
        let linear = display.map(decode);
        let y = (0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2]).max(0.0);
        let target = decode(rawtonic_engine::point_curve::curve_value(points, encode(y)));
        let ratio = (target / y.max(1e-9)).min(4.0);
        linear.map(|v| encode(v * ratio + target - y * ratio))
    };
    let expected = |display: [f32; 3]| luminance(&points, display);
    let all = masked(ImageBuffer::from_pixel(width, height, Luma([255u8])));
    let render = |profile: &serde_json::Value, display: bool, precision| -> Vec<f32> {
        let adjustments = get_all_adjustments_from_json(profile, false, Some(0));
        let bitmaps = vec![all.clone(); adjustments.mask_count as usize];
        if display {
            prepare_preview_pipeline(&processor, preview_features(&adjustments));
        }
        let (pixels, w, h, x, y) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &bitmaps,
                    lut: None,
                    roi: None,
                },
                display,
                precision,
            )
            .unwrap();
        if display {
            return read_texture_data_roi(
                &context.device,
                &context.queue,
                &processor.test_working_texture(),
                wgpu::Origin3d { x, y, z: 0 },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                4,
            )
            .unwrap()
            .iter()
            .map(|&v| v as f32 / 255.0)
            .collect();
        }
        match pixels {
            RenderedPixels::U16(words) => words.iter().map(|&v| v as f32 / 65535.0).collect(),
            RenderedPixels::U8(bytes) => bytes.iter().map(|&v| v as f32 / 255.0).collect(),
        }
    };
    let before = render(&json!({}), false, RenderOutputPrecision::SixteenBit);
    // Neutral luminance curves change nothing.
    let neutral = json!({"curves":{"luminance":[{"x":0,"y":0},{"x":255,"y":255}]}});
    assert_eq!(
        render(&neutral, false, RenderOutputPrecision::SixteenBit),
        before
    );
    for profile in [
        json!({"curves":{"luminance":s_curve}}),
        json!({"masks":[local_mask("lum", json!({"curves":{"luminance":s_curve}}))]}),
    ] {
        let after = render(&profile, false, RenderOutputPrecision::SixteenBit);
        let (mut worst, mut compared, mut moved) = (0.0f32, 0, 0.0f32);
        for (pixel, out) in before.chunks(4).zip(after.chunks(4)) {
            if pixel[..3].iter().any(|&v| !(0.001..=0.999).contains(&v)) {
                continue;
            }
            let want = expected([pixel[0], pixel[1], pixel[2]]);
            if want.iter().any(|&v| !(0.001..=0.999).contains(&v)) {
                continue;
            }
            for c in 0..3 {
                worst = worst.max((out[c] - want[c]).abs() * 255.0);
                moved = moved.max((out[c] - pixel[c]).abs() * 255.0);
            }
            // Chromaticity kept: the linear channels keep their proportions.
            let (a, b) = (
                pixel[..3].iter().map(|&v| decode(v)),
                out[..3].iter().map(|&v| decode(v)),
            );
            let (a, b): (Vec<f32>, Vec<f32>) = (a.collect(), b.collect());
            let (sa, sb) = (a.iter().sum::<f32>(), b.iter().sum::<f32>());
            for c in 0..3 {
                assert!(
                    (a[c] / sa - b[c] / sb).abs() < 0.003,
                    "{pixel:?} -> {out:?}"
                );
            }
            compared += 1;
        }
        assert!(compared > 3_000, "{profile}: {compared} pixels compared");
        assert!(worst <= 0.3, "{profile}: worst {worst} levels");
        assert!(
            moved > 10.0,
            "{profile}: the curve moved {moved} levels at most"
        );
        let export = render(&profile, false, RenderOutputPrecision::EightBit);
        let display = render(&profile, true, RenderOutputPrecision::EightBit);
        let delta = export
            .iter()
            .zip(&display)
            .map(|(a, b)| ((a - b).abs() * 255.0).round() as u32)
            .max()
            .unwrap();
        assert!(
            delta <= 1,
            "{profile}: display against export {delta} levels"
        );
        println!("{profile}: worst {worst:.3} levels over {compared} pixels; display {delta}");
    }
    // A lifted black end lifts black too, and the darks beside it
    // continuously: a dark ramp from black (first row), then black rows.
    let ramp: Vec<f16> = (0..width)
        .flat_map(|x| {
            let v = (x as f32 / width as f32).powi(3) * 0.02;
            [v, v * 0.5, v * 0.2, 1.0].map(f16::from_f32)
        })
        .chain((0..width * (height - 1)).flat_map(|_| [f16::ZERO, f16::ZERO, f16::ZERO, f16::ONE]))
        .collect();
    let dark = input_from(&context, width, height, &ramp);
    let lift = json!({"curves":{"luminance":[{"x":0,"y":32},{"x":255,"y":255}]}});
    let lift_points = vec![(0.0, 32.0), (255.0, 255.0)];
    let render_dark = |profile: &serde_json::Value| -> Vec<f32> {
        let adjustments = get_all_adjustments_from_json(profile, false, Some(0));
        match processor
            .run(
                &dark,
                width,
                height,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap()
            .0
        {
            RenderedPixels::U16(words) => words.iter().map(|&v| v as f32 / 65535.0).collect(),
            RenderedPixels::U8(_) => unreachable!(),
        }
    };
    let (before, after) = (render_dark(&json!({})), render_dark(&lift));
    let at = |image: &[f32], x: usize| [image[x * 4], image[x * 4 + 1], image[x * 4 + 2]];
    // Black (the rows below the ramp) shows the curve's level, 32.
    let black_pixel = width as usize * 2;
    assert_eq!(at(&before, black_pixel), [0.0; 3]);
    let black = at(&after, black_pixel);
    for v in black {
        assert!((v * 255.0 - 32.0).abs() < 0.6, "black shown as {black:?}");
    }
    let mut previous = black;
    for x in 0..width as usize {
        let (input, out) = (at(&before, x), at(&after, x));
        let want = luminance(&lift_points, input);
        for c in 0..3 {
            assert!(
                (out[c] - want[c]).abs() * 255.0 < 0.3,
                "x {x}: {out:?} against {want:?}"
            );
            // No step between neighbours of the ramp.
            assert!(
                (out[c] - previous[c]).abs() * 255.0 < 1.0,
                "x {x}: {out:?} after {previous:?}"
            );
        }
        previous = out;
    }
}

#[test]
#[ignore = "requires a native GPU; display curve tables against exact curve evaluation"]
fn curve_tables_match_exact_display_curves() {
    let context = context();
    let (width, height) = (517, 389);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let masks: Vec<_> = (0..20)
        .map(|i| {
            masked(ImageBuffer::from_fn(width, height, |x, y| {
                Luma([(((x * 5 + y * 3 + i * 11) % 256) as u8)])
            }))
        })
        .collect();
    let smooth = json!({"luma":[{"x":0,"y":8},{"x":55,"y":43},{"x":165,"y":191},{"x":255,"y":248}],
        "red":[{"x":0,"y":0},{"x":128,"y":138},{"x":255,"y":255}],
        "green":[{"x":0,"y":0},{"x":128,"y":118},{"x":255,"y":255}],
        "blue":[{"x":0,"y":4},{"x":128,"y":128},{"x":255,"y":251}]});
    let step = json!({"luma":[{"x":0,"y":0},{"x":100,"y":0},{"x":100,"y":255},{"x":255,"y":255}]});
    let mut comparisons = 0;
    for (name, profile, expect_tables) in [
        ("twenty_curve_stack", curve_mask_stack(20, false), true),
        (
            "brush_recipe_curves",
            json!({"sharpness":53,"vibrance":72,"saturation":27,"brightness":0.25,
                "curves":custom_curve(),
                "masks":(0..20).map(|i| local_mask(&format!("m{i}"), json!({"curves":smooth.clone()}))).collect::<Vec<_>>()}),
            true,
        ),
        // A discontinuous layer must keep the exact evaluator.
        ("step_curve", local_profile(json!({"curves":step})), false),
    ] {
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        let features = preview_features(&adjustments);
        prepare_preview_pipeline(&processor, features);
        for seed in [3, 29] {
            let view = input(&context, width, height, seed);
            let render = |exact: bool| {
                processor.force_exact_curves.store(exact, Ordering::Relaxed);
                let (_, w, h, x, y) = processor
                    .run(
                        &view,
                        width,
                        height,
                        RenderRequest {
                            adjustments: adjustments.clone(),
                            mask_bitmaps: &masks[..adjustments.mask_count as usize],
                            lut: None,
                            roi: None,
                        },
                        true,
                        RenderOutputPrecision::EightBit,
                    )
                    .unwrap();
                read_texture_data_roi(
                    &context.device,
                    &context.queue,
                    &processor.test_working_texture(),
                    wgpu::Origin3d { x, y, z: 0 },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    4,
                )
                .unwrap()
            };
            let exact = render(true);
            let tabled = render(false);
            let used_tables = processor
                .curve_lut_cache
                .lock()
                .unwrap()
                .iter()
                .any(|entry| matches!(entry, Some((_, true))));
            assert_eq!(used_tables, expect_tables, "{name}: table use");
            let max_delta = exact
                .iter()
                .zip(&tabled)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            assert!(max_delta <= 1, "{name}: display delta {max_delta}");
            if !expect_tables {
                assert_eq!(exact, tabled, "{name}: exact layers must be unchanged");
            }
            comparisons += 1;
            *processor.curve_lut_cache.lock().unwrap() = vec![None; MAX_MASKS + 1];
        }
    }
    processor.force_exact_curves.store(false, Ordering::Relaxed);
    println!(
        "{comparisons} display curve table comparisons passed: within one 8-bit level, steps stay exact"
    );
}

#[test]
#[ignore = "native GPU timing probe; set RAWTONIC_PREVIEW_RECIPE to an editor adjustments JSON"]
fn recipe_preview_timing_probe() {
    let Ok(path) = std::env::var("RAWTONIC_PREVIEW_RECIPE") else {
        println!("SKIP recipe_preview_timing_probe: RAWTONIC_PREVIEW_RECIPE is not set");
        return;
    };
    let recipe: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let context = context();
    let (width, height) = (2304, 1728);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 3);
    let masks: Vec<_> = (0..32)
        .map(|i| {
            masked(ImageBuffer::from_fn(width, height, |x, y| {
                Luma([(((x + y * 3 + i * 7) % 127) * 2) as u8])
            }))
        })
        .collect();
    // Attribute the in-app cost: each variant removes one group of work from
    // the private recipe while keeping every other setting unchanged.
    let variant = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut value = recipe.clone();
        edit(&mut value);
        value
    };
    let variants = [
        ("full", variant(&|_| {})),
        ("no_masks", variant(&|v| v["masks"] = json!([]))),
        ("no_sharpening", variant(&|v| v["sharpness"] = json!(0))),
        (
            "no_global_color",
            variant(&|v| {
                for key in ["vibrance", "saturation", "brightness"] {
                    v[key] = json!(0);
                }
            }),
        ),
        (
            "masks_only",
            variant(&|v| {
                *v = json!({"masks": v["masks"].clone()});
            }),
        ),
        (
            // Lower bound for per-layer overhead: one minimal luma curve.
            "masks_minimal_curve",
            variant(&|v| {
                let mut masks = v["masks"].as_array().unwrap().clone();
                for mask in &mut masks {
                    mask["adjustments"] =
                        json!({"curves":{"luma":[{"x":0,"y":0},{"x":255,"y":250}]}});
                }
                *v = json!({"masks": masks});
            }),
        ),
    ];
    // The general shader is the display fallback until a variant compiles.
    // A fresh processor reports which pipeline served each frame.
    let fresh = GpuProcessor::new(context.clone(), width, height).unwrap();
    let full = get_all_adjustments_from_json(&recipe, true, Some(1));
    for frame in 0..40 {
        let specialized = fresh.preview_pipelines[preview_features(&full)]
            .get()
            .is_some();
        let start = Instant::now();
        fresh
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: full.clone(),
                    mask_bitmaps: &masks[..full.mask_count as usize],
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
        println!(
            "{}",
            json!({"probe":"recipe_preview","variant":if specialized {"fresh_specialized"} else {"fresh_general_fallback"},
            "frame":frame,"complete_ms":start.elapsed().as_secs_f64()*1000.0,
            "features":preview_features(&full),"mask_count":full.mask_count,"submit_ms":0.0})
        );
    }
    // In the editor each released stroke follows an idle GPU. Reproduce that
    // cadence with the already-prepared full recipe.
    let idle_ms: u64 = std::env::var("RAWTONIC_PREVIEW_IDLE_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    if idle_ms > 0 {
        // Same binary, alternating exact and table curves in ABBA order.
        for frame in 0..16 {
            let exact = (frame % 4 == 0) || (frame % 4 == 3);
            fresh.force_exact_curves.store(exact, Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(idle_ms));
            let start = Instant::now();
            fresh
                .run(
                    &view,
                    width,
                    height,
                    RenderRequest {
                        adjustments: full.clone(),
                        mask_bitmaps: &masks[..full.mask_count as usize],
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
            println!(
                "{}",
                json!({"probe":"recipe_preview","variant":"after_idle","curves":if exact {"exact"} else {"tables"},
                "frame":frame,"idle_ms":idle_ms,"complete_ms":start.elapsed().as_secs_f64()*1000.0,
                "features":preview_features(&full),"mask_count":full.mask_count,"submit_ms":0.0})
            );
        }
        fresh.force_exact_curves.store(false, Ordering::Relaxed);
    }
    for (name, profile) in variants {
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        let features = preview_features(&adjustments);
        prepare_preview_pipeline(&processor, features);
        for frame in 0..28 {
            let exact = (frame % 4 == 0) || (frame % 4 == 3);
            processor.force_exact_curves.store(exact, Ordering::Relaxed);
            let start = Instant::now();
            processor
                .run(
                    &view,
                    width,
                    height,
                    RenderRequest {
                        adjustments: adjustments.clone(),
                        mask_bitmaps: &masks[..adjustments.mask_count as usize],
                        lut: None,
                        roi: None,
                    },
                    true,
                    RenderOutputPrecision::EightBit,
                )
                .unwrap();
            let submit_ms = start.elapsed().as_secs_f64() * 1000.0;
            context
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            if frame >= 4 {
                println!(
                    "{}",
                    json!({"probe":"recipe_preview","variant":name,"curves":if exact {"exact"} else {"tables"},"frame":frame-4,
                    "features":features,"mask_count":adjustments.mask_count,
                    "submit_ms":submit_ms,"complete_ms":start.elapsed().as_secs_f64()*1000.0})
                );
            }
        }
    }
}

#[test]
#[ignore = "requires a native GPU; exact specialized CPU outputs against general shader"]
fn export_specialization_preserves_general_pixels() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2304, 256).unwrap();
    let profiles = [
        json!({}),
        json!({"exposure":0.75,"brightness":0.1,"contrast":20}),
        json!({"sharpness":45,"shadows":35}),
        // The tone-order guards' extra gains: Shadows/Blacks lowering and
        // raising together, Whites lowering, Contrast both ways.
        json!({"shadows":-60,"blacks":-40}),
        json!({"shadows":80,"blacks":80,"whites":-80,"contrast":70}),
        json!({"contrast":-70}),
        json!({"highlights":-40}),
        json!({"lumaNoiseReduction":20,"colorNoiseReduction":15}),
        json!({"curves":custom_curve()}),
        local_profile(json!({"exposure":0.75,"brightness":0.1})),
        local_profile(json!({"curves":custom_curve()})),
        local_profile(json!({"sharpness":30,"highlights":-20,"lumaNoiseReduction":15})),
    ];
    let mut comparisons = 0;
    for (width, height) in [(127, 93), (2176, 64)] {
        let view = input(&context, width, height, 17);
        let mask = masked(ImageBuffer::from_fn(width, height, |x, y| {
            Luma([((x * 3 + y * 7) % 256) as u8])
        }));
        // Camera Raw look (mode 3): DJI stand-ins (flag 0), neutral colour
        // (2), neutral colour with an own curve (3).
        for (raw, tonemapper, flag) in [
            (true, 1, 0.0),
            (true, 0, 0.0),
            (false, 0, 0.0),
            (true, 2, 0.0),
            (true, 3, 0.0),
            (true, 3, 2.0),
            (true, 3, 3.0),
        ] {
            for profile in &profiles {
                let mut adjustments = get_all_adjustments_from_json(profile, raw, Some(tonemapper));
                if flag > 2.5 {
                    for i in 0..49 {
                        adjustments.global.profile_curve[i / 4][i % 4] =
                            (-13.0 + 0.27 * i as f32).min(0.0);
                    }
                }
                adjustments.global.profile_curve[15][3] = flag;
                let masks = if adjustments.mask_count > 0 {
                    std::slice::from_ref(&mask)
                } else {
                    &[]
                };
                for precision in [
                    RenderOutputPrecision::EightBit,
                    RenderOutputPrecision::SixteenBit,
                ] {
                    for roi in [
                        None,
                        Some(Roi {
                            x: 13,
                            y: 7,
                            width: width - 25,
                            height: height - 15,
                        }),
                    ] {
                        let render = |general| {
                            processor
                                .force_general_pipeline
                                .store(general, Ordering::Relaxed);
                            let (pixels, ..) = processor
                                .run(
                                    &view,
                                    width,
                                    height,
                                    RenderRequest {
                                        adjustments: adjustments.clone(),
                                        mask_bitmaps: masks,
                                        lut: None,
                                        roi,
                                    },
                                    false,
                                    precision,
                                )
                                .unwrap();
                            match pixels {
                                RenderedPixels::U8(bytes) => bytes,
                                RenderedPixels::U16(words) => bytemuck::cast_slice(&words).to_vec(),
                            }
                        };
                        let reference = render(true);
                        let specialized = render(false);
                        assert!(
                            specialized == reference,
                            "exact export mismatch raw={raw}, mapper={tonemapper}, precision={precision:?}, profile={profile}; differing bytes={}",
                            specialized
                                .iter()
                                .zip(&reference)
                                .filter(|(a, b)| a != b)
                                .count()
                        );
                        comparisons += 1;
                    }
                }
            }
        }
    }
    assert!(
        processor.display_targets.lock().unwrap().is_none(),
        "CPU output must not allocate full-image display targets"
    );
    println!("{comparisons} exact 8/16-bit specialized export comparisons passed");
}

#[test]
#[ignore = "requires a native GPU; verifies display allocation resizing and pixels"]
fn display_targets_follow_current_image_without_retaining_peak() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 4096, 4096).unwrap();
    assert!(processor.display_targets.lock().unwrap().is_none());
    assert_eq!(processor.tile_output_texture.width(), 2304);
    assert_eq!(processor.tile_output_texture.height(), 2304);
    processor
        .force_general_pipeline
        .store(true, Ordering::Relaxed);
    let adjustments = get_all_adjustments_from_json(&json!({"exposure":0.4}), true, Some(1));
    let mut prior = std::sync::Weak::<DisplayTargets>::new();
    for (width, height) in [(160, 100), (2176, 64), (80, 50)] {
        let view = input(&context, width, height, 19);
        let request = || RenderRequest {
            adjustments: adjustments.clone(),
            mask_bitmaps: &[],
            lut: None,
            roi: None,
        };
        let (cpu, ..) = processor
            .run(
                &view,
                width,
                height,
                request(),
                false,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        let RenderedPixels::U8(reference) = cpu else {
            unreachable!()
        };
        processor
            .run(
                &view,
                width,
                height,
                request(),
                true,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        let current = processor.display_targets(width, height);
        assert_eq!((current.width, current.height), (width, height));
        assert_eq!(
            (
                current.output_texture.width(),
                current.output_texture.height()
            ),
            (width, height)
        );
        assert!(
            prior.upgrade().is_none(),
            "old display target pair retained after a size change"
        );
        assert!(Arc::ptr_eq(
            &current,
            &processor.display_targets(width, height)
        ));
        let display = read_texture_data_roi(
            &context.device,
            &context.queue,
            &current.working_texture,
            wgpu::Origin3d::ZERO,
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            4,
        )
        .unwrap();
        assert_eq!(display, reference, "resized display output changed pixels");
        prior = Arc::downgrade(&current);
    }
}

#[test]
fn latest_analytics_worker_replaces_pending_and_drains_final() {
    use std::sync::mpsc;
    use std::time::Duration;
    let (started_tx, started_rx) = mpsc::channel();
    let (continue_tx, continue_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let worker = start_latest_job_worker(move |job: Arc<u8>| {
        if *job == 1 {
            started_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
        }
        result_tx.send(*job).unwrap();
    });
    worker.send(Arc::new(1));
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let superseded = Arc::new(2);
    let superseded_weak = Arc::downgrade(&superseded);
    worker.send(superseded);
    worker.send(Arc::new(3));
    assert!(
        superseded_weak.upgrade().is_none(),
        "replaced pending job retained its image"
    );
    drop(worker);
    continue_tx.send(()).unwrap();
    assert_eq!(result_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    assert_eq!(result_rx.recv_timeout(Duration::from_secs(5)).unwrap(), 3);
    assert_eq!(
        result_rx.recv_timeout(Duration::from_secs(5)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
}

#[test]
#[ignore = "requires a native GPU; exact direct export curve edge cases in 8/16-bit"]
fn export_direct_curves_preserve_dense_general_pixels() {
    let context = context();
    let (width, height) = (2176, 40);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let masks: Vec<_> = (0..MAX_MASKS)
        .map(|i| {
            masked(ImageBuffer::from_fn(width, height, |x, y| {
                Luma([match (x + y + i as u32) % 5 {
                    0 => 0,
                    1 => 1,
                    2 => 127,
                    3 => 254,
                    _ => 255,
                }])
            }))
        })
        .collect();
    let mut comparisons = 0;
    for alternate in [false, true] {
        let view = curve_regression_input(&context, width, height, alternate);
        for (raw, mapper) in [(true, 1), (true, 0), (false, 0)] {
            for (name, profile) in curve_regression_profiles() {
                let adjustments = get_all_adjustments_from_json(&profile, raw, Some(mapper));
                for precision in [
                    RenderOutputPrecision::EightBit,
                    RenderOutputPrecision::SixteenBit,
                ] {
                    for roi in [
                        None,
                        Some(Roi {
                            x: 17,
                            y: 3,
                            width: width - 33,
                            height: height - 6,
                        }),
                    ] {
                        let render = |general| {
                            processor
                                .force_general_pipeline
                                .store(general, Ordering::Relaxed);
                            let (pixels, ..) = processor
                                .run(
                                    &view,
                                    width,
                                    height,
                                    RenderRequest {
                                        adjustments: adjustments.clone(),
                                        mask_bitmaps: &masks[..adjustments.mask_count as usize],
                                        lut: None,
                                        roi,
                                    },
                                    false,
                                    precision,
                                )
                                .unwrap();
                            match pixels {
                                RenderedPixels::U8(bytes) => bytes,
                                RenderedPixels::U16(words) => bytemuck::cast_slice(&words).to_vec(),
                            }
                        };
                        let reference = render(true);
                        let direct = render(false);
                        assert!(
                            direct == reference,
                            "exact export curve mismatch profile={name}, alternate={alternate}, raw={raw}, mapper={mapper}, precision={precision:?}, roi={}; differing bytes={}",
                            roi.is_some(),
                            direct
                                .iter()
                                .zip(&reference)
                                .filter(|(a, b)| a != b)
                                .count()
                        );
                        comparisons += 1;
                    }
                }
            }
        }
    }
    println!("{comparisons} exact direct-curve export edge comparisons passed");
}

#[test]
fn display_sample_identity_and_edge_window_reject_stale_frames() {
    let source = Arc::new(DynamicImage::new_rgb8(20, 17));
    let identity = DisplayFrameIdentity {
        path: "source.raw".into(),
        source: Arc::downgrade(&source),
        load_generation: 4,
        render_generation: 7,
    };
    assert!(identity.matches("source.raw", &source, 4, 7));
    assert!(!identity.matches("other.raw", &source, 4, 7));
    assert!(!identity.matches("source.raw", &source, 3, 7));
    assert!(!identity.matches("source.raw", &source, 4, 8));
    assert!(!identity.matches(
        "source.raw",
        &Arc::new(DynamicImage::new_rgb8(20, 17)),
        4,
        7
    ));
    let center = preview_sample_roi(20, 17, 0.5, 0.5).unwrap();
    assert_eq!(
        (center.x, center.y, center.width, center.height),
        (5, 3, 11, 11)
    );
    let edge = preview_sample_roi(20, 17, 0.0, 0.0).unwrap();
    assert_eq!((edge.x, edge.y, edge.width, edge.height), (0, 0, 6, 6));
    let end = preview_sample_roi(20, 17, 1.0, 1.0).unwrap();
    assert_eq!((end.x, end.y, end.width, end.height), (15, 12, 5, 5));
    assert!(preview_sample_roi(20, 17, f64::NAN, 0.5).is_none());
    assert!(preview_sample_roi(20, 17, -0.1, 0.5).is_none());
}

#[test]
#[ignore = "requires a native GPU; verifies11x11 mean from developed display output and stale rejection"]
fn display_color_sample_uses_developed_output_and_clamps_edges() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 20, 17).unwrap();
    let source = Arc::new(DynamicImage::new_rgb8(20, 17));
    let targets = processor.display_targets(20, 17);
    let data: Vec<u8> = (0..17u8)
        .flat_map(|y| (0..20u8).flat_map(move |x| [x * 7, y * 11, (x + y) * 3, 127]))
        .collect();
    context.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &targets.output_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(20 * 4),
            rows_per_image: Some(17),
        },
        wgpu::Extent3d {
            width: 20,
            height: 17,
            depth_or_array_layers: 1,
        },
    );
    *processor.display_frame.lock().unwrap() = Some(DisplayFrame {
        identity: DisplayFrameIdentity {
            path: "source.raw".into(),
            source: Arc::downgrade(&source),
            load_generation: 4,
            render_generation: 7,
        },
        roi: Roi {
            x: 0,
            y: 0,
            width: 20,
            height: 17,
        },
    });
    let sample = |path, load, render, x, y| {
        processor
            .sample_display_color(path, &source, load, render, x, y)
            .unwrap()
    };
    assert_eq!(
        sample("source.raw", 4, 7, 0.5, 0.5),
        Some([70.0, 88.0, 54.0])
    );
    assert_eq!(
        sample("source.raw", 4, 7, 0.0, 0.0),
        Some([17.5, 27.5, 15.0])
    );
    assert_eq!(
        sample("source.raw", 4, 7, 1.0, 1.0),
        Some([119.0, 154.0, 93.0])
    );
    assert_eq!(sample("other.raw", 4, 7, 0.5, 0.5), None);
    assert_eq!(sample("source.raw", 5, 7, 0.5, 0.5), None);
    assert_eq!(sample("source.raw", 4, 8, 0.5, 0.5), None);
    processor
        .display_frame
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .roi = Roi {
        x: 5,
        y: 5,
        width: 5,
        height: 5,
    };
    assert_eq!(sample("source.raw", 4, 7, 0.5, 0.5), None);
}

fn brute_force_changed_bands(
    previous: &[Arc<MaskBitmap>],
    current: &[Arc<MaskBitmap>],
    height: u32,
) -> Vec<Option<Roi>> {
    let mut bands = vec![None; (height as usize).div_ceil(DIRTY_BAND_ROWS)];
    for (previous, current) in previous.iter().zip(current) {
        for (x, y, pixel) in current.enumerate_pixels() {
            if previous.get_pixel(x, y) != pixel {
                let band = &mut bands[y as usize / DIRTY_BAND_ROWS];
                let pixel = Roi {
                    x,
                    y,
                    width: 1,
                    height: 1,
                };
                *band = Some(union_roi(*band, pixel));
            }
        }
    }
    bands
}

fn xorshift(mut seed: u32) -> impl FnMut() -> u32 {
    move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    }
}

#[test]
fn overlay_upload_covers_exactly_the_changed_rows() {
    let previous = GrayImage::from_fn(37, 20, |x, y| Luma([((x * 7 + y * 3) % 251) as u8]));
    assert_eq!(changed_rows(&previous, &previous), None);
    for (first, last) in [(0, 0), (4, 4), (5, 13), (19, 19), (0, 19)] {
        let mut current = previous.clone();
        current.get_pixel_mut(36, first).0[0] ^= 1;
        current.get_pixel_mut(0, last).0[0] ^= 2;
        assert_eq!(changed_rows(&previous, &current), Some((first, last + 1)));
    }
}

#[test]
fn changed_mask_bands_match_a_pixel_scan() {
    let mut next = xorshift(0x2545_f491);
    let mut checked = 0;
    for (width, height) in [(1, 1), (1, 130), (9, 1), (37, 200), (300, 7), (5, 197)] {
        let pattern = |seed: u32| {
            masked(ImageBuffer::from_fn(width, height, move |x, y| {
                Luma([((x * 7 + y * 13 + seed) % 256) as u8])
            }))
        };
        let previous = vec![pattern(0), pattern(91)];
        let mut bands = vec![None; (height as usize).div_ceil(DIRTY_BAND_ROWS)];
        assert_eq!(
            add_changed_mask_bands(&previous[0], &previous[0], &mut bands),
            None
        );
        assert!(bands.iter().all(Option::is_none));
        for _ in 0..200 {
            let mut current = previous.clone();
            for mask in current.iter_mut() {
                let mask = Arc::make_mut(mask).bytes_mut();
                for _ in 0..(next() % 5) {
                    let x = next() % width;
                    let y = next() % height;
                    let old = mask.get_pixel(x, y)[0];
                    // Some writes keep the old value and must not count as changes.
                    let value = if next().is_multiple_of(3) {
                        old
                    } else {
                        old ^ (1 + next() % 255) as u8
                    };
                    mask.put_pixel(x, y, Luma([value]));
                }
            }
            let rows: Vec<_> = (0..2)
                .map(|layer| {
                    let changed: Vec<_> = (0..height)
                        .filter(|&y| {
                            (0..width).any(|x| {
                                previous[layer].get_pixel(x, y) != current[layer].get_pixel(x, y)
                            })
                        })
                        .collect();
                    changed
                        .first()
                        .map(|&top| (top, changed.last().unwrap() + 1))
                })
                .collect();
            let mut bands = vec![None; (height as usize).div_ceil(DIRTY_BAND_ROWS)];
            for layer in 0..2 {
                assert_eq!(
                    add_changed_mask_bands(&previous[layer], &current[layer], &mut bands),
                    rows[layer],
                    "{width}x{height} rows"
                );
            }
            assert_eq!(
                bands,
                brute_force_changed_bands(&previous, &current, height),
                "{width}x{height} bands"
            );
            checked += 1;
        }
    }
    println!("{checked} changed-mask band scans match");
}

fn area(rect: Roi) -> u64 {
    rect.width as u64 * rect.height as u64
}

fn assert_merged_cover(bands: &[Option<Roi>], merged: &[Roi]) {
    for (index, a) in merged.iter().enumerate() {
        for b in &merged[index + 1..] {
            assert!(intersect_roi(*a, *b).is_none(), "merged rectangles overlap");
        }
    }
    for band in bands.iter().flatten() {
        let covered: u64 = merged
            .iter()
            .filter_map(|rect| intersect_roi(*rect, *band))
            .map(area)
            .sum();
        assert_eq!(covered, area(*band), "band not covered");
    }
    let bounds = bands
        .iter()
        .flatten()
        .fold(None, |union, band| Some(union_roi(union, *band)));
    assert!(merged.iter().map(|rect| area(*rect)).sum::<u64>() <= bounds.map_or(0, area));
}

#[test]
fn merged_dirty_bands_cover_changes_without_overlap() {
    let band = |index: u32, x: u32, width: u32| {
        Some(Roi {
            x,
            y: index * DIRTY_BAND_ROWS as u32,
            width,
            height: DIRTY_BAND_ROWS as u32,
        })
    };
    // A compact stroke over three bands is one rectangle.
    let compact = [
        None,
        band(1, 100, 300),
        band(2, 90, 310),
        band(3, 100, 300),
        None,
    ];
    let merged = merge_dirty_bands(&compact);
    assert_eq!(merged.len(), 1);
    assert_merged_cover(&compact, &merged);
    // A diagonal stroke across 27 bands keeps about its own area.
    let diagonal: Vec<_> = (0..27).map(|i| band(i, i * 80, 180)).collect();
    let merged = merge_dirty_bands(&diagonal);
    assert_merged_cover(&diagonal, &merged);
    let bounds = diagonal
        .iter()
        .flatten()
        .fold(None, |union, band| Some(union_roi(union, *band)));
    let total: u64 = merged.iter().map(|rect| area(*rect)).sum();
    assert!(total * 5 < bounds.map_or(0, area), "diagonal area {total}");
    let mut next = xorshift(0x9e37_79b9);
    for _ in 0..500 {
        let bands: Vec<_> = (0..12)
            .map(|i| {
                (!next().is_multiple_of(3)).then(|| {
                    let x = next() % 900;
                    let top = i * DIRTY_BAND_ROWS as u32 + next() % 32;
                    Roi {
                        x,
                        y: top,
                        width: 1 + next() % 400,
                        height: 1 + next() % 32,
                    }
                })
            })
            .collect();
        assert_merged_cover(&bands, &merge_dirty_bands(&bands));
    }
    assert!(merge_dirty_bands(&[None, None]).is_empty());
}

fn partial_frame_profile(count: usize, exposure: f64) -> serde_json::Value {
    let masks: Vec<_> = (0..count)
        .map(|i| {
            let adjustments = match i % 3 {
                0 => json!({"exposure":0.6,"clarity":30,
                    "curves":{"luma":[{"x":0,"y":10},{"x":120,"y":150},{"x":255,"y":240}]}}),
                1 => json!({"temperature":-20,"contrast":25,"structure":-15}),
                _ => json!({"curves":{"red":[{"x":0,"y":0},{"x":140,"y":100},{"x":255,"y":255}]},
                    "sharpness":35}),
            };
            local_mask(&format!("partial-{i}"), adjustments)
        })
        .collect();
    json!({"exposure":exposure,"sharpness":40,"clarity":25,"structure":15,"masks":masks})
}

fn painted(
    masks: &[Arc<MaskBitmap>],
    layer: usize,
    region: (std::ops::Range<u32>, std::ops::Range<u32>),
    value: u8,
) -> Vec<Arc<MaskBitmap>> {
    let mut masks = masks.to_vec();
    let mask = Arc::make_mut(&mut masks[layer]).bytes_mut();
    for y in region.1 {
        for x in region.0.clone() {
            mask.put_pixel(x, y, Luma([value]));
        }
    }
    masks
}

fn changed_area(
    previous: &[Arc<MaskBitmap>],
    current: &[Arc<MaskBitmap>],
    roi: Option<Roi>,
) -> u64 {
    let height = current[0].height();
    let bounds = roi.unwrap_or(Roi {
        x: 0,
        y: 0,
        width: current[0].width(),
        height,
    });
    merge_dirty_bands(&brute_force_changed_bands(previous, current, height))
        .into_iter()
        .filter_map(|rect| intersect_roi(rect, bounds))
        .map(area)
        .sum()
}

fn layer_sums_profile(exposure: f64, layer_exposure: f64) -> serde_json::Value {
    let masks: Vec<_> = (0..12)
        .map(|i| {
            let adjustments = match i % 4 {
                0 => json!({"exposure":if i == 4 { layer_exposure } else { 0.6 },"clarity":30,"saturation":-20}),
                1 => json!({"temperature":-20,"contrast":25,"structure":-15,"highlights":-30}),
                2 => json!({"hsl":{"blues":{"hue":20,"saturation":-30}},"shadows":25,"tint":10}),
                _ => json!({"exposure":-0.4,"vibrance":30,"hue":-10,"curves":{"red":[{"x":0,"y":0},{"x":140,"y":100},{"x":255,"y":255}]}}),
            };
            local_mask(&format!("sums-{i}"), adjustments)
        })
        .collect();
    json!({"exposure":exposure,"sharpness":40,"clarity":25,"masks":masks})
}

#[test]
#[ignore = "requires a native GPU; layer sums against the per-layer loop"]
fn layer_sums_match_per_layer_loop() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2600, 400).unwrap();
    processor.layer_sums_enabled.store(true, Ordering::Relaxed);
    let reference = GpuProcessor::new(context.clone(), 2600, 400).unwrap();
    // Two horizontal tiles, so the sums are addressed across the seam.
    let (width, height) = (2500, 300);
    let view = input(&context, width, height, 29);
    let masks = upload_test_masks(width, height, 12);
    let features = preview_features(&get_all_adjustments_from_json(
        &layer_sums_profile(0.0, 0.6),
        true,
        Some(1),
    ));
    for processor in [&processor, &reference] {
        prepare_preview_pipeline(processor, features);
    }
    prepare_layer_sum_pipeline(&processor, features);
    let frame =
        |processor: &GpuProcessor, profile: &serde_json::Value, masks: &[Arc<MaskBitmap>]| {
            let (_, w, h, x, y) = processor
                .run(
                    &view,
                    width,
                    height,
                    RenderRequest {
                        adjustments: get_all_adjustments_from_json(profile, true, Some(1)),
                        mask_bitmaps: masks,
                        lut: None,
                        roi: None,
                    },
                    true,
                    RenderOutputPrecision::EightBit,
                )
                .unwrap();
            read_texture_data_roi(
                &context.device,
                &context.queue,
                &processor.test_working_texture(),
                wgpu::Origin3d { x, y, z: 0 },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                4,
            )
            .unwrap()
        };
    let counts = || {
        processor
            .layer_sum_frames
            .each_ref()
            .map(|c| c.load(Ordering::Relaxed))
    };
    let compare = |label: &str, profile: &serde_json::Value, masks: &[Arc<MaskBitmap>]| {
        let actual = frame(&processor, profile, masks);
        let expected = frame(&reference, profile, masks);
        let (worst, at) = actual
            .iter()
            .zip(&expected)
            .map(|(a, e)| a.abs_diff(*e))
            .enumerate()
            .max_by_key(|(_, d)| *d)
            .map(|(i, d)| (d, i))
            .unwrap();
        // The sums add the layers in another order than the loop: at most a
        // rounding step of the 8-bit display output.
        assert!(worst <= 1, "{label}: {worst} levels apart at byte {at}");
    };

    // A global slider drag: the third frame on computes the sums.
    for step in 0..6 {
        compare(
            "global drag",
            &layer_sums_profile(0.1 + 0.05 * step as f64, 0.6),
            &masks,
        );
    }
    let [all, excluded] = counts();
    assert!(
        all >= 3 && excluded == 0,
        "global drag frames from sums: {all}/{excluded}"
    );

    // A slider on layer 4: the sums leave that layer out.
    for step in 0..6 {
        compare(
            "layer drag",
            &layer_sums_profile(0.35, 0.1 * step as f64),
            &masks,
        );
    }
    let [after, excluded] = counts();
    assert!(
        after - all >= 4 && excluded >= 4,
        "layer drag frames from sums: {}/{excluded}",
        after - all
    );

    // A brush stroke changes a mask every frame: the loop renders it.
    let mut stroke = masks.clone();
    for step in 0..4u32 {
        stroke = painted(
            &stroke,
            5,
            (100 + step * 300..400 + step * 300, 40..120),
            200,
        );
        compare("stroke", &layer_sums_profile(0.35, 0.5), &stroke);
    }
    assert_eq!(counts()[0], after, "a stroke must not use stale sums");
}

#[test]
#[ignore = "requires a native GPU; partial display frames against complete renders"]
fn partial_display_frames_match_complete_renders() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2600, 400).unwrap();
    // A separate processor never keeps a previous frame, so every one of its
    // renders is complete and independent of the processor under test.
    let reference = GpuProcessor::new(context.clone(), 2600, 400).unwrap();
    reference
        .force_full_display_render
        .store(true, Ordering::Relaxed);
    // Two horizontal tiles (2048 px), an odd width and blur halos at the seam.
    let (width, height) = (2500, 300);
    let view = input(&context, width, height, 11);
    let second_view = input(&context, width, height, 53);
    let three = get_all_adjustments_from_json(&partial_frame_profile(3, 0.2), true, Some(1));
    let two = get_all_adjustments_from_json(&partial_frame_profile(2, 0.2), true, Some(1));
    let brighter = get_all_adjustments_from_json(&partial_frame_profile(3, 0.5), true, Some(1));
    for adjustments in [&three, &two, &brighter] {
        prepare_preview_pipeline(&processor, preview_features(adjustments));
        prepare_preview_pipeline(&reference, preview_features(adjustments));
    }
    let frame = |processor: &GpuProcessor,
                 view: &wgpu::TextureView,
                 adjustments: AllAdjustments,
                 masks: &[Arc<MaskBitmap>],
                 roi: Option<Roi>| {
        let (_, w, h, x, y) = processor
            .run(
                view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.into(),
                    mask_bitmaps: masks,
                    lut: None,
                    roi,
                },
                true,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        read_texture_data_roi(
            &context.device,
            &context.queue,
            &processor.test_working_texture(),
            wgpu::Origin3d { x, y, z: 0 },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            4,
        )
        .unwrap()
    };
    let full = width as u64 * height as u64;
    let mut checked = 0;
    let mut check = |label: &str,
                     view: &wgpu::TextureView,
                     adjustments: AllAdjustments,
                     masks: &[Arc<MaskBitmap>],
                     roi: Option<Roi>,
                     expected_pixels: u64| {
        let actual = frame(&processor, view, adjustments, masks, roi);
        assert_eq!(
            processor.last_display_render_pixels.load(Ordering::Relaxed),
            expected_pixels,
            "rendered pixels: {label}"
        );
        assert_eq!(
            reference.last_display_render_pixels.load(Ordering::Relaxed),
            0,
            "reference counter reset: {label}"
        );
        let expected = frame(&reference, view, adjustments, masks, roi);
        assert_eq!(
            reference.last_display_render_pixels.load(Ordering::Relaxed),
            roi.map_or(full, |roi| roi.width as u64 * roi.height as u64),
            "reference must be complete: {label}"
        );
        reference
            .last_display_render_pixels
            .store(0, Ordering::Relaxed);
        assert!(actual == expected, "partial frame differs: {label}");
        checked += 1;
    };

    let masks = upload_test_masks(width, height, 3);
    check("cold", &view, *three, &masks, None, full);
    check("unchanged", &view, *three, &masks, None, 0);
    let deep_copies: Vec<_> = masks
        .iter()
        .map(|mask| Arc::new((**mask).clone()))
        .collect();
    check(
        "identical new bitmaps",
        &view,
        *three,
        &deep_copies,
        None,
        0,
    );

    let seam = painted(&masks, 0, (1990..2110, 40..95), 230);
    check(
        "stroke across tile seam",
        &view,
        *three,
        &seam,
        None,
        changed_area(&masks, &seam, None),
    );
    let two_layers = painted(
        &painted(&seam, 1, (10..30, 5..25), 0),
        2,
        (2400..2500, 250..300),
        255,
    );
    check(
        "two layers and image edges",
        &view,
        *three,
        &two_layers,
        None,
        changed_area(&seam, &two_layers, None),
    );
    let corner = painted(&two_layers, 2, (0..1, 299..300), 1);
    check(
        "single corner pixel",
        &view,
        *three,
        &corner,
        None,
        changed_area(&two_layers, &corner, None),
    );
    assert_eq!(changed_area(&two_layers, &corner, None), 1);
    // A long diagonal stroke: banded regions instead of its bounding box.
    let mut diagonal = corner.clone();
    for step in 0..60 {
        let x = 20 + step * 40;
        let y = step * 5;
        diagonal = painted(&diagonal, 0, (x..x + 36, y..(y + 12).min(height)), 201);
    }
    let diagonal_area = changed_area(&corner, &diagonal, None);
    assert!(
        diagonal_area * 4 < full,
        "diagonal fixture area {diagonal_area}"
    );
    check(
        "long diagonal stroke",
        &view,
        *three,
        &diagonal,
        None,
        diagonal_area,
    );
    let corner = diagonal;

    let roi = Some(Roi {
        x: 1900,
        y: 20,
        width: 401,
        height: 203,
    });
    check(
        "ROI change renders the ROI",
        &view,
        *three,
        &corner,
        roi,
        401 * 203,
    );
    let inside = painted(&corner, 1, (1850..2010, 200..260), 77);
    check(
        "stroke clipped to ROI",
        &view,
        *three,
        &inside,
        roi,
        changed_area(&corner, &inside, roi),
    );
    let outside = painted(&inside, 0, (10..40, 250..280), 9);
    check("stroke outside ROI", &view, *three, &outside, roi, 0);
    check("ROI to full image", &view, *three, &outside, None, full);

    // Readback jobs share scratch tiles and uniform buffers, not display state.
    let background_masks = upload_test_masks(257, 129, 2);
    let background_view = input(&context, 257, 129, 29);
    processor
        .run(
            &background_view,
            257,
            129,
            RenderRequest {
                adjustments: two.clone(),
                mask_bitmaps: &background_masks,
                lut: None,
                roi: None,
            },
            false,
            RenderOutputPrecision::EightBit,
        )
        .unwrap();
    let after_background = painted(&outside, 2, (600..900, 100..200), 140);
    check(
        "stroke after background job",
        &view,
        *three,
        &after_background,
        None,
        changed_area(&outside, &after_background, None),
    );

    let stroke = painted(&after_background, 0, (300..340, 30..60), 50);
    check("adjustment change", &view, *brighter, &stroke, None, full);
    check(
        "source change",
        &second_view,
        *brighter,
        &stroke,
        None,
        full,
    );
    check("fewer layers", &second_view, *two, &stroke[..2], None, full);
    let two_stroke = painted(&stroke[..2], 1, (1000..1100, 0..10), 3);
    check(
        "stroke after layer change",
        &second_view,
        *two,
        &two_stroke,
        None,
        changed_area(&stroke[..2], &two_stroke, None),
    );

    processor.force_exact_curves.store(true, Ordering::Relaxed);
    let exact = frame(&processor, &second_view, *two, &two_stroke, None);
    assert_eq!(
        processor.last_display_render_pixels.load(Ordering::Relaxed),
        full
    );
    processor.force_exact_curves.store(false, Ordering::Relaxed);
    assert_ne!(
        exact,
        frame(&reference, &second_view, *two, &two_stroke, None),
        "override fixture must change pixels"
    );
    reference
        .last_display_render_pixels
        .store(0, Ordering::Relaxed);
    check(
        "frame after a test override",
        &second_view,
        *two,
        &two_stroke,
        None,
        full,
    );
    check(
        "unchanged after override",
        &second_view,
        *two,
        &two_stroke,
        None,
        0,
    );
    println!("{checked} partial display frames match complete renders");
}

fn readback_pixels(
    processor: &GpuProcessor,
    view: &wgpu::TextureView,
    (width, height): (u32, u32),
    adjustments: AllAdjustments,
    masks: &[Arc<MaskBitmap>],
    precision: RenderOutputPrecision,
    shader: ReadbackShader,
) -> RenderedPixels {
    processor
        .run_with_shader(
            view,
            width,
            height,
            RenderRequest {
                adjustments: adjustments.into(),
                mask_bitmaps: masks,
                lut: None,
                roi: None,
            },
            false,
            precision,
            shader,
            None,
        )
        .unwrap()
        .0
}

fn sharpened_curve_stack(count: usize) -> serde_json::Value {
    let mut profile = curve_mask_stack(count, false);
    profile["sharpness"] = json!(40);
    profile["clarity"] = json!(20);
    profile
}

#[test]
#[ignore = "requires a native GPU; browser preview cache correctness and background isolation"]
fn browser_preview_readback_retains_intermediates_and_matches_uncached_pixels() {
    let context = context();
    let (width, height) = (257, 193);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let oracle = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 5);
    let other = input(&context, width, height, 7);
    let mut masks = upload_test_masks(width, height, 2);
    let features = preview_features(&get_all_adjustments_from_json(
        &sharpened_curve_stack(2),
        true,
        Some(1),
    ));
    prepare_preview_pipeline(&processor, features);
    prepare_preview_pipeline(&oracle, features);
    let started = Instant::now();
    while processor.detail_pipeline().is_none() || oracle.detail_pipeline().is_none() {
        assert!(
            started.elapsed().as_secs() < 60,
            "detail pipeline did not compile"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    for frame in 0..6 {
        let mut profile = sharpened_curve_stack(2);
        profile["exposure"] = json!(frame as f32 * 0.25);
        if frame >= 3 {
            profile["sharpness"] = json!(70);
        }
        if frame == 4 {
            masks.swap(0, 1);
        }
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        // A fresh preparation using the same floating-point passes is the
        // exact cache oracle. Separate versus inline detail arithmetic can
        // differ by one 8-bit level on Vulkan even without stale data.
        *oracle.blur_cache.lock().unwrap() = BlurCache::default();
        oracle.force_mask_upload.store(true, Ordering::Relaxed);
        let expected = readback_pixels(
            &oracle,
            &view,
            (width, height),
            *adjustments,
            &masks,
            RenderOutputPrecision::EightBit,
            ReadbackShader::Preview,
        );
        for _ in 0..2 {
            let actual = readback_pixels(
                &processor,
                &view,
                (width, height),
                *adjustments,
                &masks,
                RenderOutputPrecision::EightBit,
                ReadbackShader::Preview,
            );
            let (RenderedPixels::U8(actual), RenderedPixels::U8(expected)) = (&actual, &expected)
            else {
                panic!("expected 8-bit preview pixels");
            };
            assert_eq!(
                actual, expected,
                "frame {frame}: cached browser preview pixels"
            );
        }
        let retained = processor.blur_cache.lock().unwrap();
        assert_eq!(retained.input.as_ref(), Some(&view));
        assert!(retained.bytes > 0 && retained.bytes <= BLUR_CACHE_BYTES);
        assert!(retained.tiles.iter().any(|tile| tile.detail.is_some()));
        drop(retained);
        assert!(processor.mask_cache_retained_bytes() > 0);
        // Saving a thumbnail/export must leave the foreground intermediates intact.
        let _ = readback_pixels(
            &processor,
            &other,
            (width, height),
            *adjustments,
            &[],
            RenderOutputPrecision::EightBit,
            ReadbackShader::Export,
        );
        assert_eq!(
            processor.blur_cache.lock().unwrap().input.as_ref(),
            Some(&view)
        );
    }
    let adjustments = get_all_adjustments_from_json(&sharpened_curve_stack(2), true, Some(1));
    *oracle.blur_cache.lock().unwrap() = BlurCache::default();
    let actual = readback_pixels(
        &processor,
        &other,
        (width, height),
        *adjustments,
        &masks,
        RenderOutputPrecision::EightBit,
        ReadbackShader::Preview,
    );
    let expected = readback_pixels(
        &oracle,
        &other,
        (width, height),
        *adjustments,
        &masks,
        RenderOutputPrecision::EightBit,
        ReadbackShader::Preview,
    );
    let (RenderedPixels::U8(actual), RenderedPixels::U8(expected)) = (actual, expected) else {
        panic!("expected 8-bit preview pixels");
    };
    assert_eq!(
        actual, expected,
        "source switch must invalidate cached intermediates"
    );
}

#[test]
#[ignore = "requires a native GPU; display-shader readback against display and export pixels"]
fn display_shader_readback_matches_display_frames() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2600, 1024).unwrap();
    let mut checked = 0;
    let mut largest_export_delta = 0;
    // A thumbnail-sized frame and a two-tile frame with an odd height.
    for (width, height) in [(1280, 853), (2500, 301)] {
        let view = input(&context, width, height, 17);
        for (label, profile) in [
            ("twenty sharpened curves", sharpened_curve_stack(20)),
            ("mixed local effects", partial_frame_profile(3, 0.2)),
            (
                "global only",
                json!({"exposure":0.3,"sharpness":30,"curves":custom_curve()}),
            ),
        ] {
            let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
            prepare_preview_pipeline(&processor, preview_features(&adjustments));
            let masks = upload_test_masks(width, height, adjustments.mask_count as usize);
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
            let display = read_texture_data_roi(
                &context.device,
                &context.queue,
                &processor.test_working_texture(),
                wgpu::Origin3d::ZERO,
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                4,
            )
            .unwrap();
            let size = (width, height);
            let eight = RenderOutputPrecision::EightBit;
            let RenderedPixels::U8(readback) = readback_pixels(
                &processor,
                &view,
                size,
                *adjustments,
                &masks,
                eight,
                ReadbackShader::Display,
            ) else {
                unreachable!()
            };
            assert!(
                readback == display,
                "{label} {width}x{height}: display readback"
            );
            let RenderedPixels::U8(export) = readback_pixels(
                &processor,
                &view,
                size,
                *adjustments,
                &masks,
                eight,
                ReadbackShader::Export,
            ) else {
                unreachable!()
            };
            let delta = readback
                .iter()
                .zip(&export)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            largest_export_delta = largest_export_delta.max(delta);
            assert!(delta <= 1, "{label} {width}x{height}: export delta {delta}");
            // Sixteen-bit readback keeps the exact export shader.
            let sixteen = RenderOutputPrecision::SixteenBit;
            let RenderedPixels::U16(display_16) = readback_pixels(
                &processor,
                &view,
                size,
                *adjustments,
                &masks,
                sixteen,
                ReadbackShader::Display,
            ) else {
                unreachable!()
            };
            let RenderedPixels::U16(export_16) = readback_pixels(
                &processor,
                &view,
                size,
                *adjustments,
                &masks,
                sixteen,
                ReadbackShader::Export,
            ) else {
                unreachable!()
            };
            assert!(display_16 == export_16, "{label} {width}x{height}: 16-bit");
            checked += 3;
        }
    }
    println!(
        "{checked} display-shader readback comparisons passed; largest 8-bit export delta {largest_export_delta}"
    );
}

#[test]
#[ignore = "native GPU timing probe; synthetic thumbnail-sized readback, export vs display shader"]
fn thumbnail_readback_timing_probe() {
    let context = context();
    let (width, height) = (1280, 960);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 5);
    let adjustments = get_all_adjustments_from_json(&sharpened_curve_stack(20), true, Some(1));
    prepare_preview_pipeline(&processor, preview_features(&adjustments));
    let masks = upload_test_masks(width, height, 20);
    // Readback jobs rebuild masks and blurs each time, as thumbnails do.
    for frame in 0..14 {
        let order = if frame % 2 == 0 {
            [ReadbackShader::Export, ReadbackShader::Display]
        } else {
            [ReadbackShader::Display, ReadbackShader::Export]
        };
        for shader in order {
            let start = Instant::now();
            readback_pixels(
                &processor,
                &view,
                (width, height),
                *adjustments,
                &masks,
                RenderOutputPrecision::EightBit,
                shader,
            );
            if frame >= 2 {
                println!(
                    "{}",
                    json!({"probe":"thumbnail_readback","shader":format!("{shader:?}"),
                        "frame":frame-2,"width":width,"height":height,"mask_count":20,
                        "complete_ms":start.elapsed().as_secs_f64()*1000.0})
                );
            }
        }
    }
}

#[test]
#[ignore = "native GPU timing probe; post-idle cost of partial frames and full-frame copies"]
fn post_idle_partial_frame_probe() {
    let context = context();
    let (width, height) = (2304, 1728);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 7);
    let adjustments = get_all_adjustments_from_json(&sharpened_curve_stack(20), true, Some(1));
    prepare_preview_pipeline(&processor, preview_features(&adjustments));
    let mut masks = upload_test_masks(width, height, 20);
    let device = &context.device;
    let wait = || device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let render = |masks: &[Arc<MaskBitmap>]| {
        processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: masks,
                    lut: None,
                    roi: None,
                },
                true,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
    };
    render(&masks);
    wait();
    let targets = processor.display_targets(width, height);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Probe analytics copy"),
        size: (width * 4 * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    // The display path's two full-frame copies: analytics buffer and output.
    let copies = || {
        let mut encoder = device.create_command_encoder(&Default::default());
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        encoder.copy_texture_to_buffer(
            targets.working_texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
            },
            extent,
        );
        encoder.copy_texture_to_texture(
            targets.working_texture.as_image_copy(),
            targets.output_texture.as_image_copy(),
            extent,
        );
        context.queue.submit(Some(encoder.finish()));
    };
    let mut stroke = 0u32;
    for (round, idle) in [false, true]
        .into_iter()
        .flat_map(|idle| (0..8).map(move |r| (r, idle)))
    {
        for scenario in [
            "empty",
            "copies",
            "partial",
            "partial_and_copies",
            "wake_300ms_partial",
            "wake_1000ms_partial",
            "pings_600ms_partial",
        ] {
            if idle {
                std::thread::sleep(std::time::Duration::from_millis(2500));
            }
            // Simulate a stroke that starts by waking the GPU (or keeps it
            // awake with empty submissions) and ends with the partial frame.
            match scenario {
                "wake_300ms_partial" | "wake_1000ms_partial" => {
                    context.queue.submit(None);
                    let millis = if scenario == "wake_300ms_partial" {
                        300
                    } else {
                        1000
                    };
                    std::thread::sleep(std::time::Duration::from_millis(millis));
                }
                "pings_600ms_partial" => {
                    for _ in 0..36 {
                        context.queue.submit(None);
                        std::thread::sleep(std::time::Duration::from_millis(16));
                    }
                }
                _ => {}
            }
            // A 35k-pixel stroke, matching the in-app strokes' changed area.
            let x = 100 + (stroke % 8) * 260;
            let y = 100 + (stroke / 8 % 6) * 260;
            stroke += 1;
            masks = painted(
                &masks,
                0,
                (x..x + 250, y..y + 140),
                (stroke % 200) as u8 + 30,
            );
            let start = Instant::now();
            match scenario {
                "empty" => {
                    context.queue.submit(None);
                }
                "copies" => copies(),
                "partial"
                | "wake_300ms_partial"
                | "wake_1000ms_partial"
                | "pings_600ms_partial" => render(&masks),
                _ => {
                    render(&masks);
                    copies();
                }
            }
            wait();
            if matches!(scenario, "empty" | "copies") {
                // Keep the processor's previous frame current for the next stroke.
                render(&masks);
                wait();
            }
            println!(
                "{}",
                json!({"probe":"post_idle_partial","scenario":scenario,"after_idle":idle,
                    "round":round,"pixels":processor.last_display_render_pixels.load(Ordering::Relaxed),
                    "complete_ms":start.elapsed().as_secs_f64()*1000.0})
            );
        }
    }
}

fn diagonal_stroke(masks: &[Arc<MaskBitmap>], value: u8) -> Vec<Arc<MaskBitmap>> {
    // The in-app diagonal: about 2100x780 preview pixels with a 32-pixel radius.
    let mut masks = masks.to_vec();
    let mask = Arc::make_mut(&mut masks[0]).bytes_mut();
    for step in 0..100u32 {
        let x = 60 + step * 21;
        let y = 1600 - step * 78 / 10;
        for py in y..y + 64 {
            for px in x..x + 64 {
                mask.put_pixel(px, py, Luma([value]));
            }
        }
    }
    masks
}

#[test]
#[ignore = "native GPU timing probe; CPU side of diagonal vs compact partial frames"]
fn partial_frame_cpu_timing_probe() {
    let context = context();
    let (width, height) = (2304, 1728);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 9);
    let adjustments = get_all_adjustments_from_json(&sharpened_curve_stack(20), true, Some(1));
    prepare_preview_pipeline(&processor, preview_features(&adjustments));
    let mut masks = upload_test_masks(width, height, 20);
    let render = |masks: &[Arc<MaskBitmap>]| {
        processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: masks,
                    lut: None,
                    roi: None,
                },
                true,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
    };
    render(&masks);
    context
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    for frame in 0..16u32 {
        for shape in ["diagonal", "compact"] {
            let value = (frame * 2 + (shape == "compact") as u32) as u8 + 40;
            let next = if shape == "diagonal" {
                diagonal_stroke(&masks, value)
            } else {
                painted(&masks, 0, (900..1420, 500..1020), value)
            };
            let mut bands = vec![None; (height as usize).div_ceil(DIRTY_BAND_ROWS)];
            let diff_start = Instant::now();
            add_changed_mask_bands(&masks[0], &next[0], &mut bands);
            let regions = merge_dirty_bands(&bands).len();
            let diff_ms = diff_start.elapsed().as_secs_f64() * 1000.0;
            let start = Instant::now();
            render(&next);
            let cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
            context
                .device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            if frame >= 4 {
                println!(
                    "{}",
                    json!({"probe":"partial_frame_cpu","shape":shape,"frame":frame-4,"regions":regions,
                        "pixels":processor.last_display_render_pixels.load(Ordering::Relaxed),
                        "band_diff_ms":diff_ms,"run_cpu_ms":cpu_ms,
                        "complete_ms":start.elapsed().as_secs_f64()*1000.0})
                );
            }
            masks = next;
        }
    }
}

#[test]
fn chunked_differences_match_a_byte_scan() {
    let mut next = xorshift(0x1b87_3593);
    for length in [0usize, 1, 31, 32, 33, 64, 95, 2304] {
        let base: Vec<u8> = (0..length).map(|i| (i * 7 % 256) as u8).collect();
        assert_eq!(first_difference(&base, &base), None);
        assert_eq!(last_difference(&base, &base), None);
        for _ in 0..300 {
            let mut changed = base.clone();
            for _ in 0..(next() % 4) {
                if length > 0 {
                    let index = next() as usize % length;
                    changed[index] ^= 1 + (next() % 255) as u8;
                }
            }
            let first = base.iter().zip(&changed).position(|(a, b)| a != b);
            let last = base.iter().zip(&changed).rposition(|(a, b)| a != b);
            assert_eq!(first_difference(&base, &changed), first, "{length}");
            assert_eq!(last_difference(&base, &changed), last, "{length}");
        }
    }
}

#[test]
#[ignore = "timing probe; run with --ignored --nocapture"]
fn input_upload_band_timing_probe() {
    let context = context();
    let (width, height) = (8192, 6144);
    let image = DynamicImage::ImageRgb32F(ImageBuffer::from_fn(width, height, |x, y| {
        image::Rgb([x as f32 / 8192.0, y as f32 / 6144.0, 0.5])
    }));
    let wait = |context: &GpuContext| {
        context
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(context.queue.submit(std::iter::empty())),
                timeout: None,
            })
            .unwrap();
    };
    for band_rows in [height, 128, 256, 512, 1024, 2048] {
        let mut samples = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            let texture = if band_rows == height {
                // The former single upload: parallel conversion, one staging copy.
                use rayon::prelude::*;
                let mut data = vec![f16::ZERO; width as usize * 4 * height as usize];
                data.par_chunks_mut(width as usize * 4)
                    .enumerate()
                    .for_each(|(row, texels)| rgba_f16_row(&image, 0, row as u32, texels));
                context.device.create_texture_with_data(
                    &context.queue,
                    &wgpu::TextureDescriptor {
                        label: None,
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba16Float,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    TextureDataOrder::MipMajor,
                    bytemuck::cast_slice(&data),
                )
            } else {
                create_input_texture(&context.device, &context.queue, &image, band_rows).unwrap()
            };
            wait(&context);
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            drop(texture);
            wait(&context);
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "band rows {band_rows:5}: median {:.1} ms (min {:.1}, max {:.1})",
            samples[2], samples[0], samples[4]
        );
    }
}

/// Twenty curve layers with the QA fixture's geometry (alternating radial and
/// linear, full-resolution coordinates of a 4032 × 3024 photo). `linear` sets
/// the second layer's start, end and range.
fn shape_layer_stack(linear: (f32, f32, f32, f32, f32)) -> serde_json::Value {
    const RADIAL: [(f32, f32, f32); 10] = [
        (403.0, 378.0, 0.0),
        (2016.0, 378.0, 26.0),
        (3629.0, 378.0, 52.0),
        (1210.0, 1134.0, 78.0),
        (2822.0, 1134.0, 104.0),
        (403.0, 1890.0, 130.0),
        (2016.0, 1890.0, 156.0),
        (3629.0, 1890.0, 2.0),
        (1210.0, 2646.0, 28.0),
        (2822.0, 2646.0, 54.0),
    ];
    const LINEAR: [(f32, f32, f32, f32); 10] = [
        (328.0, 11.0, 2092.0, 745.0),
        (2769.0, 0.0, 2875.0, 1133.0),
        (1229.0, 700.0, 0.0, 1568.0),
        (1088.0, 839.0, 2944.0, 1429.0),
        (3471.0, 387.0, 3787.0, 1881.0),
        (1971.0, 1394.0, 449.0, 2386.0),
        (1858.0, 1669.0, 3786.0, 2111.0),
        (142.0, 1916.0, 664.0, 3024.0),
        (2703.0, 2093.0, 1329.0, 3024.0),
        (2640.0, 2502.0, 4032.0, 2790.0),
    ];
    let masks: Vec<_> = (0..20)
        .map(|i| {
            let sub_mask = if i % 2 == 0 {
                let (x, y, rotation) = RADIAL[i / 2];
                json!({"id":format!("shape-{i}-radial"),"type":"radial","visible":true,"invert":false,
                    "opacity":100,"mode":"additive","parameters":{"centerX":x,"centerY":y,
                    "radiusX":1008,"radiusY":756,"rotation":rotation,"feather":0.5}})
            } else {
                let (sx, sy, ex, ey, range) = if i == 1 {
                    linear
                } else {
                    let (sx, sy, ex, ey) = LINEAR[i / 2];
                    (sx, sy, ex, ey, 50.0)
                };
                json!({"id":format!("shape-{i}-linear"),"type":"linear","visible":true,"invert":false,
                    "opacity":100,"mode":"additive","parameters":{"startX":sx,"startY":sy,
                    "endX":ex,"endY":ey,"range":range}})
            };
            json!({"id":format!("shape-{i}"),"name":format!("Curve {i}"),"visible":true,
                "invert":false,"opacity":25,"subMasks":[sub_mask],
                "adjustments":{"curves":{
                    "luma":[{"x":0,"y":8},{"x":55,"y":43},{"x":165,"y":191},{"x":255,"y":248}],
                    "red":[{"x":0,"y":0},{"x":128,"y":138},{"x":255,"y":255}],
                    "blue":[{"x":0,"y":4},{"x":128,"y":128},{"x":255,"y":251}]}}})
        })
        .collect();
    json!({"masks":masks})
}

#[test]
#[ignore = "native GPU timing probe; linear gradient drags on the twenty-layer fixture geometry"]
fn linear_drag_region_probe() {
    let context = context();
    // The app's preview of the 4032 × 3024 fixture.
    let (width, height) = (1808, 1356);
    let scale = width as f32 / 4032.0;
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 11);
    let masks_for = |linear| {
        let definitions: Vec<MaskDefinition> =
            serde_json::from_value(shape_layer_stack(linear)["masks"].clone()).unwrap();
        definitions
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
            .collect::<Vec<_>>()
    };
    let adjustments = get_all_adjustments_from_json(
        &shape_layer_stack((328.0, 11.0, 2092.0, 745.0, 50.0)),
        true,
        Some(1),
    );
    prepare_preview_pipeline(&processor, preview_features(&adjustments));
    let render = |masks: &[Arc<MaskBitmap>]| {
        let start = Instant::now();
        processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: masks,
                    lut: None,
                    roi: None,
                },
                true,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        let cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
        context
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        cpu_ms
    };
    let total = (width * height) as f64;
    // Per 60 Hz move: translate the gradient, rotate it about its centre, or
    // widen its transition (dragging an end line); plus a full redraw.
    for gesture in [
        "translate",
        "rotate",
        "range",
        "wide_translate",
        "wide_rotate",
    ] {
        let state = |step: u32| {
            let t = step as f32;
            match gesture {
                "translate" => (
                    328.0 + 6.0 * t,
                    11.0 + 14.0 * t,
                    2092.0 + 6.0 * t,
                    745.0 + 14.0 * t,
                    50.0,
                ),
                "rotate" => {
                    let (cx, cy, half, angle) =
                        (1210.0, 378.0, 956.0, (22.6 + 0.6 * t).to_radians());
                    let (dx, dy) = (half * angle.cos(), half * angle.sin());
                    (cx - dx, cy - dy, cx + dx, cy + dy, 50.0)
                }
                "range" => (328.0, 11.0, 2092.0, 745.0, 300.0 + 12.0 * t),
                // A gradient drawn Capture One-style over a third of the photo.
                "wide_translate" => (
                    328.0 + 6.0 * t,
                    11.0 + 14.0 * t,
                    2092.0 + 6.0 * t,
                    745.0 + 14.0 * t,
                    500.0,
                ),
                "wide_rotate" => {
                    let (cx, cy, half, angle) =
                        (1210.0, 1378.0, 956.0, (22.6 + 0.6 * t).to_radians());
                    let (dx, dy) = (half * angle.cos(), half * angle.sin());
                    (cx - dx, cy - dy, cx + dx, cy + dy, 500.0)
                }
                _ => (328.0, 11.0, 2092.0, 745.0, 50.0),
            }
        };
        // Bitmaps first: generating them between frames would idle the GPU,
        // which then clocks down. Frames follow at the 60 Hz pointer rate.
        let base = masks_for(state(0));
        let states: Vec<Vec<Arc<MaskBitmap>>> = (0..=24u32)
            .map(|step| {
                let mut masks = base.clone();
                if step > 0 {
                    masks[1] = masks_for(state(step)).swap_remove(1);
                }
                masks
            })
            .collect();
        let mut masks = states[0].clone();
        render(&masks);
        let mut next_frame = Instant::now();
        for step in 1..=24u32 {
            let next = states[step as usize].clone();
            let changed = next[1]
                .pixels()
                .zip(masks[1].pixels())
                .filter(|(a, b)| a != b)
                .count() as f64;
            let regions = merge_dirty_bands(&brute_force_changed_bands(&masks, &next, height));
            // 8 × 8 blocks (one compute workgroup) with any changed pixel.
            let mut blocks = std::collections::HashSet::new();
            for (x, y, pixel) in next[1].enumerate_pixels() {
                if masks[1].get_pixel(x, y) != pixel {
                    blocks.insert((x / 8, y / 8));
                }
            }
            // Redrawn area and regions if bands had other heights.
            let band_options: Vec<_> = [8usize, 16, 32, 64]
                .into_iter()
                .map(|rows| {
                    let mut bands = vec![None; (height as usize).div_ceil(rows)];
                    for (x, y, pixel) in next[1].enumerate_pixels() {
                        if masks[1].get_pixel(x, y) != pixel {
                            let pixel = Roi {
                                x,
                                y,
                                width: 1,
                                height: 1,
                            };
                            let band = &mut bands[y as usize / rows];
                            *band = Some(union_roi(*band, pixel));
                        }
                    }
                    let merged = merge_dirty_bands(&bands);
                    let area: u64 = merged
                        .iter()
                        .map(|r| r.width as u64 * r.height as u64)
                        .sum();
                    json!({"rows":rows,"regions":merged.len(),"fraction":area as f64 / total})
                })
                .collect();
            next_frame += std::time::Duration::from_micros(16_667);
            if let Some(wait) = next_frame.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            let start = Instant::now();
            let cpu_ms = render(&next);
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            if step > 4 {
                println!(
                    "{}",
                    json!({"probe":"linear_drag","gesture":gesture,"step":step,
                        "changed_fraction":changed / total,
                        "redrawn_fraction":processor.last_display_render_pixels.load(Ordering::Relaxed) as f64 / total,
                        "block_fraction":blocks.len() as f64 * 64.0 / total,
                        "regions":regions.len(),"cpu_ms":cpu_ms,"complete_ms":ms,
                        "bands":band_options})
                );
            }
            masks = next;
        }
    }
}

#[test]
#[ignore = "native GPU timing probe; full preview frames with and without the twenty shape layers"]
fn shape_layer_cost_probe() {
    let context = context();
    let (width, height) = (1808, 1356);
    let scale = width as f32 / 4032.0;
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 13);
    let stack = shape_layer_stack((328.0, 11.0, 2092.0, 745.0, 50.0));
    let definitions: Vec<MaskDefinition> = serde_json::from_value(stack["masks"].clone()).unwrap();
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
    let covered: Vec<f64> = masks
        .iter()
        .map(|mask| mask.pixels().filter(|p| p.0[0] > 0).count() as f64 / (width * height) as f64)
        .collect();
    println!(
        "{}",
        json!({"probe":"shape_layer_cost","mask_coverage":covered})
    );
    let raw_defaults = json!({"sharpness":40,"colorNoiseReduction":25});
    let mut with_defaults = stack.clone();
    with_defaults["sharpness"] = json!(40);
    with_defaults["colorNoiseReduction"] = json!(25);
    for (name, profile, layers) in [
        ("no_layers", json!({}), 0),
        ("no_layers_raw_defaults", raw_defaults, 0),
        ("twenty_layers", stack, 20),
        ("twenty_layers_raw_defaults", with_defaults, 20),
    ] {
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        prepare_preview_pipeline(&processor, preview_features(&adjustments));
        let mut next_frame = Instant::now();
        for frame in 0..24 {
            // Back to back for half the frames, then at the 60 Hz pointer rate:
            // with idle time between frames the GPU runs at lower clocks.
            let paced = frame >= 12;
            if paced {
                next_frame += std::time::Duration::from_micros(16_667);
                if let Some(wait) = next_frame.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
            } else {
                next_frame = Instant::now();
            }
            // A different global exposure every frame forces a complete frame.
            let mut profile = profile.clone();
            profile["exposure"] = json!(0.01 * (frame % 2) as f64);
            let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
            let start = Instant::now();
            processor
                .run(
                    &view,
                    width,
                    height,
                    RenderRequest {
                        adjustments: adjustments.clone(),
                        mask_bitmaps: &masks[..layers],
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
            if frame % 12 >= 4 {
                println!(
                    "{}",
                    json!({"probe":"shape_layer_cost","profile":name,"frame":frame,"paced":paced,
                        "pixels":processor.last_display_render_pixels.load(Ordering::Relaxed),
                        "complete_ms":start.elapsed().as_secs_f64()*1000.0})
                );
            }
        }
    }
}

#[test]
#[ignore = "native GPU timing probe; interleaved cost of RAW detail defaults on the twenty shape layers"]
fn detail_default_cost_probe() {
    let context = context();
    let (width, height) = (1808, 1356);
    let scale = width as f32 / 4032.0;
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 17);
    let stack = shape_layer_stack((328.0, 11.0, 2092.0, 745.0, 50.0));
    let definitions: Vec<MaskDefinition> = serde_json::from_value(stack["masks"].clone()).unwrap();
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
    let variants: Vec<(&str, serde_json::Value)> = [
        ("plain", 0, 0),
        ("sharpening_40", 40, 0),
        ("color_nr_25", 0, 25),
        ("both", 40, 25),
    ]
    .into_iter()
    .map(|(name, sharpness, color_nr)| {
        let mut profile = stack.clone();
        profile["sharpness"] = json!(sharpness);
        profile["colorNoiseReduction"] = json!(color_nr);
        (name, profile)
    })
    .collect();
    for (_, profile) in &variants {
        let adjustments = get_all_adjustments_from_json(profile, true, Some(1));
        prepare_preview_pipeline(&processor, preview_features(&adjustments));
    }
    for round in 0..16 {
        for (name, profile) in &variants {
            let mut profile = profile.clone();
            // A different global exposure every frame forces a complete frame.
            profile["exposure"] = json!(0.01 * (round % 2) as f64);
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
            if round >= 4 {
                println!(
                    "{}",
                    json!({"probe":"detail_default_cost","variant":name,"round":round,
                        "pixels":processor.last_display_render_pixels.load(Ordering::Relaxed),
                        "complete_ms":start.elapsed().as_secs_f64()*1000.0})
                );
            }
        }
    }
}

#[test]
fn display_cache_bytes_counts_every_tile_with_overlap() {
    // One tile: the input is the frame.
    assert_eq!(
        display_cache_bytes(1808, 1356, 2048, 128, 1),
        1808 * 1356 * 24
    );
    // Two tiles across: 0..2176 and 1920..2304.
    assert_eq!(
        display_cache_bytes(2304, 1728, 2048, 128, 1),
        (2176 + 384) * 1728 * 24
    );
    // Four tiles, two blurs.
    assert_eq!(
        display_cache_bytes(2200, 2100, 2048, 128, 2),
        (2176 + 280) * (2100 + 180) * 32
    );
    // The app's 2304 x 1728 preview fits with the noise reduction's three
    // blur levels and even with all five.
    assert!(display_cache_bytes(2304, 1728, 2048, 128, 3) <= BLUR_CACHE_BYTES);
    assert!(display_cache_bytes(2304, 1728, 2048, 128, 5) <= BLUR_CACHE_BYTES);
    assert!(display_cache_bytes(2200, 2100, 2048, 128, 3) <= BLUR_CACHE_BYTES);
    assert!(display_cache_bytes(4032, 3024, 2048, 128, 1) > BLUR_CACHE_BYTES);
}

#[test]
#[ignore = "requires a native GPU; cached detail stage against the inline one"]
fn cached_detail_matches_inline_detail() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2304, 2200).unwrap();
    let started = Instant::now();
    while processor.detail_pipeline().is_none() {
        assert!(
            started.elapsed().as_secs() < 60,
            "detail pipeline did not compile"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let profiles = [
        json!({"sharpness":40,"colorNoiseReduction":25}),
        json!({"sharpness":45,"lumaNoiseReduction":30,"colorNoiseReduction":60,"shadows":40}),
        json!({"sharpness":-30,"chromaticAberrationRedCyan":0.002}),
        // A layer changing sharpening keeps the inline stage where it applies.
        json!({"sharpness":40,"colorNoiseReduction":25,
            "masks":[local_mask("detail-layer", json!({"sharpness":30,"exposure":0.4}))]}),
        json!({"sharpness":53,"vibrance":72,"saturation":27,"brightness":0.25,
            "masks":(0..20).map(|i| local_mask(&format!("m{i}"), json!({"curves":{
                "luma":[{"x":0,"y":8},{"x":55,"y":43},{"x":165,"y":191},{"x":255,"y":248}]}})))
                .collect::<Vec<_>>()}),
    ];
    let mut checked = 0;
    // The last two sizes are two and four tiles (the app's preview on a large
    // display is 2304 x 1728).
    for (width, height) in [
        (517, 389),
        (1280, 853),
        (2000, 1500),
        (2304, 1728),
        (2200, 2100),
    ] {
        let view = input(&context, width, height, 21);
        for profile in &profiles {
            let adjustments = get_all_adjustments_from_json(profile, true, Some(1));
            prepare_preview_pipeline(&processor, preview_features(&adjustments));
            let masks = generated_profile_masks(profile, width, height);
            let render = |inline: bool| {
                processor
                    .force_inline_detail
                    .store(inline, Ordering::Relaxed);
                // A changed exposure forces a complete frame each time.
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
                *processor.display_render_state.lock().unwrap() = None;
                read_texture_data_roi(
                    &context.device,
                    &context.queue,
                    &processor.test_working_texture(),
                    wgpu::Origin3d::ZERO,
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    4,
                )
                .unwrap()
            };
            let inline = render(true);
            let cached = render(false);
            let cached_again = render(false);
            let tiles = (width.div_ceil(2048) * height.div_ceil(2048)) as usize;
            let cached_tiles = processor
                .blur_cache
                .lock()
                .unwrap()
                .tiles
                .iter()
                .filter(|tile| tile.detail.is_some())
                .count();
            assert_eq!(
                cached_tiles, tiles,
                "{width}x{height} {profile}: cached detail tiles"
            );
            processor
                .force_inline_detail
                .store(false, Ordering::Relaxed);
            assert_eq!(cached, cached_again, "{profile}: cached frames differ");
            // Stored as 32-bit floats, the cache reproduces the inline stage exactly
            // (16-bit storage flipped saturated channels by up to 220 levels).
            assert!(
                inline == cached,
                "{width}x{height} {profile}: cached detail differs"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 25);
}

#[test]
#[ignore = "native GPU timing probe; cached against inline detail stage, interleaved"]
fn detail_cache_timing_probe() {
    let context = context();
    let (width, height) = (1808, 1356);
    let scale = width as f32 / 4032.0;
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let started = Instant::now();
    while processor.detail_pipeline().is_none() {
        assert!(
            started.elapsed().as_secs() < 60,
            "detail pipeline did not compile"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let view = input(&context, width, height, 23);
    let masks_for = |linear| {
        let definitions: Vec<MaskDefinition> =
            serde_json::from_value(shape_layer_stack(linear)["masks"].clone()).unwrap();
        definitions
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
            .collect::<Vec<_>>()
    };
    let mut profile = shape_layer_stack((328.0, 11.0, 2092.0, 745.0, 500.0));
    profile["sharpness"] = json!(40);
    profile["colorNoiseReduction"] = json!(25);
    let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
    prepare_preview_pipeline(&processor, preview_features(&adjustments));
    let base = masks_for((328.0, 11.0, 2092.0, 745.0, 500.0));
    // A wide gradient moved at the 60 Hz pointer rate.
    let states: Vec<Vec<Arc<MaskBitmap>>> = (0..=48u32)
        .map(|step| {
            let t = step as f32;
            let mut masks = base.clone();
            masks[1] = masks_for((
                328.0 + 6.0 * t,
                11.0 + 14.0 * t,
                2092.0 + 6.0 * t,
                745.0 + 14.0 * t,
                500.0,
            ))
            .swap_remove(1);
            masks
        })
        .collect();
    let cpu = std::cell::Cell::new(0.0);
    let render = |adjustments: AllAdjustments, masks: &[Arc<MaskBitmap>]| {
        let start = Instant::now();
        processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.into(),
                    mask_bitmaps: masks,
                    lut: None,
                    roi: None,
                },
                true,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        cpu.set(start.elapsed().as_secs_f64() * 1000.0);
        context
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        start.elapsed().as_secs_f64() * 1000.0
    };
    let mut next_frame = Instant::now();
    let mut pace = || {
        next_frame += std::time::Duration::from_micros(16_667);
        if let Some(wait) = next_frame.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    };
    // Complete frames: a different exposure every frame.
    for frame in 0..32u32 {
        let inline = frame % 2 == 0;
        processor
            .force_inline_detail
            .store(inline, Ordering::Relaxed);
        let mut profile = profile.clone();
        profile["exposure"] = json!(0.01 * frame as f64);
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        pace();
        let ms = render(*adjustments, &states[0]);
        if frame >= 8 {
            println!(
                "{}",
                json!({"probe":"detail_cache","scenario":"complete","inline":inline,"complete_ms":ms,"cpu_ms":cpu.get(),
                    "pixels":processor.last_display_render_pixels.load(Ordering::Relaxed)})
            );
        }
    }
    // CPU: the changed layer's band diff alone.
    for step in 1..states.len() {
        let mut bands = vec![None; (height as usize).div_ceil(DIRTY_BAND_ROWS)];
        let start = Instant::now();
        let rows = add_changed_mask_bands(&states[step - 1][1], &states[step][1], &mut bands);
        let diff_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let merged = merge_dirty_bands(&bands).len();
        let merge_ms = start.elapsed().as_secs_f64() * 1000.0;
        if step > 8 {
            println!(
                "{}",
                json!({"probe":"detail_cache","scenario":"band_diff","diff_ms":diff_ms,"merge_ms":merge_ms,
                    "rows":rows.map(|(top, bottom)| bottom - top),"regions":merged})
            );
        }
    }
    // Partial frames of the gradient drag.
    render(*adjustments, &states[0]);
    for (step, masks) in states.iter().enumerate().skip(1) {
        let inline = step % 2 == 0;
        processor
            .force_inline_detail
            .store(inline, Ordering::Relaxed);
        pace();
        let ms = render(*adjustments, masks);
        if step > 8 {
            println!(
                "{}",
                json!({"probe":"detail_cache","scenario":"wide_gradient_drag","inline":inline,"complete_ms":ms,"cpu_ms":cpu.get(),
                    "pixels":processor.last_display_render_pixels.load(Ordering::Relaxed)})
            );
        }
    }
    // The floor: one changed pixel per frame.
    let mut tiny = states[48].clone();
    for step in 0..32u32 {
        let mut mask = (*tiny[1]).clone().into_bytes();
        let pixel = mask.get_pixel_mut(900 + step, 700);
        pixel.0[0] = pixel.0[0].wrapping_add(1);
        tiny[1] = masked(mask);
        processor
            .force_inline_detail
            .store(step % 2 == 0, Ordering::Relaxed);
        pace();
        let ms = render(*adjustments, &tiny);
        if step >= 8 {
            println!(
                "{}",
                json!({"probe":"detail_cache","scenario":"one_pixel","inline":step % 2 == 0,"complete_ms":ms,"cpu_ms":cpu.get(),
                    "pixels":processor.last_display_render_pixels.load(Ordering::Relaxed)})
            );
        }
    }
    processor
        .force_inline_detail
        .store(false, Ordering::Relaxed);
}

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
    for features in
        (1..PREVIEW_PIPELINE_COUNT).filter(|f| f & REFINED_NR_PREVIEW_FEATURE == 0 || f & 16 != 0)
    {
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

#[test]
#[ignore = "requires a native GPU; the all-features fallback variant against each exact variant"]
fn fallback_variant_matches_specialized_display() {
    let context = context();
    let (width, height) = (517, 389);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let started = Instant::now();
    while processor.preview_pipelines[ALL_PREVIEW_FEATURES]
        .get()
        .is_none()
    {
        assert!(
            started.elapsed().as_secs() < 60,
            "fallback variant did not compile"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let view = input(&context, width, height, 31);
    let mut checked = 0;
    for profile in [
        json!({}),
        json!({"sharpness":40,"colorNoiseReduction":25}),
        json!({"highlights":-40,"curves":custom_curve(),"lumaNoiseReduction":20}),
        json!({"sharpness":40,"lumaNoiseReduction":20,"colorNoiseReduction":25,
            "lumaNoiseDetail":70,"colorNoiseSmoothness":20}),
        curve_mask_stack(6, false),
        json!({"sharpness":53,"vibrance":72,"saturation":27,"brightness":0.25,
            "masks":(0..4).map(|i| local_mask(&format!("m{i}"), json!({"curves":custom_curve(),"highlights":30})))
                .collect::<Vec<_>>()}),
    ] {
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        prepare_preview_pipeline(&processor, preview_features(&adjustments));
        let masks = generated_profile_masks(&profile, width, height);
        let render = |fallback: bool| {
            processor
                .force_fallback_variant
                .store(fallback, Ordering::Relaxed);
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
            read_texture_data_roi(
                &context.device,
                &context.queue,
                &processor.test_working_texture(),
                wgpu::Origin3d::ZERO,
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                4,
            )
            .unwrap()
        };
        let exact = render(false);
        let fallback = render(true);
        processor
            .force_fallback_variant
            .store(false, Ordering::Relaxed);
        let max_delta = exact
            .iter()
            .zip(&fallback)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        // The same tolerance as between display variants and the general shader.
        assert!(max_delta <= 1, "{profile}: fallback delta {max_delta}");
        checked += 1;
    }
    assert_eq!(checked, 6);
}

#[test]
#[ignore = "native GPU timing probe; synthetic 25 MP input through the CPU-readback export path"]
fn export_render_timing_probe() {
    let context = context();
    let (width, height) = (6144, 4096);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 3);
    for (name, profile) in [
        (
            "raw_defaults",
            json!({"sharpness":40,"lumaNoiseReduction":15,"colorNoiseReduction":25}),
        ),
        ("sharpen_only", json!({"sharpness":40})),
        (
            "noise_only",
            json!({"lumaNoiseReduction":15,"colorNoiseReduction":25}),
        ),
        ("plain", json!({})),
    ] {
        let mut adjustments = get_all_adjustments_from_json(&profile, true, Some(2));
        // The Camera Raw look with neutral colour, as RAW files render.
        adjustments.global.tonemapper_mode = 3;
        adjustments.global.profile_curve[15][3] = 2.0;
        let mut times = Vec::new();
        for _ in 0..5 {
            let start = Instant::now();
            processor
                .run(
                    &view,
                    width,
                    height,
                    RenderRequest {
                        adjustments: adjustments.clone(),
                        mask_bitmaps: &[],
                        lut: None,
                        roi: None,
                    },
                    false,
                    RenderOutputPrecision::EightBit,
                )
                .unwrap();
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!(
            "{}",
            json!({"profile": name, "median_ms": times[2], "min_ms": times[0]})
        );
    }
}

#[test]
#[ignore = "native GPU probe; counts specialized vs general export differences"]
fn export_specialization_difference_probe() {
    let context = context();
    let (width, height) = (3000, 2000);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 5);
    for (name, profile) in [
        (
            "raw_defaults",
            json!({"sharpness":40,"lumaNoiseReduction":15,"colorNoiseReduction":25}),
        ),
        ("sharpen_only", json!({"sharpness":40})),
        (
            "noise_only",
            json!({"lumaNoiseReduction":15,"colorNoiseReduction":25}),
        ),
        (
            "edited",
            json!({"sharpness":60,"lumaNoiseReduction":30,"colorNoiseReduction":25,"exposure":0.5,"contrast":20,"shadows":30,"highlights":-40,"clarity":20}),
        ),
    ] {
        let mut adjustments = get_all_adjustments_from_json(&profile, true, Some(2));
        adjustments.global.tonemapper_mode = 3;
        adjustments.global.profile_curve[15][3] = 2.0;
        for precision in [
            RenderOutputPrecision::EightBit,
            RenderOutputPrecision::SixteenBit,
        ] {
            let render = |general: bool| {
                processor
                    .force_general_pipeline
                    .store(general, Ordering::Relaxed);
                processor
                    .run(
                        &view,
                        width,
                        height,
                        RenderRequest {
                            adjustments: adjustments.clone(),
                            mask_bitmaps: &[],
                            lut: None,
                            roi: None,
                        },
                        false,
                        precision,
                    )
                    .unwrap()
                    .0
            };
            let (general, special) = (render(true), render(false));
            let (diff, max) = match (general, special) {
                (RenderedPixels::U8(a), RenderedPixels::U8(b)) => (
                    a.iter().zip(&b).filter(|(x, y)| x != y).count(),
                    a.iter()
                        .zip(&b)
                        .map(|(x, y)| x.abs_diff(*y) as u32)
                        .max()
                        .unwrap_or(0),
                ),
                (RenderedPixels::U16(a), RenderedPixels::U16(b)) => (
                    a.iter().zip(&b).filter(|(x, y)| x != y).count(),
                    a.iter()
                        .zip(&b)
                        .map(|(x, y)| x.abs_diff(*y) as u32)
                        .max()
                        .unwrap_or(0),
                ),
                _ => panic!("precision mismatch"),
            };
            println!(
                "{}",
                json!({"profile": name, "sixteen_bit": precision == RenderOutputPrecision::SixteenBit, "differing_samples": diff, "max_difference": max})
            );
        }
    }
    processor
        .force_general_pipeline
        .store(false, Ordering::Relaxed);
}

#[test]
#[ignore = "requires a native GPU; wide-gamut export pipeline against the sRGB one"]
fn wide_gamut_exports_keep_colours_beyond_srgb_and_match_inside_it() {
    use rawtonic_engine::output_color_space::{OutputColorSpace, TransferCurve};
    let context = context();
    let (width, height) = (64u32, 32u32);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    // Left half: colours inside sRGB; right half: a Display P3 green and red
    // (linear sRGB with negative channels), as a wide decode delivers them.
    let colour = |x: u32, y: u32| -> [f32; 3] {
        let level = 0.05 + 0.25 * (y as f32 / height as f32);
        let base = if x < width / 2 {
            [
                [0.8, 0.3, 0.1],
                [0.2, 0.5, 0.7],
                [0.4, 0.4, 0.4],
                [0.1, 0.6, 0.2],
            ][(x / 8) as usize]
        } else if x < width * 3 / 4 {
            [-0.2247, 1.0421, -0.0786]
        } else {
            [1.2249, -0.0420, -0.0196]
        };
        base.map(|v| v * level * 3.0)
    };
    let data: Vec<f16> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let [r, g, b] = colour(x, y);
            [
                f16::from_f32(r),
                f16::from_f32(g),
                f16::from_f32(b),
                f16::ONE,
            ]
        })
        .collect();
    let view = context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Wide-gamut input"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&data),
        )
        .create_view(&Default::default());
    // Camera Raw look (3), AgX (1) and Classic (0).
    let render_with =
        |recipe: serde_json::Value, space: OutputColorSpace, tonemapper: u32| -> Vec<f64> {
            let mut adjustments = get_all_adjustments_from_json(&recipe, true, Some(tonemapper));
            adjustments.global.output_space[0] = space.shader_code();
            adjustments.global.output_matrix =
                crate::image_processing::gpu_mat3_from_rows(space.from_linear_srgb_f32());
            let (pixels, ..) = processor
                .run(
                    &view,
                    width,
                    height,
                    RenderRequest {
                        adjustments: adjustments.clone(),
                        mask_bitmaps: &[],
                        lut: None,
                        roi: None,
                    },
                    false,
                    RenderOutputPrecision::SixteenBit,
                )
                .unwrap();
            let RenderedPixels::U16(pixels) = pixels else {
                unreachable!()
            };
            // Back to linear sRGB (negative outside sRGB).
            let to_srgb = space.to_linear_srgb();
            let transfer = space.transfer();
            pixels
                .chunks(4)
                .flat_map(|p| {
                    let rgb = [p[0], p[1], p[2]].map(|v| transfer.to_linear(v as f64 / 65535.0));
                    to_srgb.map(|row| row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2])
                })
                .collect()
        };
    let render = |space: OutputColorSpace, tonemapper: u32| {
        render_with(json!({ "saturation": 20 }), space, tonemapper)
    };
    let at = |pixels: &[f64], x: u32, y: u32| {
        let i = ((y * width + x) * 3) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    };
    for (tonemapper, space) in [3, 1, 0].into_iter().flat_map(|t| {
        [
            OutputColorSpace::DisplayP3,
            OutputColorSpace::AdobeRgb,
            OutputColorSpace::ProPhoto,
        ]
        .map(|space| (t, space))
    }) {
        let srgb = render(OutputColorSpace::Srgb, tonemapper);
        let wide = render(space, tonemapper);
        let space_label = format!("look {tonemapper} {space:?}");
        for y in (2..height).step_by(5) {
            for x in (1..width / 2).step_by(3) {
                let (a, b) = (at(&srgb, x, y), at(&wide, x, y));
                for c in 0..3 {
                    let encoded = |v: f64| TransferCurve::Srgb.from_linear(v.clamp(0.0, 1.0));
                    assert!(
                        (encoded(a[c]) - encoded(b[c])).abs() < 2.0 / 255.0,
                        "{space_label} inside sRGB at {x},{y}: {a:?} vs {b:?}"
                    );
                }
            }
            // The P3 green keeps negative red and blue (beyond sRGB) where
            // the sRGB export clips them to zero.
            let green = at(&wide, width * 5 / 8, y);
            let clipped = at(&srgb, width * 5 / 8, y);
            let lowest = |c: [f64; 3]| c[0].min(c[1]).min(c[2]);
            if tonemapper == 1 {
                // AgX desaturates towards white by design; the wide export
                // still keeps more of the green than the sRGB one.
                assert!(
                    lowest(green) < lowest(clipped) - 1e-4,
                    "{space_label} green at {y}: {green:?} vs {clipped:?}"
                );
            } else {
                assert!(
                    green[0] < -0.01 && green[1] > 0.05,
                    "{space_label} green at {y}: {green:?}"
                );
            }
            assert!(clipped[0] >= -1e-6, "sRGB export: {clipped:?}");
            // Adobe RGB has sRGB's red primary: the P3 red clips to it.
            let red = at(&wide, width * 7 / 8, y);
            // AgX's own compression keeps this red inside sRGB.
            if space != OutputColorSpace::AdobeRgb && tonemapper != 1 {
                assert!(
                    red[1] < -0.001 || red[2] < -0.001,
                    "{space_label} red at {y}: {red:?}"
                );
            }
        }
    }
    // Soft proofing on a Display P3 display: proofing sRGB shows what the sRGB
    // export keeps, and the gamut warning marks the P3 green red.
    let srgb = render(OutputColorSpace::Srgb, 3);
    let proofed = render_with(
        json!({ "saturation": 20, "softProof": "srgb" }),
        OutputColorSpace::DisplayP3,
        3,
    );
    let warned = render_with(
        json!({ "saturation": 20, "softProof": "srgb", "softProofWarning": true }),
        OutputColorSpace::DisplayP3,
        3,
    );
    for y in (2..height).step_by(5) {
        for x in (1..width).step_by(3) {
            let (a, b) = (at(&srgb, x, y), at(&proofed, x, y));
            for c in 0..3 {
                let encoded = |v: f64| TransferCurve::Srgb.from_linear(v.clamp(0.0, 1.0));
                assert!(
                    (encoded(a[c]) - encoded(b[c])).abs() < 2.0 / 255.0,
                    "proof at {x},{y}: {a:?} vs {b:?}"
                );
            }
        }
        let kept = at(&proofed, width * 5 / 8, y);
        assert!(
            kept.iter().all(|v| *v > -2e-3),
            "proofed green at {y}: {kept:?}"
        );
        let green = at(&warned, width * 5 / 8, y);
        assert!(
            green[0] > 0.9 && green[1] < 0.01 && green[2] < 0.01,
            "warning at {y}: {green:?}"
        );
        // The grey patch: no warning.
        assert_eq!(at(&warned, 17, y), at(&proofed, 17, y), "warning on grey");
    }
}

/// Shadows and Blacks under the Camera Raw look take their gain from the
/// blurred luminance, so a step in that gain shows as contour lines along the
/// blur and, below the black ramp, as grey blobs in deep black. A smooth grey
/// ramp from deep black to mid-grey must stay smooth and ordered for extreme
/// tone settings (Contrast too, per channel). Raising Shadows or Blacks and
/// lowering Contrast lift the toe below the black point like Camera Raw
/// (by a gain: true black stays black); lowering keeps crushed black black.
#[test]
#[ignore = "requires a native GPU; Camera Raw look tone gains on a smooth ramp"]
fn camera_raw_shadow_gains_stay_smooth_down_to_black() {
    let context = context();
    let (width, height) = (2048u32, 8u32);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    // Scene luminance from 2^-14 to 2^-1, evenly in EV across the columns.
    let level = |x: u32| (2.0f32).powf(-14.0 + 13.0 * x as f32 / (width - 1) as f32);
    let data: Vec<f16> = (0..height)
        .flat_map(|_| 0..width)
        .flat_map(|x| {
            let v = f16::from_f32(level(x));
            [v, v, v, f16::ONE]
        })
        .collect();
    let view = context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Grey ramp"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&data),
        )
        .create_view(&Default::default());
    // Linear output luminance of the middle row.
    // `imageKey` in a recipe sets the measured image key (image_key.rs).
    let render = |recipe: serde_json::Value| -> Vec<f64> {
        let mut adjustments = get_all_adjustments_from_json(&recipe, true, Some(2));
        // The Camera Raw look with neutral colour, as RAW files render.
        adjustments.global.tonemapper_mode = 3;
        adjustments.global.profile_curve[15][3] = 2.0;
        adjustments.global.image_key = recipe["imageKey"].as_f64().unwrap_or(0.0) as f32;
        let (pixels, ..) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        let RenderedPixels::U16(pixels) = pixels else {
            unreachable!()
        };
        let row = (height / 2 * width * 4) as usize;
        pixels[row..row + (width * 4) as usize]
            .chunks(4)
            .map(|p| {
                let linear = |v: u16| {
                    let e = v as f64 / 65535.0;
                    if e <= 0.04045 {
                        e / 12.92
                    } else {
                        ((e + 0.055) / 1.055).powf(2.4)
                    }
                };
                0.2126 * linear(p[0]) + 0.7152 * linear(p[1]) + 0.0722 * linear(p[2])
            })
            .collect()
    };
    let neutral = render(json!({}));
    let column_of =
        |scene: f32| ((scene.log2() + 14.0) / 13.0 * (width - 1) as f32).round() as usize;
    let lstar = |y: f64| {
        if y > 216.0 / 24389.0 {
            116.0 * y.cbrt() - 16.0
        } else {
            y * 24389.0 / 27.0
        }
    };
    // The neutral look's black point (`CAMERA_RAW_BLACK_NEUTRAL`).
    let black = 0.006f32;
    // (recipe, lowest L* at half the black point): raising sliders lift the
    // toe like Camera Raw's (it showed detail inside crushed black where we
    // kept it black), lowering ones leave crushed black alone.
    for (recipe, lifted_half_black) in [
        (json!({ "shadows": 100 }), Some(1.0)),
        (json!({ "shadows": 50 }), Some(0.3)),
        (json!({ "shadows": -100 }), None),
        (json!({ "shadows": -50 }), None),
        (json!({ "blacks": 100 }), Some(1.5)),
        (json!({ "blacks": 50 }), Some(1.0)),
        (json!({ "blacks": -100 }), None),
        // Darkens the tones just above the black ramp's toe the most
        // steeply (they reversed by 0.07 EV before the ordering guard).
        (json!({ "blacks": -50 }), None),
        (json!({ "contrast": -100 }), Some(0.8)),
        (json!({ "contrast": -50 }), Some(0.2)),
        (json!({ "contrast": 100 }), None),
        (json!({ "shadows": 100, "blacks": -100 }), None),
        (json!({ "shadows": 100, "blacks": -50 }), None),
        (json!({ "shadows": 50, "blacks": -50 }), None),
        (json!({ "shadows": 100, "blacks": -25 }), None),
        (json!({ "shadows": 25, "blacks": -50 }), None),
        (json!({ "shadows": -50, "blacks": 100 }), None),
        (
            json!({ "shadows": 100, "blacks": 100, "exposure": 1.5 }),
            None,
        ),
    ] {
        let toned = render(recipe.clone());
        // Away from the image borders (the blur clamps there).
        let columns = 64..(width - 64) as usize;
        // Tones stay in order: the output never falls by 0.01 EV between
        // neighbouring steps (below 1e-4 a single 16-bit code is more than
        // that) nor, from the black point to 4 EV above it, over up to 16
        // steps (0.1 EV of scene): a slow reversal there hid in one-code
        // steps between neighbours (Blacks −50 fell 0.07 EV over 13 steps).
        let toe_band = column_of(16.0 * black);
        let mut largest_drop = 0.0f64;
        let mut drop_at = (0, 0);
        for x in columns.clone().skip(16) {
            let reach = if x <= toe_band { 16 } else { 1 };
            for k in 1..=reach {
                if toned[x - k] > 1e-4 {
                    let drop = (toned[x - k] / toned[x].max(1e-12)).log2();
                    if drop > largest_drop {
                        largest_drop = drop;
                        drop_at = (x, k);
                    }
                }
            }
        }
        assert!(
            largest_drop < 0.01,
            "{recipe}: output falls by {largest_drop:.3} EV over {} ramp steps to column {}",
            drop_at.1,
            drop_at.0
        );
        // Combinations: smoothness only.
        if recipe.as_object().is_some_and(|o| o.len() > 1) {
            continue;
        }
        // True black stays black: the toe lifts by a gain, never an offset.
        let deepest = column_of(2f32.powf(-13.0));
        assert!(
            toned[deepest] < 2e-6,
            "{recipe}: 2^-13 lifted to {}",
            toned[deepest]
        );
        let half_black = column_of(0.5 * black);
        assert_eq!(
            neutral[half_black], 0.0,
            "half the black point renders black"
        );
        match lifted_half_black {
            Some(minimum) => {
                let l = lstar(toned[half_black]);
                assert!(
                    l >= minimum,
                    "{recipe}: half the black point lifted to L* {l:.2}, expected at least {minimum}"
                );
                // Shadows and Blacks only lift (Contrast also darkens above its pivot).
                if recipe.get("contrast").is_none() {
                    for x in columns {
                        assert!(
                            toned[x] >= neutral[x] * 0.999,
                            "{recipe}: column {x} darkened from {} to {}",
                            neutral[x],
                            toned[x]
                        );
                    }
                }
            }
            None => {
                // Crushed to black without the edit: lowering keeps it there.
                for x in columns {
                    if neutral[x] == 0.0 {
                        assert!(
                            toned[x] < 2e-6,
                            "{recipe}: crushed black at column {x} lifted to {}",
                            toned[x]
                        );
                    }
                }
            }
        }
    }
    // Shadows adapts to the image key (Camera Raw lifts brighter frames'
    // shadows more and darkens them more when lowering). The reference key
    // (`CR_KEY0`) is 0.006 * 2^5.94; keys two stops either side are the
    // limit. Smooth and ordered for every key, raising never darkens and
    // lowering never lifts, and the brighter key acts more strongly.
    let reference_key = 0.006 * 2f64.powf(5.94);
    let mid_shadow = column_of(2f32.powf(-6.0));
    for amount in [100, 50, -50, -100] {
        let mut at_mid = Vec::new();
        for key in [reference_key / 4.0, reference_key, reference_key * 4.0] {
            let recipe = json!({ "shadows": amount, "imageKey": key });
            let toned = render(recipe.clone());
            let columns = 64..(width - 64) as usize;
            let mut largest_drop = 0.0f64;
            for x in columns.clone().skip(1) {
                if toned[x - 1] > 1e-4 {
                    largest_drop = largest_drop.max((toned[x - 1] / toned[x].max(1e-12)).log2());
                }
            }
            assert!(
                largest_drop < 0.01,
                "{recipe}: output falls by {largest_drop:.3} EV between neighbouring ramp steps"
            );
            for x in columns {
                if amount > 0 {
                    assert!(toned[x] >= neutral[x] * 0.999, "{recipe}: darkened at {x}");
                } else {
                    assert!(toned[x] <= neutral[x] * 1.001, "{recipe}: lifted at {x}");
                }
            }
            at_mid.push(toned[mid_shadow]);
        }
        if amount > 0 {
            assert!(
                at_mid[0] < at_mid[1] && at_mid[1] < at_mid[2],
                "shadows {amount}: {at_mid:?}"
            );
        } else {
            assert!(
                at_mid[0] > at_mid[1] && at_mid[1] > at_mid[2],
                "shadows {amount}: {at_mid:?}"
            );
        }
    }
}

#[test]
#[ignore = "requires a native GPU; Camera Raw look tone sliders on a ramp from black to past white"]
fn camera_raw_tone_sliders_keep_tones_in_order() {
    let context = context();
    let (width, height) = (4096u32, 8u32);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    // Scene luminance from 2^-14 to 2^3, evenly in EV across the columns.
    let (low_ev, span_ev) = (-14.0f32, 17.0f32);
    let level = |x: u32| (2.0f32).powf(low_ev + span_ev * x as f32 / (width - 1) as f32);
    let data: Vec<f16> = (0..height)
        .flat_map(|_| 0..width)
        .flat_map(|x| {
            let v = f16::from_f32(level(x));
            [v, v, v, f16::ONE]
        })
        .collect();
    let view = context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Grey ramp"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&data),
        )
        .create_view(&Default::default());
    // 16-bit codes (R, G, B) of the middle row.
    // Camera Raw look (mode 3) with neutral colour as RAW files render (flag
    // 2), and through the own-curve path: with neutral colour (3) and as a
    // file's own curve without the highlight roll-off (1), both given the
    // default curve's points (`DEFAULT_TONE_CURVE`).
    let default_curve: Vec<f32> = include_str!("shaders/shader.wgsl")
        .split("const DEFAULT_TONE_CURVE = array<f32, 49>(")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .unwrap()
        .split(',')
        .map(|v| v.trim().parse().unwrap())
        .collect();
    assert_eq!(default_curve.len(), 49);
    let render = |recipe: &serde_json::Value, flag: f32| -> Vec<[u16; 3]> {
        let mut adjustments = get_all_adjustments_from_json(recipe, true, Some(2));
        adjustments.global.tonemapper_mode = 3;
        set_test_image_key(&mut adjustments, recipe);
        if flag < 1.5 || flag > 2.5 {
            for (i, v) in default_curve.iter().enumerate() {
                adjustments.global.profile_curve[i / 4][i % 4] = *v;
            }
        }
        adjustments.global.profile_curve[15][3] = flag;
        let (pixels, ..) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        let RenderedPixels::U16(pixels) = pixels else {
            unreachable!()
        };
        let row = (height / 2 * width * 4) as usize;
        pixels[row..row + (width * 4) as usize]
            .chunks(4)
            .map(|p| [p[0], p[1], p[2]])
            .collect()
    };
    let linear = |v: u16| {
        let e = v as f64 / 65535.0;
        if e <= 0.04045 {
            e / 12.92
        } else {
            ((e + 0.055) / 1.055).powf(2.4)
        }
    };
    // Every tone slider alone at ±50 and ±100, every pair at ±100, and
    // Exposure ±2 alone and with each slider at ±100.
    let sliders = ["contrast", "highlights", "shadows", "whites", "blacks"];
    let mut recipes = Vec::new();
    for slider in sliders {
        for amount in [-100, -50, 50, 100] {
            recipes.push(json!({ slider: amount }));
        }
    }
    for (i, a) in sliders.iter().enumerate() {
        for b in &sliders[i + 1..] {
            for (x, y) in [(-100, -100), (-100, 100), (100, -100), (100, 100)] {
                recipes.push(json!({ *a: x, *b: y }));
            }
        }
    }
    for exposure in [-2.0, 2.0] {
        recipes.push(json!({ "exposure": exposure }));
        for slider in sliders {
            for amount in [-100, 100] {
                recipes.push(json!({ "exposure": exposure, slider: amount }));
            }
        }
    }
    recipes.push(json!({ "shadows": 100, "blacks": 100, "exposure": 1.5 }));
    with_newer_tone_versions(&mut recipes);
    // Away from the image borders (the blur clamps there). Over up to 24
    // steps (0.1 EV of scene) no channel falls by more than 0.01 EV or two
    // 16-bit codes, whichever is more: Shadows and Blacks read the f16
    // blurred luminance, which wobbles a result by up to a code either way
    // where it rises slowly near black (a code is 0.01 EV at 1.4e-4).
    let columns = 64..(width - 64) as usize;
    let mut failures = Vec::new();
    for (flag, recipe) in [2.0f32, 3.0, 1.0]
        .into_iter()
        .flat_map(|flag| recipes.iter().map(move |recipe| (flag, recipe)))
    {
        let toned = render(recipe, flag);
        let mut worst: Option<(f64, usize, usize, usize)> = None;
        for x in columns.clone().skip(24) {
            for k in 1..=24 {
                for (c, (&before, &after)) in toned[x - k].iter().zip(&toned[x]).enumerate() {
                    if after >= before {
                        continue;
                    }
                    let (lb, la) = (linear(before), linear(after));
                    let excess =
                        ((lb / la.max(1e-12)).log2() / 0.01).min((before - after) as f64 / 2.0);
                    if excess > 1.0 && worst.is_none_or(|w| excess > w.0) {
                        worst = Some((excess, x, k, c));
                    }
                }
            }
        }
        if let Some((excess, x, k, c)) = worst {
            let scene = low_ev + span_ev * x as f32 / (width - 1) as f32;
            failures.push(format!(
                "flag {flag} {recipe}: channel {c} falls from {} to {} over {k} steps to scene {scene:.2} EV ({excess:.1}× the limit)",
                toned[x - k][c], toned[x][c]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "tones out of order:\n{}",
        failures.join("\n")
    );
}

/// A night frame's image key [10th, 90th, 50th percentile] as recipe fields
/// (`imageKeyLow`, `imageKey`, `imageKeyMedian`, read by
/// `set_test_image_key`): log2(p / 0.006) = −5, 4 and 1, about a night
/// street's, where tone version 3 adapts lowered Highlights fully.
const NIGHT_KEY: [f64; 3] = [0.006 / 32.0, 0.006 * 16.0, 0.006 * 2.0];

/// Sets the measured image key from a recipe's `imageKey`, `imageKeyLow`
/// and `imageKeyMedian` (test-only fields; 0 = unknown, as without them).
fn set_test_image_key(adjustments: &mut AllAdjustments, recipe: &serde_json::Value) {
    adjustments.global.image_key = recipe["imageKey"].as_f64().unwrap_or(0.0) as f32;
    adjustments.global.image_key_low = recipe["imageKeyLow"].as_f64().unwrap_or(0.0) as f32;
    adjustments.global.image_key_median = recipe["imageKeyMedian"].as_f64().unwrap_or(0.0) as f32;
}

/// `recipe` on the measured image `key` (`NIGHT_KEY`'s form).
fn with_image_key(recipe: &serde_json::Value, key: [f64; 3]) -> serde_json::Value {
    let mut recipe = recipe.clone();
    recipe["imageKeyLow"] = json!(key[0]);
    recipe["imageKey"] = json!(key[1]);
    recipe["imageKeyMedian"] = json!(key[2]);
    recipe
}

/// Adds, for every recipe with Shadows above +50 or Highlights lowered
/// (where the tone-slider versions differ), the same recipe with
/// `toneVersion` 2, and for lowered Highlights with `toneVersion` 3 on a
/// night frame's key.
fn with_newer_tone_versions(recipes: &mut Vec<serde_json::Value>) {
    let differs = |recipe: &&serde_json::Value| {
        recipe["shadows"].as_f64().unwrap_or(0.0) > 50.0
            || recipe["highlights"].as_f64().unwrap_or(0.0) < 0.0
    };
    let mut versioned: Vec<serde_json::Value> = Vec::new();
    for recipe in recipes.iter().filter(differs) {
        let mut second = recipe.clone();
        second["toneVersion"] = json!(2);
        versioned.push(second);
        if recipe["highlights"].as_f64().unwrap_or(0.0) < 0.0 {
            let mut third = with_image_key(recipe, NIGHT_KEY);
            third["toneVersion"] = json!(3);
            versioned.push(third);
        }
    }
    recipes.extend(versioned);
}

/// An opaque grey `Rgba16Float` texture of scene luminances, row by row.
fn grey_texture(
    context: &GpuContext,
    width: u32,
    height: u32,
    levels: &[f32],
) -> wgpu::TextureView {
    assert_eq!(levels.len(), (width * height) as usize);
    let data: Vec<f16> = levels
        .iter()
        .flat_map(|&level| {
            let v = f16::from_f32(level);
            [v, v, v, f16::ONE]
        })
        .collect();
    context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Grey test scene"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&data),
        )
        .create_view(&Default::default())
}

/// The base looks a RAW renders with, as (name, tonemapper mode, curve
/// flag): Classic (0), AgX (1), Standard (2) and the Camera Raw look (3)
/// with neutral colour (flag 2) and through the own-curve path given the
/// default curve's points (flag 1, no roll-off).
const RAW_LOOKS: [(&str, u32, f32); 5] = [
    ("classic", 0, 0.0),
    ("agx", 1, 0.0),
    ("standard", 2, 0.0),
    ("camera-raw", 3, 2.0),
    ("camera-raw-own-curve", 3, 1.0),
];

/// 16-bit RGB codes of a RAW render of `view` in one of `RAW_LOOKS`.
fn render_raw_look(
    processor: &GpuProcessor,
    view: &wgpu::TextureView,
    (width, height): (u32, u32),
    recipe: &serde_json::Value,
    (mode, flag): (u32, f32),
) -> Vec<[u16; 3]> {
    let mut adjustments = get_all_adjustments_from_json(recipe, true, Some(mode.min(2)));
    adjustments.global.tonemapper_mode = mode;
    set_test_image_key(&mut adjustments, recipe);
    if mode == 3 {
        if flag < 1.5 || flag > 2.5 {
            let default_curve: Vec<f32> = include_str!("shaders/shader.wgsl")
                .split("const DEFAULT_TONE_CURVE = array<f32, 49>(")
                .nth(1)
                .and_then(|rest| rest.split(')').next())
                .unwrap()
                .split(',')
                .map(|v| v.trim().parse().unwrap())
                .collect();
            for (i, v) in default_curve.iter().enumerate() {
                adjustments.global.profile_curve[i / 4][i % 4] = *v;
            }
        }
        adjustments.global.profile_curve[15][3] = flag;
    }
    let (pixels, ..) = processor
        .run(
            view,
            width,
            height,
            RenderRequest {
                adjustments,
                mask_bitmaps: &[],
                lut: None,
                roi: None,
            },
            false,
            RenderOutputPrecision::SixteenBit,
        )
        .unwrap();
    let RenderedPixels::U16(pixels) = pixels else {
        unreachable!()
    };
    pixels.chunks(4).map(|p| [p[0], p[1], p[2]]).collect()
}

fn srgb_code_to_linear(v: u16) -> f64 {
    let e = v as f64 / 65535.0;
    if e <= 0.04045 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    }
}

/// The worst fall of any channel along `row` (input rising left to right)
/// within `columns`, over up to `reach` steps, in units of the limit: 0.01 EV
/// or two 16-bit codes, whichever is more (as in
/// `camera_raw_tone_sliders_keep_tones_in_order`). Returns (excess, column,
/// steps, channel, EV fallen) for an excess above 1.
fn worst_tone_reversal(
    row: &[[u16; 3]],
    columns: std::ops::Range<usize>,
    reach: usize,
) -> Option<(f64, usize, usize, usize, f64)> {
    let mut worst: Option<(f64, usize, usize, usize, f64)> = None;
    for x in columns.clone().skip(reach) {
        for k in 1..=reach {
            for (c, (&before, &after)) in row[x - k].iter().zip(&row[x]).enumerate() {
                if after >= before {
                    continue;
                }
                let ev =
                    (srgb_code_to_linear(before) / srgb_code_to_linear(after).max(1e-12)).log2();
                let excess = (ev / 0.01).min((before - after) as f64 / 2.0);
                if excess > 1.0 && worst.is_none_or(|w| excess > w.0) {
                    worst = Some((excess, x, k, c, ev));
                }
            }
        }
    }
    worst
}

#[test]
#[ignore = "requires a native GPU; every tone slider step on a ramp from black to past white, every base look"]
fn tone_sliders_keep_tones_in_order_at_every_step() {
    let context = context();
    let (width, height) = (4096u32, 8u32);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    // Scene luminance from 2^-14 to 2^4, evenly in EV across the columns.
    let (low_ev, span_ev) = (-14.0f32, 18.0f32);
    let levels: Vec<f32> = (0..height)
        .flat_map(|_| 0..width)
        .map(|x| (2.0f32).powf(low_ev + span_ev * x as f32 / (width - 1) as f32))
        .collect();
    let view = grey_texture(&context, width, height, &levels);
    // Every tone slider from −100 to +100 in steps of 10, Exposure from −5
    // to +5 EV in half stops, and all five sliders at ±100 together in every
    // sign combination, alone and at Exposure ±2.
    let sliders = ["contrast", "highlights", "shadows", "whites", "blacks"];
    let mut recipes = Vec::new();
    for slider in sliders {
        for amount in (-100..=100).step_by(10).filter(|a| *a != 0) {
            recipes.push(json!({ slider: amount }));
        }
    }
    for half_stops in -10..=10 {
        if half_stops != 0 {
            recipes.push(json!({ "exposure": half_stops as f64 * 0.5 }));
        }
    }
    for signs in 0..32u32 {
        for exposure in [0.0, -2.0, 2.0] {
            let mut recipe = json!({ "exposure": exposure });
            for (i, slider) in sliders.iter().enumerate() {
                recipe[*slider] = json!(if signs & (1 << i) != 0 { 100 } else { -100 });
            }
            recipes.push(recipe);
        }
    }
    with_newer_tone_versions(&mut recipes);
    let columns = 64..(width - 64) as usize;
    let row = (height / 2 * width) as usize;
    let mut failures = Vec::new();
    for (look, mode, flag) in RAW_LOOKS {
        for recipe in &recipes {
            let pixels = render_raw_look(&processor, &view, (width, height), recipe, (mode, flag));
            let toned = &pixels[row..row + width as usize];
            if let Some((excess, x, k, c, ev)) = worst_tone_reversal(toned, columns.clone(), 24) {
                let scene = low_ev + span_ev * x as f32 / (width - 1) as f32;
                let line = format!(
                    "{look} {recipe}: channel {c} falls {ev:.3} EV ({} → {}) over {k} steps to scene {scene:.2} EV ({excess:.1}× the limit)",
                    toned[x - k][c],
                    toned[x][c]
                );
                failures.push(line);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} renders out of order:\n{}",
        failures.len(),
        recipes.len() * RAW_LOOKS.len(),
        failures.join("\n")
    );
}

#[test]
#[ignore = "requires a native GPU; Shadows of tone version 2 against version 1 on a grey ramp"]
fn tone_version_2_reaches_further_with_strong_shadows_only() {
    let context = context();
    let (width, height) = (2048u32, 8u32);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let (low_ev, span_ev) = (-14.0f32, 16.0f32);
    let levels: Vec<f32> = (0..height)
        .flat_map(|_| 0..width)
        .map(|x| (2.0f32).powf(low_ev + span_ev * x as f32 / (width - 1) as f32))
        .collect();
    let view = grey_texture(&context, width, height, &levels);
    let look = (3, 2.0);
    let render = |recipe: serde_json::Value| {
        render_raw_look(&processor, &view, (width, height), &recipe, look)
    };
    // Nothing else changes: no Shadows, Shadows +50, Shadows lowering.
    for recipe in [
        json!({}),
        json!({ "shadows": 50, "highlights": 60 }),
        json!({ "shadows": -100 }),
    ] {
        let mut versioned = recipe.clone();
        versioned["toneVersion"] = json!(2);
        assert!(
            render(recipe.clone()) == render(versioned),
            "{recipe} differs"
        );
    }
    // Shadows +100 lifts the quarter-tones further (display −5 … −3 EV of
    // the neutral render), the deep shadows and the upper tones about alike.
    let neutral = render(json!({}));
    let first = render(json!({ "shadows": 100 }));
    let second = render(json!({ "shadows": 100, "toneVersion": 2 }));
    let row = (height / 2 * width) as usize;
    let luma = |p: &[u16; 3]| {
        0.2126 * srgb_code_to_linear(p[0])
            + 0.7152 * srgb_code_to_linear(p[1])
            + 0.0722 * srgb_code_to_linear(p[2])
    };
    let mut extra_quarter = Vec::new();
    let mut extra_top = Vec::new();
    for x in 64..(width - 64) as usize {
        let display = luma(&neutral[row + x]).max(1e-9).log2();
        let extra = (luma(&second[row + x]) / luma(&first[row + x]).max(1e-12)).log2();
        if (-5.0..-3.0).contains(&display) {
            extra_quarter.push(extra);
        } else if display > -1.0 {
            extra_top.push(extra);
        }
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    assert!(
        mean(&extra_quarter) > 0.25,
        "quarter-tones {:.3} EV",
        mean(&extra_quarter)
    );
    assert!(
        mean(&extra_top).abs() < 0.03,
        "upper tones {:.3} EV",
        mean(&extra_top)
    );
}

#[test]
#[ignore = "requires a native GPU; lowered Highlights of tone version 3 against version 2 on a grey ramp"]
fn tone_version_3_lowers_highlights_further_in_dark_frames_only() {
    let context = context();
    let (width, height) = (2048u32, 8u32);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let (low_ev, span_ev) = (-14.0f32, 16.0f32);
    let levels: Vec<f32> = (0..height)
        .flat_map(|_| 0..width)
        .map(|x| (2.0f32).powf(low_ev + span_ev * x as f32 / (width - 1) as f32))
        .collect();
    let view = grey_texture(&context, width, height, &levels);
    let look = (3, 2.0);
    let render = |recipe: serde_json::Value| {
        render_raw_look(&processor, &view, (width, height), &recipe, look)
    };
    let with = |recipe: &serde_json::Value, version: u64, key: [f64; 3]| {
        let mut recipe = with_image_key(recipe, key);
        recipe["toneVersion"] = json!(version);
        recipe
    };
    // As version 2 without a key or its median (measured before it was
    // kept), on frames of daylight key, on a frame of mid tones with a dark
    // part (dark p10, median 3 EV over the black point), on frames that are
    // nearly black (a moon on a black sky: lowering it more would darken
    // the one thing shown), and without lowered Highlights.
    let daylight = [0.006 * 2.0, 0.006 * 128.0, 0.006 * 12.0];
    let mid_tones = [0.006, 0.006 * 32.0, 0.006 * 10.0];
    let nearly_black = [0.0, 0.006 * 0.5, 0.006 * 0.1];
    let no_median = [NIGHT_KEY[0], NIGHT_KEY[1], 0.0];
    for recipe in [
        json!({ "highlights": -100 }),
        json!({ "highlights": -50, "shadows": 80 }),
        json!({ "highlights": 60, "shadows": 100 }),
        json!({ "shadows": -100 }),
    ] {
        for key in [
            [0.0; 3],
            no_median,
            daylight,
            mid_tones,
            nearly_black,
            NIGHT_KEY,
        ] {
            if key == NIGHT_KEY && recipe["highlights"].as_f64().unwrap_or(0.0) < 0.0 {
                continue;
            }
            assert!(
                render(with(&recipe, 2, key)) == render(with(&recipe, 3, key)),
                "{recipe} with key {key:?} differs"
            );
        }
    }
    // On a night frame's key, −100 and −50 lower the upper tones further
    // (display above −1 EV of the neutral render), the deep shadows alike.
    let neutral = render(with(&json!({}), 3, NIGHT_KEY));
    let row = (height / 2 * width) as usize;
    let luma = |p: &[u16; 3]| {
        0.2126 * srgb_code_to_linear(p[0])
            + 0.7152 * srgb_code_to_linear(p[1])
            + 0.0722 * srgb_code_to_linear(p[2])
    };
    for (amount, top_least) in [(-100, 0.5), (-50, 0.15)] {
        let recipe = json!({ "highlights": amount });
        let second = render(with(&recipe, 2, NIGHT_KEY));
        let third = render(with(&recipe, 3, NIGHT_KEY));
        let mut extra_top = Vec::new();
        let mut extra_deep = Vec::new();
        for x in 64..(width - 64) as usize {
            let display = luma(&neutral[row + x]).max(1e-9).log2();
            let extra = (luma(&second[row + x]) / luma(&third[row + x]).max(1e-12)).log2();
            if (-1.0..-0.1).contains(&display) {
                extra_top.push(extra);
            } else if (-9.0..-7.0).contains(&display) {
                extra_deep.push(extra);
            }
        }
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        assert!(
            mean(&extra_top) > top_least,
            "{amount}: upper tones {:.3} EV darker",
            mean(&extra_top)
        );
        assert!(
            mean(&extra_deep).abs() < 0.01,
            "{amount}: deep shadows {:.3} EV",
            mean(&extra_deep)
        );
    }
}

#[test]
#[ignore = "requires a native GPU; Highlights on a clipped bright disc with a soft rim on a dark background"]
fn highlights_keep_a_clipped_plateau_above_its_rim() {
    let context = context();
    let (width, height) = (384u32, 384u32);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let centre = (width / 2) as f32;
    let plateau_radius = 64.0f32;
    let mut failures = Vec::new();
    let mut worst_ratio = 0.0f64;
    // Plateaus from white to three stops past it (clipped, reconstructed
    // highlights), rims from hard to soft, dark and mid backgrounds.
    for plateau in [1.0f32, 2.0, 4.0, 8.0] {
        for rim in [4.0f32, 16.0, 48.0] {
            for background in [0.01f32, 0.1] {
                let levels: Vec<f32> = (0..height)
                    .flat_map(|y| (0..width).map(move |x| (x, y)))
                    .map(|(x, y)| {
                        let r = ((x as f32 + 0.5 - centre).powi(2)
                            + (y as f32 + 0.5 - centre).powi(2))
                        .sqrt();
                        let t = ((plateau_radius + rim - r) / rim).clamp(0.0, 1.0);
                        let t = t * t * (3.0 - 2.0 * t);
                        // Mixed in EV, so the rim covers every level between.
                        background * (plateau / background).powf(t)
                    })
                    .collect();
                let view = grey_texture(&context, width, height, &levels);
                for (look, mode, flag) in RAW_LOOKS {
                    for (amount, version) in [
                        (-100, 1),
                        (-50, 1),
                        (50, 1),
                        (100, 1),
                        (-100, 2),
                        (-50, 2),
                        (-100, 3),
                        (-50, 3),
                    ] {
                        let recipe = with_image_key(
                            &json!({ "highlights": amount, "toneVersion": version }),
                            NIGHT_KEY,
                        );
                        let pixels = render_raw_look(
                            &processor,
                            &view,
                            (width, height),
                            &recipe,
                            (mode, flag),
                        );
                        // The centre row from the left edge to the centre:
                        // background, rim, plateau.
                        let row = (height / 2 * width) as usize;
                        let half = &pixels[row..row + (width / 2) as usize];
                        let luma = |p: &[u16; 3]| {
                            0.2126 * srgb_code_to_linear(p[0])
                                + 0.7152 * srgb_code_to_linear(p[1])
                                + 0.0722 * srgb_code_to_linear(p[2])
                        };
                        let centre_luma = luma(&half[half.len() - 1]);
                        let brightest = half.iter().map(luma).fold(0.0f64, f64::max);
                        let ratio = brightest / centre_luma.max(1e-12);
                        worst_ratio = worst_ratio.max(ratio);
                        // No solarization and no bright ring: nothing on the
                        // way in is brighter than the centre. Lowering left a
                        // ring at the plateau's edge (the detail lift against
                        // the darker surround) up to 2.8 % (0.04 EV) above
                        // the centre before its lift was limited to peaks;
                        // now at most 0.11 %.
                        if ratio > 1.005 {
                            failures.push(format!(
                                "{look} highlights {amount} v{version}, plateau {plateau}, rim {rim}px, background {background}: brightest / centre luminance {ratio:.4}"
                            ));
                        }
                        // Anything darker than the background-to-plateau
                        // gradient beyond the edge ring would be a reversal.
                        let edge = (centre - plateau_radius - 4.0) as usize;
                        if let Some((excess, x, k, c, ev)) = worst_tone_reversal(half, 8..edge, 4) {
                            failures.push(format!(
                                "{look} highlights {amount} v{version}, plateau {plateau}, rim {rim}px, background {background}: channel {c} falls {ev:.3} EV over {k} px in the rim at x {x} ({excess:.1}× the limit)"
                            ));
                        }
                    }
                }
            }
        }
    }
    eprintln!("largest brightest / centre luminance: {worst_ratio:.4}");
    assert!(
        failures.is_empty(),
        "{} renders reverse towards the plateau:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A `width` × `height` texture of one opaque colour for the display pipeline.
fn solid_display_texture(
    context: &GpuContext,
    width: u32,
    height: u32,
    rgba: [u8; 4],
) -> wgpu::TextureView {
    context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Split test frame"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            TextureDataOrder::LayerMajor,
            &rgba.repeat((width * height) as usize),
        )
        .create_view(&Default::default())
}

#[test]
#[ignore = "requires a native GPU; the display pipeline composes the before/after split"]
fn display_split_shows_the_before_left_of_the_divider() {
    let context = context();
    let device = &context.device;
    let queue = &context.queue;
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (pipeline, layout, split_layout) = display_pipeline(device, format).unwrap();
    let (window_w, window_h) = (96u32, 48u32);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Split test target"),
        size: wgpu::Extent3d {
            width: window_w,
            height: window_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&Default::default());
    // The after frame sits in a padded texture (as display targets may); the
    // before has its own, smaller resolution and still covers the same rect.
    let after_rgba = [200, 40, 30, 255];
    let before_rgba = [20, 60, 210, 255];
    let after = solid_display_texture(&context, 80, 40, after_rgba);
    let before = solid_display_texture(&context, 31, 13, before_rgba);
    let overlay = solid_display_texture(&context, 1, 1, [0, 0, 0, 0]);
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
    let split_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &split_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&before),
        }],
    });
    let bg = [0.1, 0.1, 0.1, 1.0];
    let render = |split: [f32; 4]| -> Vec<u8> {
        let transform = DisplayTransform {
            // The photograph fills x 16..80, y 8..40 of the window.
            rect: [16.0, 8.0, 64.0, 32.0],
            clip: [0.0, 0.0, window_w as f32, window_h as f32],
            window: [window_w as f32, window_h as f32],
            image_size: [64.0, 32.0],
            texture_size: [80.0, 40.0],
            pixelated: 0.0,
            rotation: 0.0,
            bg_primary: bg,
            bg_secondary: bg,
            overlay_color: [0.0; 4],
            overlay_mode: [0.0; 4],
            box_rect: [0.0; 4],
            split,
        };
        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&transform),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let frame_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&after),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&overlay),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &frame_group, &[]);
            pass.set_bind_group(1, &split_group, &[]);
            pass.draw(0..4, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        read_texture_data_roi(
            device,
            queue,
            &target,
            wgpu::Origin3d::ZERO,
            wgpu::Extent3d {
                width: window_w,
                height: window_h,
                depth_or_array_layers: 1,
            },
            4,
        )
        .unwrap()
    };
    let pixel = |pixels: &[u8], x: u32, y: u32| -> [u8; 4] {
        let i = ((y * window_w + x) * 4) as usize;
        pixels[i..i + 4].try_into().unwrap()
    };
    // UNORM conversion of 0.1 (25.5/255) rounds differently on Metal and
    // Vulkan. Compare the split against the same device's unsplit background.
    let background = pixel(&render([0.0; 4]), 5, 24);
    assert!(
        background[..3]
            .iter()
            .all(|&channel| (25..=26).contains(&channel))
    );
    assert_eq!(background[3], 255);

    // Divider at screen x 40: before on 16..40, after on 40..80.
    let split = render([1.0, 40.0, 0.0, 0.0]);
    for y in [9, 24, 38] {
        assert_eq!(pixel(&split, 17, y), before_rgba, "left edge, row {y}");
        assert_eq!(
            pixel(&split, 38, y),
            before_rgba,
            "left of the divider, row {y}"
        );
        assert_eq!(
            pixel(&split, 41, y),
            after_rgba,
            "right of the divider, row {y}"
        );
        assert_eq!(pixel(&split, 78, y), after_rgba, "right edge, row {y}");
    }
    // Outside the photograph the background stays.
    assert_eq!(pixel(&split, 5, 24), background);
    assert_eq!(pixel(&split, 40, 2), background);

    // Turned off, or a divider left of the photograph: the edited frame alone.
    for frame_only in [[0.0, 40.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]] {
        let frame = render(frame_only);
        assert_eq!(pixel(&frame, 17, 24), after_rgba);
        assert_eq!(pixel(&frame, 78, 24), after_rgba);
    }
    // A divider past the right edge shows only the before.
    let all_before = render([1.0, 500.0, 0.0, 0.0]);
    assert_eq!(pixel(&all_before, 17, 24), before_rgba);
    assert_eq!(pixel(&all_before, 78, 24), before_rgba);
}

#[test]
#[ignore = "requires native GPU; relight identity, patterns, depth and cached/general parity"]
fn relight_pixels_and_depth_are_consistent() {
    use base64::Engine as _;
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 192, 128).unwrap();
    let view = input(&context, 192, 128, 17);
    let render = |profile: &serde_json::Value| {
        let request = RenderRequest {
            adjustments: get_all_adjustments_from_json(profile, false, Some(0)),
            mask_bitmaps: &[],
            lut: None,
            roi: None,
        };
        let (pixels, ..) = processor
            .run(
                &view,
                192,
                128,
                request,
                false,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U8(p) => p,
            _ => unreachable!(),
        }
    };
    let baseline = render(&json!({}));
    let light =
        json!({"intensity":65,"pattern":"blinds","rotation":30,"elevation":25,"diffusion":10});
    assert_eq!(
        baseline,
        render(&json!({"relightLights":[{"intensity":0}]}))
    );
    assert_eq!(
        baseline,
        render(&json!({"relightLights":[light], "sectionVisibility":{"relight":false}}))
    );
    // New controls must preserve old recipes when explicitly set to their defaults.
    let legacy = json!({"relightLights":[light]});
    let mut explicit = legacy.clone();
    explicit["relightLights"][0]["patternRotation"] = json!(30);
    explicit["relightLights"][0]["sourceX"] = json!(50);
    explicit["relightLights"][0]["sourceY"] = json!(50);
    assert!(render(&legacy) == render(&explicit));
    explicit["relightLights"][0]["patternRotation"] = json!(80);
    assert!(render(&legacy) != render(&explicit));
    let mut spot = json!({"relightLights":[{"type":"spot","pattern":"none","intensity":70,"sourceX":20,"sourceY":30}]});
    let at_left = render(&spot);
    spot["relightLights"][0]["sourceX"] = json!(80);
    assert!(
        at_left != render(&spot),
        "moving the source must move spot illumination"
    );
    let mut outputs = Vec::new();
    for pattern in [
        "none",
        "blinds",
        "window",
        "strip",
        "foliage",
        "grid",
        "palm",
        "branches",
        "checkerboard",
        "dots",
        "horizontal",
        "diagonal",
        "weave",
        "noise",
        "crosshatch",
    ] {
        let mut l = light.clone();
        l["pattern"] = json!(pattern);
        let pixels = render(&json!({"relightLights":[l]}));
        assert_ne!(baseline, pixels, "{pattern} must change pixels");
        assert!(!outputs.contains(&pixels), "patterns must differ");
        outputs.push(pixels);
    }
    let depth = image::ImageBuffer::from_fn(96, 64, |x, y| {
        image::Luma([((x as f32 / 95.).powi(2) * 50000. + y as f32 * 100.) as u16])
    });
    let mut png = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageLuma16(depth)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let mut profile = json!({"relightLights":[light],"relightDepthAmount":80});
    let flat = render(&profile);
    profile["relightDepth"] = json!({"data":base64::engine::general_purpose::STANDARD.encode(png.into_inner()), "geometry":rawtonic_engine::relight::geometry(&profile)});
    let surface = render(&profile);
    assert_ne!(flat, surface, "actual depth must affect illumination");
    // Both output precisions, ROI and the display specialization must match
    // the general export shader, including the stored surface normals.
    for precision in [
        RenderOutputPrecision::EightBit,
        RenderOutputPrecision::SixteenBit,
    ] {
        for roi in [
            None,
            Some(Roi {
                x: 19,
                y: 13,
                width: 151,
                height: 97,
            }),
        ] {
            let render_variant = |general: bool, shader: ReadbackShader| {
                processor
                    .force_general_pipeline
                    .store(general, Ordering::Relaxed);
                let (pixels, ..) = processor
                    .run_with_shader(
                        &view,
                        192,
                        128,
                        RenderRequest {
                            adjustments: get_all_adjustments_from_json(&profile, false, Some(0)),
                            mask_bitmaps: &[],
                            lut: None,
                            roi,
                        },
                        false,
                        precision,
                        shader,
                        None,
                    )
                    .unwrap();
                match pixels {
                    RenderedPixels::U8(p) => p.into_iter().map(u16::from).collect::<Vec<_>>(),
                    RenderedPixels::U16(p) => p,
                }
            };
            let reference = render_variant(true, ReadbackShader::Export);
            let preview = render_variant(false, ReadbackShader::Preview);
            let max_diff = reference
                .iter()
                .zip(&preview)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            println!(
                "Relight parity {precision:?}, ROI={roi:?}: maximum code difference {max_diff}"
            );
            let tolerance = if precision == RenderOutputPrecision::EightBit {
                1
            } else {
                0
            };
            assert!(
                max_diff <= tolerance,
                "maximum difference {max_diff} exceeds {tolerance}"
            );
        }
    }
    processor
        .force_general_pipeline
        .store(false, Ordering::Relaxed);

    assert_eq!(
        surface,
        render(&profile),
        "cached depth must be deterministic"
    );
    for (key, value) in [
        ("transformRotate", json!(10)),
        ("rotation", json!(10)),
        ("crop", json!({"x":0.1,"y":0.2,"width":0.5,"height":0.6})),
    ] {
        let mut changed = profile.clone();
        changed[key] = value;
        assert_eq!(flat, render(&changed), "stale {key} must disable depth");
    }
    let mut inverse = light.clone();
    inverse["invert"] = json!(true);
    assert_ne!(flat, render(&json!({"relightLights":[inverse]})));
    let mut second = light.clone();
    second["color"] = json!("#3388ff");
    assert_eq!(
        render(&json!({"relightLights":[light, second]})),
        render(&json!({"relightLights":[second, light]}))
    );

    // Two disconnected flat surfaces must receive identical unpatterned sun
    // light at every pixel, even next to their silhouette (no invented bevel).
    let with_depth = |map: image::ImageBuffer<image::Luma<u16>, Vec<u16>>| {
        let mut png = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageLuma16(map)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let mut recipe = json!({"relightLights":[{"intensity":50,"pattern":"none","rotation":90,"elevation":25,"diffusion":0}],"relightDepthAmount":100});
        recipe["relightDepth"] = json!({"data":base64::engine::general_purpose::STANDARD.encode(png.into_inner()),"geometry":rawtonic_engine::relight::geometry(&recipe)});
        recipe
    };
    let flat_recipe = with_depth(image::ImageBuffer::from_pixel(96, 64, image::Luma([10000])));
    let step_recipe = with_depth(image::ImageBuffer::from_fn(96, 64, |x, _| {
        image::Luma([if x < 48 { 10000 } else { 50000 }])
    }));
    assert_eq!(
        render(&flat_recipe),
        render(&step_recipe),
        "depth step halo"
    );
    // Real monocular silhouettes have intermediate texels whose individual
    // steps pass the range cutoff. They must not invent dark bevels either.
    for diagonal in [false, true] {
        let transition = with_depth(image::ImageBuffer::from_fn(96, 64, |x, y| {
            let coordinate = x + if diagonal { y } else { 0 };
            let t = (coordinate as i32 - 48).clamp(0, 4);
            image::Luma([(10000 + 2000 * t) as u16])
        }));
        assert_eq!(
            render(&flat_recipe),
            render(&transition),
            "transitional depth halo, diagonal={diagonal}"
        );
    }

    // A known plane's normal is the derivative of the projected height.
    // Match its Lambert response to a flat plane under an equivalent sun.
    let ramp = with_depth(image::ImageBuffer::from_fn(96, 64, |x, _| {
        image::Luma([(10000 + x * 300) as u16])
    }));
    let slope = 300. * 96. / 65535. * (128. / 192.) * 0.25;
    let angle = 25f64.to_radians();
    let response = (slope * angle.cos() + angle.sin()) / (1. + slope * slope).sqrt();
    let mut equivalent = flat_recipe;
    equivalent["relightLights"][0]["elevation"] = json!(response.asin().to_degrees());
    let max_error = render(&ramp)
        .iter()
        .zip(render(&equivalent))
        .map(|(a, b)| a.abs_diff(b))
        .max()
        .unwrap();
    assert!(
        max_error <= 1,
        "inconsistent normal/height scale: {max_error}"
    );
}

#[test]
#[ignore = "requires native GPU; relight mask restriction and imported patterns"]
fn relight_masks_and_imported_patterns() {
    use base64::Engine as _;
    let (width, height) = (192u32, 128u32);
    let context = context();
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 23);
    let left_half = masked(GrayImage::from_fn(width, height, |x, _| {
        image::Luma([if x < width / 2 { 255 } else { 0 }])
    }));
    let render = |profile: &serde_json::Value| {
        let adjustments = get_all_adjustments_from_json(profile, false, Some(0));
        let masks = vec![left_half.clone(); adjustments.mask_count as usize];
        let (pixels, ..) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &masks,
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U8(p) => p,
            _ => unreachable!(),
        }
    };
    let layer = |visible: bool| {
        json!({"id":"subject","name":"Subject","visible":visible,"invert":false,"opacity":100,
            "adjustments":{},"subMasks":[{"id":"s","type":"all","visible":true,"mode":"additive","parameters":{}}]})
    };
    let light = json!({"intensity":70,"pattern":"none","maskId":"subject"});
    let baseline = render(&json!({"masks":[layer(true)]}));
    let restricted =
        render(&json!({"masks":[layer(true)],"relightLights":[light],"relightOriginalLight":100}));
    let row = (width * 4) as usize;
    let (mut inside, mut outside) = (0, 0);
    for (i, (a, b)) in baseline.iter().zip(&restricted).enumerate() {
        if (i % row) / 4 < (width / 2) as usize {
            inside += usize::from(a != b);
        } else {
            outside += usize::from(a != b);
        }
    }
    assert!(inside > 0, "the light must act inside its mask layer");
    assert_eq!(outside, 0, "the light must not act outside its mask layer");
    assert_eq!(
        baseline,
        render(&json!({"masks":[layer(false)],"relightLights":[light],"relightOriginalLight":100})),
        "a hidden mask layer switches its lights off"
    );

    // An imported half-black, half-white pattern shades like a gobo.
    let mut png = std::io::Cursor::new(Vec::new());
    GrayImage::from_fn(16, 16, |x, _| image::Luma([if x < 8 { 0 } else { 255 }]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let data = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png.into_inner())
    );
    let custom = json!({"intensity":70,"pattern":"custom","scale":100,"softness":0,"shadows":100,
        "customPattern":{"id":"half","data":data}});
    let plain = json!({"intensity":70,"pattern":"none","scale":100,"softness":0,"shadows":100});
    let mut inverted = custom.clone();
    inverted["invert"] = json!(true);
    let patterned = render(&json!({"relightLights":[custom]}));
    assert_ne!(patterned, render(&json!({"relightLights":[plain]})));
    assert_ne!(patterned, render(&json!({"relightLights":[inverted]})));
    assert_eq!(patterned, render(&json!({"relightLights":[custom]})));
}

#[test]
#[ignore = "requires native GPU: facial direction, distance, colour and export equivalence"]
fn relight_refined_surface_light_transport() {
    relight_surface_transport_case(2);
}

#[test]
#[ignore = "requires a GPU"]
fn relight_camera_surface_light_transport() {
    relight_surface_transport_case(4);
}

fn relight_surface_transport_case(version: u64) {
    use base64::Engine as _;
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 128, 128).unwrap();
    let image = DynamicImage::ImageRgba32F(image::ImageBuffer::from_pixel(
        128,
        128,
        image::Rgba([0.18, 0.18, 0.18, 1.]),
    ));
    let view = context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("neutral matte relight fixture"),
                size: wgpu::Extent3d {
                    width: 128,
                    height: 128,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&to_rgba_f16(&image)),
        )
        .create_view(&Default::default());
    let mut png = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageLuma16(image::ImageBuffer::from_fn(64, 64, |x, _| {
        image::Luma([if x < 32 { 12000 } else { 50000 }])
    }))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    let mut recipe = json!({"relightLights":[{"type":"sun","intensity":55,"color":"#ffffff","pattern":"none","rotation":90,"elevation":20,"diffusion":10,"softness":30,"shadows":100}],"relightOriginalLight":15,"relightDepthAmount":100});
    recipe["relightDepth"] = json!({"data":base64::engine::general_purpose::STANDARD.encode(png.into_inner()),"geometry":rawtonic_engine::relight::geometry(&recipe),"version":version});
    if version == 4 {
        recipe["relightDepth"]["camera"] = json!({"fx":1.2,"fy":1.2,"cx":0.5,"cy":0.5,"inverseMin":0.6,"inverseMax":1.4,"reference":1.0});
        recipe["relightSkinReflections"] = json!(0);
    }
    let render = |recipe: &serde_json::Value, general: bool| {
        processor
            .force_general_pipeline
            .store(general, Ordering::Relaxed);
        let (pixels, ..) = processor
            .run(
                &view,
                128,
                128,
                RenderRequest {
                    adjustments: get_all_adjustments_from_json(recipe, false, Some(0)),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U8(p) => p,
            _ => unreachable!(),
        }
    };
    // Opposing light must exchange lit and shadow cheeks on the 3D fixture.
    recipe["relightDepth"]["faces"] = json!([rawtonic_engine::relight::test_face_geometry()]);
    let left = render(&recipe, false);
    assert_eq!(
        left,
        render(&recipe, true),
        "refined preview/export divergence"
    );
    let average = |pixels: &[u8], x: usize| {
        (59..69)
            .flat_map(|y| (x - 3..x + 3).map(move |x| pixels[(y * 128 + x) * 4] as f32))
            .sum::<f32>()
            / 60.
    };
    assert!(
        average(&left, 43) > average(&left, 85) + 20.,
        "left-facing light must model cheeks"
    );
    recipe["relightLights"][0]["rotation"] = json!(-90);
    let right = render(&recipe, false);
    assert!(
        average(&right, 85) > average(&right, 43) + 20.,
        "right-facing light must reverse cheeks"
    );
    recipe["relightLights"][0]["shadows"] = json!(0);
    let no_shadows = render(&recipe, false);
    assert!(
        right
            .iter()
            .zip(&no_shadows)
            .any(|(a, b)| a.abs_diff(*b) > 5),
        "nose visibility must affect shading"
    );

    recipe["relightLights"][0]["rotation"] = json!(180);
    recipe["relightLights"][0]["shadows"] = json!(100);
    let top_shadow = render(&recipe, false);
    recipe["relightLights"][0]["shadows"] = json!(0);
    let top_open = render(&recipe, false);
    let philtrum = (72 * 128 + 64) * 4;
    assert!(
        top_open[philtrum] > top_shadow[philtrum] + 5,
        "overhead light must cast a nose shadow: {} vs {}",
        top_open[philtrum],
        top_shadow[philtrum]
    );

    // Equal screen distances, different scene depth: the foreground receives
    // more light. Moving the source away reduces both by geometric falloff.
    recipe["relightDepth"]["faces"] = json!([]);
    recipe["relightLights"][0] = json!({"type":"spot","intensity":40,"color":"#ffffff","pattern":"none","sourceX":50,"sourceY":50,"size":200,"distance":30,"diffusion":0});
    let near = render(&recipe, false);
    assert!(
        average(&near, 96) > average(&near, 32) + 5.,
        "foreground must be closer to the point source"
    );
    recipe["relightLights"][0]["distance"] = json!(120);
    let far = render(&recipe, false);
    assert!(
        average(&near, 96) > average(&far, 96) + 20.
            && average(&near, 32) > average(&far, 32) + if version == 4 { 5. } else { 20. },
        "point source must fall off with distance: near {},{} far {},{}",
        average(&near, 32),
        average(&near, 96),
        average(&far, 32),
        average(&far, 96)
    );

    if version == 4 {
        // Compare actual linear irradiance with an independently exposed
        // reference image, including perspective position and inverse-square
        // falloff. Keep gains below one to exclude the JPEG highlight rolloff.
        let camera = rawtonic_engine::relight::Camera::from_recipe(&recipe).unwrap();
        let mut physical = recipe.clone();
        physical["relightOriginalLight"] = json!(0);
        physical["relightLights"][0]["intensity"] = json!(10);
        for distance in [0.3_f32, 1.2] {
            physical["relightLights"][0]["distance"] = json!(distance * 100.);
            let actual = render(&physical, false);
            for (x, code) in [(32usize, 12000u16), (96, 50000)] {
                let p = camera.position([x as f32 / 128., 0.5], camera.inverse(code).recip(), 1.);
                let source_z = (camera.reference - camera.inverse_max.recip())
                    / camera.world_unit()
                    + distance;
                let direction = [-p[0], -p[1], source_z - p[2]];
                let distance2: f32 = direction.iter().map(|v| v * v).sum();
                let irradiance = 0.3 * 0.36 / (distance2 + 0.01) * direction[2] / distance2.sqrt();
                assert!(irradiance > 0. && irradiance < 1.);
                let exposure_unit =
                    get_all_adjustments_from_json(&json!({"exposure":1}), false, Some(0))
                        .global
                        .exposure;
                let reference = render(
                    &json!({"exposure": irradiance.log2() / exposure_unit}),
                    false,
                );
                let pixel = (64 * 128 + x) * 4;
                assert!(
                    actual[pixel].abs_diff(reference[pixel]) <= 1,
                    "camera irradiance distance={distance}, x={x}: {} != {}",
                    actual[pixel],
                    reference[pixel]
                );
            }
        }
    }

    // A red emitter cannot add green/blue illumination to a neutral surface.
    recipe["relightLights"][0]["color"] = json!("#ff0000");
    recipe["relightLights"][0]["distance"] = json!(30);
    recipe["relightOriginalLight"] = json!(0);
    let red = render(&recipe, false);
    assert!(red.as_chunks::<4>().0.iter().any(|p| p[0] > 50));
    assert!(
        red.as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[1] <= 1 && p[2] <= 1),
        "coloured light must preserve additive channel behaviour"
    );
    let light = recipe["relightLights"][0].clone();
    let mut blue = light.clone();
    blue["color"] = json!("#0044ff");
    recipe["relightLights"] = json!([light, blue]);
    let ordered = render(&recipe, false);
    recipe["relightLights"] = json!([blue, light]);
    assert_eq!(
        ordered,
        render(&recipe, false),
        "lights must add independently of ordering"
    );
}

#[test]
#[ignore = "requires a GPU"]
fn relight_face_reflections_follow_light() {
    use base64::Engine as _;
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 128, 128).unwrap();
    let image = DynamicImage::ImageRgba32F(image::ImageBuffer::from_pixel(
        128,
        128,
        image::Rgba([0., 0., 0., 1.]),
    ));
    let view = context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("black dielectric relight fixture"),
                size: wgpu::Extent3d {
                    width: 128,
                    height: 128,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&to_rgba_f16(&image)),
        )
        .create_view(&Default::default());
    let mut png = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageLuma16(image::ImageBuffer::from_fn(64, 64, |x, _| {
        image::Luma([if x < 32 { 12000 } else { 50000 }])
    }))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    let mut recipe = json!({"relightLights":[{"type":"sun","intensity":55,"color":"#ffffff","pattern":"none","rotation":90,"elevation":45,"diffusion":10,"softness":30,"shadows":0}],"relightOriginalLight":0,"relightDepthAmount":100});
    recipe["relightDepth"] = json!({"data":base64::engine::general_purpose::STANDARD.encode(png.into_inner()),"geometry":rawtonic_engine::relight::geometry(&recipe),"version":2});
    let render_source = |recipe: &serde_json::Value, general: bool, is_raw: bool| {
        processor
            .force_general_pipeline
            .store(general, Ordering::Relaxed);
        let (pixels, ..) = processor
            .run(
                &view,
                128,
                128,
                RenderRequest {
                    adjustments: get_all_adjustments_from_json(recipe, is_raw, Some(0)),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U8(p) => p,
            _ => unreachable!(),
        }
    };
    let render = |recipe: &serde_json::Value, general: bool| render_source(recipe, general, false);
    recipe["relightDepth"]["faces"] = json!([rawtonic_engine::relight::test_face_geometry()]);
    let old = render(&recipe, false);
    assert!(old.as_chunks::<4>().0.iter().all(|p| p[..3] == [0, 0, 0]));
    recipe["relightDepth"]["version"] = json!(3);
    recipe["relightSkinReflections"] = json!(0);
    assert_eq!(
        old,
        render(&recipe, false),
        "zero preserves legacy appearance"
    );
    recipe["relightSkinReflections"] = json!(100);
    let left = render(&recipe, false);
    assert_eq!(
        left,
        render(&recipe, true),
        "reflection preview/export parity"
    );
    // On a black dielectric, doubling exposure and doubling emitter radiance
    // must give the same reflection; there is no diffuse signal to hide a bug.
    for ev in [-2.0_f64, -1.0, 1.0, 2.0] {
        let mut exposed = recipe.clone();
        exposed["relightLights"][0]["intensity"] = json!(20);
        exposed["exposure"] = json!(ev * 0.8);
        let mut brighter_light = exposed.clone();
        brighter_light["exposure"] = json!(0);
        brighter_light["relightLights"][0]["intensity"] = json!(20.0 * ev.exp2());
        for is_raw in [false, true] {
            let actual = render_source(&exposed, false, is_raw);
            let expected = render_source(&brighter_light, false, is_raw);
            let worst = actual
                .iter()
                .zip(&expected)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            assert!(
                worst <= 1,
                "reflection exposure {ev} EV (raw={is_raw}) differs from emitter radiance by {worst}"
            );
            assert_eq!(
                actual,
                render_source(&exposed, true, is_raw),
                "exposed reflection preview/export parity"
            );
        }
    }
    let centroid = |pixels: &[u8]| {
        let mut weight = 0.;
        let mut xsum = 0.;
        for (i, p) in pixels.as_chunks::<4>().0.iter().enumerate() {
            weight += p[0] as f32;
            xsum += (i % 128) as f32 * p[0] as f32;
        }
        assert!(
            weight > 1000.,
            "a black dielectric still reflects a white emitter"
        );
        xsum / weight
    };
    recipe["relightLights"][0]["rotation"] = json!(-90);
    let right = render(&recipe, false);
    println!(
        "Reflection centroids: {} -> {}",
        centroid(&left),
        centroid(&right)
    );
    assert!(
        centroid(&right) > centroid(&left) + 10.,
        "highlight must move across the face"
    );
    for y in 0..128 {
        for x in 0..128 {
            if !(20..108).contains(&x) || !(10..118).contains(&y) {
                assert_eq!(
                    &right[(y * 128 + x) * 4..(y * 128 + x) * 4 + 3],
                    &[0, 0, 0],
                    "no invented background reflections"
                );
            }
        }
    }
    recipe["relightLights"][0]["color"] = json!("#ff0000");
    let red = render(&recipe, false);
    assert!(red.as_chunks::<4>().0.iter().any(|p| p[0] > 10));
    assert!(
        red.as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[1] == 0 && p[2] == 0),
        "reflections inherit emitter colour"
    );
    recipe["relightLights"][0]["pattern"] = json!("blinds");
    recipe["relightLights"][0]["shadows"] = json!(100);
    let patterned = render(&recipe, false);
    assert!(
        red.iter()
            .zip(&patterned)
            .any(|(a, b)| u16::from(*a) > u16::from(*b) + 5),
        "gobo must occlude the reflection too"
    );
    recipe["relightDepth"]["faces"] = json!([]);
    assert_eq!(
        old,
        render(&recipe, false),
        "no face means no specular guess"
    );
}

#[test]
#[ignore = "manual real-photo render and timing; RELIGHT_PHOTO and RELIGHT_OUT containing depth16.png"]
fn relight_real_photo_and_timing() {
    use base64::Engine as _;
    let photo = image::open(std::env::var("RELIGHT_PHOTO").expect("RELIGHT_PHOTO"))
        .unwrap()
        .resize(1280, 1280, image::imageops::FilterType::Triangle);
    let dir = std::path::PathBuf::from(std::env::var("RELIGHT_OUT").expect("RELIGHT_OUT"));
    let png = std::fs::read(dir.join("depth16.png")).unwrap();
    let context = context();
    let (w, h) = photo.dimensions();
    let processor = GpuProcessor::new(context.clone(), w, h).unwrap();
    let view = context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Relight photo test"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&to_rgba_f16(&photo)),
        )
        .create_view(&Default::default());
    let mut source = std::env::var("RELIGHT_RECIPE").ok().map(|path| {
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap()).unwrap()
    }).unwrap_or_else(|| json!({"relightLights":[{"intensity":40,"pattern":"none","rotation":-135,"elevation":25,"diffusion":15,"color":"#fff0df"}],"relightOriginalLight":55}));
    let saved_depth = source
        .as_object_mut()
        .unwrap()
        .remove("relightDepth")
        .unwrap_or_default();
    let mut depth = source.clone();
    depth["relightDepth"] = json!({"data":base64::engine::general_purpose::STANDARD.encode(png),"geometry":rawtonic_engine::relight::geometry(&source)});
    depth["relightDepth"]["faces"] = saved_depth["faces"].clone();
    depth["relightDepth"]["camera"] = saved_depth["camera"].clone();
    depth["relightDepth"]["version"] = saved_depth["version"].clone();
    let render = |profile: &serde_json::Value| {
        let (p, ..) = processor
            .run_with_shader(
                &view,
                w,
                h,
                RenderRequest {
                    adjustments: get_all_adjustments_from_json(profile, false, Some(0)),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::EightBit,
                ReadbackShader::Preview,
                None,
            )
            .unwrap();
        match p {
            RenderedPixels::U8(p) => p,
            _ => unreachable!(),
        }
    };
    for (name, recipe) in [
        ("before", json!({})),
        ("projected", source),
        ("surface", depth.clone()),
    ] {
        let pixels = render(&recipe);
        image::RgbaImage::from_raw(w, h, pixels)
            .unwrap()
            .save(dir.join(format!("{name}.png")))
            .unwrap();
    }
    for (name, mut profile) in [
        ("neutral", json!({})),
        ("one-light-depth", depth.clone()),
        ("four-lights-depth", depth.clone()),
        ("four-lights-face-stress", depth),
    ] {
        if name.starts_with("four-lights") {
            profile["relightLights"] = json!(vec![profile["relightLights"][0].clone(); 4]);
        }
        if name == "four-lights-face-stress" {
            // Deliberately large analytic face coverage, not inferred geometry
            // or a quality rendering of this photo. Bound the shadow cost.
            profile["relightDepth"]["faces"] =
                json!([rawtonic_engine::relight::test_face_geometry()]);
            profile["relightDepth"]["version"] =
                json!(if profile["relightDepth"]["camera"].is_object() {
                    4
                } else {
                    2
                });
        }
        let features = preview_features(&get_all_adjustments_from_json(&profile, false, Some(0)));
        prepare_preview_pipeline(&processor, features);
        for _ in 0..4 {
            render(&profile);
        }
        let mut timings = Vec::new();
        for i in 0..30 {
            if name != "neutral" {
                profile["relightLights"][0]["rotation"] = json!(-135. + i as f64);
            }
            let start = std::time::Instant::now();
            render(&profile);
            timings.push(start.elapsed().as_secs_f64() * 1000.);
        }
        timings.sort_by(f64::total_cmp);
        println!(
            "Relight {name}, {w}x{h}, warm GPU render + readback incl. recipe parsing: p50={:.2}ms p95={:.2}ms",
            timings[15], timings[28]
        );
    }
}

/// A version-five subject (crates/engine/src/relight_source.rs): a new light
/// lights the person's material (divided by the photo's own shading), a far
/// background behind the subject receives none of it, and a tile renders as
/// the full frame.
#[test]
#[ignore = "native GPU; version-five Relight subject"]
fn relight_people_subject_lights_material_and_spares_far_background() {
    use base64::Engine as _;
    let (w, h) = (64u32, 32u32);
    let context = context();
    let processor = GpuProcessor::new(context.clone(), w, h).unwrap();
    processor
        .force_general_pipeline
        .store(true, Ordering::Relaxed);
    let gray: Vec<f16> = (0..w * h)
        .flat_map(|_| {
            [
                f16::from_f32(0.18),
                f16::from_f32(0.18),
                f16::from_f32(0.18),
                f16::ONE,
            ]
        })
        .collect();
    let view = context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Relight subject test input"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&gray),
        )
        .create_view(&Default::default());
    let png = |image: DynamicImage| {
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        base64::engine::general_purpose::STANDARD.encode(out.into_inner())
    };
    let depth = png(DynamicImage::ImageLuma16(image::ImageBuffer::from_pixel(
        w,
        h,
        image::Luma([32768]),
    )));
    // x < 16: person facing the light (from the left), originally in shadow;
    // 16..32: person facing away; 32..48: background reached; 48..: far.
    let code = |v: f32| ((v * 0.5 + 0.5) * 65535.).round() as u16;
    // The atlas (relight_source.rs encode_subject): left normal, matte and
    // weight; right shine share and background reach.
    let subject = png(DynamicImage::ImageRgba16(image::ImageBuffer::from_fn(
        w * 2,
        h,
        |x, _| match x {
            0..16 => image::Rgba([code(-0.8), code(0.), 65535, 65535]),
            16..32 => image::Rgba([code(0.8), code(0.), 65535, 65535]),
            32..64 => image::Rgba([code(0.), code(0.), 0, 0]),
            64..112 => image::Rgba([0, 65535, 0, 65535]),
            _ => image::Rgba([0, 0, 0, 65535]),
        },
    )));
    let mut recipe = json!({
        "relightLights":[{"intensity":20,"rotation":90,"elevation":20,"diffusion":0,"shadows":0}],
        "relightOriginalLight":0,
        "relightDepthAmount":100,
    });
    recipe["relightDepth"] = json!({
        "data": depth,
        "geometry": rawtonic_engine::relight::geometry(&recipe),
        "version": 5,
        "faces": [],
        "camera": {"fx":1.0,"fy":2.0,"cx":0.5,"cy":0.5,"inverseMin":0.6,"inverseMax":1.4,"reference":1.0},
        "subject": format!("data:image/png;base64,{subject}"),
        // The photo's light came from +x: S = 0.1 + 0.9·max(nx, 0).
        "sourceLight": [0.1, 0.9, 1., 0., 0., 0., 0., 0., 0.],
        "subjectDepth": 1.0,
    });
    let render = |recipe: &serde_json::Value, roi| {
        let (pixels, ..) = processor
            .run(
                &view,
                w,
                h,
                RenderRequest {
                    adjustments: get_all_adjustments_from_json(recipe, true, Some(1)),
                    mask_bitmaps: &[],
                    lut: None,
                    roi,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U16(p) => p,
            RenderedPixels::U8(p) => p.into_iter().map(u16::from).collect(),
        }
    };
    let full = render(&recipe, None);
    let at = |pixels: &[u16], x: usize| pixels[(16 * w as usize + x) * 4 + 1];
    // Far background: no new light, and no original light at 0 %.
    assert_eq!(at(&full, 56), 0);
    // A surface right behind the subject is lit.
    assert!(at(&full, 40) > 1000, "near background {}", at(&full, 40));
    // The side facing away from the new light stays dark.
    assert!(at(&full, 24) < at(&full, 8) / 4);
    // Lighting the material: the originally shaded side, lit now, is
    // brighter than when the photo itself counts as the material.
    let mut photo_as_material = recipe.clone();
    photo_as_material["relightDepth"]
        .as_object_mut()
        .unwrap()
        .remove("sourceLight");
    let plain = render(&photo_as_material, None);
    assert!(
        at(&full, 8) > at(&plain, 8) * 2,
        "material {} vs photo {}",
        at(&full, 8),
        at(&plain, 8)
    );
    // A tile renders as the full frame.
    let crop = render(
        &recipe,
        Some(Roi {
            x: 4,
            y: 4,
            width: 52,
            height: 20,
        }),
    );
    let expected: Vec<u16> = (4..24)
        .flat_map(|y| {
            full[(y * w as usize + 4) * 4..(y * w as usize + 56) * 4]
                .iter()
                .copied()
        })
        .collect();
    assert_eq!(crop, expected);
}

/// The subject's shadow on its background (version five): with a light from
/// the left, the background right of the subject is darker where the
/// subject card shades it than farther right, and no shadow without
/// `shadows`.
#[test]
#[ignore = "native GPU; version-five subject shadow"]
fn relight_subject_casts_its_shadow_on_the_background() {
    use base64::Engine as _;
    let (w, h) = (128u32, 32u32);
    let context = context();
    let processor = GpuProcessor::new(context.clone(), w, h).unwrap();
    processor
        .force_general_pipeline
        .store(true, Ordering::Relaxed);
    let gray: Vec<f16> = (0..w * h)
        .flat_map(|_| {
            [
                f16::from_f32(0.18),
                f16::from_f32(0.18),
                f16::from_f32(0.18),
                f16::ONE,
            ]
        })
        .collect();
    let view = context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Relight shadow test input"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&gray),
        )
        .create_view(&Default::default());
    let png = |image: DynamicImage| {
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        base64::engine::general_purpose::STANDARD.encode(out.into_inner())
    };
    // Subject (x < 40) near, a wall behind it at 0.7× its inverse depth.
    let depth = png(DynamicImage::ImageLuma16(image::ImageBuffer::from_fn(
        w,
        h,
        |x, _| image::Luma([if x < 40 { 65535 } else { 26214 }]),
    )));
    let subject = png(DynamicImage::ImageRgba16(image::ImageBuffer::from_fn(
        w * 2,
        h,
        |x, _| match x {
            0..40 => image::Rgba([32768, 32768, 65535, 0]),
            40..128 => image::Rgba([32768, 32768, 0, 0]),
            128..168 => image::Rgba([0, 65535, 0, 65535]),
            // Reach of the wall: (0.7 / 1.0)².
            _ => image::Rgba([0, 32113, 0, 65535]),
        },
    )));
    let mut recipe = json!({
        "relightLights":[{"intensity":40,"rotation":90,"elevation":60,"diffusion":0,"softness":0,"shadows":100}],
        "relightOriginalLight":50,
        "relightDepthAmount":100,
    });
    recipe["relightDepth"] = json!({
        "data": depth,
        "geometry": rawtonic_engine::relight::geometry(&recipe),
        "version": 5,
        "faces": [],
        // inverse 0.5..1.0: subject 1.0, wall 0.7.
        "camera": {"fx":1.0,"fy":4.0,"cx":0.5,"cy":0.5,"inverseMin":0.5,"inverseMax":1.0,"reference":1.0},
        "subject": format!("data:image/png;base64,{subject}"),
        "subjectDepth": 1.0,
    });
    let render = |recipe: &serde_json::Value| {
        let (pixels, ..) = processor
            .run(
                &view,
                w,
                h,
                RenderRequest {
                    adjustments: get_all_adjustments_from_json(recipe, true, Some(1)),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U16(p) => p,
            RenderedPixels::U8(p) => p.into_iter().map(u16::from).collect(),
        }
    };
    let row = |pixels: &[u16]| -> Vec<u16> {
        // Clear of the matte edge, where the reach map is interpolated.
        (44..w as usize)
            .map(|x| pixels[(16 * w as usize + x) * 4 + 1])
            .collect()
    };
    let lit = row(&render(&recipe));
    let darkest = *lit.iter().min().unwrap() as u32;
    let brightest = *lit.iter().max().unwrap() as u32;
    assert!(
        darkest * 10 < brightest * 8,
        "shadow {darkest} vs lit {brightest}"
    );
    // The shadow lies right of the subject (light from the left), not at the
    // far right edge.
    let at = lit.iter().position(|v| *v as u32 == darkest).unwrap();
    assert!(at < 60, "shadow at {}", at + 44);
    recipe["relightLights"][0]["shadows"] = json!(0);
    let unshaded = row(&render(&recipe));
    assert!(
        *unshaded.iter().min().unwrap() as u32 * 10 >= *unshaded.iter().max().unwrap() as u32 * 9
    );
}

#[test]
#[ignore = "native GPU; relight must keep its coordinates across the 2048px tile boundary"]
fn relight_tiled_roi_matches_full_frame() {
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2176, 64).unwrap();
    processor
        .force_general_pipeline
        .store(true, Ordering::Relaxed);
    let view = input(&context, 2176, 64, 11);
    let profile = json!({"relightLights":[{"pattern":"foliage","intensity":65,"scale":80,"rotation":17},{"type":"spot","pattern":"window","x":95,"y":40,"intensity":30}],"relightOriginalLight":70});
    use base64::Engine as _;
    let mut refined = profile.clone();
    let mut png = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageLuma16(image::ImageBuffer::from_pixel(
        1024,
        32,
        image::Luma([32768]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    let face: Vec<_> = rawtonic_engine::relight::test_face_geometry()
        .into_iter()
        .map(|mut p| {
            p[0] = 2100. / 2176. + (p[0] - 0.5) * 64. / 2176.;
            p
        })
        .collect();
    refined["relightDepth"] = json!({"data":base64::engine::general_purpose::STANDARD.encode(png.into_inner()),"geometry":rawtonic_engine::relight::geometry(&refined),"version":2,"faces":[face]});
    refined["relightDepthAmount"] = json!(100);
    let mut reflective = refined.clone();
    reflective["relightDepth"]["version"] = json!(3);
    reflective["relightSkinReflections"] = json!(100);
    let mut camera = reflective.clone();
    camera["relightDepth"]["version"] = json!(4);
    camera["relightDepth"]["camera"] = json!({"fx":0.06,"fy":2.04,"cx":0.5,"cy":0.5,"inverseMin":0.6,"inverseMax":1.4,"reference":1.0});
    let mut camera_edge = camera.clone();
    let mut edge_png = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageLuma16(image::ImageBuffer::from_fn(1024, 32, |x, _| {
        image::Luma([if x < 964 { 18000 } else { 52000 }])
    }))
    .write_to(&mut edge_png, image::ImageFormat::Png)
    .unwrap();
    camera_edge["relightDepth"]["data"] =
        json!(base64::engine::general_purpose::STANDARD.encode(edge_png.into_inner()));
    for profile in [profile, refined, reflective, camera, camera_edge] {
        for precision in [
            RenderOutputPrecision::EightBit,
            RenderOutputPrecision::SixteenBit,
        ] {
            let render = |roi| {
                let (pixels, ..) = processor
                    .run(
                        &view,
                        2176,
                        64,
                        RenderRequest {
                            adjustments: get_all_adjustments_from_json(&profile, true, Some(1)),
                            mask_bitmaps: &[],
                            lut: None,
                            roi,
                        },
                        false,
                        precision,
                    )
                    .unwrap();
                match pixels {
                    RenderedPixels::U8(p) => p.into_iter().map(u16::from).collect::<Vec<_>>(),
                    RenderedPixels::U16(p) => p,
                }
            };
            let full = render(None);
            let crop = render(Some(Roi {
                x: 2030,
                y: 8,
                width: 120,
                height: 40,
            }));
            let expected: Vec<u16> = (8..48)
                .flat_map(|y| {
                    full[(y * 2176 + 2030) * 4..(y * 2176 + 2150) * 4]
                        .iter()
                        .copied()
                })
                .collect();
            let diff = crop
                .iter()
                .zip(&expected)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            println!("Relight tile/ROI {precision:?}: maximum code difference {diff}");
            assert!(
                diff <= if precision == RenderOutputPrecision::EightBit {
                    1
                } else {
                    0
                },
                "Relight tile/ROI coordinate mismatch {precision:?}: maximum {diff}"
            );
        }
    }
}

#[path = "relight_reference_tests.rs"]
mod relight_reference_tests;

#[test]
#[ignore = "requires native GPU; queued Relight geometry outlives the reuse cache"]
fn relight_queued_depth_survives_cache_eviction() {
    use base64::Engine as _;
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 64, 48).unwrap();
    let view = input(&context, 64, 48, 17);
    let render = |adjustments| {
        let (pixels, ..) = processor
            .run(
                &view,
                64,
                48,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U16(p) => p,
            _ => unreachable!(),
        }
    };
    let mut queued = Vec::new();
    for i in 0..24u16 {
        let depth = image::ImageBuffer::from_fn(16, 12, |x, y| {
            image::Luma([16000 + i * 997 + (x * 431 + y * 127) as u16])
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma16(depth)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let mut recipe = json!({"relightLights":[{"intensity":65,"pattern":"blinds","elevation":25}],"relightDepthAmount":100,"relightDepth":{"version":4,"data":base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),"camera":{"fx":1.2,"fy":1.3,"cx":0.5,"cy":0.5,"inverseMin":0.6,"inverseMax":1.4,"reference":1.0}}});
        recipe["relightDepth"]["geometry"] = rawtonic_engine::relight::geometry(&recipe);
        let prepared = get_all_adjustments_from_json(&recipe, false, Some(0));
        let expected = render(prepared.clone());
        queued.push((prepared, expected));
    }
    assert!(
        queued.windows(2).all(|pair| pair[0].1 != pair[1].1),
        "fixture must distinguish depth maps"
    );
    // Every pending render keeps its own surface after 24 distinct registrations.
    // Reusing the processor cannot hide eviction: its last cached surface is #24.
    for (prepared, expected) in queued {
        assert_eq!(render(prepared), expected);
    }
}

/// A CPU source with gradients, edges, fine texture and highlights, so every
/// neighbourhood operation changes pixels near chunk borders.
fn chunk_test_source(width: u32, height: u32) -> DynamicImage {
    let mut seed = 0x9e37_79b9_u32;
    DynamicImage::ImageRgb32F(ImageBuffer::from_fn(width, height, |x, y| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let noise = (seed % 1000) as f32 / 1000.0 - 0.5;
        let gradient = x as f32 / width as f32 * 0.6 + y as f32 / height as f32 * 0.3;
        let edge = if (x / 97 + y / 61) % 2 == 0 {
            0.25
        } else {
            0.0
        };
        let stripes = if (x / 3) % 2 == 0 { 0.04 } else { -0.04 };
        let highlight = if ((x as i32 - 1500).pow(2) + (y as i32 - 900).pow(2)) < 160_000 {
            1.4
        } else {
            0.0
        };
        let base = gradient + edge + stripes + highlight + noise * 0.08;
        image::Rgb([
            (base * 1.1).max(0.0),
            base.max(0.0),
            (base * 0.8 + noise * 0.05).max(0.0),
        ])
    }))
}

fn chunk_test_profiles() -> Vec<(&'static str, serde_json::Value)> {
    let neighbourhood = json!({
        "exposure":0.3,"contrast":25,"highlights":-80,"shadows":40,"whites":10,"blacks":-10,
        "clarity":60,"structure":50,"dehaze":30,"centré":40,
        "sharpness":70,"sharpenRadius":3,"sharpnessFalloff":80,
        "lumaNoiseReduction":40,"colorNoiseReduction":50,
        "moireAmount":100,"moirePattern":100,
        "chromaticAberrationRedCyan":100,"chromaticAberrationBlueYellow":-100,
        "defringePurpleAmount":20,"defringeGreenAmount":20,
        "vignetteAmount":-50,"grainAmount":40,"glowAmount":50,"halationAmount":50
    });
    let mut masked = neighbourhood.clone();
    masked["masks"] = json!([local_mask(
        "local",
        json!({"exposure":0.5,"clarity":-40,"structure":30,"sharpness":40,
            "highlights":-30,"lumaNoiseReduction":20,"moireAmount":60,"moirePattern":80})
    )]);
    // A noisy photo: the luminance noise reduction's edge-preserving pass.
    let mut noisy_masked = masked.clone();
    noisy_masked["testNoiseLevel"] = json!(0.02);
    vec![
        ("plain", json!({})),
        ("neighbourhood", neighbourhood),
        ("masked", masked),
        ("noisy_masked", noisy_masked),
        (
            "noisy_defaults",
            json!({"sharpness":40,"lumaNoiseReduction":15,"colorNoiseReduction":25,
                "testNoiseLevel":0.012}),
        ),
        (
            "relight",
            json!({"relightLights":[{"intensity":65,"pattern":"blinds","elevation":25}]}),
        ),
        // The multi-scale colour noise reduction: the pyramid of the whole
        // photo, its level-1 window and the finest level's neighbourhood.
        (
            "multiscale_color_noise",
            json!({"colorNoiseReduction":90,"colorNoiseVersion":2,"colorNoiseDetail":30,
                "colorNoiseSmoothness":80,"lumaNoiseReduction":20,"sharpness":40}),
        ),
        // The second Defringe detector, photo and layer: sides up to 16 px
        // away and a patch test around them.
        (
            "defringe_v2",
            json!({"defringePurpleAmount":20,"defringeGreenAmount":20,"defringeVersion":2,
                "masks":[local_mask("local", json!({"defringe":80}))]}),
        ),
        // Light flare: its map is built from samples of the whole photo.
        (
            "flare",
            json!({"flareAmount":60,"exposure":1.5,"contrast":20}),
        ),
    ]
}

/// An export render on a background-only processor (the app's exports)
/// pauses between its tiles while Develop frames render on the shared one
/// (notes: parity-gaps/G44): its pixels stay byte for byte those of a render
/// without pauses, and of one on a shared processor.
#[test]
#[ignore = "requires a native GPU"]
fn export_renders_paused_between_tiles_keep_their_pixels() {
    let context = context();
    // 3 x 2 tiles.
    let (width, height) = (4300, 2300);
    let photo = |seed: u32| {
        DynamicImage::ImageRgba32F(ImageBuffer::from_fn(width, height, move |x, y| {
            let n = ((x * 7919 + y * 104_729 + seed) % 251) as f32 / 251.0;
            Rgba([
                0.2 + 0.5 * x as f32 / width as f32 + 0.05 * n,
                0.3 + 0.2 * n,
                0.6 * y as f32 / height as f32 + 0.1 * (1.0 - n),
                1.0,
            ])
        }))
    };
    let (export_photo, frame_photo) = (photo(0), photo(17));
    let export_recipe = json!({"exposure":0.4,"contrast":15,"clarity":30,"sharpness":40,
        "lumaNoiseReduction":20,"colorNoiseReduction":25,"colorNoiseVersion":2,
        "flareAmount":30,"vignetteAmount":-20});
    let frame_recipe = json!({"exposure":-0.6,"structure":40,"dehaze":20,"colorNoiseReduction":40});
    let render = |slot: &GpuProcessorSlot, image: &DynamicImage, recipe, caller_id, hash| {
        process_and_get_dynamic_image_inner(
            &context,
            slot,
            &Mutex::new(None),
            RenderSource::Image {
                image,
                transform_hash: hash,
            },
            RenderRequest {
                adjustments: get_all_adjustments_from_json(recipe, true, Some(2)),
                mask_bitmaps: &[],
                lut: None,
                roi: None,
            },
            caller_id,
            RenderOutputPrecision::EightBit,
            false,
            None,
            None,
            ReadbackShader::Export,
        )
        .expect("renders")
    };
    let shared = ProcessorSlot::new(None);
    let exports = ProcessorSlot::background(None);
    let expected = render(&shared, &export_photo, &export_recipe, "export_test", 1);
    let alone = render(&exports, &export_photo, &export_recipe, "export_test", 1);
    assert!(alone.as_bytes() == expected.as_bytes());
    let pauses = |exports: &GpuProcessorSlot| {
        exports
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .processor
            .tile_pauses
            .load(Ordering::Relaxed)
    };
    assert_eq!(pauses(&exports), 0, "no frame to pause for");

    for round in 0..3 {
        let done = std::sync::atomic::AtomicBool::new(false);
        let paused = std::thread::scope(|scope| {
            let before = pauses(&exports);
            let export = scope.spawn(|| {
                let image = render(&exports, &export_photo, &export_recipe, "export_test", 1);
                done.store(true, Ordering::SeqCst);
                image
            });
            // Frames of a drag from when the export render has its processor.
            while exports.try_lock().is_ok() && !done.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            let drag = Instant::now();
            while drag.elapsed() < std::time::Duration::from_millis(600) {
                render(
                    &shared,
                    &frame_photo,
                    &frame_recipe,
                    "apply_adjustments",
                    2 + round,
                );
            }
            let image = export.join().unwrap();
            assert!(
                image.as_bytes() == expected.as_bytes(),
                "round {round}: the paused render's pixels differ"
            );
            pauses(&exports) - before
        });
        assert!(
            paused > 0,
            "round {round}: the export render paused for the frames"
        );
    }
}

/// A photo exported with several recipes renders each from one kept upload
/// ([`KeptUpload`]): byte for byte what a fresh upload renders, in every
/// colour space and precision, also once an earlier render from it built
/// its colour noise textures.
#[test]
#[ignore = "requires a native GPU"]
fn renders_from_a_kept_upload_match_fresh_uploads() {
    use rawtonic_engine::output_color_space::OutputColorSpace;
    let context = context();
    let source = DynamicImage::ImageRgba32F(ImageBuffer::from_fn(700, 460, |x, y| {
        let n = ((x * 7919 + y * 104_729) % 251) as f32 / 251.0;
        Rgba([
            0.2 + 0.5 * x as f32 / 700.0 + 0.05 * n,
            0.3 + 0.2 * n,
            0.6 * y as f32 / 460.0 + 0.1 * (1.0 - n),
            1.0,
        ])
    }));
    let recipe = json!({"exposure":0.4,"contrast":15,"sharpness":40,
        "lumaNoiseReduction":20,"colorNoiseReduction":25,"colorNoiseVersion":2});
    let adjustments = |space: OutputColorSpace| {
        let mut all = get_all_adjustments_from_json(&recipe, true, Some(2));
        all.global.output_space = [space.shader_code(), 0, 0, 0];
        all.global.output_matrix =
            crate::image_processing::gpu_mat3_from_rows(space.from_linear_srgb_f32());
        all
    };
    let global = adjustments(OutputColorSpace::Srgb).global;
    assert!(
        global.color_noise[0] > 0.0,
        "the recipe uses the multi-scale colour noise"
    );
    let color_noise = Some((true, color_noise_tuning(&global).1));
    let processor = ProcessorSlot::new(None);
    let cache = Mutex::new(None);
    let render = |source: RenderSource, space, precision| {
        process_and_get_dynamic_image_inner(
            &context,
            &processor,
            &cache,
            source,
            RenderRequest {
                adjustments: adjustments(space),
                mask_bitmaps: &[],
                lut: None,
                roi: None,
            },
            "kept_upload_test",
            precision,
            false,
            None,
            None,
            ReadbackShader::Export,
        )
        .expect("renders")
    };
    let upload = || {
        upload_render_source(&context, &source, 11, color_noise)
            .unwrap()
            .unwrap()
    };
    let recipes = [
        (OutputColorSpace::Srgb, RenderOutputPrecision::EightBit),
        (
            OutputColorSpace::AdobeRgb,
            RenderOutputPrecision::SixteenBit,
        ),
        (OutputColorSpace::DisplayP3, RenderOutputPrecision::EightBit),
        (OutputColorSpace::Srgb, RenderOutputPrecision::EightBit),
    ];
    let kept = upload().keep();
    let mut expected = Vec::new();
    for (space, precision) in recipes {
        let fresh = render(RenderSource::Uploaded(upload()), space, precision);
        let from_kept = render(RenderSource::Kept(&kept), space, precision);
        assert!(
            fresh.as_bytes() == from_kept.as_bytes(),
            "{space:?} {precision:?} differs from a fresh upload"
        );
        expected.push(fresh);
    }
    // All of them from one render, from a fresh kept upload (whose colour
    // noise textures that render builds).
    let outputs: Vec<RenderOutput> = recipes
        .iter()
        .map(|&(space, precision)| {
            let global = adjustments(space).global;
            RenderOutput {
                precision,
                output_space: global.output_space,
                output_matrix: global.output_matrix,
            }
        })
        .collect();
    let together = process_and_get_dynamic_images(
        &context,
        &processor,
        &upload().keep(),
        RenderRequest {
            adjustments: adjustments(OutputColorSpace::Srgb),
            mask_bitmaps: &[],
            lut: None,
            roi: None,
        },
        "kept_upload_test",
        &outputs,
    )
    .expect("renders");
    assert_eq!(together.len(), recipes.len());
    for ((space, precision), (together, expected)) in
        recipes.iter().zip(together.iter().zip(&expected))
    {
        assert!(
            together.as_bytes() == expected.as_bytes(),
            "{space:?} {precision:?} of one render differs from its own render"
        );
    }
}

/// One render for several outputs (an export's recipes, `DEFER_OUTPUT` and
/// `encode_deferred`) gives each output the bytes of its own render: 8- and
/// 16-bit, every output space, over two tiles, with the specialized export
/// variant, the general pipeline, mask layers and colours beyond the
/// output gamut (the input reaches 2.0).
#[test]
#[ignore = "requires a native GPU"]
fn one_render_for_several_outputs_matches_their_own_renders() {
    use rawtonic_engine::output_color_space::OutputColorSpace;
    let context = context();
    let processor = GpuProcessor::new(context.clone(), 2304, 512).unwrap();
    let (width, height) = (2200u32, 300u32);
    let view = input(&context, width, height, 29);
    let outputs = [
        (OutputColorSpace::Srgb, RenderOutputPrecision::EightBit),
        (
            OutputColorSpace::AdobeRgb,
            RenderOutputPrecision::SixteenBit,
        ),
        (OutputColorSpace::DisplayP3, RenderOutputPrecision::EightBit),
        (
            OutputColorSpace::ProPhoto,
            RenderOutputPrecision::SixteenBit,
        ),
        (OutputColorSpace::Srgb, RenderOutputPrecision::SixteenBit),
    ];
    let with_space = |mut all: crate::image_processing::PreparedAdjustments,
                      space: OutputColorSpace| {
        all.global.output_space = [space.shader_code(), 0, 0, 0];
        all.global.output_matrix =
            crate::image_processing::gpu_mat3_from_rows(space.from_linear_srgb_f32());
        all
    };
    let masks = upload_test_masks(width, height, 3);
    let recipes = [
        (json!({}), false),
        (json!({"exposure":0.3,"contrast":20,"saturation":15}), false),
        (
            json!({"exposure":0.4,"contrast":15,"sharpness":40,"lumaNoiseReduction":20,
                "clarity":30,"structure":20,"vignetteAmount":-30,"grainAmount":20}),
            false,
        ),
        (layer_sums_profile(0.2, 0.5), true),
    ];
    let mut checked = 0;
    for (recipe, with_masks) in &recipes {
        let base = get_all_adjustments_from_json(recipe, true, Some(2));
        let uploaded = with_masks.then(|| {
            let mut uploader = MaskUploader::new(&context, width, height, masks.len(), false);
            for (index, mask) in masks.iter().enumerate() {
                uploader.push_at(&context, index, mask).unwrap();
            }
            uploader.finish(&context).unwrap()
        });
        let whole = Roi {
            x: 0,
            y: 0,
            width,
            height,
        };
        let request = |adjustments| RenderRequest {
            adjustments,
            mask_bitmaps: &[],
            lut: None,
            roi: None,
        };
        let expected: Vec<RenderedPixels> = outputs
            .iter()
            .map(|&(space, precision)| {
                processor
                    .run_in_window(
                        &view,
                        width,
                        height,
                        whole,
                        request(with_space(base.clone(), space)),
                        false,
                        precision,
                        ReadbackShader::Export,
                        None,
                        None,
                        uploaded.as_ref(),
                    )
                    .unwrap()
                    .0
            })
            .collect();
        let render_outputs: Vec<RenderOutput> = outputs
            .iter()
            .map(|&(space, precision)| {
                let global = with_space(base.clone(), space).global;
                RenderOutput {
                    precision,
                    output_space: global.output_space,
                    output_matrix: global.output_matrix,
                }
            })
            .collect();
        let (together, ..) = processor
            .render_window(
                &view,
                width,
                height,
                whole,
                request(base.clone()),
                false,
                RenderOutputPrecision::EightBit,
                ReadbackShader::Export,
                None,
                None,
                uploaded.as_ref(),
                &render_outputs,
            )
            .unwrap();
        for (index, (together, expected)) in together.iter().zip(&expected).enumerate() {
            let same = match (together, expected) {
                (RenderedPixels::U8(a), RenderedPixels::U8(b)) => a == b,
                (RenderedPixels::U16(a), RenderedPixels::U16(b)) => a == b,
                _ => false,
            };
            assert!(same, "recipe {recipe} output {:?} differs", outputs[index]);
            checked += 1;
        }
    }
    assert_eq!(checked, recipes.len() * outputs.len());
}

/// A CPU-readback render the GPU fails (here: a source texture destroyed,
/// as an out-of-memory upload leaves an invalid one) is an error, not a
/// black image an export would write as its file.
#[test]
#[ignore = "requires a native GPU"]
fn readback_render_fails_when_the_gpu_reports_an_error() {
    let context = context();
    // As in the app: errors outside a scope are only logged.
    context
        .device
        .on_uncaptured_error(Arc::new(|error: wgpu::Error| {
            log::error!("[wgpu-error] {error}")
        }));
    let source =
        DynamicImage::ImageRgba32F(ImageBuffer::from_pixel(64, 48, Rgba([0.5, 0.4, 0.3, 1.0])));
    let render = |source: RenderSource| {
        let processor = ProcessorSlot::new(None);
        let cache = Mutex::new(None);
        process_and_get_dynamic_image_inner(
            &context,
            &processor,
            &cache,
            source,
            RenderRequest {
                adjustments: get_all_adjustments_from_json(&json!({}), false, Some(2)),
                mask_bitmaps: &[],
                lut: None,
                roi: None,
            },
            "gpu_error_test",
            RenderOutputPrecision::EightBit,
            false,
            None,
            None,
            ReadbackShader::Export,
        )
    };
    let intact = upload_render_source(&context, &source, 7, None)
        .unwrap()
        .unwrap();
    let image = render(RenderSource::Uploaded(intact)).expect("an intact source renders");
    assert!(image.to_rgb8().pixels().any(|p| p.0 != [0, 0, 0]));

    let broken = upload_render_source(&context, &source, 8, None)
        .unwrap()
        .unwrap();
    broken.0.texture.destroy();
    let error =
        render(RenderSource::Uploaded(broken)).expect_err("a GPU error must fail the render");
    assert!(error.contains("graphics card"), "{error}");
}

/// GPU work a render hands to the upload pool (the colour noise scales'
/// reconstruction) runs outside the caller's error scopes: it fails only
/// through its own ([`gpu_scoped`] on the pool's thread).
#[test]
#[ignore = "requires a native GPU"]
fn gpu_work_on_the_pool_fails_inside_its_own_scope() {
    let context = context();
    context
        .device
        .on_uncaptured_error(Arc::new(|error: wgpu::Error| {
            log::error!("[wgpu-error] {error}")
        }));
    let too_wide = context.limits.max_texture_dimension_2d + 1;
    let invalid_texture = || {
        context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("too wide"),
            size: wgpu::Extent3d {
                width: too_wide,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    };
    // The test thread is not a rayon thread, so the work moves to the pool.
    assert!(rayon::current_thread_index().is_none());
    let missed = gpu_scoped(&context.device, || off_global_pool(invalid_texture));
    assert!(
        missed.is_ok(),
        "the caller's scope does not see the pool's errors"
    );
    let caught = gpu_scoped(&context.device, || {
        off_global_pool(|| gpu_scoped(&context.device, invalid_texture))
    })
    .expect("the caller's scope sees nothing");
    assert!(caught.is_err(), "the pool's own scope catches its error");
}

/// A mask layer that cannot be written (here: its array destroyed, as one
/// the GPU had no memory for is invalid) fails the upload instead of
/// rendering an empty mask.
#[test]
#[ignore = "requires a native GPU"]
fn mask_upload_fails_when_the_gpu_reports_an_error() {
    let context = context();
    context
        .device
        .on_uncaptured_error(Arc::new(|error: wgpu::Error| {
            log::error!("[wgpu-error] {error}")
        }));
    let mask = masked(ImageBuffer::from_pixel(32, 16, Luma([200u8])));
    let mut intact = MaskUploader::new(&context, 32, 16, 1, false);
    intact
        .push_at(&context, 0, &mask)
        .expect("an intact array takes the mask");
    intact.finish(&context).expect("an intact array finishes");
    let mut broken = MaskUploader::new(&context, 32, 16, 1, false);
    broken.texture.destroy();
    let error = broken
        .push_at(&context, 0, &mask)
        .expect_err("a GPU error must fail the upload");
    assert!(error.contains("graphics card"), "{error}");
}

fn chunk_test_render(
    context: &GpuContext,
    source: &DynamicImage,
    profile: &serde_json::Value,
    raw: bool,
    precision: RenderOutputPrecision,
    roi: Option<Roi>,
) -> Result<DynamicImage, String> {
    let (width, height) = source.dimensions();
    let mut adjustments = get_all_adjustments_from_json(profile, raw, Some(2));
    // A measured noise level as the photo's (not a recipe key).
    if let Some(level) = profile["testNoiseLevel"].as_f64() {
        adjustments.uniforms.global.noise_level = level as f32;
    }
    let mask = masked(ImageBuffer::from_fn(width, height, |x, y| {
        Luma([((x * 3 + y * 7) % 256) as u8])
    }));
    let masks = if adjustments.mask_count > 0 {
        vec![mask]
    } else {
        Vec::new()
    };
    let processor = ProcessorSlot::new(None);
    let cache = Mutex::new(None);
    process_and_get_dynamic_image_inner(
        context,
        &processor,
        &cache,
        RenderSource::Image {
            image: source,
            transform_hash: 1,
        },
        RenderRequest {
            adjustments,
            mask_bitmaps: &masks,
            lut: None,
            roi,
        },
        "chunk_test",
        precision,
        false,
        None,
        None,
        ReadbackShader::Export,
    )
}

#[test]
#[ignore = "requires a native GPU; prints whole-render hashes to compare across commits"]
fn whole_render_hash_probe() {
    use std::hash::{Hash, Hasher};
    let context = context();
    let source = chunk_test_source(3100, 2300);
    let mut profiles = chunk_test_profiles();
    profiles.push(("flare_only", json!({"flareAmount":60,"exposure":1.5})));
    for (key, value) in [
        ("highlights", -80.0),
        ("clarity", 60.0),
        ("structure", 50.0),
        ("dehaze", 30.0),
        ("centré", 40.0),
        ("sharpness", 70.0),
        ("lumaNoiseReduction", 40.0),
        ("colorNoiseReduction", 50.0),
        ("moireAmount", 100.0),
        ("chromaticAberrationRedCyan", 100.0),
        ("defringePurpleAmount", 20.0),
        ("vignetteAmount", -50.0),
        ("grainAmount", 40.0),
        ("glowAmount", 50.0),
        ("halationAmount", 50.0),
        ("sharpnessFalloff", 80.0),
    ] {
        profiles.push((key, json!({ key: value })));
    }
    for (name, profile) in profiles {
        for raw in [true, false] {
            for precision in [
                RenderOutputPrecision::EightBit,
                RenderOutputPrecision::SixteenBit,
            ] {
                let image =
                    chunk_test_render(&context, &source, &profile, raw, precision, None).unwrap();
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                image.as_bytes().hash(&mut hasher);
                println!(
                    "HASH {name} raw={raw} {precision:?} {:016x}",
                    hasher.finish()
                );
            }
        }
    }
}

#[test]
fn render_windows_cover_the_region_in_tiles_within_the_limit() {
    for (width, height, bounds, max_dim, margin) in [
        (3100, 2300, None, 2600, 216),
        (40_000, 9000, None, 16_384, 500),
        (
            9000,
            5000,
            Some(Roi {
                x: 1500,
                y: 700,
                width: 6000,
                height: 3000,
            }),
            4096,
            300,
        ),
    ] {
        let bounds = bounds.unwrap_or(Roi {
            x: 0,
            y: 0,
            width,
            height,
        });
        let plan = plan_render_windows(width, height, bounds, max_dim, margin).unwrap();
        let mut covered = 0u64;
        for (region, window) in &plan {
            assert!(window.width <= max_dim && window.height <= max_dim);
            assert!(intersect_roi(*region, bounds) == Some(*region));
            // Regions start on tile boundaries or the requested region's edge,
            // and end on one or at its edge, as the tiles of a whole render.
            assert!(region.x % TILE_SIZE == 0 || region.x == bounds.x);
            assert!(region.y % TILE_SIZE == 0 || region.y == bounds.y);
            let right = region.x + region.width;
            let bottom = region.y + region.height;
            assert!(right % TILE_SIZE == 0 || right == bounds.x + bounds.width);
            assert!(bottom % TILE_SIZE == 0 || bottom == bounds.y + bounds.height);
            // The window holds the region and the margin, up to the photo's edges.
            assert_eq!(window.x, region.x.saturating_sub(margin));
            assert_eq!(window.y, region.y.saturating_sub(margin));
            assert_eq!(window.x + window.width, (right + margin).min(width));
            assert_eq!(window.y + window.height, (bottom + margin).min(height));
            covered += area(*region);
        }
        assert_eq!(covered, area(bounds), "regions must tile the bounds");
        for (i, (a, _)) in plan.iter().enumerate() {
            for (b, _) in &plan[i + 1..] {
                assert!(intersect_roi(*a, *b).is_none(), "regions overlap");
            }
        }
    }
    // A tile with its margins must fit the limit.
    let whole = Roi {
        x: 0,
        y: 0,
        width: 20_000,
        height: 20_000,
    };
    assert!(plan_render_windows(20_000, 20_000, whole, 2048 + 2 * 300 - 1, 300).is_none());
    assert!(plan_render_windows(20_000, 20_000, whole, 2048 + 2 * 300, 300).is_some());
    assert!(plan_render_windows(20_000, 20_000, whole, 500, 300).is_none());
}

#[test]
fn render_window_margin_covers_the_widest_read() {
    let plain = get_all_adjustments_from_json(&json!({}), true, Some(2));
    // Structure's blur at the photo's scale plus the tile overlap.
    let margin = render_window_margin(&plain.uniforms, 3100, 2300);
    assert!(margin >= TILE_OVERLAP + (40.0_f32 * 2300.0 / 1080.0).ceil() as u32);
    let shifted = get_all_adjustments_from_json(
        &json!({"chromaticAberrationRedCyan": 100, "chromaticAberrationBlueYellow": -100}),
        true,
        Some(2),
    );
    // 1 % of the half diagonal of a 60 000 x 2000 px panorama: 300 px.
    let wide = render_window_margin(&shifted.uniforms, 60_000, 2000);
    assert!(wide > 300, "{wide}");
    assert!(wide > render_window_margin(&plain.uniforms, 60_000, 2000));
}

/// A context whose texture limit is lowered to `max_dim`.
fn limited_context(context: &GpuContext, max_dim: u32) -> GpuContext {
    let mut limited = context.clone();
    limited.limits.max_texture_dimension_2d = max_dim;
    limited
}

#[test]
#[ignore = "requires a native GPU; renders over the texture limit from windows"]
fn renders_over_the_texture_limit_match_whole_renders() {
    let context = context();
    let (width, height) = (3100, 2300);
    let source = chunk_test_source(width, height);
    let limited = limited_context(&context, 2600);
    let mut comparisons = 0;
    for (name, profile) in chunk_test_profiles() {
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(2));
        let margin = render_window_margin(&adjustments.uniforms, width, height);
        let whole_photo = Roi {
            x: 0,
            y: 0,
            width,
            height,
        };
        let windows = plan_render_windows(width, height, whole_photo, 2600, margin).unwrap();
        assert!(windows.len() >= 4, "{name}: {} windows", windows.len());
        for raw in [true, false] {
            for precision in [
                RenderOutputPrecision::EightBit,
                RenderOutputPrecision::SixteenBit,
            ] {
                for roi in [
                    None,
                    Some(Roi {
                        x: 1001,
                        y: 1500,
                        width: 2003,
                        height: 701,
                    }),
                ] {
                    let whole = chunk_test_render(&context, &source, &profile, raw, precision, roi)
                        .unwrap();
                    let windowed =
                        chunk_test_render(&limited, &source, &profile, raw, precision, roi)
                            .unwrap();
                    assert_eq!(windowed.dimensions(), whole.dimensions());
                    let differing = whole
                        .as_bytes()
                        .iter()
                        .zip(windowed.as_bytes())
                        .filter(|(a, b)| a != b)
                        .count();
                    assert!(
                        differing == 0,
                        "{name} raw={raw} {precision:?} roi={roi:?}: {differing} bytes differ"
                    );
                    comparisons += 1;
                }
            }
        }
    }
    println!("{comparisons} windowed renders matched whole renders byte for byte");
}

/// Clarity's large blur reaches 40 × short side / 1080 pixels: beyond the
/// tiles' 128-pixel overlap above a 3456-pixel short side. From
/// `renderVersion` 2 its vertical pass reads the whole radius at tile edges,
/// so rows next to a tile seam render as the same content inside a tile
/// (the photo shifted down by 1024 rows); before, they differed there.
#[test]
#[ignore = "requires a native GPU; Clarity at tile seams of a photo above 3456 px"]
fn the_large_blur_has_its_whole_radius_at_tile_seams() {
    let context = context();
    // Radius 200 at a 5400-pixel short side.
    let width = 5400;
    let content = |y: i64, x: u32| {
        // A different brightness every 16 rows.
        let mut h = (y.div_euclid(16) as u32).wrapping_mul(0x9e37_79b9);
        h ^= h >> 15;
        let band = 0.1 + 0.8 * (h % 1000) as f32 / 1000.0;
        let v = band + 0.05 * (x as f32 / width as f32);
        image::Rgb([v, v * 0.9, v * 0.8])
    };
    let photo = |height: u32, shift: i64| {
        DynamicImage::ImageRgb32F(ImageBuffer::from_fn(width, height, |x, y| {
            content(y as i64 - shift, x)
        }))
    };
    let seam = 2048;
    let rows = |shift: u32| Roi {
        x: 0,
        y: seam - 150 + shift,
        width,
        height: 300,
    };
    let mut worst = Vec::new();
    for version in [1, 2] {
        let profile = json!({"clarity": 80, "renderVersion": version});
        let render = |source: &DynamicImage, roi: Roi| {
            chunk_test_render(
                &context,
                source,
                &profile,
                false,
                RenderOutputPrecision::SixteenBit,
                Some(roi),
            )
            .unwrap()
            .into_rgb16()
        };
        let at_seam = render(&photo(6000, 0), rows(0));
        let inside = render(&photo(7024, 1024), rows(1024));
        let diff = at_seam
            .as_raw()
            .iter()
            .zip(inside.as_raw())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        worst.push(diff);
    }
    println!(
        "largest difference at the seam: v1 {} v2 {} (of 65535)",
        worst[0], worst[1]
    );
    assert!(worst[0] > 64, "version 1 shows the seam: {}", worst[0]);
    assert!(
        worst[1] <= 1,
        "version 2 renders the seam as a tile's inside: {}",
        worst[1]
    );
}

/// The blurs Clarity version 2 reads are exact where it reads them on every
/// render version, at any photo size (a seam render above a 17280-pixel short
/// side, where the small blur outgrows the overlap, is too large to test).
#[test]
fn clarity_version_2_blurs_have_their_whole_radius_on_every_render_version() {
    let overlap = TILE_OVERLAP;
    for (index, radius) in [(2usize, 8 * 18), (3, 40 * 18)] {
        // Render version 1, no Clarity version 2: blurs stop at the overlap.
        assert_eq!(blur_rows_beyond(index, radius, false, 0), 0);
        // Clarity version 2 on render version 1: the whole radius, and the
        // large blur its reach beyond the tile too.
        let reach = 120;
        let extra = if index == 3 { reach } else { 0 };
        assert_eq!(
            blur_rows_beyond(index, radius, false, reach),
            radius + extra - overlap
        );
        assert_eq!(
            blur_rows_beyond(index, radius, true, reach),
            radius + extra - overlap
        );
        assert_eq!(blur_rows_beyond(index, radius, true, 0), radius - overlap);
    }
    // Blurs Clarity version 2 does not read keep their render version's rows.
    assert_eq!(blur_rows_beyond(1, 300, false, 120), 0);
    assert_eq!(blur_rows_beyond(4, 300, true, 120), 300 - overlap);
}

/// Clarity version 2 samples the large blur up to 120 pixels beyond a pixel
/// (`clarity_base`): within the tiles' 128-pixel overlap, so rows next to a
/// tile seam render as the same content inside a tile.
#[test]
#[ignore = "requires a native GPU; Clarity version 2 at tile seams"]
fn clarity_version_2_joins_tiles_exactly() {
    let context = context();
    let width = 5400;
    let content = |y: i64, x: u32| {
        let mut h = (y.div_euclid(16) as u32).wrapping_mul(0x9e37_79b9);
        h ^= h >> 15;
        let band = 0.1 + 0.8 * (h % 1000) as f32 / 1000.0;
        // Narrow bright columns as well, for the base's sideways samples.
        let v = if x % 97 < 3 {
            0.95
        } else {
            band + 0.05 * (x as f32 / width as f32)
        };
        image::Rgb([v, v * 0.9, v * 0.8])
    };
    let photo = |height: u32, shift: i64| {
        DynamicImage::ImageRgb32F(ImageBuffer::from_fn(width, height, |x, y| {
            content(y as i64 - shift, x)
        }))
    };
    let seam = 2048;
    let rows = |shift: u32| Roi {
        x: 0,
        y: seam - 150 + shift,
        width,
        height: 300,
    };
    // Also on render version 1, whose own blurs stop at the tile overlap: an
    // older recipe gets Clarity version 2 when Clarity is first added to it.
    for render_version in [1, 3] {
        let profile = json!({"clarity": 80, "renderVersion": render_version, "clarityVersion": 2});
        let render = |source: &DynamicImage, roi: Roi| {
            chunk_test_render(
                &context,
                source,
                &profile,
                false,
                RenderOutputPrecision::SixteenBit,
                Some(roi),
            )
            .unwrap()
            .into_rgb16()
        };
        let at_seam = render(&photo(6000, 0), rows(0));
        let inside = render(&photo(7024, 1024), rows(1024));
        let diff = at_seam
            .as_raw()
            .iter()
            .zip(inside.as_raw())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(
            diff <= 1,
            "render version {render_version}: a seam row renders as a tile's inside: {diff}"
        );
    }
}

/// Clarity version 2 measures the band against an edge-aware base: the dark
/// surroundings of a narrow bright stripe are darkened about half as much as
/// with version 1's plain blur (Metal: 1114 vs 2233 of 65535), while texture
/// away from it keeps at least its gain.
#[test]
#[ignore = "requires a native GPU; Clarity halos beside a narrow bright stripe"]
fn clarity_version_2_cuts_the_halo_beside_a_narrow_bright_edge() {
    let context = context();
    let (width, height) = (1200u32, 800u32);
    let source = DynamicImage::ImageRgb32F(ImageBuffer::from_fn(width, height, |x, y| {
        let v = if (598..602).contains(&x) && y < 400 {
            0.9
        } else if y >= 500 {
            // Fine texture well away from the stripe.
            0.3 + 0.06 * (((x / 6 + y / 6) % 2) as f32 - 0.5)
        } else {
            0.2
        };
        image::Rgb([v, v, v])
    }));
    let render = |profile: serde_json::Value| {
        chunk_test_render(
            &context,
            &source,
            &profile,
            false,
            RenderOutputPrecision::SixteenBit,
            None,
        )
        .unwrap()
        .into_rgb16()
    };
    let neutral = render(json!({"renderVersion": 3}));
    let v1 = render(json!({"renderVersion": 3, "clarity": 100}));
    let v2 = render(json!({"renderVersion": 3, "clarity": 100, "clarityVersion": 2}));
    let level = |img: &image::ImageBuffer<image::Rgb<u16>, Vec<u16>>, x: u32, y: u32| {
        img.get_pixel(x, y)[1] as f64
    };
    // Darkening 6-20 px beside the stripe, half way down it.
    let halo = |img: &image::ImageBuffer<image::Rgb<u16>, Vec<u16>>| {
        let xs: Vec<u32> = (580..=594).chain(606..=620).collect();
        xs.iter()
            .map(|&x| level(&neutral, x, 200) - level(img, x, 200))
            .sum::<f64>()
            / xs.len() as f64
    };
    // Texture amplitude well away from the stripe.
    let amplitude = |img: &image::ImageBuffer<image::Rgb<u16>, Vec<u16>>| {
        let row: Vec<f64> = (100..1100).map(|x| level(img, x, 650)).collect();
        let mean = row.iter().sum::<f64>() / row.len() as f64;
        (row.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / row.len() as f64).sqrt()
    };
    let (h1, h2) = (halo(&v1), halo(&v2));
    let (a0, a1, a2) = (amplitude(&neutral), amplitude(&v1), amplitude(&v2));
    println!("halo v1 {h1:.0} v2 {h2:.0}; texture amplitude neutral {a0:.0} v1 {a1:.0} v2 {a2:.0}");
    assert!(h1 > 500.0, "version 1 darkens beside the stripe: {h1}");
    assert!(h2 < 0.6 * h1, "version 2 cuts the halo: {h2} vs {h1}");
    assert!(
        (a2 - a0) > 0.75 * (a1 - a0),
        "version 2 keeps most of the texture gain: {a2} vs {a1} (neutral {a0})"
    );
}

#[test]
#[ignore = "requires a native GPU; renders over the texture limit never return the source"]
fn renders_over_the_texture_limit_fail_instead_of_skipping_edits() {
    let context = context();
    let source = chunk_test_source(600, 400);
    let limited = limited_context(&context, 512);
    // A tile with its margins does not fit 512 px: no windows either.
    let edited = chunk_test_render(
        &limited,
        &source,
        &json!({"exposure": 1}),
        true,
        RenderOutputPrecision::EightBit,
        None,
    );
    assert_eq!(edited.unwrap_err(), texture_limit_error(600, 400, 512));
    // Display frames are never rendered from windows.
    let processor = ProcessorSlot::new(None);
    let cache = Mutex::new(None);
    let display = process_and_get_dynamic_image_inner(
        &limited,
        &processor,
        &cache,
        RenderSource::Image {
            image: &source,
            transform_hash: 1,
        },
        RenderRequest {
            adjustments: get_all_adjustments_from_json(&json!({}), true, Some(2)),
            mask_bitmaps: &[],
            lut: None,
            roi: None,
        },
        "chunk_test",
        RenderOutputPrecision::EightBit,
        true,
        None,
        None,
        ReadbackShader::Export,
    );
    assert_eq!(display.unwrap_err(), texture_limit_error(600, 400, 512));
}

/// A noisy linear frame with colour detail for the multi-scale colour noise
/// reduction: grey and coloured halves with chroma noise and thin lines.
fn chroma_noise_frame(width: u32, height: u32) -> DynamicImage {
    let mut state = 11u32;
    let mut noise = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
    };
    DynamicImage::ImageRgb32F(ImageBuffer::from_fn(width, height, |x, y| {
        let base = if x < width / 2 {
            [0.18, 0.18, 0.18]
        } else {
            [0.40, 0.25, 0.15]
        };
        let line = (x % 23 == 3 || y % 29 == 5) as u32 as f32;
        let colour = [0.05 * line, -0.03 * line, 0.06 * line];
        image::Rgb(std::array::from_fn(|c| {
            let v: f32 = base[c] + colour[c];
            (v.sqrt() + 0.02 * noise()).max(0.0).powi(2)
        }))
    }))
}

fn render_source(
    context: &GpuContext,
    image: &DynamicImage,
    profile: serde_json::Value,
    precision: RenderOutputPrecision,
) -> DynamicImage {
    let processor = ProcessorSlot::new(None);
    let cache = Mutex::new(None);
    process_and_get_dynamic_image_inner(
        context,
        &processor,
        &cache,
        RenderSource::Image {
            image,
            transform_hash: 1,
        },
        RenderRequest {
            adjustments: get_all_adjustments_from_json(&profile, true, Some(1)),
            mask_bitmaps: &[],
            lut: None,
            roi: None,
        },
        "test",
        precision,
        false,
        None,
        None,
        ReadbackShader::Export,
    )
    .unwrap()
}

#[test]
#[ignore = "requires a native GPU"]
fn multiscale_color_noise_matches_its_cpu_reference() {
    use rawtonic_engine::color_noise;
    let context = context();
    // Wider than one tile, so the half-resolution texture is addressed in
    // image coordinates across tiles.
    let image = chroma_noise_frame(2300, 300);
    // Version 7 also below Color 25 and above Detail 50, where its speck
    // swap is weakened (`Tuning::speck_strength`), beside version 6 there.
    let cases = [
        (2, 25, 50),
        (3, 25, 50),
        (4, 25, 50),
        (5, 25, 50),
        (6, 25, 50),
        (7, 25, 50),
        (6, 15, 65),
        (7, 15, 65),
        (8, 25, 50),
        (9, 25, 50),
        (9, 15, 65),
        (10, 25, 50),
        (10, 15, 65),
    ];
    for (version, color, detail) in cases {
        let gpu = render_source(
            &context,
            &image,
            json!({
                "colorNoiseReduction": color,
                "colorNoiseDetail": detail,
                "colorNoiseVersion": version
            }),
            RenderOutputPrecision::SixteenBit,
        )
        .to_rgba16();
        let settings = color_noise::Settings {
            amount: color as f32 / 100.0,
            detail: detail as f32 / 100.0,
            smoothness: 0.5,
        };
        // The renderer reads its input as 16-bit floats: the CPU reference
        // filters the same values, so discontinuous decisions (the switched
        // guide, version 7's speck swap) see the same chroma.
        let mut input = image.to_rgb32f();
        input
            .iter_mut()
            .for_each(|v| *v = half::f16::from_f32(*v).to_f32());
        let filtered = color_noise::apply_to_linear(
            &input,
            settings,
            &color_noise::Tuning::for_version(version),
        );
        let cpu = render_source(
            &context,
            &DynamicImage::ImageRgb32F(filtered),
            json!({}),
            RenderOutputPrecision::SixteenBit,
        )
        .to_rgba16();
        let unfiltered = render_source(
            &context,
            &image,
            json!({}),
            RenderOutputPrecision::SixteenBit,
        )
        .to_rgba16();
        let (mut worst, mut changed, mut sum, mut far) = (0i32, 0i64, 0i64, 0u32);
        for ((g, c), u) in gpu.pixels().zip(cpu.pixels()).zip(unfiltered.pixels()) {
            far += u32::from((0..3).any(|k| (g[k] as i32 - c[k] as i32).abs() > 64));
            for k in 0..3 {
                let d = (g[k] as i32 - c[k] as i32).abs();
                worst = worst.max(d);
                sum += d as i64;
                changed += (c[k] as i32 - u[k] as i32).abs() as i64;
            }
        }
        let n = (gpu.width() * gpu.height() * 3) as f64;
        println!(
            "version {version}, Color {color}, Detail {detail}: GPU vs CPU reference: max {worst}, mean {:.2}, {far} pixels above 64; filter changed mean {:.1} levels",
            sum as f64 / n,
            changed as f64 / n
        );
        // The renderer reads level 1 as 16-bit floats; the filter itself moves
        // pixels by far more than that.
        assert!(sum as f64 / n < 4.0, "mean {}", sum as f64 / n);
        assert!(changed as f64 / n > 40.0 * (sum as f64 / n).max(1.0));
        assert!(worst < 600, "max {worst}");
        // The switched guide of versions 5-7 and version 7's speck swap are
        // discontinuous: where a pixel's chroma lies at their thresholds,
        // GPU and CPU rounding can flip the decision (on the same 16-bit
        // input: 2 of 690 000 pixels above 64 levels for version 7, none
        // for versions 2-6 at Color 25; at Color 15, Detail 65 14 for
        // version 6, 16 for version 7). Such flips must stay rare.
        assert!(
            far * 10_000 < gpu.width() * gpu.height(),
            "{far} pixels above 64"
        );
    }
}

#[test]
#[ignore = "requires a native GPU"]
fn multiscale_color_noise_at_zero_or_without_version_is_unchanged() {
    let context = context();
    let image = chroma_noise_frame(300, 200);
    let render = |profile| {
        render_source(&context, &image, profile, RenderOutputPrecision::SixteenBit).into_bytes()
    };
    // Color 0: no colour noise reduction with any version.
    assert_eq!(
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 2})),
        render(json!({"colorNoiseReduction": 0}))
    );
    assert_eq!(
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 3})),
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 2}))
    );
    // Version 4's linear-mean levels change only the chroma: at Color 0
    // (luminance noise reduction only) it renders as version 3.
    assert_eq!(
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 4})),
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 3}))
    );
    assert_ne!(
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 4})),
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 3}))
    );
    // Version 5's switched guide acts only in the colour expansion: at
    // Color 0 it renders as version 4.
    assert_eq!(
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 5})),
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 4}))
    );
    assert_ne!(
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 5})),
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 4}))
    );
    // Version 6 changes only how the finest detail is measured: at Color 0
    // it renders as version 5.
    assert_eq!(
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 6})),
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 5}))
    );
    assert_ne!(
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 6})),
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 5}))
    );
    // Version 7 changes only the detail of isolated off-colour pixels: at
    // Color 0 it renders as version 6.
    assert_eq!(
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 7})),
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 6}))
    );
    assert_ne!(
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 7})),
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 6}))
    );
    // Version 8 changes only the speck swap's luminance gate: at Color 0
    // it renders as version 7.
    assert_eq!(
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 8})),
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 7}))
    );
    // Version 9 changes only the filtered chroma: at Color 0 it renders as
    // version 8.
    assert_eq!(
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 9})),
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 8}))
    );
    assert_ne!(
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 9})),
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 8}))
    );
    // So does version 10.
    assert_eq!(
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 10})),
        render(json!({"colorNoiseReduction": 0, "colorNoiseVersion": 9}))
    );
    assert_ne!(
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 10})),
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 9}))
    );
    // Version 3 keeps more of the quiet coarse levels than version 2.
    assert_ne!(
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 3})),
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 2}))
    );
    // Version 1 is the cross-bilateral filter, as recipes without the key.
    assert_eq!(
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 1})),
        render(json!({"colorNoiseReduction": 25}))
    );
    assert_ne!(
        render(json!({"colorNoiseReduction": 25, "colorNoiseVersion": 2})),
        render(json!({"colorNoiseReduction": 25}))
    );
}

#[test]
#[ignore = "requires a native GPU"]
fn multiscale_color_noise_version_8_judges_specks_after_luminance_noise_reduction() {
    // A flat grey under colour and luminance noise with single off-colour
    // pixels a few noise levels darker than the grey (noise the luminance
    // noise reduction takes back: version 7's gate, reading the input's
    // luminance, let them keep their colour) and single coloured points far
    // brighter (features the luminance noise reduction keeps: they keep
    // their colour in both versions).
    let context = context();
    let (w, h) = (256u32, 192u32);
    let mut state = 5u32;
    let mut noise = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
    };
    let grey = 0.18f32.sqrt();
    let mut image = ImageBuffer::from_fn(w, h, |_, _| {
        image::Rgb(std::array::from_fn(|_| {
            (grey + 0.03 * noise()).max(0.0).powi(2)
        }))
    });
    let spots = |row: u32| -> Vec<(u32, u32)> {
        (0..48)
            .map(|k| (8 + (k % 12) * 20, row + (k / 12) * 20))
            .collect()
    };
    let (specks, points) = (spots(10), spots(100));
    // Square-root RGB: chroma pushed, luminance lowered by `dark`.
    let put = |image: &mut image::Rgb32FImage, (x, y): (u32, u32), dark: f32| {
        let (r, b) = (grey - dark + 0.12, grey - dark - 0.10);
        let g = grey - dark;
        image.put_pixel(x, y, image::Rgb([r, g, b].map(|v: f32| v.max(0.0).powi(2))));
    };
    // The specks' square-root luminance ends up 0.027 below the grey's
    // (about 3 noise levels; the chroma push itself adds 0.018).
    for &p in &specks {
        put(&mut image, p, 0.045);
    }
    for &p in &points {
        put(&mut image, p, -0.25);
    }
    let image = DynamicImage::ImageRgb32F(image);
    let chroma_at = |version: u64, spots: &[(u32, u32)]| -> f32 {
        let out = render_source(
            &context,
            &image,
            json!({"colorNoiseReduction": 25, "colorNoiseVersion": version,
                   "lumaNoiseReduction": 15, "lumaNoiseVersion": 2, "sharpness": 0}),
            RenderOutputPrecision::SixteenBit,
        )
        .to_rgb32f();
        spots
            .iter()
            .map(|&(x, y)| {
                let [r, g, b] = out.get_pixel(x, y).0.map(|v| v.max(0.0).sqrt());
                (r - g).hypot(b - g)
            })
            .sum::<f32>()
            / spots.len() as f32
    };
    let (s6, s7, s8) = (
        chroma_at(6, &specks),
        chroma_at(7, &specks),
        chroma_at(8, &specks),
    );
    let (p7, p8, p9, p10) = (
        chroma_at(7, &points),
        chroma_at(8, &points),
        chroma_at(9, &points),
        chroma_at(10, &points),
    );
    println!(
        "speck chroma v6 {s6:.4}, v7 {s7:.4}, v8 {s8:.4}; bright points v7 {p7:.4}, v8 {p8:.4}, v9 {p9:.4}, v10 {p10:.4}"
    );
    assert!(s7 > 0.8 * s6, "version 7 changed: {s6} -> {s7}");
    assert!(s8 < 0.7 * s7, "version 8 left the specks: {s7} -> {s8}");
    assert!(p8 > 0.95 * p7, "bright points lost colour: {p7} -> {p8}");
    // Version 9's range clamp fades out for points this far beyond it.
    assert!(p9 > 0.95 * p8, "bright points lost colour: {p8} -> {p9}");
    assert!(p10 > 0.95 * p8, "bright points lost colour: {p8} -> {p10}");
    // Without luminance noise reduction the gate reads the input's
    // luminance: version 8 renders as version 7 (the CPU reference).
    let render = |version: u64| {
        render_source(
            &context,
            &image,
            json!({"colorNoiseReduction": 25, "colorNoiseVersion": version,
                   "lumaNoiseReduction": 0}),
            RenderOutputPrecision::SixteenBit,
        )
        .into_bytes()
    };
    assert_eq!(render(8), render(7));
}

#[test]
#[ignore = "requires a native GPU"]
fn multiscale_color_noise_version_8_keeps_wide_gamut_points() {
    // Single points of a valid wide-gamut colour (a negative linear sRGB
    // channel inside ProPhoto) brighter than a noisy grey, under a luminance
    // noise reduction that leaves them as they are (Luminance 1): version 8
    // compares their luminance after it with the same working-gamut
    // luminance it starts from, so they stand out as in version 7 and keep
    // their colour (clipping the negative channel there took them for
    // pixels the luminance noise reduction had darkened, and swapped them).
    let context = context();
    let (w, h) = (192u32, 128u32);
    let mut state = 7u32;
    let mut noise = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
    };
    let grey = 0.08185f32.sqrt();
    let mut image = ImageBuffer::from_fn(w, h, |_, _| {
        image::Rgb(std::array::from_fn(|_| {
            (grey + 0.03 * noise()).max(0.0).powi(2)
        }))
    });
    let points: Vec<(u32, u32)> = (0..40)
        .map(|k| (8 + (k % 10) * 18, 10 + (k / 10) * 28))
        .collect();
    for &(x, y) in &points {
        image.put_pixel(x, y, image::Rgb([-0.25, 0.3, 0.3]));
    }
    let image = DynamicImage::ImageRgb32F(image);
    let chroma_at = |version: u64| -> f32 {
        let out = render_source(
            &context,
            &image,
            json!({"colorNoiseReduction": 25, "colorNoiseVersion": version,
                   "lumaNoiseReduction": 1, "lumaNoiseVersion": 2, "sharpness": 0}),
            RenderOutputPrecision::SixteenBit,
        )
        .to_rgb32f();
        points
            .iter()
            .map(|&(x, y)| {
                let [r, g, b] = out.get_pixel(x, y).0.map(|v| v.max(0.0).sqrt());
                (r - g).hypot(b - g)
            })
            .sum::<f32>()
            / points.len() as f32
    };
    let (v6, v7, v8, v9) = (chroma_at(6), chroma_at(7), chroma_at(8), chroma_at(9));
    let v10 = chroma_at(10);
    println!(
        "wide-gamut point chroma v6 {v6:.4}, v7 {v7:.4}, v8 {v8:.4}, v9 {v9:.4}, v10 {v10:.4}"
    );
    assert!(
        v9 > 0.95 * v8,
        "wide-gamut points lost colour: {v8} -> {v9}"
    );
    assert!(
        v10 > 0.95 * v8,
        "wide-gamut points lost colour: {v8} -> {v10}"
    );
    assert!(v7 > 0.95 * v6, "version 7 changed: {v6} -> {v7}");
    assert!(
        v8 > 0.95 * v7,
        "wide-gamut points lost colour: {v7} -> {v8}"
    );
}

#[test]
#[ignore = "requires a native GPU"]
fn multiscale_color_noise_version_9_keeps_chroma_within_the_neighbourhood() {
    // A noisy grey beside an orange area (a colour edge at x = 64) with
    // single darker off-colour pixels on the grey side next to the edge,
    // which stand out in brightness (the speck swap leaves them) and whose
    // chroma the detail kept along the edge kept in version 8; version 9
    // pulls them into the range of their neighbourhood's reconstruction,
    // the edge itself unchanged (rawtonic-engine
    // `version_9_keeps_filtered_chroma_within_the_neighbourhood`).
    let context = context();
    let (w, h) = (128u32, 128u32);
    let mut state = 13u32;
    let mut noise = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
    };
    let (grey, orange) = ([0.2f32; 3], [0.40f32, 0.25, 0.15]);
    let mut image = ImageBuffer::from_fn(w, h, |x, _| {
        let base = if x < 64 { grey } else { orange };
        image::Rgb(base.map(|v: f32| (v.sqrt() + 0.035 * noise()).max(0.0).powi(2)))
    });
    let specks: Vec<(u32, u32)> = (0..30).map(|k| (62 + k % 2, 4 + k * 4)).collect();
    for &(x, y) in &specks {
        let p = grey[0].sqrt() - 0.08;
        image.put_pixel(
            x,
            y,
            image::Rgb([p - 0.05, p, p + 0.05].map(|v: f32| v.max(0.0).powi(2))),
        );
    }
    let image = DynamicImage::ImageRgb32F(image);
    let rows: Vec<u32> = (0..h).filter(|y| y % 4 != 0).collect();
    let measure = |version: u64| -> (f32, f32) {
        let out = render_source(
            &context,
            &image,
            json!({"colorNoiseReduction": 50, "colorNoiseVersion": version,
                   "lumaNoiseReduction": 0, "sharpness": 0}),
            RenderOutputPrecision::SixteenBit,
        )
        .to_rgb32f();
        let chroma = |x: u32, y: u32| {
            let [r, g, b] = out.get_pixel(x, y).0.map(|v| v.max(0.0).sqrt());
            [r - g, b - g]
        };
        let column = |x: u32| {
            let s = rows.iter().fold([0.0f32; 2], |s, &y| {
                let c = chroma(x, y);
                [s[0] + c[0], s[1] + c[1]]
            });
            s.map(|v| v / rows.len() as f32)
        };
        let off = specks
            .iter()
            .map(|&(x, y)| {
                let (c, m) = (chroma(x, y), column(x));
                (c[0] - m[0]).hypot(c[1] - m[1])
            })
            .sum::<f32>()
            / specks.len() as f32;
        let (a, b) = (column(61), column(66));
        (off, (b[0] - a[0]).hypot(b[1] - a[1]))
    };
    let ((s8, e8), (s9, e9), (s10, e10)) = (measure(8), measure(9), measure(10));
    println!(
        "speck chroma v8 {s8:.4}, v9 {s9:.4}, v10 {s10:.4}; edge step v8 {e8:.4}, v9 {e9:.4}, v10 {e10:.4}"
    );
    assert!(s9 < 0.6 * s8, "version 9 left the specks: {s8} -> {s9}");
    assert!((e9 / e8 - 1.0).abs() < 0.01, "edge step {e8} -> {e9}");
    assert!(
        s10 <= s9 * 1.01,
        "version 10 kept more of the specks: {s9} -> {s10}"
    );
    assert!((e10 / e8 - 1.0).abs() < 0.01, "edge step {e8} -> {e10}");
}

#[test]
#[ignore = "native GPU timing probe; multi-scale colour noise reduction at preview size"]
fn multiscale_color_noise_latency_probe() {
    use rawtonic_engine::color_noise;
    let context = context();
    for (width, height) in [(2304u32, 1536u32), (6000, 4000)] {
        let image = chroma_noise_frame(width, height);
        let mut builds = Vec::new();
        for _ in 0..5 {
            // Version 3's square-root levels, then the newest's.
            let start = Instant::now();
            let _ = color_noise::ChromaPyramid::build(&image, true).unwrap();
            let build_v3 = start.elapsed().as_secs_f64() * 1000.0;
            let start = Instant::now();
            let pyramid = color_noise::ChromaPyramid::build_for(
                &image,
                true,
                &color_noise::Tuning::default(),
            )
            .unwrap();
            builds.push(start.elapsed().as_secs_f64() * 1000.0);
            println!(
                "{}",
                json!({"probe":"color_noise_build","size":[width,height],"v3_ms":build_v3,
                    "newest_ms":builds.last()})
            );
            let thresholds = color_noise::Tuning::default().thresholds(
                color_noise::Settings {
                    amount: 0.25,
                    detail: 0.5,
                    smoothness: 0.5,
                },
                &pyramid.noise_for(&color_noise::Tuning::default()),
            );
            let start = Instant::now();
            let _ = pyramid.reconstruct(&thresholds, 0.0, &color_noise::Tuning::default());
            let first = start.elapsed().as_secs_f64() * 1000.0;
            // Slider changes: the per-source preparation is kept.
            let start = Instant::now();
            let _ = pyramid.reconstruct(&thresholds, 0.0, &color_noise::Tuning::default());
            println!(
                "{}",
                json!({"probe":"color_noise_cpu","size":[width,height],"build_ms":builds.last(),
                    "first_reconstruct_ms":first,
                    "reconstruct_ms":start.elapsed().as_secs_f64()*1000.0})
            );
        }
    }
    let (width, height) = (2304, 1536);
    let image = chroma_noise_frame(width, height);
    {
        let gpu = GpuProcessor::new(context.clone(), width, height).unwrap();
        for (version, interactive) in [(2, false), (2, true), (4, false), (4, true)] {
            let texture =
                create_input_texture(&context.device, &context.queue, &image, 256).unwrap();
            let mut source = GpuImageCache {
                texture_view: texture.create_view(&Default::default()),
                texture,
                width,
                height,
                transform_hash: 0,
                chroma: None,
                chroma_gpu: None,
                spare: None,
            };
            let mut times = Vec::new();
            for frame in 0..12 {
                let profile =
                    json!({"colorNoiseReduction": 20 + frame, "colorNoiseVersion": version});
                let mut adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
                let start = Instant::now();
                prepare_color_noise(
                    &context,
                    &gpu,
                    &mut source,
                    Some(&image),
                    &mut adjustments,
                    interactive,
                    None,
                )
                .unwrap();
                context
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                if frame >= 2 {
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                }
            }
            times.sort_by(f64::total_cmp);
            println!(
                "{}",
                json!({"probe":"color_noise_prepare","version":version,"on_gpu":interactive,
                    "median_ms":times[times.len()/2]})
            );
        }
    }
    let processor = ProcessorSlot::new(None);
    let cache = Mutex::new(None);
    // Twice, so the first pass compiles the pipelines and fills the caches.
    for pass in 0..3 {
        for version in 1..=10 {
            for slider in ["exposure", "colorNoiseReduction"] {
                let mut times = Vec::new();
                for frame in 0..24 {
                    let mut profile = json!({"colorNoiseReduction": 25, "lumaNoiseReduction": 15,
                    "sharpness": 40, "colorNoiseVersion": version});
                    profile[slider] = if slider == "exposure" {
                        json!(0.01 * (frame % 7) as f64)
                    } else {
                        json!(20 + frame % 9)
                    };
                    let start = Instant::now();
                    process_and_get_dynamic_image_inner(
                        &context,
                        &processor,
                        &cache,
                        RenderSource::Image {
                            image: &image,
                            transform_hash: 7,
                        },
                        RenderRequest {
                            adjustments: get_all_adjustments_from_json(&profile, true, Some(1)),
                            mask_bitmaps: &[],
                            lut: None,
                            roi: None,
                        },
                        "apply_adjustments",
                        RenderOutputPrecision::EightBit,
                        false,
                        None,
                        None,
                        ReadbackShader::Preview,
                    )
                    .unwrap();
                    if frame >= 4 {
                        times.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                times.sort_by(f64::total_cmp);
                if pass == 0 {
                    continue;
                }
                println!(
                    "{}",
                    json!({"probe":"color_noise_preview","pass":pass,"version":version,"slider":slider,
                    "median_ms":times[times.len()/2],"p90_ms":times[times.len()*9/10]})
                );
            }
        }
    }
}

#[test]
#[ignore = "requires a native GPU"]
fn multiscale_color_noise_rebuilds_alike_on_gpu_and_cpu() {
    let context = context();
    let (width, height) = (1500, 1000);
    let image = chroma_noise_frame(width, height);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    for version in [2, 3] {
        let render = |interactive: bool| {
            let texture =
                create_input_texture(&context.device, &context.queue, &image, 256).unwrap();
            let mut source = GpuImageCache {
                texture_view: texture.create_view(&Default::default()),
                texture,
                width,
                height,
                transform_hash: 0,
                chroma: None,
                chroma_gpu: None,
                spare: None,
            };
            let profile = json!({"colorNoiseReduction": 60, "colorNoiseVersion": version});
            let mut adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
            let views = prepare_color_noise(
                &context,
                &processor,
                &mut source,
                Some(&image),
                &mut adjustments,
                interactive,
                None,
            )
            .unwrap();
            let (pixels, ..) = processor
                .run_with_shader(
                    &source.texture_view,
                    width,
                    height,
                    RenderRequest {
                        adjustments,
                        mask_bitmaps: &[],
                        lut: None,
                        roi: None,
                    },
                    false,
                    RenderOutputPrecision::SixteenBit,
                    ReadbackShader::Export,
                    Some((&views.0, &views.1)),
                )
                .unwrap();
            match pixels {
                RenderedPixels::U16(pixels) => pixels,
                RenderedPixels::U8(_) => unreachable!(),
            }
        };
        let (cpu, gpu) = (render(false), render(true));
        let worst = cpu
            .iter()
            .zip(&gpu)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        let over = cpu
            .iter()
            .zip(&gpu)
            .filter(|(a, b)| (**a as i32 - **b as i32).abs() > 4)
            .count();
        let mut diffs: Vec<i32> = cpu
            .iter()
            .zip(&gpu)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .collect();
        diffs.sort_unstable();
        let pct = |p: f64| diffs[((diffs.len() - 1) as f64 * p) as usize];
        println!(
            "version {version}: GPU vs CPU reconstruction: max difference {worst} of 65535, {over} of {} values above 4; p99 {} p99.9 {} p99.99 {}",
            cpu.len(),
            pct(0.99),
            pct(0.999),
            pct(0.9999)
        );
        // Level 1 rebuilds alike on both (within one f16 step of the packed
        // texture); the aware expansions' steep weights turn that step into rare
        // larger differences at colour edges: 99.9 % within 6 of 65535, at most
        // 85 (a third of an 8-bit step) in this frame.
        assert!(worst <= 128, "max {worst}");
        assert!(pct(0.999) <= 8, "p99.9 {}", pct(0.999));
    }
}

#[test]
#[ignore = "requires a native GPU"]
fn multiscale_color_noise_version_switch_rebuilds_the_prepared_levels() {
    // One source, its pyramid and GPU reconstruction kept across recipes
    // (a display frame after the colour noise version changed by Reset,
    // paste or a preset): each version renders as from a fresh source.
    let context = context();
    let (width, height) = (1500, 1000);
    let image = chroma_noise_frame(width, height);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let source = || {
        let texture = create_input_texture(&context.device, &context.queue, &image, 256).unwrap();
        GpuImageCache {
            texture_view: texture.create_view(&Default::default()),
            texture,
            width,
            height,
            transform_hash: 0,
            chroma: None,
            chroma_gpu: None,
            spare: None,
        }
    };
    let render = |source: &mut GpuImageCache, version: u64| {
        let profile = json!({"colorNoiseReduction": 40, "colorNoiseVersion": version});
        let mut adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        let views = prepare_color_noise(
            &context,
            &processor,
            source,
            Some(&image),
            &mut adjustments,
            true,
            None,
        )
        .unwrap();
        let (pixels, ..) = processor
            .run_with_shader(
                &source.texture_view,
                width,
                height,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
                ReadbackShader::Export,
                Some((&views.0, &views.1)),
            )
            .unwrap();
        match pixels {
            RenderedPixels::U16(pixels) => pixels,
            RenderedPixels::U8(_) => unreachable!(),
        }
    };
    let fresh2 = render(&mut source(), 2);
    let fresh3 = render(&mut source(), 3);
    let fresh4 = render(&mut source(), 4);
    let fresh5 = render(&mut source(), 5);
    let fresh6 = render(&mut source(), 6);
    assert_ne!(fresh2, fresh3);
    assert_ne!(fresh3, fresh4);
    assert_ne!(fresh4, fresh5);
    assert_ne!(fresh5, fresh6);
    // Version 4 builds the pyramid again (linear means) and back.
    let mut kept = source();
    assert!(render(&mut kept, 2) == fresh2);
    assert!(render(&mut kept, 3) == fresh3);
    assert!(render(&mut kept, 4) == fresh4);
    assert!(render(&mut kept, 3) == fresh3);
    assert!(render(&mut kept, 4) == fresh4);
    // Versions 5 and 6 share version 4's levels; only the full-resolution
    // step differs.
    assert!(render(&mut kept, 5) == fresh5);
    assert!(render(&mut kept, 6) == fresh6);
    assert!(render(&mut kept, 4) == fresh4);
    assert!(render(&mut kept, 2) == fresh2);
    assert!(render(&mut kept, 5) == fresh5);
}

#[test]
fn edge_noise_reduction_follows_the_noise_level_and_the_luminance_amount() {
    // The start is the shader's (exports below it use the pipeline without
    // the pass).
    let start: f32 = include_str!("shaders/shader.wgsl")
        .split("const LUMA_NR_EDGE_NOISE: vec2<f32> = vec2<f32>(")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(start, LUMA_NR_EDGE_NOISE_START);
    let with = |recipe: serde_json::Value, noise: f32| {
        let mut adjustments = get_all_adjustments_from_json(&recipe, true, None);
        adjustments.global.noise_level = noise;
        edge_noise_reduction(&adjustments)
    };
    let luma = json!({"lumaNoiseReduction": 15});
    assert!(!with(luma.clone(), 0.0));
    assert!(!with(luma.clone(), LUMA_NR_EDGE_NOISE_START));
    assert!(with(luma, 0.02));
    assert!(!with(json!({"colorNoiseReduction": 25}), 0.02));
    let local = json!({"masks": [local_mask("m", json!({"lumaNoiseReduction": 30}))]});
    assert!(with(local, 0.02));
}

/// A dark scene with bright discs and point lights at high-ISO noise,
/// developed as a RAW (Bayer mosaic, demosaic, decode-time preprocessing),
/// and the noise-free development of the same mosaic.
struct SpeckleScene {
    noisy: Vec<[f32; 3]>,
    clean: Vec<[f32; 3]>,
    /// Dark pixels 3–16 px from a bright object.
    near: Vec<bool>,
    /// Bright pixels on the objects' outline.
    edge: Vec<bool>,
}

fn speckle_scene(size: usize) -> SpeckleScene {
    let mut scene = vec![[0.012f32, 0.012, 0.011]; size * size];
    let mut bright = vec![false; size * size];
    let cell = 128;
    for cy in 0..size / cell {
        for cx in 0..size / cell {
            let (ox, oy) = ((cx * cell + cell / 2) as i32, (cy * cell + cell / 2) as i32);
            if (cx + cy) % 2 == 0 {
                let r = 6 + ((cx * 7 + cy * 3) % 14) as i32;
                for dy in -r..=r {
                    for dx in -r..=r {
                        if dx * dx + dy * dy <= r * r {
                            let i = (oy + dy) as usize * size + (ox + dx) as usize;
                            scene[i] = [0.7, 0.6, 0.45];
                            bright[i] = true;
                        }
                    }
                }
            } else {
                for k in 0..5i32 {
                    let (x, y) = (ox - 40 + k * 19, oy - 30 + (k * 23) % 60);
                    for (dx, dy, w) in [
                        (0, 0, 1.0),
                        (1, 0, 0.4),
                        (-1, 0, 0.4),
                        (0, 1, 0.4),
                        (0, -1, 0.4),
                    ] {
                        let i = (y + dy) as usize * size + (x + dx) as usize;
                        scene[i] = [0.9 * w; 3];
                        bright[i] = true;
                    }
                }
            }
        }
    }
    // Dark pixels 3..16 px from the bright objects.
    let mut near = vec![false; size * size];
    for y in 16..size - 16 {
        for x in 16..size - 16 {
            if bright[y * size + x] {
                continue;
            }
            let mut d2 = i32::MAX;
            for dy in -16i32..=16 {
                for dx in -16i32..=16 {
                    if bright[(y as i32 + dy) as usize * size + (x as i32 + dx) as usize] {
                        d2 = d2.min(dx * dx + dy * dy);
                    }
                }
            }
            near[y * size + x] = (9..=256).contains(&d2);
        }
    }
    let pattern = rawtonic_rawdecode::demosaic::CfaPattern::new(2, 2, vec![0, 1, 1, 2]).unwrap();
    let develop = |noise: bool| {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut gaussian = || {
            let mut uniform = || {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((state >> 40) as f32 + 0.5) / (1u64 << 24) as f32
            };
            let (u1, u2) = (uniform(), uniform());
            (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
        };
        // ISO 12800-like: 300 electrons at white, 3 electrons read noise.
        let full = 300.0f32;
        let mosaic: Vec<f32> = (0..size * size)
            .map(|i| {
                let c = pattern.color((i / size) as isize, (i % size) as isize);
                let v = scene[i][c];
                if noise {
                    let e = v * full;
                    ((e + (e + 9.0).sqrt() * gaussian()) / full).min(1.0)
                } else {
                    v
                }
            })
            .collect();
        let rgb = rawtonic_rawdecode::demosaic::demosaic(&mosaic, size, size, &pattern);
        let mut image = image::DynamicImage::ImageRgb32F(
            image::Rgb32FImage::from_raw(size as u32, size as u32, rgb.into_flattened()).unwrap(),
        );
        let defaults = rawtonic_engine::image_loader::BaseImageOptions::default();
        rawtonic_engine::image_processing::remove_raw_artifacts_and_enhance(
            &mut image,
            defaults.color_nr_amount,
            defaults.sharpening_amount,
        );
        image
            .into_rgb32f()
            .pixels()
            .map(|p| p.0)
            .collect::<Vec<[f32; 3]>>()
    };
    let edge: Vec<bool> = (0..size * size)
        .map(|i| {
            let (x, y) = (i % size, i / size);
            x > 0
                && y > 0
                && x + 1 < size
                && y + 1 < size
                && bright[i]
                && [i - 1, i + 1, i - size, i + size]
                    .iter()
                    .any(|&j| !bright[j])
        })
        .collect();
    SpeckleScene {
        noisy: develop(true),
        clean: develop(false),
        near,
        edge,
    }
}

fn linear_texture(
    context: &GpuContext,
    rgb: &[[f32; 3]],
    width: u32,
    height: u32,
) -> wgpu::TextureView {
    let data: Vec<f16> = rgb
        .iter()
        .flat_map(|p| [p[0], p[1], p[2], 1.0].map(f16::from_f32))
        .collect();
    context
        .device
        .create_texture_with_data(
            &context.queue,
            &wgpu::TextureDescriptor {
                label: Some("Speckle regression input"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            bytemuck::cast_slice(&data),
        )
        .create_view(&Default::default())
}

#[test]
#[ignore = "requires a native GPU; speckles near bright objects at high ISO"]
#[allow(clippy::excessive_precision)] // Oklab constants as published.
fn high_iso_noise_reduction_leaves_no_speckles_near_bright_objects() {
    const SIZE: usize = 1024;
    let context = context();
    let processor = GpuProcessor::new(context.clone(), SIZE as u32, SIZE as u32).unwrap();
    let SpeckleScene {
        noisy,
        clean,
        near,
        edge,
    } = speckle_scene(SIZE);
    let level = rawtonic_rawdecode::noise_level::measure(&image::DynamicImage::ImageRgb32F(
        image::Rgb32FImage::from_raw(SIZE as u32, SIZE as u32, noisy.as_flattened().to_vec())
            .unwrap(),
    ))
    .unwrap();
    assert!(
        level > LUMA_NR_EDGE_NOISE_START * 2.0,
        "noise level {level}"
    );
    let render = |rgb: &[[f32; 3]], recipe: serde_json::Value, noise: f32| -> Vec<f32> {
        let view = linear_texture(&context, rgb, SIZE as u32, SIZE as u32);
        let mut adjustments = get_all_adjustments_from_json(&recipe, true, None);
        adjustments.global.noise_level = noise;
        let (pixels, ..) = processor
            .run(
                &view,
                SIZE as u32,
                SIZE as u32,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        let RenderedPixels::U16(words) = pixels else {
            unreachable!()
        };
        let channels = words.len() / (SIZE * SIZE);
        // Oklab lightness ×100 of the sRGB output.
        words
            .chunks(channels)
            .map(|p| {
                let lin = |v: u16| {
                    let c = v as f32 / 65535.0;
                    if c <= 0.04045 {
                        c / 12.92
                    } else {
                        ((c + 0.055) / 1.055).powf(2.4)
                    }
                };
                let [r, g, b] = [lin(p[0]), lin(p[1]), lin(p[2])];
                let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
                let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
                let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
                100.0 * (0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s)
            })
            .collect()
    };
    let defaults = json!({"sharpness": 40, "lumaNoiseReduction": 15, "colorNoiseReduction": 25});
    let truth = render(
        &clean,
        json!({"sharpness": 0, "lumaNoiseReduction": 0, "colorNoiseReduction": 0}),
        0.0,
    );
    let fixed = render(&noisy, defaults.clone(), level);
    processor
        .force_plain_noise_reduction
        .store(true, Ordering::Relaxed);
    let plain = render(&noisy, defaults, level);
    processor
        .force_plain_noise_reduction
        .store(false, Ordering::Relaxed);
    // Speckles: near-zone pixels whose error differs from the median error
    // of their 5 x 5 neighbourhood by more than 8 (Oklab ×100).
    let speckles = |out: &[f32]| {
        let error: Vec<f32> = out.iter().zip(&truth).map(|(o, t)| o - t).collect();
        let (mut count, mut total) = (0usize, 0usize);
        let mut window = [0.0f32; 25];
        for y in 2..SIZE - 2 {
            for x in 2..SIZE - 2 {
                if !near[y * SIZE + x] {
                    continue;
                }
                for (k, w) in window.iter_mut().enumerate() {
                    *w = error[(y + k / 5 - 2) * SIZE + x + k % 5 - 2];
                }
                window.sort_unstable_by(f32::total_cmp);
                if (error[y * SIZE + x] - window[12]).abs() > 8.0 {
                    count += 1;
                }
                total += 1;
            }
        }
        1000.0 * count as f64 / total as f64
    };
    // Edges of the bright objects: mean lightness step to the outside.
    let sharpness = |out: &[f32]| {
        let (mut step, mut n) = (0.0f64, 0usize);
        for i in 0..SIZE * SIZE {
            if edge[i] {
                let outside = [i - 1, i + 1, i - SIZE, i + SIZE]
                    .iter()
                    .map(|&j| out[j])
                    .fold(f32::INFINITY, f32::min);
                step += f64::from(out[i] - outside);
                n += 1;
            }
        }
        step / n as f64
    };
    let (s_plain, s_fixed) = (speckles(&plain), speckles(&fixed));
    let (e_plain, e_fixed, e_truth) = (sharpness(&plain), sharpness(&fixed), sharpness(&truth));
    println!(
        "speckles per 1000 near bright objects: {s_plain:.1} -> {s_fixed:.1}; edge step {e_plain:.2} -> {e_fixed:.2} (noise-free {e_truth:.2})"
    );
    assert!(
        s_plain > 20.0,
        "the scene shows no speckles to remove: {s_plain:.1}"
    );
    assert!(
        s_fixed < 0.25 * s_plain,
        "speckles {s_plain:.1} -> {s_fixed:.1}"
    );
    assert!(
        e_fixed > 0.93 * e_plain,
        "edges softened: {e_plain:.2} -> {e_fixed:.2}"
    );
}

#[test]
#[ignore = "requires a native GPU; isolated luminance specks at high ISO"]
#[allow(clippy::excessive_precision)] // Oklab constants as published.
fn luma_noise_version_2_removes_isolated_specks_and_keeps_point_lights() {
    const SIZE: usize = 1024;
    let context = context();
    let processor = GpuProcessor::new(context.clone(), SIZE as u32, SIZE as u32).unwrap();
    let SpeckleScene {
        noisy, clean, edge, ..
    } = speckle_scene(SIZE);
    let level = rawtonic_rawdecode::noise_level::measure(&image::DynamicImage::ImageRgb32F(
        image::Rgb32FImage::from_raw(SIZE as u32, SIZE as u32, noisy.as_flattened().to_vec())
            .unwrap(),
    ))
    .unwrap();
    let render = |rgb: &[[f32; 3]], recipe: serde_json::Value, noise: f32| -> Vec<u16> {
        let view = linear_texture(&context, rgb, SIZE as u32, SIZE as u32);
        let mut adjustments = get_all_adjustments_from_json(&recipe, true, None);
        adjustments.global.noise_level = noise;
        let (pixels, ..) = processor
            .run(
                &view,
                SIZE as u32,
                SIZE as u32,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        let RenderedPixels::U16(words) = pixels else {
            unreachable!()
        };
        words
    };
    // Oklab lightness ×100 of the sRGB output.
    let lightness = |words: &[u16]| -> Vec<f32> {
        let channels = words.len() / (SIZE * SIZE);
        words
            .chunks(channels)
            .map(|p| {
                let lin = |v: u16| {
                    let c = v as f32 / 65535.0;
                    if c <= 0.04045 {
                        c / 12.92
                    } else {
                        ((c + 0.055) / 1.055).powf(2.4)
                    }
                };
                let [r, g, b] = [lin(p[0]), lin(p[1]), lin(p[2])];
                let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
                let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
                let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
                100.0 * (0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s)
            })
            .collect()
    };
    let recipe = |version: u64| {
        json!({"sharpness": 40, "lumaNoiseReduction": 15, "colorNoiseReduction": 25,
               "lumaNoiseVersion": version})
    };
    let truth = lightness(&render(
        &clean,
        json!({"sharpness": 0, "lumaNoiseReduction": 0, "colorNoiseReduction": 0}),
        0.0,
    ));
    let v1 = lightness(&render(&noisy, recipe(1), level));
    let v2 = lightness(&render(&noisy, recipe(2), level));
    // Isolated specks: pixels whose error differs from the median error of
    // their 5 x 5 neighbourhood by more than 4 (Oklab ×100) while that
    // neighbourhood's median difference stays below 4/3.
    let specks = |out: &[f32]| {
        let error: Vec<f32> = out.iter().zip(&truth).map(|(o, t)| o - t).collect();
        let mut deviation = vec![0.0f32; SIZE * SIZE];
        let mut window = [0.0f32; 25];
        for y in 2..SIZE - 2 {
            for x in 2..SIZE - 2 {
                for (k, w) in window.iter_mut().enumerate() {
                    *w = error[(y + k / 5 - 2) * SIZE + x + k % 5 - 2];
                }
                window.sort_unstable_by(f32::total_cmp);
                deviation[y * SIZE + x] = (error[y * SIZE + x] - window[12]).abs();
            }
        }
        let mut count = 0usize;
        for y in 4..SIZE - 4 {
            for x in 4..SIZE - 4 {
                for (k, w) in window.iter_mut().enumerate() {
                    *w = deviation[(y + k / 5 - 2) * SIZE + x + k % 5 - 2];
                }
                window.sort_unstable_by(f32::total_cmp);
                if deviation[y * SIZE + x] > 4.0 && window[12] < 4.0 / 3.0 {
                    count += 1;
                }
            }
        }
        1e6 * count as f64 / (SIZE * SIZE) as f64
    };
    let rms = |out: &[f32]| {
        let sum: f64 = out
            .iter()
            .zip(&truth)
            .map(|(o, t)| f64::from(o - t).powi(2))
            .sum();
        (sum / (SIZE * SIZE) as f64).sqrt()
    };
    // Edges of the bright objects (discs and point lights): mean lightness
    // step to the outside.
    let sharpness = |out: &[f32]| {
        let (mut step, mut n) = (0.0f64, 0usize);
        for i in 0..SIZE * SIZE {
            if edge[i] {
                let outside = [i - 1, i + 1, i - SIZE, i + SIZE]
                    .iter()
                    .map(|&j| out[j])
                    .fold(f32::INFINITY, f32::min);
                step += f64::from(out[i] - outside);
                n += 1;
            }
        }
        step / n as f64
    };
    // The point lights' peaks (single bright pixels with a faint rim).
    let peaks: Vec<usize> = (SIZE + 1..SIZE * SIZE - SIZE - 1)
        .filter(|&i| {
            truth[i] > 80.0
                && [i - 1, i + 1, i - SIZE, i + SIZE]
                    .iter()
                    .all(|&j| truth[j] < truth[i] - 10.0)
        })
        .collect();
    let peak =
        |out: &[f32]| peaks.iter().map(|&i| f64::from(out[i])).sum::<f64>() / peaks.len() as f64;
    let (s1, s2) = (specks(&v1), specks(&v2));
    println!(
        "noise level {level:.4}; isolated specks per Mpx {s1:.0} -> {s2:.0}; error RMS {:.3} -> {:.3}; \
         edge step {:.2} -> {:.2}; {} point-light peaks {:.2} -> {:.2}",
        rms(&v1),
        rms(&v2),
        sharpness(&v1),
        sharpness(&v2),
        peaks.len(),
        peak(&v1),
        peak(&v2)
    );
    assert!(s2 < 0.8 * s1, "isolated specks {s1:.0} -> {s2:.0}");
    assert!(
        rms(&v2) <= rms(&v1),
        "error {:.3} -> {:.3}",
        rms(&v1),
        rms(&v2)
    );
    assert!(
        sharpness(&v2) > 0.99 * sharpness(&v1),
        "edges softened: {:.2} -> {:.2}",
        sharpness(&v1),
        sharpness(&v2)
    );
    assert!(peaks.len() > 50, "{} point lights", peaks.len());
    assert!(
        peak(&v2) > peak(&v1) - 0.5,
        "point lights dimmed: {:.2} -> {:.2}",
        peak(&v1),
        peak(&v2)
    );
    // Clean photos (measured noise below LUMA_SPECK_NOISE) and recipes
    // without the key render exactly as before.
    let quiet = 0.0015;
    assert_eq!(
        render(&noisy, recipe(2), quiet),
        render(&noisy, recipe(1), quiet)
    );
    let mut keyless = recipe(1);
    keyless.as_object_mut().unwrap().remove("lumaNoiseVersion");
    assert_eq!(
        render(&noisy, keyless, level),
        render(&noisy, recipe(1), level)
    );
}

/// A byte mask as the renderer takes it.
fn masked(image: GrayImage) -> Arc<MaskBitmap> {
    Arc::new(image.into())
}

/// M6 of the local-adjustments plan (notes: plans/local-adjustments-g29-g31.md):
/// what one preview job costs natively with many layers, stage by stage, as
/// `editor_preview::process_preview_job` does it: the IPC adjustments JSON is
/// parsed, the layers are read from it, each layer's bitmap comes from a
/// shared `MaskBitmapCache`, and the frame renders on the GPU. Scenarios:
/// a slider on the first and on the last layer (one value changes per move)
/// and a live brush stroke on the top layer (one point per move). IPC
/// transport, the webview and presentation are excluded.
#[test]
#[ignore = "native GPU timing probe; set RAWTONIC_M6_RAW and RAWTONIC_M6_RECIPES"]
fn many_layer_frame_probe() {
    use rawtonic_engine::mask_generation::{
        MaskBitmapCache, cached_or_generated_mask, prepare_parametric_mask_source,
    };
    let (Ok(raw), Ok(recipes)) = (
        std::env::var("RAWTONIC_M6_RAW"),
        std::env::var("RAWTONIC_M6_RECIPES"),
    ) else {
        println!("SKIP many_layer_frame_probe: RAWTONIC_M6_RAW / RAWTONIC_M6_RECIPES not set");
        return;
    };
    let env = |name: &str, default: usize| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let long_side = env("RAWTONIC_M6_DIM", 2304) as f32;
    let frames = env("RAWTONIC_M6_FRAMES", 40);
    let bytes = std::fs::read(&raw).unwrap();
    let full = rawtonic_engine::image_loader::load_base_image(
        &bytes,
        &raw,
        false,
        &Default::default(),
        None,
    )
    .unwrap();
    let (full_w, full_h) = full.dimensions();
    let scale = long_side / full_w.max(full_h) as f32;
    let (width, height) = (
        (full_w as f32 * scale).round() as u32,
        (full_h as f32 * scale).round() as u32,
    );
    let preview = full.resize_exact(width, height, image::imageops::FilterType::Triangle);
    let context = context();
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let texture = create_input_texture(
        &context.device,
        &context.queue,
        &preview,
        INPUT_UPLOAD_BAND_ROWS,
    )
    .unwrap();
    let view = texture.create_view(&Default::default());
    let median = |mut v: Vec<f64>, q: f64| {
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * q).round() as usize]
    };
    for recipe_path in recipes.split(',') {
        let base: serde_json::Value =
            serde_json::from_slice(&std::fs::read(recipe_path).unwrap()).unwrap();
        let layers = base["masks"].as_array().unwrap().len();
        let source = Arc::new(prepare_parametric_mask_source(&full, true, 1.0, &base).unwrap());
        let geometry =
            PhotoGeometry::from_adjustments(&base).with_photo_size(full_w as f64, full_h as f64);
        if std::env::var("RAWTONIC_M6_EXPORT_MASKS").is_ok() {
            // What an export's mask stage spends per layer at full resolution.
            let definitions: Vec<MaskDefinition> =
                serde_json::from_value(base["masks"].clone()).unwrap();
            let mut total = 0.0;
            for def in &definitions {
                let t = Instant::now();
                let mask = generate_layer_mask(
                    def,
                    full_w,
                    full_h,
                    1.0,
                    (0.0, 0.0),
                    &geometry,
                    Some(&source),
                );
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                total += ms;
                println!(
                    "{}",
                    json!({"probe":"m6_export_mask","recipe":recipe_path,"layer":def.id,
                    "kind":def.sub_masks.first().map(|s| s.mask_type.clone()),
                    "refine":def.refine_edge,"wide":mask.as_ref().is_some_and(|m| m.wide().is_some()),
                    "ms":ms})
                );
            }
            println!(
                "{}",
                json!({"probe":"m6_export_masks","recipe":recipe_path,"layers":layers,"ms":total})
            );
            continue;
        }
        let warm = get_all_adjustments_from_json(&base, true, Some(1));
        prepare_preview_pipeline(&processor, preview_features(&warm));
        prepare_layer_sum_pipeline(&processor, preview_features(&warm));
        processor.layer_sums_enabled.store(true, Ordering::Relaxed);
        // A live stroke on the top layer: a new brush layer (Auto Mask v3, as
        // the app paints) in place of the last one, one point per move.
        let mut brush_base = base.clone();
        {
            let masks = brush_base["masks"].as_array_mut().unwrap();
            masks.pop();
            masks.push(
                json!({"id":"live","name":"live","visible":true,"invert":false,
                "opacity":100,"maskVersion":3,"adjustments":{"exposure":0.5},
                "subMasks":[{"id":"live-s","type":"brush","visible":true,"invert":false,
                "opacity":100,"mode":"additive","parameters":{"lines":[{"tool":"brush",
                "brushSize":0.05 * full_w as f64,"feather":0.5,"opacity":100,"flow":100,
                "hardness":50,"autoMask":true,"autoMaskVersion":3,"points":[]}]}}]}),
            );
        }
        type Scenario<'a> = (&'a str, Box<dyn Fn(usize) -> serde_json::Value + 'a>);
        // Layer solo (src/utils/layerSolo.ts) hides the other layers in the
        // display: stepping the solo through the layers, solo on/off, and for
        // comparison one layer's eye toggled (Lightroom's and Capture One's
        // one click per layer).
        let solo = |v: &mut serde_json::Value, shown: Option<usize>| {
            for (i, m) in v["masks"].as_array_mut().unwrap().iter_mut().enumerate() {
                m["visible"] = json!(shown.is_none_or(|s| s == i));
            }
        };
        let scenarios: [Scenario; 7] = [
            (
                "slider_global",
                Box::new(|f| {
                    let mut v = base.clone();
                    v["exposure"] = json!(0.01 * f as f64);
                    v
                }),
            ),
            (
                "slider_first_layer",
                Box::new(|f| {
                    let mut v = base.clone();
                    v["masks"][0]["adjustments"]["exposure"] = json!(0.01 * f as f64);
                    v
                }),
            ),
            (
                "slider_last_layer",
                Box::new(|f| {
                    let mut v = base.clone();
                    v["masks"][layers - 1]["adjustments"]["exposure"] = json!(0.01 * f as f64);
                    v
                }),
            ),
            (
                "brush_top_layer",
                Box::new(|f| {
                    let mut v = brush_base.clone();
                    let points: Vec<_> = (0..=f)
                        .map(|k| {
                            json!({"x": full_w as f64 * (0.3 + 0.4 * k as f64 / frames as f64),
                                   "y": full_h as f64 * (0.5 + 0.05 * (k as f64 * 0.3).sin())})
                        })
                        .collect();
                    v["masks"][layers - 1]["subMasks"][0]["parameters"]["lines"][0]["points"] =
                        json!(points);
                    v
                }),
            ),
            (
                "solo_step",
                Box::new(|f| {
                    // The first (warm-up) frame shows every layer, as the
                    // editor does before a solo, so their masks are cached.
                    let mut v = base.clone();
                    solo(&mut v, (f > 0).then(|| f % layers));
                    v
                }),
            ),
            (
                "solo_toggle",
                Box::new(|f| {
                    let mut v = base.clone();
                    solo(&mut v, (f % 2 == 1).then_some(layers - 1));
                    v
                }),
            ),
            (
                "eye_toggle",
                Box::new(|f| {
                    let mut v = base.clone();
                    v["masks"][layers - 1]["visible"] = json!(f % 2 == 0);
                    v
                }),
            ),
        ];
        let only = std::env::var("RAWTONIC_M6_ONLY").ok();
        for (name, make) in &scenarios {
            if only
                .as_ref()
                .is_some_and(|only| !only.split(',').any(|o| o == *name))
            {
                continue;
            }
            // One cache per scenario, warmed by a first (cold) move.
            let cache = std::sync::Mutex::new(MaskBitmapCache::new());
            let mut previous: Vec<Arc<MaskBitmap>> = Vec::new();
            let mut rows: Vec<[f64; 6]> = Vec::new();
            let mut regenerated_sum = 0;
            // Slider moves as the editor sends them: brush strokes once with a
            // token, then by reference (mask_parameters). Live strokes send all.
            let references =
                std::env::var("RAWTONIC_M6_REFS").is_ok() && name.starts_with("slider");
            for f in 0..=frames {
                let mut request = make(f);
                if references {
                    for sub in request["masks"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .flat_map(|m| m["subMasks"].as_array_mut().unwrap().iter_mut())
                        .filter(|sub| sub["type"] == "brush")
                    {
                        let token = format!("{}#{recipe_path}", sub["id"].as_str().unwrap());
                        if f == 0 {
                            sub["paramsToken"] = json!(token);
                        } else {
                            sub["parameters"] = json!({"$paramsRef": token});
                        }
                    }
                }
                let text = serde_json::to_string(&request).unwrap();
                let started = Instant::now();
                let mut adjustments: serde_json::Value = serde_json::from_str(&text).unwrap();
                rawtonic_engine::mask_parameters::hydrate(&mut adjustments).unwrap();
                let parse_ms = started.elapsed().as_secs_f64() * 1000.0;
                let t = Instant::now();
                let definitions =
                    rawtonic_engine::image_processing::mask_definitions_from_json(&adjustments);
                let read_ms = t.elapsed().as_secs_f64() * 1000.0;
                let t = Instant::now();
                let masks: Vec<Arc<MaskBitmap>> = definitions
                    .iter()
                    .filter_map(|def| {
                        cached_or_generated_mask(
                            &cache,
                            def,
                            width,
                            height,
                            scale,
                            (0.0, 0.0),
                            &geometry,
                            Some(&source),
                        )
                        .unwrap()
                    })
                    .collect();
                let mask_ms = t.elapsed().as_secs_f64() * 1000.0;
                let regenerated = masks
                    .iter()
                    .enumerate()
                    .filter(|(i, m)| previous.get(*i).is_none_or(|p| !Arc::ptr_eq(p, m)))
                    .count();
                let t = Instant::now();
                let all = rawtonic_engine::image_processing::get_all_adjustments_with_masks(
                    &adjustments,
                    true,
                    Some(1),
                    &definitions,
                );
                let adjust_ms = t.elapsed().as_secs_f64() * 1000.0;
                let t = Instant::now();
                processor
                    .run(
                        &view,
                        width,
                        height,
                        RenderRequest {
                            adjustments: all,
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
                let gpu_ms = t.elapsed().as_secs_f64() * 1000.0;
                let total_ms = started.elapsed().as_secs_f64() * 1000.0;
                previous = masks;
                if f == 0 {
                    println!(
                        "{}",
                        json!({"probe":"m6","recipe":recipe_path,"layers":layers,"scenario":name,
                        "move":"first","total_ms":total_ms,"mask_ms":mask_ms,"gpu_ms":gpu_ms})
                    );
                    continue;
                }
                regenerated_sum += regenerated;
                rows.push([parse_ms, read_ms, mask_ms, adjust_ms, gpu_ms, total_ms]);
                if f == frames {
                    let column = |i: usize| rows.iter().map(|r| r[i]).collect::<Vec<_>>();
                    let stats = |i: usize| json!([median(column(i), 0.5), median(column(i), 0.95)]);
                    println!(
                        "{}",
                        json!({"probe":"m6","recipe":recipe_path,"layers":layers,"scenario":name,
                        "width":width,"height":height,"moves":rows.len(),
                        "layers_regenerated_per_move":regenerated_sum as f64 / rows.len() as f64,
                        "parse_ms":stats(0),"read_layers_ms":stats(1),"mask_ms":stats(2),
                        "adjustments_ms":stats(3),"gpu_ms":stats(4),"total_ms":stats(5),
                        "recipe_bytes":text.len()})
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "requires a native GPU; a 16-bit export compiles no 8-bit pipeline"]
fn sixteen_bit_export_compiles_no_eight_bit_pipeline() {
    let context = context();
    let (width, height) = (97, 65);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input(&context, width, height, 5);
    // A noisy photo with luminance noise reduction: its exports use the
    // general pipelines with the edge-preserving pass.
    let profile = json!({"sharpness": 40, "colorNoiseReduction": 25, "lumaNoiseReduction": 30});
    let mut adjustments = get_all_adjustments_from_json(&profile, true, None);
    adjustments.global.noise_level = 0.02;
    processor
        .run(
            &view,
            width,
            height,
            RenderRequest {
                adjustments,
                mask_bitmaps: &[],
                lut: None,
                roi: None,
            },
            false,
            RenderOutputPrecision::SixteenBit,
        )
        .unwrap();
    assert!(processor.edge_pipelines[1].get().is_some());
    assert!(
        processor.edge_pipelines[0].get().is_none(),
        "the 16-bit export compiled the 8-bit general pipeline"
    );
}

/// Before its display variant is ready a frame may render with the general
/// shader only when that has the noise passes the recipe needs: the plain
/// one (no edge-preserving pass) drew a noisy photo's first frame visibly
/// different from the next.
#[test]
#[ignore = "requires a native GPU; the first display frame of a noisy photo before its variant"]
fn general_display_frame_has_the_noise_passes_of_the_variants() {
    const SIZE: usize = 512;
    let context = context();
    let (width, height) = (SIZE as u32, SIZE as u32);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let noisy = speckle_scene(SIZE).noisy;
    let level = rawtonic_rawdecode::noise_level::measure(&image::DynamicImage::ImageRgb32F(
        image::Rgb32FImage::from_raw(width, height, noisy.as_flattened().to_vec()).unwrap(),
    ))
    .unwrap();
    let view = linear_texture(&context, &noisy, width, height);
    // The RAW detail defaults.
    let profile = json!({"sharpness": 40, "lumaNoiseReduction": 15, "colorNoiseReduction": 25});
    let mut adjustments = get_all_adjustments_from_json(&profile, true, None);
    adjustments.global.noise_level = level;
    assert!(edge_noise_reduction(&adjustments));
    let quiet = get_all_adjustments_from_json(&json!({"exposure": 0.5}), true, None);
    processor.main_pipeline.wait().unwrap();
    // Nothing with the edge-preserving pass is compiled yet: no general
    // pipeline may stand in; a quiet recipe takes the plain one.
    assert!(processor.compiled_general_pipeline(&adjustments).is_none());
    assert!(std::ptr::eq(
        processor
            .compiled_general_pipeline(&quiet)
            .unwrap()
            .unwrap(),
        processor.main_pipeline.wait().unwrap()
    ));
    prepare_preview_pipeline(&processor, preview_features(&adjustments));
    // Right after start the detail cache pass may not be compiled either:
    // the frame computes the detail stage in the main pipeline.
    processor.force_inline_detail.store(true, Ordering::Relaxed);
    let render = |general: bool| {
        processor
            .force_general_display
            .store(general, Ordering::Relaxed);
        processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                true,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        processor
            .force_general_display
            .store(false, Ordering::Relaxed);
        read_texture_data_roi(
            &context.device,
            &context.queue,
            &processor.test_working_texture(),
            wgpu::Origin3d::ZERO,
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            4,
        )
        .unwrap()
    };
    let variant = render(false);
    // An export compiled the general pipeline with the pass.
    let edge = processor
        .general_pipeline(&adjustments, MainOutput::EightBit)
        .unwrap();
    assert!(std::ptr::eq(
        processor
            .compiled_general_pipeline(&adjustments)
            .unwrap()
            .unwrap(),
        edge
    ));
    let general = render(true);
    let max_delta = variant
        .iter()
        .zip(&general)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    // The same tolerance as between display variants and the general shader.
    assert!(max_delta <= 1, "general first frame delta {max_delta}");
}

/// The portable grain (`grainVersion` 2) evaluated on the CPU from its
/// definition: the integer lattice and PCG hash of `portable_noise`, the
/// layers of `portable_grain_noise`, interpolation in f32.
mod portable_grain_reference {
    const ONE: u32 = 4096;

    fn pcg3d(v: [u32; 3]) -> [u32; 3] {
        let mut v = v.map(|c| c.wrapping_mul(1664525).wrapping_add(1013904223));
        for round in 0..2 {
            v[0] = v[0].wrapping_add(v[1].wrapping_mul(v[2]));
            v[1] = v[1].wrapping_add(v[2].wrapping_mul(v[0]));
            v[2] = v[2].wrapping_add(v[0].wrapping_mul(v[1]));
            if round == 0 {
                v = v.map(|c| c ^ (c >> 16));
            }
        }
        v
    }

    fn gradient(x: u32, y: u32, seed: u32) -> [f32; 2] {
        let h = pcg3d([x, y, seed]);
        [0, 1].map(|i| (h[i] >> 8) as f32 / 8388608.0 - 1.0)
    }

    /// Lattice cells per pixel, 12 fractional bits, for a grain size and
    /// image dimensions.
    pub fn cells_per_px(size: f32, width: u32, height: u32) -> u32 {
        // WGSL's round() rounds halfway cases to even.
        let size_q = (size.clamp(0.1, 4.0) * ONE as f32).round_ties_even() as u64;
        let den = size_q * u64::from(width.min(height).max(108));
        ((1080u64 << 24) + den / 2).div_euclid(den) as u32
    }

    fn noise(x: u32, y: u32, cells: u32, k: u32, offset: [u32; 2], seed: u32) -> f32 {
        let step = (u64::from(cells) * u64::from(k) + u64::from(ONE / 2)) >> 12;
        let axis = |c: u32, o: u32| {
            let pos = u64::from(c) * step + u64::from(o);
            (
                (pos >> 12) as u32,
                (pos & u64::from(ONE - 1)) as f32 / ONE as f32,
            )
        };
        let ((ix, fx), (iy, fy)) = (axis(x, offset[0]), axis(y, offset[1]));
        let fade = |f: f32| f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
        let corner = |dx: u32, dy: u32| {
            let g = gradient(ix.wrapping_add(dx), iy.wrapping_add(dy), seed);
            g[0] * (fx - dx as f32) + g[1] * (fy - dy as f32)
        };
        let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let bottom = mix(corner(0, 0), corner(1, 0), fade(fx));
        let top = mix(corner(0, 1), corner(1, 1), fade(fx));
        mix(bottom, top, fade(fy))
    }

    /// `portable_grain_noise` for a structure index.
    pub fn grain(structure: u32, x: u32, y: u32, cells: u32, roughness: f32) -> f32 {
        let n = |k: u32, ox: u32, oy: u32, seed: u32| noise(x, y, cells, k, [ox, oy], seed);
        match structure {
            1 => {
                let (a, c) = (n(3277, 12698, 31539, 2), n(1229, 69632, 36864, 3));
                let w = 0.35 + 0.65 * roughness;
                0.71 * (a + w * c) / (1.0 + w * w).sqrt()
            }
            2 => {
                let fine = n(ONE, 0, 0, 4);
                let c = 0.6 * n(1638, 167936, 12288, 5) + 0.4 * n(532, 53248, 118784, 6);
                let k = 1.6 * roughness;
                0.71 * (fine * (1.0 + 0.8 * k * c) + 0.9 * k * c) / (1.0 + 0.43 * k * k).sqrt()
            }
            3 => {
                let base = n(3686, 6963, 18022, 7);
                let s = n(2867, 94208, 126976, 8);
                let c = n(ONE / 4, 28672, 8192, 9);
                let speck = (s - 0.12).max(0.0) * (1.6 + 6.0 * roughness * (c + 0.1).max(0.0));
                0.575 * (base + speck - (0.0418 + 0.0206 * roughness))
            }
            4 => {
                let v = n(2253, 9421, 33178, 10)
                    + 0.45 * n(ONE, 27034, 7782, 11)
                    + (0.5 + 0.8 * roughness) * n(819, 77824, 20480, 12);
                0.524 * (1.5 * v).tanh()
            }
            _ => {
                let base = n(ONE, 0, 0, 0);
                base + (n(2458, 21299, 5325, 1) - base) * roughness
            }
        }
    }
}

#[test]
#[ignore = "requires a native GPU; compares the portable grain with its CPU evaluation"]
fn portable_grain_is_the_same_on_every_gpu() {
    // A flat mid-grey: the grain is the only change, with one strength
    // everywhere, so render − render without grain = gain × noise.
    let (width, height) = (320u32, 192u32);
    let grey = f16::from_f32(0.18);
    let data: Vec<f16> = (0..width * height)
        .flat_map(|_| [grey, grey, grey, f16::ONE])
        .collect();
    let context = context();
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let view = input_from(&context, width, height, &data);
    let render = |profile: serde_json::Value| {
        let adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        let (pixels, ..) = processor
            .run(
                &view,
                width,
                height,
                RenderRequest {
                    adjustments,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::SixteenBit,
            )
            .unwrap();
        match pixels {
            RenderedPixels::U16(pixels) => pixels,
            _ => unreachable!(),
        }
    };
    let flat = render(json!({}));
    for (structure, name) in ["even", "soft", "clumped", "speckled", "gritty"]
        .into_iter()
        .enumerate()
    {
        // Size 2048.5 * 50 / 4096 (exact): a halfway case of the rounding.
        for (size, roughness) in [
            (25.0f32, 50.0f32),
            (80.0, 0.0),
            (10.0, 100.0),
            (2048.5 * 50.0 / 4096.0, 50.0),
        ] {
            let recipe = json!({"grainAmount": 30, "grainSize": size, "grainRoughness": roughness,
                "grainStructure": name, "grainVersion": 2});
            let pixels = render(recipe);
            let cells = portable_grain_reference::cells_per_px(size / 50.0, width, height);
            let expected: Vec<f64> = (0..width * height)
                .map(|i| {
                    portable_grain_reference::grain(
                        structure as u32,
                        i % width,
                        i / width,
                        cells,
                        roughness / 100.0,
                    ) as f64
                })
                .collect();
            let change: Vec<f64> = (0..(width * height) as usize)
                .map(|i| pixels[i * 4] as f64 - flat[i * 4] as f64)
                .collect();
            let gain = change
                .iter()
                .zip(&expected)
                .map(|(c, e)| c * e)
                .sum::<f64>()
                / expected.iter().map(|e| e * e).sum::<f64>();
            let worst = change
                .iter()
                .zip(&expected)
                .map(|(c, e)| (c - gain * e).abs())
                .fold(0.0, f64::max);
            let spread = (change.iter().map(|c| c * c).sum::<f64>() / change.len() as f64).sqrt();
            assert!(
                spread > 300.0,
                "{name} {size}/{roughness}: grain of {spread:.0} codes"
            );
            // Rounding of the 16-bit output and of the float interpolation;
            // version 1's float hash gives an unrelated pattern.
            assert!(
                worst <= 2.0,
                "{name} {size}/{roughness}: {worst:.1} codes from the CPU evaluation (grain {spread:.0})"
            );
        }
    }
}
