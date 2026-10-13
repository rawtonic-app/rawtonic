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
        let depth = image.thumbnail(1024, 1024).to_luma16();
        // Smooth quantization before differentiating; never upscale to photo size.
        let depth = image::imageops::blur(&depth, 1.2);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
