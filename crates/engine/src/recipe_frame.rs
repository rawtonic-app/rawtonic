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
    #[cfg(any(test, feature = "test-support"))]
    if let Some(frames) = pretended(path) {
        return Some(frames);
    }
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

#[cfg(any(test, feature = "test-support"))]
static PRETENDED: Mutex<Vec<(PathBuf, TiffFrames)>> = Mutex::new(Vec::new());

#[cfg(any(test, feature = "test-support"))]
fn pretended(path: &Path) -> Option<TiffFrames> {
    PRETENDED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, frames)| *frames)
}

/// Tests of other crates: `path` reads as a camera TIFF of `frames`.
#[cfg(any(test, feature = "test-support"))]
pub fn pretend_frames(path: &Path, frames: TiffFrames) {
    PRETENDED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((path.to_path_buf(), frames));
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
/// New recipes and stamped ones are left alone, and so are recipes that
/// cannot tell their frame ([`Made::Unknown`]; [`undecided`]).
pub fn conform(adjustments: &mut Value, path: &Path) -> bool {
    match unstamped_frames(adjustments, path) {
        Some(frames) if made_on(adjustments, frames) == Made::Thumbnail => {
            migrate(adjustments, frames);
            true
        }
        _ => false,
    }
}

/// Whether the recipe of the camera TIFF at `path` cannot tell whether it
/// was made on the thumbnail or the full frame ([`Made::Unknown`]): it is
/// left as it is, and the editor offers to move it ([`move_to_full_frame`]).
pub fn undecided(adjustments: &Value, path: &Path) -> bool {
    unstamped_frames(adjustments, path)
        .is_some_and(|frames| made_on(adjustments, frames) == Made::Unknown)
}

/// Moves an undecided recipe ([`undecided`]) to the full frame, as
/// [`conform`] moves a thumbnail edit: the user's choice. Returns whether
/// it changed.
pub fn move_to_full_frame(adjustments: &mut Value, path: &Path) -> bool {
    match unstamped_frames(adjustments, path) {
        Some(frames) if made_on(adjustments, frames) == Made::Unknown => {
            migrate(adjustments, frames);
            true
        }
        _ => false,
    }
}

/// The camera TIFF frames of `path` when `adjustments` is a saved recipe
/// without a stamp (the ones that may need moving).
fn unstamped_frames(adjustments: &Value, path: &Path) -> Option<TiffFrames> {
    if !adjustments.is_object()
        || stamped(adjustments).is_some()
        || crate::image_processing::is_new_recipe(adjustments)
    {
        return None;
    }
    tiff_frames(path)
}

fn migrate(adjustments: &mut Value, frames: TiffFrames) {
    scale_geometry(adjustments, frames.thumbnail, frames.full);
    stamp(adjustments, frames.full.0, frames.full.1);
}

/// On which frame of a camera TIFF a recipe without a stamp was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Made {
    Thumbnail,
    Full,
    /// Nothing tells: the RAW passes' versions with only a crop or spots
    /// within the thumbnail's corner (see [`made_on`]).
    Unknown,
}

/// Whether `adjustments`, without a stamp, was made on the camera TIFF's
/// thumbnail rather than its full frame ([`made_on`]).
pub fn made_on_thumbnail(adjustments: &Value, frames: TiffFrames) -> bool {
    made_on(adjustments, frames) == Made::Thumbnail
}

