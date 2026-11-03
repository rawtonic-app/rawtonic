//! Coverage of the render-input snapshots (notes: plans/architecture-cleanup-2026-11.md,
//! step 0.3): every value of the editor's new-edit RAW recipe, changed on its
//! own, must change the snapshot, unless a list below says why it cannot.
//! A refactor that drops a recipe value on the way to the GPU then shows here
//! before any export is compared.
//!
//! The snapshot is taken on a small synthetic RAW-like source without a GPU:
//! the CPU branch of `prepare_export_render` (warp, lens blur, orientation
//! and crop on the CPU, masks on the CPU), which the GPU branches equal by
//! their own tests; the edge-fringe pass, GPU-only, enters by its version. Everything else is the export's own
//! code: decode options, base retouching, the uniforms, the resources.
//!
//! The recipe is `crates/engine/tests/fixtures/new-edit-recipe.raw.json`, kept
//! equal to the editor's `initialAdjustmentsFor(true)` by
//! `src/contracts/newEditRecipe.test.ts`.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use image::{DynamicImage, Rgb32FImage};
use rawtonic_engine::adjustment_utils::{
    apply_transformations_after_warp, apply_warp, hydrate_adjustments_from_cache,
};
use rawtonic_engine::image_loader::{BaseImageOptions, ai_denoise_settings};
use rawtonic_engine::image_processing::{MAX_MASKS, mask_definitions_from_json};
use rawtonic_engine::mask_generation::{
    MaskDefinition, PhotoGeometry, generate_layer_mask, prepare_parametric_mask_source,
};
use rawtonic_engine::output_color_space::OutputColorSpace;
use rawtonic_export::export_processing::{
    export_adjustments, prepare_export_source, set_output_color_space,
};
use rawtonic_export::render_inputs::{RenderInputs, snapshot_hash};
use rawtonic_gpu::gpu_processing::RenderOutputPrecision;
use serde_json::{Value, json};

const SOURCE: &str = "synthetic.dng";
const WIDTH: u32 = 96;
const HEIGHT: u32 = 64;

/// Values that change nothing the GPU render receives because they act after
/// the snapshot point (on the rendered pixels or on the written file).
const ACTS_AFTER_SNAPSHOT: &[(&str, &str)] = &[];

/// Values only the editor reads (panels, overlays, its own bookkeeping).
const UI_ONLY: &[(&str, &str)] = &[
    (
        "/pointCurves",
        "the Curves panel's point-curve state; the editor writes the composed curve to `curves`, which the engine reads",
    ),
    (
        "/parametricCurve",
        "the Curves panel's parametric state; composed into `curves` by the editor",
    ),
    ("/curveMode", "which Curves panel the editor shows"),
    (
        "/showClipping",
        "the editor's clipping overlay; `export_adjustments` sets it off for every export",
    ),
    (
        "/aspectRatio",
        "the crop tool's ratio lock; the crop itself is `crop`",
    ),
    (
        "/lutData",
        "the editor's copy of the loaded LUT; the render loads `lutPath`",
    ),
    ("/lutName", "the LUT's name shown in the editor"),
    ("/lutSize", "the LUT's size shown in the editor"),
];

/// Values the synthetic source cannot exercise: they need a model, a real
/// camera file or a resource this test does not have. The preservation run's
/// exports cover them where its matrix has them.
const NOT_EXERCISED: &[(&str, &str)] = &[
    (
        "/aiPatches",
        "needs a generated patch (inpainting model); its result is the snapshot's `source.patched` hash",
    ),
    (
        "/faceRetouch",
        "needs faces found by the face models; its result is the snapshot's `source.patched` hash",
    ),
    (
        "/sourceFrameVersion",
        "acts only on RAW files whose frame versions differ (`source_frame::effective`); recorded as `decode.frame`",
    ),
    (
        "/cameraCaVersion",
        "acts only on a RAW's embedded lens data in camera mode (`embedded_lens`), resolved into `lensDistortionParams` by `prepare_recipe_lens`",
    ),
    (
        "/cameraGeometryVersion",
        "acts only on a RAW's embedded lens data in camera mode (`embedded_lens`), resolved into `lensDistortionParams` by `prepare_recipe_lens`",
    ),
    (
        "/lensMaker",
        "looked up in the Lensfun database, which this test does not load; resolved into `lensDistortionParams`",
    ),
    (
        "/lensModel",
        "looked up in the Lensfun database, which this test does not load; resolved into `lensDistortionParams`",
    ),
];

