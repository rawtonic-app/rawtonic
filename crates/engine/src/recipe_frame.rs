//! The photo size a recipe's pixel geometry was made on (`frameSize`), and
//! the migration of camera-TIFF edits made on the embedded thumbnail.
//!
//! Crop (`crop`, pixels of the photo as shown), spots (`spots`, pixels of
//! the mask frame) and AI bitmaps (`maskDataBase64`, the photo's own frame)
//! are stored in absolute pixels; drawn masks record the photo's size in
//! their `frame` (`mask_generation::ShapeFrame`), the rest did not. The
//! editor stamps every recipe it saves with the size of the photo it was
//! edited on (`KEY`: the photo's own size, full resolution, before
//! orientation; `src/hooks/useEditorActions.ts`), so a later change of the
//! photo's decoded size can move them with the photo ([`scale_geometry`]).
//!
//! Camera RAW TIFFs (Phase One, Kodak DCS, Canon EOS-1D/1Ds, … with a
//! `.tif` extension) developed as ordinary TIFFs until 0.1.0-alpha.17.1
//! (PR #916): the image decoder read their first image, the embedded
//! thumbnail (156 × 104 … 608 × 456), so their edits were made on it. Since
//! then they develop their raw data (2496 × 1662 … 7320 × 5484). An edit of
//! such a file without the stamp is moved from the thumbnail to the full
//! frame when it opens and renders ([`conform`]), unless the recipe shows
//! that it was made on the full frame: no version tells the two apart
//! (render version 13 spans both), so the recipe's own geometry decides
//! ([`made_on_thumbnail`]). Nothing is written until the user saves.
//! Notes: benchmark-results/tifr1-2026-11-21, tifr2-2026-11-21.

use std::collections::HashMap;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;
use rawtonic_recipe::geometry;
use serde_json::{Value, json};

/// The recipe key: `{"width": w, "height": h}`, the photo's own size (full
/// resolution, before orientation) the recipe's pixel geometry refers to.
pub const KEY: &str = "frameSize";

/// The size a recipe records ([`KEY`]), when it is usable.
pub fn stamped(adjustments: &Value) -> Option<(f64, f64)> {
    let frame = adjustments.get(KEY)?;
    let w = frame.get("width")?.as_f64()?;
    let h = frame.get("height")?.as_f64()?;
    (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0).then_some((w, h))
}

/// Records that `adjustments` was made on a photo of `width` × `height`.
pub fn stamp(adjustments: &mut Value, width: u32, height: u32) {
    if let Some(map) = adjustments.as_object_mut() {
        map.insert(KEY.to_string(), json!({"width": width, "height": height}));
    }
}

/// A camera TIFF's two frames: the one the image decoder developed until
/// 0.1.0-alpha.17.1 and the one its raw data develops now (each the
/// photo's own size, after the file's orientation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TiffFrames {
    pub thumbnail: (u32, u32),
    pub full: (u32, u32),
}

/// The frames of the camera RAW TIFF at `path`, when they differ: None for
/// other files, files the RAW decoder cannot frame, and files whose two
/// developments do not have the same shape (aspect ratios more than 3 %
/// apart: a quarter turn between them would need more than a scale, so
/// such edits are left as they are; none is known). Both show the camera's
/// upright view: the Phase One H 25 stores its raw data turned (EXIF
/// orientation 6 from the IIQ data) and its thumbnail upright (1), and both
/// develop 4:3 landscape. Read once per size and modification time.
pub fn tiff_frames(path: &Path) -> Option<TiffFrames> {
    let extension = path.extension()?.to_str()?;
    if !crate::formats::is_tiff_extension(extension) || !crate::formats::is_raw_file(path) {
        return None;
    }
    type Cache = Mutex<HashMap<PathBuf, (u64, std::time::SystemTime, Option<TiffFrames>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let meta = std::fs::metadata(path).ok()?;
    let stamp = (meta.len(), meta.modified().ok()?);
    if let Some((len, modified, found)) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .cloned()
        && (len, modified) == stamp
    {
        return found;
    }
    let found = read_tiff_frames(path);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_path_buf(), (stamp.0, stamp.1, found));
    found
}

fn read_tiff_frames(path: &Path) -> Option<TiffFrames> {
    let thumbnail = raster_frame(path)?;
    let source = rawler::rawsource::RawSource::new(path).ok()?;
    let full = rawtonic_rawdecode::source_frame::frame(
        &source,
        rawtonic_rawdecode::source_frame::VERSION,
    )?
    .size();
    if thumbnail == full || [thumbnail.0, thumbnail.1, full.0, full.1].contains(&0) {
        return None;
    }
    let aspect = |(w, h): (u32, u32)| f64::from(w) / f64::from(h);
    ((aspect(thumbnail) / aspect(full) - 1.0).abs() <= 0.03)
        .then_some(TiffFrames { thumbnail, full })
}