/// On which frame `adjustments`, without a stamp, was made, from what in
/// it only one frame can have made:
/// - the full frame: a drawn mask's frame or an AI bitmap of the full
///   frame's size, or a crop or spot more than a pixel beyond the thumbnail
///   (an edit of the full frame made since PR #916);
/// - the thumbnail: a drawn mask's frame or an AI bitmap of the
///   thumbnail's size, or a crop of exactly the whole thumbnail;
/// - otherwise the RAW passes' versions (`fringeVersion` since 2026-10-16,
///   `autoCaVersion`), which every edit begun on a RAW carries and an
///   ordinary TIFF's edit never gets, tell the full frame, unless a crop or
///   spots are there to place: Processing Versions pasted from a RAW onto a
///   thumbnail edit add them too, so the two look alike ([`Made::Unknown`];
///   a cloned dust speck in the corner of an edit begun on the RAW, or a
///   thumbnail edit given the versions);
/// - without them the thumbnail: begun as an ordinary TIFF's edit, with
///   everything within the thumbnail, as all edits from before
///   0.1.0-alpha.17.1 are. (A thumbnail edit that got a full-frame spot
///   within the thumbnail's corner in alpha.17.1 or .18 is indistinguishable
///   from one; it is moved as one.)
///
/// Full-frame evidence wins over thumbnail evidence: an edit with both was
/// changed on the full frame and is left as the user last saw it.
pub fn made_on(adjustments: &Value, frames: TiffFrames) -> Made {
    let near = |a: (f64, f64), b: (u32, u32)| {
        (a.0 - f64::from(b.0)).abs() <= 1.0 && (a.1 - f64::from(b.1)).abs() <= 1.0
    };
    let mut sizes = crate::mask_generation::drawn_mask_sizes(adjustments);
    sizes.extend(
        ai_bitmap_sizes(adjustments)
            .into_iter()
            .map(|(w, h)| (f64::from(w), f64::from(h))),
    );
    let (tw, th) = shown(adjustments, frames.thumbnail);
    let inside = |x: f64, y: f64| x <= tw + 1.0 && y <= th + 1.0;
    let crop = adjustments
        .get(geometry::CROP.name())
        .and_then(|c| serde_json::from_value::<geometry::Crop>(c.clone()).ok());
    let beyond = crop
        .as_ref()
        .is_some_and(|c| !inside(c.x + c.width, c.y + c.height))
        || spot_items(adjustments).any(|spot| {
            [("x", "y"), ("sourceX", "sourceY")].iter().any(|(x, y)| {
                matches!((spot[*x].as_f64(), spot[*y].as_f64()), (Some(x), Some(y)) if !inside(x, y))
            })
        });
    if beyond || sizes.iter().any(|&s| near(s, frames.full)) {
        return Made::Full;
    }
    let whole_thumbnail = crop.as_ref().is_some_and(|c| {
        c.x.abs() <= 1.0
            && c.y.abs() <= 1.0
            && (c.width - tw).abs() <= 1.0
            && (c.height - th).abs() <= 1.0
    });
    if whole_thumbnail || sizes.iter().any(|&s| near(s, frames.thumbnail)) {
        return Made::Thumbnail;
    }
    let raw_versions = [
        crate::edge_fringe::VERSION_KEY,
        crate::lateral_ca::VERSION_KEY,
    ]
    .iter()
    .any(|key| adjustments.get(*key).is_some_and(|v| !v.is_null()));
    if !raw_versions {
        return Made::Thumbnail;
    }
    let placed = crop.is_some_and(|c| c.width > 0.0 && c.height > 0.0)
        || spot_items(adjustments).next().is_some();
    if placed { Made::Unknown } else { Made::Full }
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
/// proportionally as the others; AI components' boxes and prompts scale as
/// the spots, and their bitmaps record `from` (`BITMAP_FRAME`), so they are
/// stretched over the photo when rendered (`ai_bitmap_at_photo_size`).
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
    let recipe_steps = steps(adjustments);
    let own = (
        f64::from(to.0) / f64::from(from.0),
        f64::from(to.1) / f64::from(from.1),
    );
    for_each_ai_component(adjustments, |layer, kind, parameters| {
        // An Object selection's box and prompts, in pixels of the photo as
        // shown when it was made: turned by the component's own orientation
        // steps (the recipe's when it records none).
        let turned = parameters
            .get(geometry::ORIENTATION_STEPS.name())
            .and_then(Value::as_i64)
            .unwrap_or(recipe_steps)
            .rem_euclid(2)
            == 1;
        let (sx, sy) = if turned { (own.1, own.0) } else { own };
        for (key, scale) in [("startX", sx), ("startY", sy), ("endX", sx), ("endY", sy)] {
            if let Some(v) = parameters[key].as_f64() {
                parameters[key] = json!(v * scale);
            }
        }
        if let Some(prompts) = parameters.get_mut("prompts").and_then(Value::as_array_mut) {
            for prompt in prompts.iter_mut().filter(|p| p.is_object()) {
                for (key, scale) in [
                    ("x", sx),
                    ("y", sy),
                    ("startX", sx),
                    ("startY", sy),
                    ("endX", sx),
                    ("endY", sy),
                ] {
                    if let Some(v) = prompt[key].as_f64() {
                        prompt[key] = json!(v * scale);
                    }
                }
            }
        }
        // The bitmap is stretched over the photo when rendered; a selection
        // a network makes on its own is computed again on the full frame
        // (the stretched one stands in until then).
        if parameters
            .get("maskDataBase64")
            .is_some_and(Value::is_string)
        {
            parameters[crate::mask_generation::BITMAP_FRAME] =
                json!({"width": from.0, "height": from.1});
            // (A mask layer's: an AI patch keeps the area it was filled in.)
            if layer
                && crate::ai_selections::AutoSelection::of(kind).is_some()
                && !crate::masks::nothing_to_detect(parameters)
            {
                parameters[DETECT_AGAIN] = json!(true);
            }
        }
    });
}

/// The parameter that marks an AI selection migrated from a camera TIFF's
/// thumbnail ([`scale_geometry`]): Sky, Subject, People and depth range
/// selections express what to select, so they are computed again on the
/// full frame — by the editor when the photo opens (one History step, the
/// stretched thumbnail bitmap one undo back) and by exports
/// (`ai_selections::undetected`) — keeping their refinements (Grow,
/// Feather, Invert; brush strokes are components of their own). Object
/// selections keep their stretched bitmap: their prompts were made on the
/// thumbnail, and an export cannot point at an object.
pub const DETECT_AGAIN: &str = "detectAgain";

/// `visit(in a mask layer, type, parameters)` for every AI component
/// (`ai-*`) of `adjustments`' masks and AI patches.
fn for_each_ai_component(adjustments: &mut Value, mut visit: impl FnMut(bool, &str, &mut Value)) {
    for key in [rawtonic_recipe::masks::MASKS.name(), "aiPatches"] {
        let Some(list) = adjustments.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        for sub in list
            .iter_mut()
            .filter_map(|c| {
                c.get_mut(rawtonic_recipe::masks::SUB_MASKS.name())
                    .and_then(Value::as_array_mut)
            })
            .flatten()
        {
            let kind = sub
                .get(rawtonic_recipe::masks::TYPE.name())
                .and_then(Value::as_str)
                .filter(|t| t.starts_with("ai-"))
                .map(str::to_string);
            if let (Some(kind), Some(parameters)) = (
                kind,
                sub.get_mut(rawtonic_recipe::masks::PARAMETERS.name())
                    .filter(|p| p.is_object()),
            ) {
                visit(
                    key == rawtonic_recipe::masks::MASKS.name(),
                    &kind,
                    parameters,
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "recipe_frame_tests.rs"]
mod tests;
