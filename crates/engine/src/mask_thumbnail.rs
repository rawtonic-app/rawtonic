//! Mask thumbnails for the layer list (Lightroom's Masks panel shows one per
//! mask): the mask in white on black, in the photo's frame as the viewer shows
//! it (orientation, straightening, crop), a few dozen pixels across.
//!
//! A thumbnail is box-filtered down from a mask that is already rasterized:
//! the preview's (when it is cached for the same frame) or one drawn in a
//! small frame (`MaskFrame::fitting`) a few times the thumbnail's size, so
//! edges stay smooth without rasterizing the full-resolution mask.

use std::sync::Mutex;

use base64::{Engine as _, engine::general_purpose};
use image::GrayImage;
use rayon::prelude::*;
use serde_json::Value;

use crate::image_processing::{Crop, crop_rect};
use crate::mask_generation::{
    MaskBitmapCache, MaskDefinition, OverlayStyle, ParametricMaskSource, PhotoGeometry, SubMask,
    SubMaskMode, cached_or_generated_mask, encode_mask_png, peek_cached_mask,
};

/// Mask pixels per thumbnail pixel (each way) when a mask is drawn for a
/// thumbnail: a 4 × 4 box per pixel smooths edges at little cost.
pub const THUMBNAIL_OVERSAMPLING: u32 = 4;

/// Largest thumbnail side served (device pixels).
pub const MAX_THUMBNAIL_SIDE: u32 = 256;

/// The frame a mask is rasterized in, with `generate_mask_bitmap`'s
/// arguments: size, mask pixels per full-resolution pixel, and the crop's
/// origin in mask pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskFrame {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub crop_offset: (f32, f32),
}

impl MaskFrame {
    /// The photo's frame as the viewer shows it (after orientation, flips,
    /// straightening and crop, as `apply_spatial_transformations` makes it)
    /// for a photo of `full_dims` full-resolution pixels, scaled so that its
    /// longer side is `longest` pixels (never above full resolution).
    pub fn fitting(adjustments: &Value, full_dims: (u32, u32), longest: u32) -> Option<Self> {
        let steps = adjustments["orientationSteps"].as_u64().unwrap_or(0) % 4;
        let (oriented_w, oriented_h) = if steps % 2 == 1 {
            (full_dims.1, full_dims.0)
        } else {
            full_dims
        };
        if oriented_w == 0 || oriented_h == 0 || longest == 0 {
            return None;
        }
        let (frame_w, frame_h) = crop_rect(&adjustments["crop"], oriented_w, oriented_h)
            .map_or((oriented_w, oriented_h), |(_, _, w, h)| (w, h));
        // The preview's crop offset: the crop's unrounded origin.
        let origin = serde_json::from_value::<Crop>(adjustments["crop"].clone())
            .map_or((0.0, 0.0), |c| (c.x as f32, c.y as f32));
        let scale = (longest as f32 / frame_w.max(frame_h) as f32).min(1.0);
        Some(Self {
            width: ((frame_w as f32 * scale).round() as u32).max(1),
            height: ((frame_h as f32 * scale).round() as u32).max(1),
            scale,
            crop_offset: (origin.0 * scale, origin.1 * scale),
        })
    }
}

/// The largest size with `width`:`height`'s aspect that fits `max_width` ×
/// `max_height` (at least 1 × 1).
pub fn fit_within(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (max_width.max(1), max_height.max(1));
    }
    let scale = (max_width as f64 / width as f64).min(max_height as f64 / height as f64);
    (
        ((width as f64 * scale).round() as u32).clamp(1, max_width.max(1)),
        ((height as f64 * scale).round() as u32).clamp(1, max_height.max(1)),
    )
}