/// The size the image decoder develops the TIFF at `path` at
/// (`image_loader::load_image_with_orientation`, the path every TIFF took
/// before PR #916): its first image, turned by the primary EXIF
/// orientation.
fn raster_frame(path: &Path) -> Option<(u32, u32)> {
    let file = std::fs::File::open(path).ok()?;
    let (w, h) = image::ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    let file = std::fs::File::open(path).ok()?;
    let orientation = exif::Reader::new()
        .read_from_container(&mut BufReader::new(file))
        .ok()
        .and_then(|exif| {
            exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
                .value
                .get_uint(0)
        })
        .map_or(1, |o| o as u16);
    // `apply_orientation` turns 5–8 a quarter; other values leave it.
    Some(if (5..=8).contains(&orientation) {
        (h, w)
    } else {
        (w, h)
    })
}

/// Moves the pixel geometry of the recipe of the photo at `path` to the
/// photo's frame now when it was made on another one: a camera TIFF's edit
/// from before 0.1.0-alpha.17.1 (see the module), scaled from the
/// thumbnail and stamped with the full frame. Returns whether it changed.
/// New recipes and stamped ones are left alone.
pub fn conform(adjustments: &mut Value, path: &Path) -> bool {
    if !adjustments.is_object()
        || stamped(adjustments).is_some()
        || crate::image_processing::is_new_recipe(adjustments)
    {
        return false;
    }
    let Some(frames) = tiff_frames(path) else {
        return false;
    };
    if !made_on_thumbnail(adjustments, frames) {
        return false;
    }
    scale_geometry(adjustments, frames.thumbnail, frames.full);
    stamp(adjustments, frames.full.0, frames.full.1);
    true
}

/// Whether `adjustments`, without a stamp, was made on the camera TIFF's
/// thumbnail rather than its full frame. A drawn mask's frame or an AI
/// bitmap of the full frame's size says full frame; geometry outside the
/// thumbnail (a crop, a spot) does too (more than a pixel beyond it).
/// Otherwise thumbnail: everything fits it, as all edits from before
/// 0.1.0-alpha.17.1 do; an edit of the full frame fitting the thumbnail
/// would have to lie within its top-left 1/9 to 1/30 (and on photos with
/// drawn masks or AI bitmaps their sizes decide).
pub fn made_on_thumbnail(adjustments: &Value, frames: TiffFrames) -> bool {
    let near = |a: (f64, f64), b: (u32, u32)| {
        (a.0 - f64::from(b.0)).abs() <= 1.0 && (a.1 - f64::from(b.1)).abs() <= 1.0
    };
    let sizes = crate::mask_generation::drawn_mask_sizes(adjustments);
    if sizes.iter().any(|&s| near(s, frames.full)) {
        return false;
    }
    let bitmaps = ai_bitmap_sizes(adjustments);
    if bitmaps
        .iter()
        .any(|&(w, h)| near((f64::from(w), f64::from(h)), frames.full))
    {
        return false;
    }
    let (tw, th) = shown(adjustments, frames.thumbnail);
    let inside = |x: f64, y: f64| x <= tw + 1.0 && y <= th + 1.0;
    if let Some(crop) = adjustments
        .get(geometry::CROP.name())
        .and_then(|c| serde_json::from_value::<geometry::Crop>(c.clone()).ok())
        && !inside(crop.x + crop.width, crop.y + crop.height)
    {
        return false;
    }
    for spot in spot_items(adjustments) {
        for (x, y) in [("x", "y"), ("sourceX", "sourceY")] {
            if let (Some(x), Some(y)) = (spot[x].as_f64(), spot[y].as_f64())
                && !inside(x, y)
            {
                return false;
            }
        }
    }
    true
}

/// `size` (the photo's own) as the photo is shown: turned by the recipe's
/// orientation steps.
fn shown(adjustments: &Value, size: (u32, u32)) -> (f64, f64) {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    if steps(adjustments) % 2 == 1 {
        (h, w)
    } else {
        (w, h)
    }
}

fn steps(adjustments: &Value) -> i64 {
    adjustments
        .get(geometry::ORIENTATION_STEPS.name())
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .rem_euclid(4)
}