fn fixture() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../engine/tests/fixtures/new-edit-recipe.raw.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// A RAW-like linear source with gradients, saturated colours, clipped
/// highlights and fine detail, so geometry, retouching and masks move pixels.
fn source() -> Arc<DynamicImage> {
    let image = Rgb32FImage::from_fn(WIDTH, HEIGHT, |x, y| {
        let u = x as f32 / WIDTH as f32;
        let v = y as f32 / HEIGHT as f32;
        let checker = if (x / 3 + y / 3) % 2 == 0 { 0.05 } else { 0.0 };
        let hot = if x > WIDTH * 3 / 4 && y < HEIGHT / 4 {
            1.6
        } else {
            0.0
        };
        image::Rgb([
            0.02 + 0.8 * u + checker + hot,
            0.02 + 0.6 * v + hot,
            0.02 + 0.5 * (1.0 - u) * v + checker + hot,
        ])
    });
    Arc::new(DynamicImage::ImageRgb32F(image))
}

/// The snapshot of `recipe` on the synthetic source, following the export's
/// path (`FileExporter::export`, `prepare_export_render`'s CPU branch,
/// `render_prepared_export`).
fn snapshot(recipe: &Value) -> Value {
    let is_raw = true;
    let inputs = RenderInputs::new();
    let mut js = recipe.clone();
    hydrate_adjustments_from_cache(&mut HashMap::new(), &mut js);
    let _ = rawtonic_export::exif_processing::prepare_recipe_lens(
        &mut js,
        std::path::Path::new(SOURCE),
    );
    let base = BaseImageOptions::default().for_recipe(&js, SOURCE);
    inputs.decode(is_raw, &base, ai_denoise_settings(&js));
    let source = prepare_export_source(SOURCE, source(), None, &js).unwrap();
    inputs.source(&source);

    let mask_definitions = mask_definitions_from_json(&js);
    let mut adjustments = export_adjustments(SOURCE, &js, is_raw, None, &mask_definitions, None);
    let warped = apply_warp(Cow::Borrowed(source.base()), &js, is_raw).unwrap();
    // The edge-fringe pass of the current versions runs only on the GPU and
    // reads only `fringeVersion`, which the geometry entry records; the
    // pixels here are transformed without it.
    let mut without_fringe = js.clone();
    if let Some(map) = without_fringe.as_object_mut() {
        map.remove("fringeVersion");
    }
    let (transformed, crop_offset) =
        apply_transformations_after_warp(warped, &without_fringe).unwrap();
    let (width, height) = (transformed.width(), transformed.height());
    let geometry = PhotoGeometry::from_adjustments(&js).with_decoded_size(WIDTH, HEIGHT);
    inputs.geometry(
        "cpu",
        &js,
        is_raw,
        None,
        (width, height),
        crop_offset,
        None,
        &geometry,
        Some(&transformed),
    );
    let warped_image = mask_definitions
        .iter()
        .any(MaskDefinition::requires_warped_image)
        .then(|| prepare_parametric_mask_source(&source.pristine, is_raw, 1.0, &js).unwrap());
    for (index, def) in mask_definitions
        .iter()
        .filter(|def| def.produces_bitmap())
        .take(MAX_MASKS)
        .enumerate()
    {
        let mask = generate_layer_mask(
            def,
            width,
            height,
            1.0,
            crop_offset,
            &geometry,
            warped_image.as_ref(),
        )
        .expect("a bitmap");
        inputs.mask_bitmap(index, &mask);
    }
    let lut = js["lutPath"]
        .as_str()
        .and_then(|path| rawtonic_engine::lut::parse_lut_file(path).ok());
    set_output_color_space(&mut adjustments, OutputColorSpace::Srgb);
    inputs.render(
        &adjustments,
        lut.as_ref(),
        (width, height),
        RenderOutputPrecision::SixteenBit,
        OutputColorSpace::Srgb,
    );
    inputs.snapshot()
}

/// Every changeable value of `value` as (JSON pointer, value).
fn leaves(pointer: String, value: &Value, out: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, item) in map {
                leaves(format!("{pointer}/{key}"), item, out);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (index, item) in items.iter().enumerate() {
                leaves(format!("{pointer}/{index}"), item, out);
            }
        }
        _ => out.push((pointer, value.clone())),
    }
}

/// The candidate values one leaf is changed to; the leaf counts as seen when
/// any of them changes the snapshot.
fn candidates(pointer: &str, value: &Value, special: &BTreeMap<&str, Vec<Value>>) -> Vec<Value> {
    if let Some(values) = special.get(pointer) {
        return values.clone();
    }
    match value {
        Value::Number(n) => {
            let v = n.as_f64().unwrap();
            let integral = n.is_i64() || n.is_u64();
            let mut out = vec![v + 10.0, v - 10.0, v + 1.0, v - 1.0, v * 0.5, 1.0, 0.0];
            if !integral {
                out.push(v + 0.25);
            }
            out.into_iter()
                .filter(|c| *c != v)
                .map(|c| {
                    if c.fract() == 0.0 {
                        json!(c as i64)
                    } else {
                        json!(c)
                    }
                })
                .collect()
        }
        Value::Bool(b) => vec![json!(!b)],
        _ => Vec::new(),
    }
}

