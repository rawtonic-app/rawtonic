//! Non-generative projected lighting in scene-linear RGB. Coordinates are
//! normalized to the developed photograph; no original pixels are changed.
use serde_json::Value;
#[path = "relight_source.rs"]
pub mod source;
#[path = "relight_surface.rs"]
mod surface;
#[cfg(any(test, feature = "test-support"))]
pub use surface::test_face_geometry;
pub use surface::{surface_texture, surface_textures};

pub const MAX_LIGHTS: usize = 4;

/// Pattern names in GPU order: the index is the WGSL `relight_pattern` case.
/// Parity with `RELIGHT_PATTERNS` in src/utils/relight.ts is tested there.
pub const PATTERNS: [&str; 16] = [
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
    "custom",
];
const CUSTOM_PATTERN: usize = 15;
/// Side of one imported pattern in the 2×2 atlas (one slot per light).
pub const PATTERN_SLOT: u32 = 512;

// Values for absent light fields: those of a new light, `createRelightLight`
// in src/utils/relight.ts (parity tested there).
const DEFAULT_COLOR: u32 = 0xfff1db;
const DEFAULT_INTENSITY: f64 = 25.;
const DEFAULT_ELEVATION: f64 = 45.;
const DEFAULT_X: f64 = 50.;
const DEFAULT_Y: f64 = 50.;
const DEFAULT_SIZE: f64 = 60.;
const DEFAULT_ROTATION: f64 = 135.;
const DEFAULT_DISTANCE: f64 = 60.;
const DEFAULT_SCALE: f64 = 50.;
const DEFAULT_SOFTNESS: f64 = 30.;
const DEFAULT_DIFFUSION: f64 = 25.;
const DEFAULT_SHADOWS: f64 = 100.;

/// Where a light may act: `Some(0)` everywhere, `Some(n)` inside mask layer
/// n − 1 (its index among the layers that produce a bitmap, as in
/// `get_all_adjustments_from_json`), `None` nowhere (the layer is hidden,
/// empty or beyond the GPU limit). A deleted layer no longer restricts.
fn mask_slot(recipe: &Value, light: &Value) -> Option<u32> {
    let Some(id) = light["maskId"].as_str() else {
        return Some(0);
    };
    let mut index = 0;
    for mask in recipe["masks"].as_array().into_iter().flatten() {
        let produces = mask["visible"].as_bool() == Some(true)
            && mask["subMasks"].as_array().is_some_and(|s| !s.is_empty());
        if mask["id"].as_str() == Some(id) {
            return (produces && index < crate::image_processing::MAX_MASKS)
                .then_some(index as u32 + 1);
        }
        index += usize::from(produces);
    }
    Some(0)
}

fn custom_pattern(light: &Value) -> Option<&str> {
    (light["pattern"].as_str() == Some("custom"))
        .then(|| light["customPattern"]["data"].as_str())
        .flatten()
}

/// Range version 2 (lights since 2026-10-31, `rangeVersion`): Intensity 100 %
/// is a clearly visible key light and the low end moves finely; measured on
/// portraits and scenes (notes: benchmark-results/rl-relight-ux-2026-10-31).
const RANGE_2_MAX_GAIN: f32 = 4.;
const RANGE_2_EXPONENT: f32 = 1.5;

/// Bit of a light's GPU flags: Softness and Diffusion act on surfaces as
/// range version 2 defines (shader `range2`).
const RANGE_2_FLAG: u32 = 128;

/// The ranges a light's values mean; absent = 1 (lights before 2026-10-31).
fn range_version(light: &Value) -> u64 {
    light["rangeVersion"].as_u64().unwrap_or(1)
}

/// The light's gain for its Intensity (0–100). Lights without
/// `rangeVersion` keep the first range, linear up to 3.
fn intensity_gain(light: &Value, intensity: f32) -> f32 {
    let t = intensity / 100.;
    if range_version(light) >= 2 {
        RANGE_2_MAX_GAIN * t.powf(RANGE_2_EXPONENT)
    } else {
        t * 3.
    }
}

