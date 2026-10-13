//! Non-generative projected lighting in scene-linear RGB. Coordinates are
//! normalized to the developed photograph; no original pixels are changed.
use serde_json::Value;

pub const MAX_LIGHTS: usize = 4;

pub fn gpu_lights(recipe: &Value) -> [[f32; 4]; 16] {
    let mut out = [[0.; 4]; 16];
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
            _ => 0.,
        };
        let j = i * 4;
        out[j] = [
            linear(hex >> 16),
            linear(hex >> 8),
            linear(hex),
            n("intensity", 50., 0., 100.) / 100. * 3.,
        ];
        out[j + 1] = [
            n("x", 50., 0., 100.) / 100.,
            n("y", 50., 0., 100.) / 100.,
            n("size", 60., 5., 200.) / 100.,
            f32::from(light["type"].as_str() == Some("spot")),
        ];
        out[j + 2] = [
            n("rotation", -30., -180., 180.).to_radians(),
            n("scale", 50., 5., 200.) / 100.,
            n("softness", 30., 0., 100.) / 100.,
            n("diffusion", 25., 0., 100.) / 100.,
        ];
        out[j + 3] = [
            pattern,
            f32::from(light["invert"].as_bool() == Some(true)),
            n("shadows", 35., 0., 100.) / 100.,
            n("elevation", 45., 5., 90.).to_radians(),
        ];
    }
    out
}

use base64::{Engine as _, engine::general_purpose::STANDARD};
type DepthImage = image::ImageBuffer<image::Luma<u16>, Vec<u16>>;
use std::collections::{VecDeque, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

type DepthEntry = (u64, Arc<DepthImage>);
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
    let Some(data) = recipe["relightDepth"]["data"].as_str() else {
        return [0; 4];
    };
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
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
        cache.push_front((key, Arc::new(depth)));
        cache.truncate(8);
    }
    [key as u32, (key >> 32) as u32, 0, 0]
}

pub fn depth(key: [u32; 4]) -> Option<Arc<DepthImage>> {
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
/// at image boundaries. Unlike blurring then differentiating, a depth step
/// between two flat surfaces cannot create a bright/dark silhouette halo.
/// Derivatives are per normalized image coordinate; height stays unfiltered.
pub fn surface_sample(depth: &DepthImage, x: u32, y: u32) -> [f32; 4] {
    let center = depth.get_pixel(x, y)[0] as f32 / 65535.;
    let slope = |horizontal: bool| {
        let mut sw = 0.;
        let mut st = 0.;
        let mut sz = 0.;
        let mut stt = 0.;
        let mut stz = 0.;
        for offset in -3i32..=3 {
            let px = x as i32 + if horizontal { offset } else { 0 };
            let py = y as i32 + if horizontal { 0 } else { offset };
            if px < 0 || py < 0 || px >= depth.width() as i32 || py >= depth.height() as i32 {
                continue;
            }
            let z = depth.get_pixel(px as u32, py as u32)[0] as f32 / 65535. - center;
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
        if determinant > 1e-6 {
            (sw * stz - st * sz) / determinant
                * if horizontal {
                    depth.width()
                } else {
                    depth.height()
                } as f32
        } else {
            0.
        }
    };
    [slope(true), slope(false), center, 1.]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
    fn legacy_and_hidden_recipes_are_identity() {
        assert_eq!(gpu_lights(&json!({})), [[0.; 4]; 16]);
        for r in [
            json!({"relightLights":[{"enabled":false}]}),
            json!({"relightLights":[{}],"sectionVisibility":{"relight":false}}),
        ] {
            assert_eq!(gpu_lights(&r), [[0.; 4]; 16]);
        }
    }
    #[test]
    fn bounds_and_color_are_safe() {
        let p = gpu_lights(
            &json!({"relightLights":[{"intensity":999,"color":"#ff0000","size":0,"x":-999,"pattern":"window"}]}),
        );
        assert_eq!(p[0], [1., 0., 0., 3.]);
        assert_eq!(p[1][0], 0.);
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