/// The documented reason a pointer is not seen, if any.
fn listed(pointer: &str) -> Option<(&'static str, &'static str)> {
    let lists = [
        ("ACTS_AFTER_SNAPSHOT", ACTS_AFTER_SNAPSHOT),
        ("UI_ONLY", UI_ONLY),
        ("NOT_EXERCISED", NOT_EXERCISED),
    ];
    lists.into_iter().find_map(|(list, entries)| {
        entries
            .iter()
            .find(|(key, _)| pointer.starts_with(key))
            .map(|(_, reason)| (list, *reason))
    })
}

fn set(recipe: &mut Value, pointer: &str, value: Value) {
    *recipe.pointer_mut(pointer).expect("pointer in the recipe") = value;
}

fn merge(recipe: &Value, patch: &Value) -> Value {
    let mut out = recipe.clone();
    for (key, value) in patch.as_object().unwrap() {
        out[key] = value.clone();
    }
    out
}

/// Base64 of `bytes` (standard alphabet, padded).
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A depth map as the editor stores it: a PNG data URL.
fn depth_map(invert: bool) -> Value {
    let image = image::GrayImage::from_fn(16, 12, |x, _| {
        let v = (x * 16) as u8;
        image::Luma([if invert { 255 - v } else { v }])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    json!(format!("data:image/png;base64,{}", base64(&png)))
}

/// A small `.cube` LUT file in a folder kept for the test run.
fn lut_file(name: &str, swap: bool) -> Value {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().unwrap());
    let path = dir.path().join(name);
    let mut text = String::from("LUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                let (r, g) = if swap { (g, r) } else { (r, g) };
                text.push_str(&format!("{r} {g} {b}\n"));
            }
        }
    }
    std::fs::write(&path, text).unwrap();
    json!(path.to_string_lossy())
}

fn radial_mask(exposure: f64) -> Value {
    json!({
        "id": "mask-radial", "name": "Radial", "visible": true, "invert": false, "opacity": 100,
        "adjustments": {"exposure": exposure, "saturation": 20},
        "subMasks": [{
            "id": "sub-radial", "name": "Radial", "type": "radial", "visible": true,
            "invert": false, "opacity": 100, "mode": "additive",
            "parameters": {"centerX": 48, "centerY": 32, "radiusX": 20, "radiusY": 16,
                           "rotation": 0, "feather": 0.5},
        }],
    })
}

fn guide_lines() -> Value {
    json!([
        {"id": "g1", "type": "vertical", "p1": {"x": 0.2, "y": 0.1}, "p2": {"x": 0.25, "y": 0.9}},
        {"id": "g2", "type": "vertical", "p1": {"x": 0.8, "y": 0.1}, "p2": {"x": 0.75, "y": 0.9}},
    ])
}

/// Settings that make a gated value act: each value is changed on top of
/// the first context whose pointer prefix it has. The gate itself is seen
/// on its own (it changes the snapshot from the new-edit recipe).
fn contexts() -> Vec<(&'static str, Value)> {
    let recipe = fixture();
    let mut grading = recipe["colorGrading"].clone();
    for wheel in ["global", "highlights", "midtones", "shadows"] {
        grading[wheel]["saturation"] = json!(30);
    }
    let mut region = recipe["regionCurve"].clone();
    for key in ["shadows", "darks", "lights", "highlights"] {
        region[key] = json!(20);
    }
    vec![
        ("/colorGrading", json!({"colorGrading": grading})),
        ("/regionCurve", json!({"regionCurve": region})),
        (
            "/curves/version",
            json!({"curves": {"luma": [{"x": 0, "y": 0}, {"x": 64, "y": 40}, {"x": 192, "y": 220},
                                        {"x": 255, "y": 255}],
                              "red": recipe["curves"]["red"], "green": recipe["curves"]["green"],
                              "blue": recipe["curves"]["blue"], "version": 3}}),
        ),
        ("/moirePattern", json!({"moireAmount": 40})),
        (
            "/lensBlur",
            json!({"lensBlurEnabled": true, "lensBlurDepthMap": depth_map(false),
                   "lensBlurAmount": 100, "lensBlurMaxDepth": 70}),
        ),
        ("/bwVersion", json!({"blackAndWhite": true})),
        ("/aiDenoiseVersion", json!({"aiDenoise": 50})),
        ("/vignetteStyle", json!({"vignetteAmount": -40})),
        (
            "/sectionVisibility",
            json!({"exposure": 1, "contrast": 30, "saturation": 20, "vibrance": 20,
                   "sharpness": 60, "vignetteAmount": -40, "grainAmount": 30}),
        ),
        (
            "/guidedPerspective/autoCrop",
            json!({"guidedPerspective": {"enabled": true, "lines": guide_lines(), "autoCrop": true,
                                         "upright": "vertical"}}),
        ),
        ("/lutPath", json!({})),
    ]
}