pub fn gpu_lights(recipe: &Value) -> [[f32; 4]; 20] {
    let mut out = [[0.; 4]; 20];
    if recipe["sectionVisibility"]["relight"].as_bool() == Some(false) {
        return out;
    }
    let Some(lights) = recipe["relightLights"].as_array() else {
        return out;
    };
    for (i, light) in lights.iter().take(MAX_LIGHTS).enumerate() {
        if light["enabled"].as_bool() == Some(false) {
            continue;
        }
        let Some(mask) = mask_slot(recipe, light) else {
            continue;
        };
        let n = |key: &str, default: f64, lo: f64, hi: f64| {
            light[key]
                .as_f64()
                .filter(|v| v.is_finite())
                .unwrap_or(default)
                .clamp(lo, hi) as f32
        };
        let hex = light["color"]
            .as_str()
            .and_then(|c| c.strip_prefix('#'))
            .filter(|s| s.len() == 6)
            .and_then(|s| u32::from_str_radix(s, 16).ok())
            .unwrap_or(DEFAULT_COLOR);
        let linear = |v: u32| {
            let c = (v & 255) as f32 / 255.;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        // A custom light without its bitmap stays unpatterned.
        let pattern = match light["pattern"]
            .as_str()
            .and_then(|name| PATTERNS.iter().position(|p| *p == name))
        {
            Some(CUSTOM_PATTERN) if custom_pattern(light).is_none() => 0,
            Some(index) => index,
            None => 0,
        };
        let j = i * 5;
        out[j] = [
            linear(hex >> 16),
            linear(hex >> 8),
            linear(hex),
            intensity_gain(light, n("intensity", DEFAULT_INTENSITY, 0., 100.)),
        ];
        out[j + 1] = [
            n("x", DEFAULT_X, -100., 200.) / 100.,
            n("y", DEFAULT_Y, -100., 200.) / 100.,
            n("size", DEFAULT_SIZE, 5., 200.) / 100.,
            f32::from(light["type"].as_str() == Some("spot")),
        ];
        let rotation = n("rotation", DEFAULT_ROTATION, -180., 180.);
        out[j + 2] = [
            rotation.to_radians(),
            n("scale", DEFAULT_SCALE, 5., 300.) / 100.,
            n("softness", DEFAULT_SOFTNESS, 0., 100.) / 100.,
            n("diffusion", DEFAULT_DIFFUSION, 0., 100.) / 100.,
        ];
        // Flags: bit 0 inverts the pattern, bits 1–6 hold the mask slot, bit 7
        // marks range version 2.
        let flags = u32::from(light["invert"].as_bool() == Some(true))
            | (mask << 1)
            | if range_version(light) >= 2 {
                RANGE_2_FLAG
            } else {
                0
            };
        out[j + 3] = [
            pattern as f32,
            flags as f32,
            n("shadows", DEFAULT_SHADOWS, 0., 100.) / 100.,
            n("elevation", DEFAULT_ELEVATION, 5., 90.).to_radians(),
        ];
        // Legacy recipes: the spot sits at the pattern center and the pattern
        // turns with the light, as before these controls were independent.
        out[j + 4] = [
            n("sourceX", out[j + 1][0] as f64 * 100., -100., 200.) / 100.,
            n("sourceY", out[j + 1][1] as f64 * 100., -100., 200.) / 100.,
            n("patternRotation", rotation as f64, -180., 180.).to_radians(),
            n("distance", DEFAULT_DISTANCE, 10., 300.) / 100.,
        ];
    }
    out
}

use base64::{Engine as _, engine::general_purpose::STANDARD};
type DepthImage = image::ImageBuffer<image::Luma<u16>, Vec<u16>>;
use std::collections::{VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

/// Version-four inverse-depth maps use normalized camera intrinsics. Distances
/// remain relative; the reference image plane defines one short-side world unit.
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Camera {
    pub fx: f32,
    pub fy: f32,
    pub cx: f32,
    pub cy: f32,
    pub inverse_min: f32,
    pub inverse_max: f32,
    pub reference: f32,
}
impl Camera {
    pub fn from_recipe(recipe: &Value) -> Option<Self> {
        if !(4..=5).contains(&recipe["relightDepth"]["version"].as_u64()?) {
            return None;
        }
        let c: Self = serde_json::from_value(recipe["relightDepth"]["camera"].clone()).ok()?;
        let values = [
            c.fx,
            c.fy,
            c.cx,
            c.cy,
            c.inverse_min,
            c.inverse_max,
            c.reference,
        ];
        (values.iter().all(|v| v.is_finite())
            && (0.05..=100.).contains(&c.fx)
            && (0.05..=100.).contains(&c.fy)
            && (-1.0..=2.).contains(&c.cx)
            && (-1.0..=2.).contains(&c.cy)
            && c.inverse_min >= 1e-4
            && c.inverse_max >= c.inverse_min
            && c.inverse_max <= 100.
            && c.reference >= c.inverse_max.recip()
            && c.reference <= c.inverse_min.recip())
        .then_some(c)
    }
    pub fn world_unit(self) -> f32 {
        self.reference / self.fx.max(self.fy)
    }
    pub fn inverse(self, code: u16) -> f32 {
        self.inverse_min + (self.inverse_max - self.inverse_min) * code as f32 / 65535.
    }
    pub fn position(self, uv: [f32; 2], depth: f32, strength: f32) -> [f32; 3] {
        let d = self.reference + (depth - self.reference) * strength;
        [
            (uv[0] - self.cx) * d / self.fx / self.world_unit(),
            (uv[1] - self.cy) * d / self.fy / self.world_unit(),
            (self.reference - d) / self.world_unit(),
        ]
    }
    pub fn uniforms(recipe: &Value) -> ([f32; 4], [f32; 4]) {
        Self::from_recipe(recipe).map_or(([0.; 4], [0.; 4]), |c| {
            (
                [c.fx, c.fy, c.cx, c.cy],
                [c.reference, c.world_unit(), c.inverse_max.recip(), 1.],
            )
        })
    }
}

#[derive(Debug)]
pub struct DepthSurface {
    pub camera: Option<Camera>,
    pub image: DepthImage,
    pub faces: Vec<Vec<[f32; 4]>>,
    pub refined: bool,
    /// Version five: the subject's matte and people normals (relight_source.rs).
    pub subject: Option<source::SubjectImage>,
    /// Version five: scene slopes smoothed over a wide, depth-aware window
    /// (monocular depth normals blotch backgrounds and skies).
    pub smooth_scene: bool,
}
type DepthEntry = (u64, Arc<DepthSurface>);
static DEPTHS: OnceLock<Mutex<VecDeque<DepthEntry>>> = OnceLock::new();

pub fn geometry(recipe: &Value) -> Value {
    let mut map = serde_json::Map::new();
    for key in crate::cache_utils::GEOMETRY_KEYS.iter().copied().chain([
        "orientationSteps",
        "flipHorizontal",
        "flipVertical",
    ]) {
        map.insert(key.into(), recipe[key].clone());
    }
    // Depth is analyzed in the displayed (straightened and cropped) frame.
    // Keep legacy identity-frame maps valid, but reject them after a crop.
    if !recipe["crop"].is_null() {
        map.insert("crop".into(), recipe["crop"].clone());
    }
    if recipe["rotation"].as_f64().unwrap_or(0.) != 0. {
        map.insert("rotation".into(), recipe["rotation"].clone());
    }
    Value::Object(map)
}

/// Register once per depth bitmap, shared by editor, thumbnails and export.
/// The bounded cache holds at most eight inference-resolution images.
pub fn register_depth(recipe: &Value) -> [u32; 4] {
    prepare_depth(recipe).0
}

/// Keep a render's geometry alive independently of the bounded reuse cache.
pub fn prepare_depth(recipe: &Value) -> ([u32; 4], Option<Arc<DepthSurface>>) {
    if recipe["sectionVisibility"]["relight"].as_bool() == Some(false)
        || !recipe["relightLights"].as_array().is_some_and(|lights| {
            lights.iter().any(|l| {
                l["enabled"].as_bool() != Some(false) && l["intensity"].as_f64().unwrap_or(50.) > 0.
            })
        })
        || recipe["relightDepth"]["geometry"] != geometry(recipe)
    {
        return ([0; 4], None);
    }
    let camera = Camera::from_recipe(recipe);
    if recipe["relightDepth"]["version"].as_u64().unwrap_or(0) >= 4 && camera.is_none() {
        return ([0; 4], None);
    }
    let data = recipe["relightDepth"]["data"].as_str();
    let mut hasher = DefaultHasher::new();
    recipe["relightDepth"]["camera"]
        .to_string()
        .hash(&mut hasher);
    recipe["relightDepth"]["version"].as_u64().hash(&mut hasher);
    // The editor names each analysis (`cacheKey`), so a preview frame need
    // not hash the bitmap and its landmarks again; files carry no key.
    if let Some(name) = recipe["relightDepth"]["cacheKey"].as_str() {
        name.hash(&mut hasher);
    } else {
        let Some(data) = data else {
            return ([0; 4], None);
        };
        data.hash(&mut hasher);
        recipe["relightDepth"]["faces"]
            .to_string()
            .hash(&mut hasher);
        recipe["relightDepth"]["subject"].as_str().hash(&mut hasher);
    }
    let key = hasher.finish().max(1);
    let cache = DEPTHS.get_or_init(Mutex::default);
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(index) = cache.iter().position(|(k, _)| *k == key) {
        let entry = cache.remove(index).unwrap();
        cache.push_front(entry);
    } else {
        let Some(data) = data else {
            return ([0; 4], None);
        };
        let Some(bytes) = STANDARD
            .decode(data.split_once(',').map_or(data, |(_, d)| d))
            .ok()
        else {
            return ([0; 4], None);
        };
        let Some(image) = image::load_from_memory(&bytes).ok() else {
            return ([0; 4], None);
        };
        // `thumbnail` also enlarges small images: retain the inference grid.
        // Upsampling before fitting would turn a depth jump into a ramp.
        let depth = if image.width() > 1024 || image.height() > 1024 {
            image.thumbnail(1024, 1024).to_luma16()
        } else {
            image.to_luma16()
        };
        let faces: Vec<Vec<[f32; 4]>> =
            serde_json::from_value(recipe["relightDepth"]["faces"].clone()).unwrap_or_default();
        let faces = faces
            .into_iter()
            .take(8)
            .filter(|points| {
                points.len() == 468
                    && points.iter().all(|p| {
                        p.iter().all(|v| v.is_finite())
                            && (-0.5..=1.5).contains(&p[0])
                            && (-0.5..=1.5).contains(&p[1])
                            && p[2].abs() <= 1.
                            && (0.0..=1.0).contains(&p[3])
                    })
            })
            .collect();
        cache.push_front((
            key,
            Arc::new(DepthSurface {
                camera,
                image: depth,
                faces,
                refined: recipe["relightDepth"]["version"].as_u64().unwrap_or(0) >= 2,
                subject: subject_image(recipe),
                smooth_scene: recipe["relightDepth"]["version"].as_u64().unwrap_or(0) >= 5,
            }),
        ));
        cache.truncate(8);
    }
    let surface = Arc::clone(&cache.front().unwrap().1);
    ([key as u32, (key >> 32) as u32, 0, 0], Some(surface))
}

/// The subject bitmap of a version-five analysis (a 16-bit RGBA PNG).
fn subject_image(recipe: &Value) -> Option<source::SubjectImage> {
    if recipe["relightDepth"]["version"].as_u64()? < 5 {
        return None;
    }
    let data = recipe["relightDepth"]["subject"].as_str()?;
    let bytes = STANDARD
        .decode(data.split_once(',').map_or(data, |(_, d)| d))
        .ok()?;
    let image = image::load_from_memory(&bytes).ok()?.to_rgba16();
    (image.width() <= 2048 && image.height() <= 2048).then_some(image)
}

/// Version-five flags and the photo's own lighting on its people
/// (`sourceLight`: ambient, light, direction; relight_source.rs) for the
/// GPU: [0..9] the lighting,
/// [9] subject texture bound, [10] lighting fitted, [11] version five,
/// [12] the subject's median inverse depth (`subjectDepth`; 0 = unknown).
pub fn source_uniforms(recipe: &Value) -> [[f32; 4]; 4] {
    let mut out = [0f32; 16];
    if recipe["relightDepth"]["version"].as_u64().unwrap_or(0) < 5
        || recipe["relightDepth"]["camera"].is_null()
    {
        return [[0.; 4]; 4];
    }
    out[11] = 1.;
    if recipe["relightDepth"]["subject"].is_string() {
        out[9] = 1.;
        let coefficients: Option<Vec<f32>> = recipe["relightDepth"]["sourceLight"]
            .as_array()
            .filter(|values| values.len() == 9)
            .and_then(|values| {
                values
                    .iter()
                    .map(|v| v.as_f64().map(|v| v as f32).filter(|v| v.is_finite()))
                    .collect()
            });
        if let Some(c) = coefficients {
            out[..9].copy_from_slice(&c);
            out[10] = 1.;
        }
        out[12] = recipe["relightDepth"]["subjectDepth"]
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0. && *v <= 100.)
            .unwrap_or(0.) as f32;
    }
    std::array::from_fn(|i| std::array::from_fn(|j| out[i * 4 + j]))
}

/// Surface normals of the scene depth at `width × height`, in the renderer's
/// frame and at full relief, from the version-five smoothed scene slopes:
/// objects' normals where no people model applies.
pub fn scene_normals(
    depth: &DepthImage,
    camera: Camera,
    width: u32,
    height: u32,
) -> source::Normals {
    let surface = DepthSurface {
        camera: Some(camera),
        image: depth.clone(),
        faces: Vec::new(),
        refined: true,
        subject: None,
        smooth_scene: true,
    };
    let (scene, _) = surface_textures(&surface);
    image::ImageBuffer::from_fn(width, height, |x, y| {
        let u = (x as f32 + 0.5) / width as f32;
        let v = (y as f32 + 0.5) / height as f32;
        let sx = ((u * scene.width() as f32) as u32).min(scene.width() - 1);
        let sy = ((v * scene.height() as f32) as u32).min(scene.height() - 1);
        let p = scene.get_pixel(sx, sy).0;
        let n = [
            -camera.fx * p[0],
            -camera.fy * p[1],
            p[2] - (p[0] * (u - camera.cx) + p[1] * (v - camera.cy)),
        ];
        let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
        image::Rgb(n.map(|c| c / length))
    })
}

/// The subject's median inverse depth (relative, as `Camera::inverse`)
/// under its matte (`matte`, the displayed frame at any size), for lights
/// that reach a background only near the subject.
pub fn subject_inverse(
    depth: &DepthImage,
    camera: Camera,
    matte: &image::GrayImage,
) -> Option<f32> {
    let (w, h) = depth.dimensions();
    let (mw, mh) = matte.dimensions();
    let mut values: Vec<f32> = depth
        .enumerate_pixels()
        .filter(|(x, y, _)| {
            let mx = ((*x as f32 + 0.5) / w as f32 * mw as f32) as u32;
            let my = ((*y as f32 + 0.5) / h as f32 * mh as f32) as u32;
            matte.get_pixel(mx.min(mw - 1), my.min(mh - 1))[0] > 128
        })
        .map(|(_, _, code)| camera.inverse(code[0]))
        .collect();
    if values.len() < 16 {
        return None;
    }
    values.sort_by(f32::total_cmp);
    Some(values[values.len() / 2])
}

pub fn depth(key: [u32; 4]) -> Option<Arc<DepthSurface>> {
    let id = key[0] as u64 | ((key[1] as u64) << 32);
    DEPTHS
        .get()?
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, v)| Arc::clone(v))
}

