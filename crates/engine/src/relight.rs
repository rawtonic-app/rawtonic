//! Non-generative projected lighting in scene-linear RGB. Coordinates are
//! normalized to the developed photograph; no original pixels are changed.
use serde_json::Value;
#[path = "relight_surface.rs"]
mod surface;
#[cfg(any(test, feature = "test-support"))]
pub use surface::test_face_geometry;
pub use surface::{surface_texture, surface_textures};

pub const MAX_LIGHTS: usize = 4;

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
        let n = |key: &str, default: f64, lo: f64, hi: f64| {
            light[key]
                .as_f64()
                .filter(|v| v.is_finite())
                .unwrap_or(default)
                .clamp(lo, hi) as f32
        };
        let color = light["color"].as_str().unwrap_or("#fff1db");
        let hex = color
            .strip_prefix('#')
            .filter(|s| s.len() == 6)
            .and_then(|s| u32::from_str_radix(s, 16).ok())
            .unwrap_or(0xfff1db);
        let linear = |v: u32| {
            let c = (v & 255) as f32 / 255.;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let pattern = match light["pattern"].as_str() {
            Some("blinds") => 1.,
            Some("window") => 2.,
            Some("strip") => 3.,
            Some("foliage") => 4.,
            Some("grid") => 5.,
            Some("palm") => 6.,
            Some("branches") => 7.,
            Some("checkerboard") => 8.,
            Some("dots") => 9.,
            Some("horizontal") => 10.,
            Some("diagonal") => 11.,
            Some("weave") => 12.,
            Some("noise") => 13.,
            Some("crosshatch") => 14.,
            _ => 0.,
        };
        let j = i * 5;
        out[j] = [
            linear(hex >> 16),
            linear(hex >> 8),
            linear(hex),
            n("intensity", 50., 0., 100.) / 100. * 3.,
        ];
        out[j + 1] = [
            n("x", 50., -100., 200.) / 100.,
            n("y", 50., -100., 200.) / 100.,
            n("size", 60., 5., 200.) / 100.,
            f32::from(light["type"].as_str() == Some("spot")),
        ];
        out[j + 2] = [
            n("rotation", -30., -180., 180.).to_radians(),
            n("scale", 50., 5., 300.) / 100.,
            n("softness", 30., 0., 100.) / 100.,
            n("diffusion", 25., 0., 100.) / 100.,
        ];
        out[j + 3] = [
            pattern,
            f32::from(light["invert"].as_bool() == Some(true)),
            n("shadows", 35., 0., 100.) / 100.,
            n("elevation", 45., 5., 90.).to_radians(),
        ];
        out[j + 4] = [
            n("sourceX", out[j + 1][0] as f64 * 100., -100., 200.) / 100.,
            n("sourceY", out[j + 1][1] as f64 * 100., -100., 200.) / 100.,
            n(
                "patternRotation",
                n("rotation", -30., -180., 180.) as f64,
                -180.,
                180.,
            )
            .to_radians(),
            n("distance", 60., 10., 300.) / 100.,
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
        if recipe["relightDepth"]["version"].as_u64()? != 4 {
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

pub struct DepthSurface {
    pub camera: Option<Camera>,
    pub image: DepthImage,
    pub faces: Vec<Vec<[f32; 4]>>,
    pub refined: bool,
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
    if recipe["sectionVisibility"]["relight"].as_bool() == Some(false)
        || !recipe["relightLights"].as_array().is_some_and(|lights| {
            lights.iter().any(|l| {
                l["enabled"].as_bool() != Some(false) && l["intensity"].as_f64().unwrap_or(50.) > 0.
            })
        })
        || recipe["relightDepth"]["geometry"] != geometry(recipe)
    {
        return [0; 4];
    }
    let camera = Camera::from_recipe(recipe);
    if recipe["relightDepth"]["version"].as_u64().unwrap_or(0) >= 4 && camera.is_none() {
        return [0; 4];
    }
    let Some(data) = recipe["relightDepth"]["data"].as_str() else {
        return [0; 4];
    };
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    recipe["relightDepth"]["camera"]
        .to_string()
        .hash(&mut hasher);
    recipe["relightDepth"]["version"].as_u64().hash(&mut hasher);
    recipe["relightDepth"]["faces"]
        .to_string()
        .hash(&mut hasher);
    let key = hasher.finish().max(1);
    let cache = DEPTHS.get_or_init(Mutex::default);
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(index) = cache.iter().position(|(k, _)| *k == key) {
        let entry = cache.remove(index).unwrap();
        cache.push_front(entry);
    } else {
        let Some(bytes) = STANDARD
            .decode(data.split_once(',').map_or(data, |(_, d)| d))
            .ok()
        else {
            return [0; 4];
        };
        let Some(image) = image::load_from_memory(&bytes).ok() else {
            return [0; 4];
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
            }),
        ));
        cache.truncate(8);
    }
    [key as u32, (key >> 32) as u32, 0, 0]
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
        let first = register_depth(&recipe);
        assert_ne!(first, [0; 4]);
        assert_eq!(first, register_depth(&recipe));
        assert!(depth(first).unwrap().camera.is_some());
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
        recipe["relightDepth"]["version"] = serde_json::json!(5);
        assert_eq!(register_depth(&recipe), [0; 4]);
        recipe["relightDepth"]["version"] = serde_json::json!(3);
        assert!(depth(register_depth(&recipe)).unwrap().camera.is_none());
        assert_eq!(Camera::uniforms(&recipe), ([0.; 4], [0.; 4]));
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