fn spot_items(adjustments: &Value) -> impl Iterator<Item = &Value> {
    adjustments
        .get("spots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| s.is_object())
}

/// The sizes of the AI bitmaps (`ai-*` components' `maskDataBase64`) of
/// `adjustments`, read from their headers.
fn ai_bitmap_sizes(adjustments: &Value) -> Vec<(u32, u32)> {
    let mut sizes = Vec::new();
    for key in [rawtonic_recipe::masks::MASKS.name(), "aiPatches"] {
        for container in adjustments
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for sub in container
                .get(rawtonic_recipe::masks::SUB_MASKS.name())
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let is_ai = sub
                    .get(rawtonic_recipe::masks::TYPE.name())
                    .and_then(Value::as_str)
                    .is_some_and(|t| t.starts_with("ai-"));
                let data = sub
                    .get(rawtonic_recipe::masks::PARAMETERS.name())
                    .and_then(|p| p.get("maskDataBase64"))
                    .and_then(Value::as_str);
                if let (true, Some(data)) = (is_ai, data)
                    && let Some(size) = bitmap_size(data)
                {
                    sizes.push(size);
                }
            }
        }
    }
    sizes
}

fn bitmap_size(data_url: &str) -> Option<(u32, u32)> {
    let b64 = data_url.split_once(',').map_or(data_url, |(_, b)| b);
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Moves the pixel geometry of `adjustments` from a photo of size `from`
/// to one of size `to` (each the photo's own size, full resolution, before
/// orientation), proportionally, as drawn masks move (`ShapeFrame`): the
/// crop and the spots (centres, sources and stroke points at the same
/// fraction of the width and height, radii with the mean of both scales),
/// a crop of a locked aspect ratio keeping its ratio (centred where the
/// proportional one is, kept inside the photo). Drawn masks that record no
/// size get `from` (`record_drawn_mask_size`), so they are placed
/// proportionally as the others; AI bitmaps are stretched to the photo when
/// they are rendered (`mask_generation::ai_bitmap_at_photo_size`).
pub fn scale_geometry(adjustments: &mut Value, from: (u32, u32), to: (u32, u32)) {
    if from.0 == 0 || from.1 == 0 {
        return;
    }
    let (fx, fy) = shown(adjustments, from);
    let (tx, ty) = shown(adjustments, to);
    let (sx, sy) = (tx / fx, ty / fy);
    let locked = adjustments
        .get(geometry::ASPECT_RATIO.name())
        .and_then(Value::as_f64)
        .is_some_and(|r| r.is_finite() && r > 0.0);
    if let Some(crop) = adjustments
        .get(geometry::CROP.name())
        .and_then(|c| serde_json::from_value::<geometry::Crop>(c.clone()).ok())
        && crop.width > 0.0
        && crop.height > 0.0
    {
        let (cx, cy) = (
            (crop.x + crop.width / 2.0) * sx,
            (crop.y + crop.height / 2.0) * sy,
        );
        let (mut w, mut h) = (crop.width * sx, crop.height * sy);
        if locked {
            // The ratio of the crop as made, at the mean scale, then
            // within the photo.
            let s = (sx * sy).sqrt();
            (w, h) = (crop.width * s, crop.height * s);
            let fit = (tx / w).min(ty / h).min(1.0);
            (w, h) = (w * fit, h * fit);
        }
        let x = (cx - w / 2.0).clamp(0.0, (tx - w).max(0.0));
        let y = (cy - h / 2.0).clamp(0.0, (ty - h).max(0.0));
        let crop_value = &mut adjustments[geometry::CROP.name()];
        crop_value["x"] = json!(x);
        crop_value["y"] = json!(y);
        crop_value["width"] = json!(w);
        crop_value["height"] = json!(h);
    }
    let radius = (sx + sy) / 2.0;
    if let Some(spots) = adjustments.get_mut("spots").and_then(Value::as_array_mut) {
        for spot in spots.iter_mut().filter(|s| s.is_object()) {
            for (key, scale) in [
                ("x", sx),
                ("y", sy),
                ("sourceX", sx),
                ("sourceY", sy),
                ("radius", radius),
            ] {
                if let Some(v) = spot[key].as_f64() {
                    spot[key] = json!(v * scale);
                }
            }
            if let Some(points) = spot.get_mut("points").and_then(Value::as_array_mut) {
                for point in points.iter_mut().filter(|p| p.is_object()) {
                    for (key, scale) in [("x", sx), ("y", sy)] {
                        if let Some(v) = point[key].as_f64() {
                            point[key] = json!(v * scale);
                        }
                    }
                }
            }
        }
    }
    crate::mask_generation::record_drawn_mask_size(
        adjustments,
        (f64::from(from.0), f64::from(from.1)),
    );
}

#[cfg(test)]
#[path = "recipe_frame_tests.rs"]
mod tests;