type PatternEntry = (u64, Arc<image::GrayImage>);
static PATTERN_ATLASES: OnceLock<Mutex<VecDeque<PatternEntry>>> = OnceLock::new();

/// Imported patterns of the enabled lights as one 2×2 atlas (slot = light
/// index, white where a light has none), registered once like the depth.
/// Returns its key for the GPU (zero without imported patterns) and the atlas,
/// which a queued render keeps alive independently of the bounded cache.
pub fn prepare_patterns(recipe: &Value) -> ([u32; 2], Option<Arc<image::GrayImage>>) {
    if recipe["sectionVisibility"]["relight"].as_bool() == Some(false) {
        return ([0; 2], None);
    }
    let mut slots: [Option<&str>; MAX_LIGHTS] = [None; MAX_LIGHTS];
    let mut hasher = DefaultHasher::new();
    for (i, light) in recipe["relightLights"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_LIGHTS)
        .enumerate()
    {
        if light["enabled"].as_bool() == Some(false) {
            continue;
        }
        let Some(data) = custom_pattern(light) else {
            continue;
        };
        i.hash(&mut hasher);
        // An import's id names its immutable bitmap.
        match light["customPattern"]["id"].as_str() {
            Some(id) => id.hash(&mut hasher),
            None => data.hash(&mut hasher),
        }
        slots[i] = Some(data);
    }
    if slots.iter().all(Option::is_none) {
        return ([0; 2], None);
    }
    let key = hasher.finish().max(1);
    let cache = PATTERN_ATLASES.get_or_init(Mutex::default);
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(index) = cache.iter().position(|(k, _)| *k == key) {
        let entry = cache.remove(index).unwrap();
        cache.push_front(entry);
    } else {
        let mut atlas =
            image::GrayImage::from_pixel(PATTERN_SLOT * 2, PATTERN_SLOT * 2, image::Luma([255]));
        for (i, data) in slots.iter().enumerate() {
            let Some(pattern) = data.and_then(decode_pattern) else {
                continue;
            };
            let (x, y) = ((i as u32 % 2) * PATTERN_SLOT, (i as u32 / 2) * PATTERN_SLOT);
            image::imageops::replace(&mut atlas, &pattern, x.into(), y.into());
        }
        cache.push_front((key, Arc::new(atlas)));
        cache.truncate(4);
    }
    let atlas = Arc::clone(&cache.front().unwrap().1);
    ([key as u32, (key >> 32) as u32], Some(atlas))
}