/// `mask` reduced to `width` × `height` by a box filter: every thumbnail pixel
/// is the mean of the mask pixels it covers (each mask pixel counts towards
/// exactly one thumbnail pixel). Sizes above the mask's are clamped to it.
pub fn downscale_box(mask: &GrayImage, width: u32, height: u32) -> GrayImage {
    let (source_w, source_h) = mask.dimensions();
    if source_w == 0 || source_h == 0 {
        return GrayImage::new(width.max(1), height.max(1));
    }
    let width = width.clamp(1, source_w);
    let height = height.clamp(1, source_h);
    if (width, height) == (source_w, source_h) {
        return mask.clone();
    }
    // Mask pixel x falls in thumbnail column floor(x * width / source_w):
    // column o covers [bound(o), bound(o + 1)) with bound(o) = ceil(o *
    // source_w / width), and likewise for rows.
    let bound = |o: u32, source: u32, target: u32| {
        ((o as u64 * source as u64).div_ceil(target as u64)) as usize
    };
    let columns: Vec<(usize, usize)> = (0..width)
        .map(|o| (bound(o, source_w, width), bound(o + 1, source_w, width)))
        .collect();
    let raw = mask.as_raw();
    let stride = source_w as usize;
    let mut out = GrayImage::new(width, height);
    out.as_mut()
        .par_chunks_mut(width as usize)
        .enumerate()
        .for_each(|(row, line)| {
            let (y0, y1) = (
                bound(row as u32, source_h, height),
                bound(row as u32 + 1, source_h, height),
            );
            let mut sums = vec![0u64; width as usize];
            for y in y0..y1 {
                let source_line = &raw[y * stride..(y + 1) * stride];
                for (sum, &(x0, x1)) in sums.iter_mut().zip(&columns) {
                    *sum += source_line[x0..x1]
                        .iter()
                        .map(|&v| u64::from(v))
                        .sum::<u64>();
                }
            }
            for ((pixel, sum), &(x0, x1)) in line.iter_mut().zip(&sums).zip(&columns) {
                let n = ((x1 - x0) * (y1 - y0)).max(1) as u64;
                *pixel = ((sum + n / 2) / n) as u8;
            }
        });
    out
}

/// A layer's mask as its thumbnail shows it: shown even when the layer is
/// hidden, at full opacity (opacity scales the layer's adjustments; the
/// overlay shows the mask itself, too).
pub fn layer_thumbnail_definition(layer: &MaskDefinition) -> MaskDefinition {
    MaskDefinition {
        visible: true,
        opacity: 100.0,
        ..layer.clone()
    }
}

/// One component's own coverage (with its invert and opacity), whatever its
/// mode in the layer: Lightroom's component thumbnails show the component,
/// the row shows how it combines.
pub fn component_thumbnail_definition(component: &SubMask) -> MaskDefinition {
    MaskDefinition {
        id: component.id.clone(),
        name: String::new(),
        visible: true,
        invert: false,
        opacity: 100.0,
        mask_version: None,
        refine_edge: None,
        adjustments: Value::Null,
        sub_masks: vec![SubMask {
            visible: true,
            mode: SubMaskMode::Additive,
            ..component.clone()
        }],
    }
}

/// The thumbnail as a PNG data URL (white mask on black).
pub fn thumbnail_data_url(thumbnail: &GrayImage) -> Result<String, String> {
    let png = encode_mask_png(thumbnail, OverlayStyle::Grayscale)?;
    Ok(format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(png)
    ))
}

/// Where thumbnails come from: the preview's masks when the preview is
/// current (its frame and mask cache: a hit costs only the reduction), else
/// masks drawn in `small` (kept in `small_cache`).
pub struct ThumbnailSources<'a> {
    pub preview: Option<(MaskFrame, &'a Mutex<MaskBitmapCache>)>,
    pub small: MaskFrame,
    pub small_cache: &'a Mutex<MaskBitmapCache>,
    /// The photo for colour, luminance and Auto Mask components.
    pub warped: Option<&'a ParametricMaskSource>,
    /// The photo's geometry the masks are placed by (AI masks follow it).
    pub geometry: &'a PhotoGeometry,
}