/// Replacement values for leaves that are not numbers or booleans (strings,
/// nulls, empty lists).
fn special() -> BTreeMap<&'static str, Vec<Value>> {
    BTreeMap::from([
        ("/baseCurve", vec![json!("highContrast"), json!("linear")]),
        ("/toneMapper", vec![json!("agx"), json!("basic")]),
        (
            "/vignetteStyle",
            vec![json!("colorPriority"), json!("paintOverlay")],
        ),
        (
            "/lensBlurShape",
            vec![json!("hexagon"), json!("octagon"), json!("ring")],
        ),
        ("/lensBlurDepthMap", vec![depth_map(true)]),
        (
            "/crop",
            vec![json!({"unit": "px", "x": 10, "y": 8, "width": 60, "height": 40})],
        ),
        ("/lensCorrectionMode", vec![json!("auto"), json!("camera")]),
        (
            "/lensDistortionParams",
            vec![
                json!({"k1": 0.02, "k2": -0.01, "k3": 0.0, "model": 1, "tca_vr": 1.001,
                        "tca_vb": 0.999, "vig_k1": -0.2, "vig_k2": 0.05, "vig_k3": 0.0}),
            ],
        ),
        ("/lutPath", vec![lut_file("swap.cube", true)]),
        ("/masks", vec![json!([radial_mask(0.7)])]),
        (
            "/spots",
            vec![json!([{"x": 30.0, "y": 20.0, "radius": 6.0, "sourceX": 60.0, "sourceY": 40.0}])],
        ),
        (
            "/redEyes",
            vec![
                json!([{"version": 1, "kind": "red", "x": 0.5, "y": 0.5, "radiusX": 0.08,
                         "radiusY": 0.08, "angle": 0.0, "pupilSize": 50, "darken": 50,
                         "catchlight": false, "catchlightX": -0.3, "catchlightY": -0.35,
                         "visible": true}]),
            ],
        ),
        (
            "/colorRanges",
            vec![
                json!([{"id": "r1", "hue": 0, "hueRange": 60, "chroma": 0.1, "chromaRange": 1,
                         "lightness": 0.5, "lightnessRange": 1, "smoothness": 50,
                         "hueShift": 20, "saturation": 30, "lightnessShift": 10}]),
            ],
        ),
        ("/guidedPerspective/lines", vec![guide_lines()]),
        ("/aspectRatio", vec![json!(1.5)]),
        ("/lutName", vec![json!("Look")]),
        ("/lutData", vec![json!("data")]),
    ])
}

#[test]
fn every_new_edit_recipe_value_reaches_the_render_inputs() {
    let recipe = fixture();
    let special = special();
    let contexts = contexts();
    let mut all = Vec::new();
    leaves(String::new(), &recipe, &mut all);
    let mut unseen = Vec::new();
    let mut stale = Vec::new();
    let mut seen = 0;
    let mut base_cache: HashMap<String, String> = HashMap::new();
    for (pointer, value) in &all {
        let context = contexts
            .iter()
            .find(|(prefix, _)| pointer.starts_with(prefix))
            .map(|(_, patch)| merge(&recipe, patch))
            .unwrap_or_else(|| recipe.clone());
        let base = base_cache
            .entry(context.to_string())
            .or_insert_with(|| snapshot_hash(&snapshot(&context)))
            .clone();
        // Candidates from the value in its context (a context may set it).
        let value = context.pointer(pointer).unwrap_or(value);
        let changed = candidates(pointer, value, &special)
            .into_iter()
            .any(|candidate| {
                let mut changed = context.clone();
                set(&mut changed, pointer, candidate);
                snapshot_hash(&snapshot(&changed)) != base
            });
        match (changed, listed(pointer)) {
            (true, None) => seen += 1,
            (true, Some((list, _))) => stale.push(format!("{pointer} (on {list})")),
            (false, Some(_)) => {}
            (false, None) => unseen.push(format!("{pointer} = {value}")),
        }
    }
    eprintln!(
        "render-input coverage: {} values, {seen} seen; ACTS_AFTER_SNAPSHOT {}, UI_ONLY {}, NOT_EXERCISED {}",
        all.len(),
        ACTS_AFTER_SNAPSHOT.len(),
        UI_ONLY.len(),
        NOT_EXERCISED.len()
    );
    assert!(
        unseen.is_empty() && stale.is_empty(),
        "values the snapshot does not see and no list explains:\n{}\nlisted values the snapshot does see:\n{}",
        unseen.join("\n"),
        stale.join("\n")
    );
}