pub fn register_patterns(recipe: &Value) -> [u32; 2] {
    prepare_patterns(recipe).0
}

fn decode_pattern(data: &str) -> Option<image::GrayImage> {
    let bytes = STANDARD
        .decode(data.split_once(',').map_or(data, |(_, d)| d))
        .ok()?;
    let image = image::load_from_memory(&bytes).ok()?.to_luma8();
    Some(if image.dimensions() == (PATTERN_SLOT, PATTERN_SLOT) {
        image
    } else {
        image::imageops::resize(
            &image,
            PATTERN_SLOT,
            PATTERN_SLOT,
            image::imageops::FilterType::Triangle,
        )
    })
}

pub fn pattern_atlas(key: [u32; 2]) -> Option<Arc<image::GrayImage>> {
    let id = key[0] as u64 | ((key[1] as u64) << 32);
    PATTERN_ATLASES
        .get()?
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, v)| Arc::clone(v))
}

/// An imported light pattern as stored in a recipe: the image's central
/// square, at the atlas slot size, in grayscale (white transmits light).
pub fn pattern_png(image: &image::DynamicImage) -> Result<Vec<u8>, String> {
    let side = image.width().min(image.height());
    if side == 0 {
        return Err("The image is empty".into());
    }
    let square = image.crop_imm(
        (image.width() - side) / 2,
        (image.height() - side) / 2,
        side,
        side,
    );
    let gray = square
        .resize_exact(
            PATTERN_SLOT,
            PATTERN_SLOT,
            image::imageops::FilterType::Lanczos3,
        )
        .to_luma8();
    let mut png = std::io::Cursor::new(Vec::new());
    gray.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png.into_inner())
}