impl ThumbnailSources<'_> {
    /// `def`'s thumbnail fitting `max_width` × `max_height`, as a data URL;
    /// None when it has no pixels yet (an AI mask still computing).
    pub fn thumbnail(
        &self,
        def: &MaskDefinition,
        max_width: u32,
        max_height: u32,
    ) -> Result<Option<String>, String> {
        if !has_pixels(def) {
            return Ok(None);
        }
        let cached = self.preview.and_then(|(frame, cache)| {
            peek_cached_mask(
                cache,
                def,
                frame.width,
                frame.height,
                frame.scale,
                frame.crop_offset,
                self.geometry,
                self.warped,
            )
        });
        let mask = match cached {
            Some(mask) => mask,
            None => {
                let frame = self.small;
                match cached_or_generated_mask(
                    self.small_cache,
                    def,
                    frame.width,
                    frame.height,
                    frame.scale,
                    frame.crop_offset,
                    self.geometry,
                    self.warped,
                )? {
                    Some(mask) => mask,
                    None => return Ok(None),
                }
            }
        };
        let (width, height) = fit_within(mask.width(), mask.height(), max_width, max_height);
        thumbnail_data_url(&downscale_box(&mask, width, height)).map(Some)
    }
}

/// False while a component's pixels are missing: an AI mask whose bitmap is
/// still being computed draws nothing, which is no thumbnail.
fn has_pixels(def: &MaskDefinition) -> bool {
    def.sub_masks.iter().filter(|sub| sub.visible).all(|sub| {
        !sub.mask_type.starts_with("ai-")
            || ["maskDataBase64", "mask_data_base64"]
                .iter()
                .any(|key| sub.parameters[*key].is_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;
    use serde_json::json;

    #[test]
    fn box_filter_averages_the_pixels_each_thumbnail_pixel_covers() {
        // A 6 × 4 mask: the left half white, the right half black, and one
        // grey pixel; reduced by 3 × 2 boxes.
        let mut mask = GrayImage::from_fn(6, 4, |x, _| Luma([if x < 3 { 255 } else { 0 }]));
        mask.put_pixel(5, 3, Luma([60]));
        let thumbnail = downscale_box(&mask, 2, 2);
        assert_eq!(thumbnail.dimensions(), (2, 2));
        assert_eq!(thumbnail.as_raw(), &[255, 0, 255, 10]);
    }

    #[test]
    fn box_filter_keeps_the_mean_coverage_of_uneven_ratios() {
        // A soft synthetic blob, 45 MP-like aspect, reduced 1000 × 667 → 63 × 42
        // (bins of 15 or 16 pixels): each thumbnail pixel lies within the
        // range of its pixels, and the overall coverage is kept.
        let (w, h) = (1000u32, 667u32);
        let mask = GrayImage::from_fn(w, h, |x, y| {
            let d = ((x as f32 - 600.0).powi(2) + (y as f32 - 300.0).powi(2)).sqrt();
            Luma([(255.0 * (1.0 - (d - 150.0) / 100.0).clamp(0.0, 1.0)) as u8])
        });
        let (tw, th) = fit_within(w, h, 64, 42);
        assert_eq!((tw, th), (63, 42));
        let thumbnail = downscale_box(&mask, tw, th);
        let mean = |image: &GrayImage| {
            image.as_raw().iter().map(|&v| v as f64).sum::<f64>() / image.as_raw().len() as f64
        };
        assert!((mean(&mask) - mean(&thumbnail)).abs() < 1.0);
        // Centre of the blob is white, a far corner black.
        assert_eq!(thumbnail.get_pixel(38, 19)[0], 255);
        assert_eq!(thumbnail.get_pixel(0, 41)[0], 0);
        // A uniform mask stays uniform, whatever the ratio.
        let grey = GrayImage::from_pixel(97, 61, Luma([77]));
        assert!(
            downscale_box(&grey, 13, 7)
                .as_raw()
                .iter()
                .all(|&v| v == 77)
        );
        // Sizes above the mask's are clamped to it.
        assert_eq!(downscale_box(&grey, 500, 500).dimensions(), (97, 61));
    }

    #[test]
    fn frame_follows_crop_and_orientation() {
        // A 6000 × 4000 photo turned a quarter: 4000 × 6000; a 2000 × 3000
        // crop at (1000, 600), at a longest side of 300 px.
        let adjustments = json!({
            "orientationSteps": 1,
            "crop": {"x": 1000.0, "y": 600.0, "width": 2000.0, "height": 3000.0}
        });
        let frame = MaskFrame::fitting(&adjustments, (6000, 4000), 300).unwrap();
        assert_eq!((frame.width, frame.height), (200, 300));
        assert_eq!(frame.scale, 0.1);
        assert_eq!(frame.crop_offset, (100.0, 60.0));
        // No crop: the whole (oriented) photo.
        let frame = MaskFrame::fitting(&json!({}), (6000, 4000), 300).unwrap();
        assert_eq!(
            (frame.width, frame.height, frame.crop_offset),
            (300, 200, (0.0, 0.0))
        );
        // Never above full resolution.
        let frame = MaskFrame::fitting(&json!({}), (120, 80), 300).unwrap();
        assert_eq!((frame.width, frame.height, frame.scale), (120, 80, 1.0));
    }

    #[test]
    fn thumbnail_is_a_grayscale_png_data_url() {
        let url = thumbnail_data_url(&GrayImage::from_pixel(4, 3, Luma([200]))).unwrap();
        let bytes = general_purpose::STANDARD
            .decode(url.strip_prefix("data:image/png;base64,").unwrap())
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_luma8();
        assert_eq!(decoded.dimensions(), (4, 3));
        assert!(decoded.as_raw().iter().all(|&v| v == 200));
    }

    #[test]
    fn a_component_thumbnail_shows_the_component_alone() {
        let layer: MaskDefinition = serde_json::from_value(json!({
            "id": "l", "name": "Layer", "visible": false, "invert": true, "opacity": 40.0,
            "adjustments": {},
            "subMasks": [
                {"id": "a", "type": "all", "visible": true, "mode": "additive", "parameters": {}},
                {"id": "r", "type": "all", "visible": false, "invert": true, "mode": "subtractive", "parameters": {}}
            ]
        }))
        .unwrap();
        let def = component_thumbnail_definition(&layer.sub_masks[1]);
        assert!(def.produces_bitmap() && !def.invert && def.opacity == 100.0);
        assert_eq!(def.sub_masks.len(), 1);
        assert_eq!(def.sub_masks[0].mode, SubMaskMode::Additive);
        // Its own invert stays: an inverted "all" is empty.
        let mask = crate::mask_generation::generate_mask_bitmap(
            &def,
            8,
            8,
            1.0,
            (0.0, 0.0),
            &PhotoGeometry::default(),
            None,
        )
        .unwrap();
        assert!(mask.as_raw().iter().all(|&v| v == 0));
        // The layer's thumbnail shows its mask while hidden, at full opacity.
        let def = layer_thumbnail_definition(&layer);
        let mask = crate::mask_generation::generate_mask_bitmap(
            &def,
            8,
            8,
            1.0,
            (0.0, 0.0),
            &PhotoGeometry::default(),
            None,
        )
        .unwrap();
        // Visible "all" minus nothing (the hidden component), inverted: empty.
        assert!(mask.as_raw().iter().all(|&v| v == 0));
    }

    fn decode(url: &str) -> GrayImage {
        let bytes = general_purpose::STANDARD
            .decode(url.strip_prefix("data:image/png;base64,").unwrap())
            .unwrap();
        image::load_from_memory(&bytes).unwrap().to_luma8()
    }

    #[test]
    fn thumbnails_reuse_the_preview_mask_and_match_the_small_frame() {
        let radial: MaskDefinition = serde_json::from_value(json!({
            "id": "l", "name": "Layer", "visible": true, "invert": false, "adjustments": {},
            "subMasks": [{"id": "r", "type": "radial", "visible": true, "mode": "additive",
                "parameters": {"centerX": 2000.0, "centerY": 1500.0, "radiusX": 900.0,
                    "radiusY": 600.0, "rotation": 0.0, "feather": 0.3}}]
        }))
        .unwrap();
        // A 6000 × 4000 photo cropped to its right 4000 × 3000.
        let adjustments =
            json!({"crop": {"x": 2000.0, "y": 500.0, "width": 4000.0, "height": 3000.0}});
        let preview = MaskFrame::fitting(&adjustments, (6000, 4000), 1200).unwrap();
        let small = MaskFrame::fitting(&adjustments, (6000, 4000), 320).unwrap();
        let preview_cache = Mutex::new(MaskBitmapCache::new());
        let small_cache = Mutex::new(MaskBitmapCache::new());
        let sources = ThumbnailSources {
            preview: Some((preview, &preview_cache)),
            small,
            small_cache: &small_cache,
            warped: None,
            geometry: &PhotoGeometry::default(),
        };
        // Not in the preview's cache: drawn in the small frame.
        let drawn = decode(&sources.thumbnail(&radial, 80, 80).unwrap().unwrap());
        assert_eq!(drawn.dimensions(), (80, 60));
        // Cached for the preview: reduced from it, without drawing.
        let mask = cached_or_generated_mask(
            &preview_cache,
            &radial,
            preview.width,
            preview.height,
            preview.scale,
            preview.crop_offset,
            &PhotoGeometry::default(),
            None,
        )
        .unwrap()
        .unwrap();
        let reduced = decode(&sources.thumbnail(&radial, 80, 80).unwrap().unwrap());
        assert_eq!(reduced.dimensions(), (80, 60));
        // Both show the radial left of the cropped frame's centre (its centre
        // at (0, 1000) of the crop): white at the left edge, black at the right.
        let at = |image: &GrayImage, x, y| image.get_pixel(x, y)[0];
        assert_eq!(at(&reduced, 0, 20), 255);
        assert_eq!(at(&reduced, 79, 20), 0);
        let differences = drawn.as_raw().iter().zip(reduced.as_raw());
        assert!(
            differences
                .map(|(a, b)| a.abs_diff(*b) as u32)
                .max()
                .unwrap()
                <= 8
        );
        assert_eq!(
            downscale_box(&mask, 80, 60).as_raw(),
            reduced.as_raw(),
            "the preview's mask, reduced"
        );

        // An AI mask still computing has no pixels: no thumbnail.
        let computing: MaskDefinition = serde_json::from_value(json!({
            "id": "s", "name": "Subject", "visible": true, "invert": false, "adjustments": {},
            "subMasks": [{"id": "a", "type": "ai-foreground", "visible": true, "mode": "additive",
                "parameters": {"maskDataBase64": null}}]
        }))
        .unwrap();
        assert_eq!(sources.thumbnail(&computing, 80, 80).unwrap(), None);
    }

    /// Time per thumbnail for a 45 MP photo (8256 × 5504) with a radial, a
    /// linear gradient, 40 brush strokes, a Subject mask stored at the
    /// photo's size and a luminance range: from the preview's cached masks
    /// (1920 × 1280), drawn in the small frame (first time), and from the
    /// small cache. `cargo test -p rawtonic-engine --release --lib
    /// measures_thumbnails_of_a_45_mp_photo -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn measures_thumbnails_of_a_45_mp_photo() {
        use crate::mask_generation::cached_or_generated_mask;
        use std::sync::Arc;
        use std::time::Instant;

        let full = (8256u32, 5504u32);
        let subject = GrayImage::from_fn(full.0, full.1, |x, y| {
            let d = ((x as f32 - 4000.0).powi(2) / 4.0 + (y as f32 - 3000.0).powi(2)).sqrt();
            Luma([(255.0 * (1.0 - (d - 1500.0) / 40.0).clamp(0.0, 1.0)) as u8])
        });
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma8(subject)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let subject_url = format!(
            "data:image/png;base64,{}",
            general_purpose::STANDARD.encode(&png)
        );
        let photo = image::DynamicImage::ImageRgb32F(image::Rgb32FImage::from_fn(
            full.0,
            full.1,
            |x, y| image::Rgb([x as f32 / full.0 as f32, y as f32 / full.1 as f32, 0.3]),
        ));
        let warped =
            ParametricMaskSource::new(Arc::new(photo), (full.0 as f64, full.1 as f64)).unwrap();
        let lines: Vec<Value> = (0..40)
            .map(|i| {
                json!({"tool": "brush", "brushSize": 300.0, "feather": 0.5,
                    "points": (0..30).map(|p| json!({"x": 500.0 + p as f64 * 200.0, "y": 400.0 + i as f64 * 120.0})).collect::<Vec<_>>()})
            })
            .collect();
        let sub = |id: &str, kind: &str, parameters: Value| json!({"id": id, "type": kind, "visible": true, "mode": "additive", "parameters": parameters});
        let layer = |id: &str, subs: Vec<Value>| -> MaskDefinition {
            serde_json::from_value(
                json!({"id": id, "name": id, "visible": true, "invert": false,
                "opacity": 100.0, "adjustments": {}, "subMasks": subs}),
            )
            .unwrap()
        };
        let layers = vec![
            layer(
                "radial",
                vec![sub(
                    "r",
                    "radial",
                    json!({"centerX": 4128.0, "centerY": 2752.0,
                "radiusX": 2500.0, "radiusY": 1800.0, "rotation": 10.0, "feather": 0.6}),
                )],
            ),
            layer(
                "linear",
                vec![sub(
                    "g",
                    "linear",
                    json!({"startX": 4000.0, "startY": 0.0,
                "endX": 4000.0, "endY": 2500.0, "range": 600.0}),
                )],
            ),
            layer("brush", vec![sub("b", "brush", json!({"lines": lines}))]),
            layer(
                "subject",
                vec![sub(
                    "s",
                    "ai-foreground",
                    json!({"maskDataBase64": subject_url}),
                )],
            ),
            layer(
                "luma",
                vec![sub(
                    "l",
                    "luma-range",
                    json!({"rangeLow": 20.0, "rangeHigh": 80.0, "falloff": 20.0}),
                )],
            ),
        ];
        let adjustments = json!({});
        let preview = MaskFrame::fitting(&adjustments, full, 1920).unwrap();
        let (thumb_w, thumb_h) = (84, 56);
        let small =
            MaskFrame::fitting(&adjustments, full, thumb_w * THUMBNAIL_OVERSAMPLING).unwrap();
        let preview_cache = Mutex::new(MaskBitmapCache::new());
        let small_cache = Mutex::new(MaskBitmapCache::new());
        // The preview rasterizes its masks first (not timed here).
        for def in &layers {
            cached_or_generated_mask(
                &preview_cache,
                def,
                preview.width,
                preview.height,
                preview.scale,
                preview.crop_offset,
                &PhotoGeometry::default(),
                Some(&warped),
            )
            .unwrap();
        }
        let with_preview = ThumbnailSources {
            preview: Some((preview, &preview_cache)),
            small,
            small_cache: &small_cache,
            warped: Some(&warped),
            geometry: &PhotoGeometry::default(),
        };
        let without_preview = ThumbnailSources {
            preview: None,
            ..with_preview
        };
        // Median of 9 runs; the first draw in the small frame uses a fresh
        // cache each run (brush prefixes stay warm, as after a stroke).
        let median = |mut runs: Vec<f64>| {
            runs.sort_by(f64::total_cmp);
            runs[runs.len() / 2]
        };
        let time = |sources: &ThumbnailSources, def: &MaskDefinition| {
            let started = Instant::now();
            let url = sources.thumbnail(def, thumb_w, thumb_h).unwrap().unwrap();
            (started.elapsed().as_secs_f64() * 1000.0, url.len())
        };
        time(&with_preview, &layers[0]);
        println!(
            "frame: preview {}x{}, small {}x{}, thumbnail {thumb_w}x{thumb_h}",
            preview.width, preview.height, small.width, small.height
        );
        for def in &layers {
            let cached = median((0..9).map(|_| time(&with_preview, def).0).collect());
            let first = median(
                (0..9)
                    .map(|_| {
                        let fresh = Mutex::new(MaskBitmapCache::new());
                        time(
                            &ThumbnailSources {
                                small_cache: &fresh,
                                ..without_preview
                            },
                            def,
                        )
                        .0
                    })
                    .collect(),
            );
            let again = median((0..9).map(|_| time(&without_preview, def).0).collect());
            let bytes = time(&with_preview, def).1;
            println!(
                "{:8} preview cache {cached:6.2} ms | small frame first {first:7.2} ms, cached {again:5.2} ms | {bytes} B data URL",
                def.id
            );
        }
    }
}
