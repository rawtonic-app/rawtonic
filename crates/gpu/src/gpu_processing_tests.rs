use super::*;
use crate::mask_generation::{MaskDefinition, PhotoGeometry, generate_mask_bitmap};
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

/// Thumbnail batches run on rayon's global pool and wait there for the GPU
/// processor lock; a caller holding that lock must not need the pool for its
/// input upload (an Edit-all write in the multi view froze every render).
#[test]
fn input_uploads_do_not_wait_for_a_busy_rayon_pool() {
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;
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
) -> Vec<Arc<GrayImage>> {
    let definitions: Vec<MaskDefinition> = profile
        .get("masks")
        .and_then(|masks| serde_json::from_value(masks.clone()).ok())
        .unwrap_or_default();
    definitions
        .iter()
        .filter_map(|definition| {
            generate_mask_bitmap(
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
        let profile = if expected & 8 != 0 {
            local_profile(effects)
        } else {
            effects
        };
        let mut adjustments = get_all_adjustments_from_json(&profile, true, Some(1));
        assert_eq!(preview_features(&adjustments), expected, "{profile}");
        if expected & 8 != 0 {
            // Stale entries beyond mask_count must not affect the current image.
            adjustments.mask_count = 0;
            assert_eq!(preview_features(&adjustments), 0);
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
#[ignore = "requires a native GPU; invalid WGSL must fail instead of rendering black"]
fn invalid_shader_source_fails_processor_construction() {
    let context = context();
    let broken = format!(
        "{}\nconst BROKEN_CONSTANT: f32 = ;\n",
        include_str!("shaders/shader.wgsl")
    );
    let error = match GpuProcessor::with_shader_source(context.clone(), 64, 64, &broken) {
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
    let features = preview_features(&adjustments) | (1 << 2);
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
        processor.export_pipeline(&adjustments, false),
        &processor.main_pipeline
    ));
    assert!(std::ptr::eq(
        processor.export_pipeline(&adjustments, true),
        &processor.high_precision_pipeline
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
            bytemuck::cast_slice(&data),
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
            Arc::new(ImageBuffer::from_fn(width, height, |x, _| {
                Luma([((x % 127) * 2) as u8])
            })),
            Arc::new(ImageBuffer::from_fn(width, height, |_, y| {
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

fn prepare_preview_pipeline(processor: &GpuProcessor, features: usize) {
    processor.preview_pipelines[features].get_or_init(|| {
        create_preview_pipeline(
            &processor.context.device,
            &processor.preview_shader,
            &processor.preview_layout,
            features,
            true,
            true,
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
        let mask = Arc::new(ImageBuffer::from_fn(width, height, |x, _| {
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
            Arc::new(ImageBuffer::from_fn(width, height, |x, y| {
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
            Arc::new(ImageBuffer::from_fn(width, height, |x, y| {
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

fn upload_test_masks(width: u32, height: u32, count: usize) -> Vec<Arc<image::GrayImage>> {
    (0..count)
        .map(|i| {
            Arc::new(ImageBuffer::from_fn(width, height, |x, y| {
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
    masks: &[Arc<image::GrayImage>],
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
    let frame = |masks: &[Arc<image::GrayImage>], writes, label| {
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
    Arc::make_mut(&mut masks[0]).fill(255);
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
            Arc::new(ImageBuffer::from_fn(width, height, |x, y| {
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
            Arc::new(ImageBuffer::from_fn(width, height, |x, y| {
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

#[test]
#[ignore = "requires a native GPU; display curve tables against exact curve evaluation"]
fn curve_tables_match_exact_display_curves() {
    let context = context();
    let (width, height) = (517, 389);
    let processor = GpuProcessor::new(context.clone(), width, height).unwrap();
    let masks: Vec<_> = (0..20)
        .map(|i| {
            Arc::new(ImageBuffer::from_fn(width, height, |x, y| {
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
            Arc::new(ImageBuffer::from_fn(width, height, |x, y| {
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
        let mask = Arc::new(ImageBuffer::from_fn(width, height, |x, y| {
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
            Arc::new(ImageBuffer::from_fn(width, height, |x, y| {
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
    previous: &[Arc<GrayImage>],
    current: &[Arc<GrayImage>],
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
            Arc::new(ImageBuffer::from_fn(width, height, move |x, y| {
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
                let mask = Arc::make_mut(mask);
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
    masks: &[Arc<GrayImage>],
    layer: usize,
    region: (std::ops::Range<u32>, std::ops::Range<u32>),
    value: u8,
) -> Vec<Arc<GrayImage>> {
    let mut masks = masks.to_vec();
    let mask = Arc::make_mut(&mut masks[layer]);
    for y in region.1 {
        for x in region.0.clone() {
            mask.put_pixel(x, y, Luma([value]));
        }
    }
    masks
}

fn changed_area(previous: &[Arc<GrayImage>], current: &[Arc<GrayImage>], roi: Option<Roi>) -> u64 {
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
                 masks: &[Arc<GrayImage>],
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
                     masks: &[Arc<GrayImage>],
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
    masks: &[Arc<GrayImage>],
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
    let render = |masks: &[Arc<GrayImage>]| {
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

fn diagonal_stroke(masks: &[Arc<GrayImage>], value: u8) -> Vec<Arc<GrayImage>> {
    // The in-app diagonal: about 2100x780 preview pixels with a 32-pixel radius.
    let mut masks = masks.to_vec();
    let mask = Arc::make_mut(&mut masks[0]);
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
    let render = |masks: &[Arc<GrayImage>]| {
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
                    generate_mask_bitmap(
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
    let render = |masks: &[Arc<GrayImage>]| {
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
        let states: Vec<Vec<Arc<GrayImage>>> = (0..=24u32)
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
                generate_mask_bitmap(
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
                generate_mask_bitmap(
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
                    generate_mask_bitmap(
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
    let states: Vec<Vec<Arc<GrayImage>>> = (0..=48u32)
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
    let render = |adjustments: AllAdjustments, masks: &[Arc<GrayImage>]| {
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
        let mut mask = (*tiny[1]).clone();
        let pixel = mask.get_pixel_mut(900 + step, 700);
        pixel.0[0] = pixel.0[0].wrapping_add(1);
        tiny[1] = Arc::new(mask);
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
    for features in 1..PREVIEW_PIPELINE_COUNT {
        let start = Instant::now();
        create_preview_pipeline(
            &context.device,
            &processor.preview_shader,
            &processor.preview_layout,
            features,
            true,
            true,
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
                generate_mask_bitmap(
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
    assert_eq!(checked, 5);
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
    let left_half = Arc::new(GrayImage::from_fn(width, height, |x, _| {
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
    vec![
        ("plain", json!({})),
        ("neighbourhood", neighbourhood),
        ("masked", masked),
        (
            "relight",
            json!({"relightLights":[{"intensity":65,"pattern":"blinds","elevation":25}]}),
        ),
    ]
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
    let adjustments = get_all_adjustments_from_json(profile, raw, Some(2));
    let mask = Arc::new(ImageBuffer::from_fn(width, height, |x, y| {
        Luma([((x * 3 + y * 7) % 256) as u8])
    }));
    let masks = if adjustments.mask_count > 0 {
        vec![mask]
    } else {
        Vec::new()
    };
    let processor = Mutex::new(None);
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
    profiles.push(("flare", json!({"flareAmount":60,"exposure":1.5})));
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

#[test]
#[ignore = "requires a native GPU; renders over the texture limit never return the source"]
fn renders_over_the_texture_limit_fail_instead_of_skipping_edits() {
    let context = context();
    let source = chunk_test_source(600, 400);
    let limited = limited_context(&context, 512);
    // Lens flare is built from the whole photo: a clear error.
    let flare = chunk_test_render(
        &limited,
        &source,
        &json!({"flareAmount": 60}),
        true,
        RenderOutputPrecision::EightBit,
        None,
    );
    assert_eq!(flare.unwrap_err(), flare_texture_limit_error(600, 400, 512));
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
    let processor = Mutex::new(None);
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