/// Fit local slopes without joining disconnected surfaces. A spatial/range
/// weighted linear fit averages depth noise while preserving ramps, including
/// at image boundaries. A curvature confidence suppresses invented bevels at
/// short transitional silhouettes while retaining smooth surface curvature.
/// Derivatives are per normalized image coordinate; height stays unfiltered.
pub fn surface_sample(depth: &DepthImage, x: u32, y: u32) -> [f32; 4] {
    let center = depth.get_pixel(x, y)[0] as f32 / 65535.;
    let slope = |horizontal: bool| {
        let mut sw = 0.;
        let mut st = 0.;
        let mut sz = 0.;
        let mut stt = 0.;
        let mut stz = 0.;
        let mut curvature = 0.0_f32;
        for offset in -3i32..=3 {
            let px = x as i32 + if horizontal { offset } else { 0 };
            let py = y as i32 + if horizontal { 0 } else { offset };
            if px < 0 || py < 0 || px >= depth.width() as i32 || py >= depth.height() as i32 {
                continue;
            }
            let sample = depth.get_pixel(px as u32, py as u32)[0] as f32 / 65535.;
            let (ax, ay) = if horizontal { (1, 0) } else { (0, 1) };
            if px >= ax
                && py >= ay
                && px + ax < depth.width() as i32
                && py + ay < depth.height() as i32
            {
                let before = depth.get_pixel((px - ax) as u32, (py - ay) as u32)[0] as f32 / 65535.;
                let after = depth.get_pixel((px + ax) as u32, (py + ay) as u32)[0] as f32 / 65535.;
                curvature = curvature.max((before + after - 2. * sample).abs());
            }
            let z = sample - center;
            // Compact support: samples across a >4% normalized depth jump
            // contribute exactly zero, rather than a tiny but steep slope.
            let range = (1. - (z / 0.04).powi(2)).max(0.);
            let t = offset as f32;
            let weight = range * range / (1. + t * t);
            sw += weight;
            st += weight * t;
            sz += weight * z;
            stt += weight * t * t;
            stz += weight * t * z;
        }
        let determinant = sw * stt - st * st;
        let fitted = if determinant > 1e-6 {
            (sw * stz - st * sz) / determinant
                * if horizontal {
                    depth.width()
                } else {
                    depth.height()
                } as f32
        } else {
            0.
        };
        (fitted, curvature)
    };
    let (dx, cx) = slope(true);
    let (dy, cy) = slope(false);
    // Monocular silhouettes often contain 1–3 intermediate depth texels.
    // Their small individual steps can pass the range fit above and invent a
    // steep bevel. Curvature in the fit support marks that orientation as
    // unreliable. Fade its normal contribution, preserving unfiltered height
    // and exact linear planes; this is a confidence heuristic, not new depth.
    let edge = ((cx.max(cy) - 0.004) / 0.016).clamp(0., 1.);
    let confidence = 1. - edge * edge * (3. - 2. * edge);
    [dx * confidence, dy * confidence, center, confidence]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn camera_metadata_is_validated_and_part_of_surface_cache_identity() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma16(DepthImage::from_pixel(16, 16, image::Luma([32768])))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let mut recipe = serde_json::json!({"relightLights":[{"intensity":50}],"relightDepth":{"version":4,"data":STANDARD.encode(bytes.into_inner()),"camera":{"fx":1.2,"fy":1.3,"cx":0.5,"cy":0.5,"inverseMin":0.6,"inverseMax":1.4,"reference":1.0}}});
        recipe["relightDepth"]["geometry"] = geometry(&recipe);
        let (first, retained) = prepare_depth(&recipe);
        assert_ne!(first, [0; 4]);
        assert_eq!(first, register_depth(&recipe));
        assert!(retained.unwrap().camera.is_some());
        recipe["relightDepth"]["camera"]["fx"] = serde_json::json!(1.4);
        assert_ne!(first, register_depth(&recipe));
        for (key, value) in [
            ("fx", 0.),
            ("inverseMin", 0.),
            ("inverseMax", 200.),
            ("reference", 0.1),
        ] {
            let mut invalid = recipe.clone();
            invalid["relightDepth"]["camera"][key] = serde_json::json!(value);
            assert_eq!(register_depth(&invalid), [0; 4], "{key}");
            assert_eq!(Camera::uniforms(&invalid), ([0.; 4], [0.; 4]));
        }
        // Version five keeps the camera; an unknown newer version is not
        // decoded as an older map.
        recipe["relightDepth"]["version"] = serde_json::json!(5);
        assert!(prepare_depth(&recipe).1.unwrap().camera.is_some());
        recipe["relightDepth"]["version"] = serde_json::json!(6);
        assert_eq!(register_depth(&recipe), [0; 4]);
        recipe["relightDepth"]["version"] = serde_json::json!(3);
        assert!(prepare_depth(&recipe).1.unwrap().camera.is_none());
        assert_eq!(Camera::uniforms(&recipe), ([0.; 4], [0.; 4]));
    }

    #[test]
    fn prepared_renders_retain_geometry_and_release_it_after_cache_eviction() {
        use crate::image_processing::get_all_adjustments_from_json;
        let recipe = |code| {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageLuma16(DepthImage::from_pixel(8, 8, image::Luma([code])))
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            let mut r = json!({"relightLights":[{"intensity":50}],"relightDepth":{"version":3,"data":STANDARD.encode(bytes.into_inner())}});
            r["relightDepth"]["geometry"] = geometry(&r);
            r
        };
        let first = get_all_adjustments_from_json(&recipe(11123), false, None);
        let weak = Arc::downgrade(first.relight_surface.as_ref().unwrap());
        let queued = first.clone();
        for i in 0..24 {
            let other = get_all_adjustments_from_json(&recipe(12000 + i * 701), false, None);
            assert_eq!(
                other.relight_surface.as_ref().unwrap().image[(0, 0)][0],
                12000 + i * 701
            );
        }
        assert!(depth(first.global.relight_depth).is_none());
        assert_eq!(
            queued.relight_surface.as_ref().unwrap().image[(0, 0)][0],
            11123
        );
        drop(first);
        assert!(weak.upgrade().is_some());
        drop(queued);
        assert!(weak.upgrade().is_none(), "evicted geometry must not leak");
        assert!(DEPTHS.get().unwrap().lock().unwrap().len() <= 8);
    }

    #[test]
    fn surface_fit_preserves_planes_and_rejects_depth_steps() {
        for (w, h) in [(64, 32), (32, 64)] {
            let plane = DepthImage::from_fn(w, h, |x, y| {
                image::Luma([(10000 + x * 100 + y * 200) as u16])
            });
            let step = DepthImage::from_fn(w, h, |x, y| {
                image::Luma([if x < w / 2 && y < h / 2 { 10000 } else { 50000 }])
            });
            for y in 0..h {
                for x in 0..w {
                    let actual = surface_sample(&plane, x, y);
                    assert!((actual[0] - 100. * w as f32 / 65535.).abs() < 1e-4);
                    assert!((actual[1] - 200. * h as f32 / 65535.).abs() < 1e-4);
                    let edge = surface_sample(&step, x, y);
                    assert_eq!(&edge[..2], &[0., 0.], "false normal at {x},{y}");
                    assert_eq!(edge[2], step.get_pixel(x, y)[0] as f32 / 65535.);
                }
            }
        }
        assert_eq!(
            surface_sample(&DepthImage::from_pixel(1, 1, image::Luma([0])), 0, 0),
            [0., 0., 0., 1.]
        );
    }
    #[test]
    fn smooth_curvature_retains_surface_direction() {
        let map = DepthImage::from_fn(96, 64, |x, _| {
            let offset = x as i32 - 48;
            image::Luma([(20000 + 3 * offset * offset) as u16])
        });
        for x in [24, 48, 72] {
            let sample = surface_sample(&map, x, 32);
            let expected = 6. * (x as f32 - 48.) * 96. / 65535.;
            assert!((sample[0] - expected).abs() < 0.01);
            assert!(sample[1].abs() < 1e-6);
        }
    }

    #[test]
    fn transitional_depth_edges_do_not_invent_surface_bevels() {
        // Inferred silhouettes contain intermediate pixels, unlike an ideal
        // binary depth step. Each increment is below the old 4% range cutoff.
        for axis in 0..3 {
            for intermediates in 1..=3 {
                let map = DepthImage::from_fn(48, 32, |x, y| {
                    let coordinate = match axis {
                        0 => x,
                        1 => y,
                        _ => x + y,
                    };
                    let t = (coordinate as i32 - 12).clamp(0, intermediates + 1);
                    image::Luma([(18000 + 2000 * t) as u16])
                });
                for y in 0..map.height() {
                    for x in 0..map.width() {
                        let sample = surface_sample(&map, x, y);
                        assert!(
                            sample[0].abs() < 1e-6 && sample[1].abs() < 1e-6,
                            "false normal at {x},{y}, axis {axis}, transition {intermediates}: {sample:?}"
                        );
                        assert_eq!(sample[2], map.get_pixel(x, y)[0] as f32 / 65535.);
                    }
                }
            }
        }
    }

    #[test]
    fn legacy_and_hidden_recipes_are_identity() {
        assert_eq!(gpu_lights(&json!({})), [[0.; 4]; 20]);
        for r in [
            json!({"relightLights":[{"enabled":false}]}),
            json!({"relightLights":[{}],"sectionVisibility":{"relight":false}}),
        ] {
            assert_eq!(gpu_lights(&r), [[0.; 4]; 20]);
        }
    }
    #[test]
    fn mask_restriction_follows_bitmap_layers() {
        let layer = |id: &str, visible: bool, subs: usize| json!({"id": id, "visible": visible, "subMasks": vec![json!({}); subs]});
        let recipe = |mask_id: &str| {
            json!({
                "masks": [layer("hidden", false, 1), layer("empty", true, 0), layer("a", true, 1), layer("b", true, 2)],
                "relightLights": [{"maskId": mask_id, "invert": true, "pattern": "dots"}],
            })
        };
        // Flags: invert in bit 0, mask slot (bitmap index + 1) above.
        assert_eq!(gpu_lights(&recipe("a"))[3][1], (1 | (1 << 1)) as f32);
        assert_eq!(gpu_lights(&recipe("b"))[3][1], (1 | (2 << 1)) as f32);
        assert_eq!(gpu_lights(&recipe("deleted"))[3][1], 1.);
        for unavailable in ["hidden", "empty"] {
            assert_eq!(gpu_lights(&recipe(unavailable)), [[0.; 4]; 20]);
        }
        assert_eq!(gpu_lights(&recipe("a"))[3][0], 9.);
    }

    #[test]
    fn imported_patterns_share_one_atlas_per_light_set() {
        let png = |value: u8| {
            let mut out = std::io::Cursor::new(Vec::new());
            image::GrayImage::from_pixel(8, 4, image::Luma([value]))
                .write_to(&mut out, image::ImageFormat::Png)
                .unwrap();
            format!(
                "data:image/png;base64,{}",
                STANDARD.encode(out.into_inner())
            )
        };
        let light = |id: &str, value: u8| json!({"pattern": "custom", "customPattern": {"id": id, "data": png(value)}});
        let recipe =
            json!({"relightLights": [light("a", 0), {"pattern": "dots"}, {}, light("b", 100)]});
        let key = register_patterns(&recipe);
        assert_ne!(key, [0; 2]);
        assert_eq!(register_patterns(&recipe), key);
        let atlas = pattern_atlas(key).unwrap();
        assert_eq!(atlas.dimensions(), (PATTERN_SLOT * 2, PATTERN_SLOT * 2));
        assert_eq!(atlas.get_pixel(10, 10)[0], 0);
        assert_eq!(atlas.get_pixel(PATTERN_SLOT + 10, 10)[0], 255);
        assert_eq!(
            atlas.get_pixel(PATTERN_SLOT + 10, PATTERN_SLOT + 10)[0],
            100
        );
        assert_eq!(gpu_lights(&recipe)[3][0], CUSTOM_PATTERN as f32);
        // Without its bitmap a custom light is unpatterned, not an error.
        let missing = json!({"relightLights": [{"pattern": "custom"}]});
        assert_eq!(register_patterns(&missing), [0; 2]);
        assert_eq!(gpu_lights(&missing)[3][0], 0.);
        assert_eq!(
            register_patterns(&json!({"relightLights": [{"pattern": "dots"}]})),
            [0; 2]
        );
    }

    #[test]
    fn prepared_pattern_atlas_outlives_cache_eviction() {
        let png = {
            let mut out = std::io::Cursor::new(Vec::new());
            image::GrayImage::from_pixel(4, 4, image::Luma([7]))
                .write_to(&mut out, image::ImageFormat::Png)
                .unwrap();
            format!(
                "data:image/png;base64,{}",
                STANDARD.encode(out.into_inner())
            )
        };
        let recipe = |id: &str| json!({"relightLights": [{"pattern": "custom", "customPattern": {"id": id, "data": png}}]});
        let (_, queued) = prepare_patterns(&recipe("queued"));
        for i in 0..8 {
            prepare_patterns(&recipe(&format!("other-{i}")));
        }
        assert_eq!(queued.unwrap().get_pixel(1, 1)[0], 7);
    }

    #[test]
    fn imported_pattern_is_the_central_gray_square() {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(300, 100, |x, _| {
            if (100..200).contains(&x) {
                image::Rgb([255, 255, 255])
            } else {
                image::Rgb([0, 0, 0])
            }
        }));
        let decoded = image::load_from_memory(&pattern_png(&image).unwrap())
            .unwrap()
            .to_luma8();
        assert_eq!(decoded.dimensions(), (PATTERN_SLOT, PATTERN_SLOT));
        assert!(decoded.pixels().all(|p| p[0] > 250));
    }

    #[test]
    fn named_depth_skips_bitmap_hashing_and_survives_without_data() {
        let mut png = std::io::Cursor::new(Vec::new());
        DepthImage::from_pixel(4, 4, image::Luma([30000]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let mut recipe = json!({"relightLights": [{"intensity": 40}]});
        recipe["relightDepth"] = json!({
            "data": format!("data:image/png;base64,{}", STANDARD.encode(png.into_inner())),
            "geometry": geometry(&recipe),
            "cacheKey": "analysis-1",
        });
        let key = register_depth(&recipe);
        assert_ne!(key, [0; 4]);
        // A preview whose bitmap was sent before carries only its name.
        recipe["relightDepth"]["data"] = Value::Null;
        assert_eq!(register_depth(&recipe), key);
        assert!(depth(key).is_some());
    }

    #[test]
    fn range_version_two_changes_only_lights_that_carry_it() {
        let old = json!({"intensity": 50, "softness": 40, "diffusion": 10, "invert": true});
        let mut new = old.clone();
        new["rangeVersion"] = json!(2);
        let [a, b] = [old, new].map(|light| gpu_lights(&json!({"relightLights": [light]})));
        // The first range: linear up to 3, flags as before.
        assert_eq!(a[0][3], 1.5);
        assert_eq!(a[3][1], 1.);
        assert_eq!(b[3][1], 1. + RANGE_2_FLAG as f32);
        assert_eq!(b[0][3], RANGE_2_MAX_GAIN * 0.5f32.powf(RANGE_2_EXPONENT));
        // Everything else is the same uniform.
        assert_eq!([a[1], a[2], a[4]], [b[1], b[2], b[4]]);
        let gain = |intensity: f32| intensity_gain(&json!({"rangeVersion": 2}), intensity);
        assert_eq!(gain(100.), RANGE_2_MAX_GAIN);
        assert_eq!(gain(0.), 0.);
        assert!(gain(25.) < 25. / 100. * 3.);
        // The mask slot keeps its bits below the flag.
        assert!(crate::image_processing::MAX_MASKS << 1 < RANGE_2_FLAG as usize);
    }

    #[test]
    fn bounds_and_color_are_safe() {
        let p = gpu_lights(
            &json!({"relightLights":[{"intensity":999,"color":"#ff0000","size":0,"x":-999,"pattern":"window"}]}),
        );
        assert_eq!(p[0], [1., 0., 0., 3.]);
        assert_eq!(p[1][0], -1.);
        assert_eq!(p[1][2], 0.05);
        assert_eq!(p[3][0], 2.);
        assert!(
            gpu_lights(&json!({"relightLights":vec![json!({});100]}))
                .iter()
                .flatten()
                .all(|v| v.is_finite())
        );
    }
}
