//! The frame a recipe develops of its RAW (`sourceFrameVersion`, ADR 0020;
//! the frames themselves: `rawtonic_rawdecode::source_frame`), and the one
//! migration that moves a recipe's coordinates from one frame to another.
//!
//! The key belongs to the photo, like its spots: never copied by paste,
//! presets or sync, written for new edits (`image_processing::apply_render_default`
//! and the editor's new edits), and changed only by "Update frame" or a
//! Reset, both of which carry the recipe's coordinates over with
//! [`migrate`] in the same history step. A recipe without the key develops
//! the frame it always did, so its exports stay byte-identical.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use base64::{Engine as _, engine::general_purpose};
use image::GrayImage;
use serde_json::{Value, json};

pub use rawtonic_rawdecode::source_frame::{FrameChange, VERSION};

use crate::image_processing::{WarpPoints, get_geometry_params_from_json};
use crate::mask_generation::SourceGeometry;

/// The recipe key.
pub const KEY: &str = "sourceFrameVersion";

/// The frame version a recipe asks for: its own (versions this build does
/// not know read as its newest), the newest for a new recipe
/// (`image_processing::is_new_recipe`: it renders as a new edit), 0 for an
/// edit from before the key.
pub fn requested(adjustments: &Value) -> u32 {
    if crate::image_processing::is_new_recipe(adjustments) {
        return VERSION;
    }
    adjustments
        .get(KEY)
        .and_then(Value::as_u64)
        .map_or(0, |v| v.min(u64::from(VERSION)) as u32)
}

/// For each frame version up to the newest (the index), the oldest one
/// that develops the RAW at `path` the same (`rawdecode::source_frame::effective`):
/// all 0 for files no frame version changes. From the file (read once per
/// size and modification time), else, offline, from what its previews keep
/// (`source_facts`); None when neither knows.
pub fn frames(path: &str) -> Option<Vec<u32>> {
    let extension = std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !crate::formats::is_raw_file(path)
        || !rawtonic_rawdecode::source_frame::may_change(&extension)
    {
        return Some(vec![0; VERSION as usize + 1]);
    }
    // Offline: what its previews keep (nothing is registered while the
    // file can be read).
    if let Some(frames) = crate::source_facts::registered(path)
        .and_then(|facts| facts.frames.clone())
        .filter(|f| !f.is_empty())
    {
        return Some(frames);
    }
    type Cache = Mutex<HashMap<String, (u64, std::time::SystemTime, Vec<u32>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let stamp = std::fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)));
    if let Some((len, modified)) = stamp {
        if let Some((l, m, found)) = cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .cloned()
            && l == len
            && m == modified
        {
            return Some(found);
        }
        if let Some(found) = rawler::rawsource::RawSource::new(std::path::Path::new(path))
            .ok()
            .filter(|source| rawtonic_rawdecode::source_frame::frame(source, 0).is_some())
            .map(|source| {
                (0..=VERSION)
                    .map(|v| rawtonic_rawdecode::source_frame::effective(&source, v))
                    .collect::<Vec<u32>>()
            })
        {
            cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(path.to_string(), (len, modified, found.clone()));
            return Some(found);
        }
    }
    None
}

/// The frame version that develops the RAW at `path` as `version` does
/// (see `frames`): the frame part of a decode variant, so the bases of two
/// versions that frame a file alike are shared. `version` itself when the
/// file and its previews tell nothing (an offline photograph from before
/// frames were recorded).
pub fn effective(path: &str, version: u32) -> u32 {
    if version == 0 {
        return 0;
    }
    frames(path).map_or(version, |frames| {
        frames
            .get(version as usize)
            .or(frames.last())
            .copied()
            .unwrap_or(version)
    })
}

/// The change of the RAW at `path`'s developed frame from version `from` to
/// `to`; None when the frames are alike or the file cannot be read.
pub fn change(path: &str, from: u32, to: u32) -> Option<FrameChange> {
    if effective(path, from) == effective(path, to) {
        return None;
    }
    let source = rawler::rawsource::RawSource::new(std::path::Path::new(path)).ok()?;
    rawtonic_rawdecode::source_frame::change(&source, from, to)
}

/// Where a point of the old frame lies in the new one, for each of the
/// spaces a recipe keeps coordinates in.
pub struct FrameMap {
    change: FrameChange,
    /// The recipe's lens and transform warp on the old frame (its
    /// parameters too) and the migrated recipe's on the new one.
    then_params: crate::image_processing::GeometryParams,
    then: WarpPoints,
    now: WarpPoints,
}

impl FrameMap {
    /// From the warp (lens and transform corrections) of `before` on the old
    /// frame to that of `after` on the new one: the same corrections, centred
    /// and scaled anew on the new frame, and guide lines moved with it.
    pub fn between(change: FrameChange, before: &Value, after: &Value) -> Self {
        let then_params = get_geometry_params_from_json(before);
        let now_params = get_geometry_params_from_json(after);
        Self {
            then: WarpPoints::new(&then_params, change.from.0, change.from.1),
            now: WarpPoints::new(&now_params, change.to.0, change.to.1),
            then_params,
            change,
        }
    }

    /// `between` for a warp the migration leaves as it is.
    pub fn new(change: FrameChange, adjustments: &Value) -> Self {
        Self::between(change, adjustments, adjustments)
    }

    /// The old warp's parameters (bitmaps without a warp of their own were
    /// made in it).
    pub(crate) fn then_params(&self) -> &crate::image_processing::GeometryParams {
        &self.then_params
    }

    pub fn change(&self) -> &FrameChange {
        &self.change
    }

    /// The old frame's size and the new one's, oriented, as `f64`.
    pub fn sizes(&self) -> ((f64, f64), (f64, f64)) {
        let (from, to) = (self.change.from, self.change.to);
        (
            (f64::from(from.0), f64::from(from.1)),
            (f64::from(to.0), f64::from(to.1)),
        )
    }

    /// A pixel of the base image (as decoded, before the warp).
    pub fn base(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (x + self.change.offset.0, y + self.change.offset.1)
    }

    /// A point of the mask frame (the warped photo before its orientation
    /// steps, flips and straightening): through the source, so it stays on
    /// the content the warp puts there (the warp is centred on the frame).
    pub fn mask(&self, (x, y): (f64, f64)) -> (f64, f64) {
        self.then
            .to_source(x as f32, y as f32)
            .map(|(sx, sy)| self.base((f64::from(sx), f64::from(sy))))
            .and_then(|(sx, sy)| self.now.to_output(sx as f32, sy as f32))
            .map(|(nx, ny)| (f64::from(nx), f64::from(ny)))
            .filter(|(nx, ny)| nx.is_finite() && ny.is_finite())
            .unwrap_or_else(|| self.base((x, y)))
    }

    /// The old mask-frame point the new one at (`x`, `y`) shows (for
    /// bitmaps kept in the mask frame), through a warp `then` of the old
    /// frame (the bitmap's own) and the recipe's on the new one.
    pub fn mask_back(&self, then: &WarpPoints, (x, y): (f64, f64)) -> Option<(f64, f64)> {
        let (sx, sy) = self.now.to_source(x as f32, y as f32)?;
        let (sx, sy) = (
            f64::from(sx) - self.change.offset.0,
            f64::from(sy) - self.change.offset.1,
        );
        then.to_output(sx as f32, sy as f32)
            .map(|(px, py)| (f64::from(px), f64::from(py)))
            .filter(|(px, py)| px.is_finite() && py.is_finite())
    }

    /// A point of the photo shown in `geometry` (full-resolution pixels after
    /// orientation steps, flips and straightening, before the crop).
    pub fn shown(&self, geometry: SourceGeometry, p: (f64, f64)) -> (f64, f64) {
        let (from, to) = self.sizes();
        let old = crate::mask_generation::Placement::new(geometry, from);
        let new = crate::mask_generation::Placement::new(geometry, to);
        new.to_shown(self.mask(old.to_photo(p)))
    }

    /// The photo's size shown in `geometry`, old and new.
    pub fn shown_sizes(&self, geometry: SourceGeometry) -> ((f64, f64), (f64, f64)) {
        let (from, to) = self.sizes();
        let turn = |(w, h): (f64, f64)| {
            if geometry.orientation_steps % 2 == 1 {
                (h, w)
            } else {
                (w, h)
            }
        };
        (turn(from), turn(to))
    }
}

/// A recipe carried over to another frame, and what could not be.
#[derive(Debug, Clone)]
pub struct Migrated {
    pub adjustments: Value,
    /// AI patches hidden because their pixels do not fit the new frame
    /// (`frameHidden`): they have to be generated again.
    pub hidden_patches: usize,
}

/// The marker on an AI patch the migration hid: kept with its pixels and
/// its generated marking, shown again by undo, made again by regenerating.
pub const HIDDEN_PATCH: &str = "frameHidden";

/// `adjustments` (a recipe of the old frame of `map`, or of no frame change
/// when `map` is None) moved to frame version `to`: the key set, and every
/// coordinate the recipe keeps moved onto the same content of the new
/// frame — the crop, drawn masks (with or without their frame), AI mask
/// bitmaps, colour samples and object prompts, spots, red eyes, face
/// retouch, AI patches (their pixels where the new frame holds the old one,
/// else hidden), the guided perspective's lines, relight lights and its
/// depth (kept where the shown frame stays, else left to be analysed again),
/// the lens blur's depth map and Lightroom's items not yet placed. The one
/// migration for "Update frame" and Reset. `warp` is the recipe as it is
/// rendered (its lens correction filled in, `lens_recipe`): points are
/// carried through its warp on the old frame and the migrated one's (guide
/// lines moved) on the new frame.
pub fn migrate(
    adjustments: &Value,
    change: Option<&FrameChange>,
    warp: &Value,
    to: u32,
) -> Migrated {
    let mut out = if adjustments.is_object() {
        adjustments.clone()
    } else {
        json!({})
    };
    let from = requested(adjustments);
    out[KEY] = json!(to);
    let Some(change) = change else {
        return Migrated {
            adjustments: out,
            hidden_patches: 0,
        };
    };
    // The guide lines first: they are part of the warp the rest moves
    // through on the new frame.
    move_guided_lines(&mut out, change);
    let mut warp_after = warp.clone();
    if warp_after.is_object() {
        warp_after["guidedPerspective"] = out["guidedPerspective"].clone();
    }
    let map = &FrameMap::between(*change, warp, &warp_after);
    let faces = face_retouch_applies(adjustments);
    let current = SourceGeometry::from_adjustments(adjustments);
    let shown = |p: (f64, f64)| map.shown(current, p);

    move_crop(&mut out, map, current);
    follow_aspect(&mut out, map);
    move_points(
        &mut out,
        "spots",
        &["x", "y"],
        &["sourceX", "sourceY"],
        shown,
    );
    move_red_eyes(&mut out, map);
    if faces {
        move_face_retouch(&mut out, map, current);
    }
    let hidden_patches = move_patches(&mut out, map);
    move_relight(&mut out, adjustments, map, current, from);
    move_lens_blur_depth(&mut out, map);
    crate::lightroom_retouch::move_pending(&mut out, &map.change);
    crate::lightroom_masks::move_pending(&mut out, &map.change);
    crate::mask_generation::move_masks(&mut out, adjustments, map);
    // Faces that applied keep applying: their stamp follows the geometry
    // the migration changed (guide lines), by their stamp's own rule.
    if faces && let Some(frame) = out["faceRetouch"]["frame"].as_str().map(str::to_string) {
        out["faceRetouch"]["frame"] = json!(crate::face_retouch::frame_key_like(&frame, &out));
    }
    Migrated {
        adjustments: out,
        hidden_patches,
    }
}

/// The crop (pixels of the photo shown, before the crop) keeps its size and
/// stays on the content at its centre, inside the new photo.
fn move_crop(out: &mut Value, map: &FrameMap, geometry: SourceGeometry) {
    let crop = &out["crop"];
    let num = |k: &str| crop.get(k).and_then(Value::as_f64);
    let (Some(x), Some(y), Some(w), Some(h)) = (num("x"), num("y"), num("width"), num("height"))
    else {
        return;
    };
    let (cx, cy) = map.shown(geometry, (x + w / 2.0, y + h / 2.0));
    let (_, (sw, sh)) = map.shown_sizes(geometry);
    let nx = (cx - w / 2.0).clamp(0.0, (sw - w).max(0.0));
    let ny = (cy - h / 2.0).clamp(0.0, (sh - h).max(0.0));
    let unit = crop.get("unit").cloned();
    out["crop"]["x"] = json!(nx);
    out["crop"]["y"] = json!(ny);
    if let Some(unit) = unit {
        out["crop"]["unit"] = unit;
    }
}

/// Without a crop, an aspect ratio that is the photo's own (the crop tool's
/// Original) becomes the new frame's.
fn follow_aspect(out: &mut Value, map: &FrameMap) {
    if !out["crop"].is_null() {
        return;
    }
    let Some(aspect) = out.get("aspectRatio").and_then(Value::as_f64) else {
        return;
    };
    let ((ow, oh), (nw, nh)) = map.sizes();
    let same = |a: f64, b: f64| (a - b).abs() <= 1e-9 * b.abs().max(1.0);
    if same(aspect, ow / oh) {
        out["aspectRatio"] = json!(nw / nh);
    } else if same(aspect, oh / ow) {
        out["aspectRatio"] = json!(nh / nw);
    }
}

/// Moves the (`x`, `y`)-pairs named in `points` and `more` of every item of
/// `out[key]`, and of its `points` list, with `map`.
fn move_points(
    out: &mut Value,
    key: &str,
    points: &[&str; 2],
    more: &[&str; 2],
    map: impl Fn((f64, f64)) -> (f64, f64),
) {
    let Some(items) = out.get_mut(key).and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        if item.get(crate::lightroom_retouch::PENDING).is_some() {
            continue;
        }
        for [kx, ky] in [points, more] {
            move_pair(item, kx, ky, &map);
        }
        if let Some(list) = item.get_mut("points").and_then(Value::as_array_mut) {
            for point in list {
                move_pair(point, "x", "y", &map);
            }
        }
    }
}

fn move_pair(item: &mut Value, kx: &str, ky: &str, map: &impl Fn((f64, f64)) -> (f64, f64)) {
    if let (Some(x), Some(y)) = (
        item.get(kx).and_then(Value::as_f64),
        item.get(ky).and_then(Value::as_f64),
    ) {
        let (nx, ny) = map((x, y));
        item[kx] = json!(nx);
        item[ky] = json!(ny);
    }
}

/// Red eyes are kept in the photo as decoded, in units of its long edge.
fn move_red_eyes(out: &mut Value, map: &FrameMap) {
    let ((ow, oh), (nw, nh)) = map.sizes();
    let (old_unit, new_unit) = (ow.max(oh), nw.max(nh));
    let Some(items) = out.get_mut("redEyes").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        if item.get(crate::lightroom_retouch::PENDING).is_some() {
            continue;
        }
        move_pair(item, "x", "y", &|(x, y)| {
            let (bx, by) = map.base((x * old_unit, y * old_unit));
            (bx / new_unit, by / new_unit)
        });
        for key in ["radiusX", "radiusY"] {
            if let Some(r) = item.get(key).and_then(Value::as_f64) {
                item[key] = json!(r * old_unit / new_unit);
            }
        }
    }
}

/// Whether the recipe's face retouch applies to its geometry, by the
/// renderer's rule (`face_retouch::frame_matches`): faces of another
/// geometry are not applied, so they are left as they are.
fn face_retouch_applies(adjustments: &Value) -> bool {
    adjustments["faceRetouch"]["frame"]
        .as_str()
        .is_none_or(|frame| crate::face_retouch::frame_matches(frame, adjustments))
}

/// Face retouch is kept in the mask frame shown in the photo's geometry
/// (faces of another geometry, not applied, are left as they are): each
/// blemish moves on its own; the crop map (weights' pixels to the mask
/// frame, affine) is fitted again through three points of the weights
/// carried over (the warp's change is smooth at the scale of a face), and
/// the box, the eye distance with it.
fn move_face_retouch(out: &mut Value, map: &FrameMap, geometry: SourceGeometry) {
    let Some(faces) = out
        .get_mut("faceRetouch")
        .and_then(|r| r.get_mut("faces"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let shown = |p: (f64, f64)| map.shown(geometry, p);
    let num = |v: &Value| v.as_f64();
    for face in faces {
        let crop: Option<Vec<f64>> = face["crop"]
            .as_array()
            .and_then(|c| c.iter().map(num).collect::<Option<Vec<f64>>>())
            .filter(|c| c.len() == 6);
        let size = face["size"].as_f64().filter(|s| *s > 0.0).unwrap_or(1.0);
        if let Some(c) = crop {
            let at = |u: f64, v: f64| (c[0] * u + c[1] * v + c[2], c[3] * u + c[4] * v + c[5]);
            let o = shown(at(0.0, 0.0));
            let pu = shown(at(size, 0.0));
            let pv = shown(at(0.0, size));
            let fitted = [
                (pu.0 - o.0) / size,
                (pv.0 - o.0) / size,
                o.0,
                (pu.1 - o.1) / size,
                (pv.1 - o.1) / size,
                o.1,
            ];
            if fitted.iter().all(|v| v.is_finite()) {
                let scale = ((fitted[0] * fitted[4] - fitted[1] * fitted[3]).abs()
                    / (c[0] * c[4] - c[1] * c[3]).abs().max(1e-12))
                .sqrt();
                face["crop"] = json!(fitted);
                if let Some(d) = face["eyeDistance"].as_f64() {
                    face["eyeDistance"] = json!(d * scale);
                }
            }
        }
        let b: Option<Vec<f64>> = face["box"]
            .as_array()
            .and_then(|b| b.iter().map(num).collect::<Option<Vec<f64>>>())
            .filter(|b| b.len() == 4);
        if let Some(b) = b {
            let corners = [
                shown((b[0], b[1])),
                shown((b[0] + b[2], b[1])),
                shown((b[0], b[1] + b[3])),
                shown((b[0] + b[2], b[1] + b[3])),
            ];
            let (x0, x1) = corners
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, z), p| (a.min(p.0), z.max(p.0)));
            let (y0, y1) = corners
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, z), p| (a.min(p.1), z.max(p.1)));
            face["box"] = json!([x0, y0, x1 - x0, y1 - y0]);
        }
        if let Some(blemishes) = face.get_mut("blemishes").and_then(Value::as_array_mut) {
            for blemish in blemishes {
                move_pair(blemish, "x", "y", &shown);
            }
        }
    }
}

/// AI patches are pixels of the base image placed at `offsetX`/`offsetY`
/// (or covering it whole): moved by the frame's offset where the new frame
/// holds the old one (it only grows), else hidden for regeneration. Their
/// drawn masks move with the other masks (`mask_generation::move_masks`).
fn move_patches(out: &mut Value, map: &FrameMap) -> usize {
    let change = map.change();
    let (dx, dy) = change.offset;
    let whole = dx.fract() == 0.0 && dy.fract() == 0.0;
    let fits = change.only_grows() && whole;
    let Some(patches) = out.get_mut("aiPatches").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut hidden = 0;
    for patch in patches {
        let Some(data) = patch.get_mut("patchData").filter(|d| d.is_object()) else {
            continue;
        };
        if !fits {
            if patch["visible"].as_bool().unwrap_or(true) {
                patch["visible"] = json!(false);
                patch[HIDDEN_PATCH] = json!(true);
                hidden += 1;
            }
            continue;
        }
        let offset = |key: &str| data.get(key).and_then(Value::as_u64);
        match (offset("offsetX"), offset("offsetY")) {
            (Some(x), Some(y)) => {
                data["offsetX"] = json!(x + dx as u64);
                data["offsetY"] = json!(y + dy as u64);
            }
            _ => {
                // A patch over the whole old frame keeps its pixels there.
                data["offsetX"] = json!(dx as u64);
                data["offsetY"] = json!(dy as u64);
                data["width"] = json!(change.from.0);
                data["height"] = json!(change.from.1);
            }
        }
    }
    hidden
}

/// Guide lines are kept in units of the base image's width and height.
fn move_guided_lines(out: &mut Value, change: &FrameChange) {
    let size = |(w, h): (u32, u32)| (f64::from(w), f64::from(h));
    let ((ow, oh), (nw, nh)) = (size(change.from), size(change.to));
    let (dx, dy) = change.offset;
    let Some(lines) = out
        .get_mut("guidedPerspective")
        .and_then(|g| g.get_mut("lines"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for line in lines {
        for end in ["p1", "p2"] {
            if let Some(point) = line.get_mut(end) {
                move_pair(point, "x", "y", &|(x, y)| {
                    ((x * ow + dx) / nw, (y * oh + dy) / nh)
                });
            }
        }
    }
}

/// Relight works in the shown, cropped frame. With a crop that frame stays
/// (the crop moves with the content): the lights stay and the depth is
/// still that frame's. Without one the frame grows or shrinks: the lights
/// move with the content, and the depth (analysed in the old frame) is
/// left to be analysed again (its geometry records the old frame version).
fn move_relight(
    out: &mut Value,
    before: &Value,
    map: &FrameMap,
    geometry: SourceGeometry,
    from: u32,
) {
    let cropped = !before["crop"].is_null();
    if !cropped {
        let ((ow, oh), (nw, nh)) = map.shown_sizes(geometry);
        if let Some(lights) = out.get_mut("relightLights").and_then(Value::as_array_mut) {
            let percent = |(x, y): (f64, f64)| {
                let (sx, sy) = map.shown(geometry, (x / 100.0 * ow, y / 100.0 * oh));
                (sx / nw * 100.0, sy / nh * 100.0)
            };
            for light in lights {
                move_pair(light, "x", "y", &percent);
                move_pair(light, "sourceX", "sourceY", &percent);
            }
        }
    }
    if !out["relightDepth"].is_object() {
        return;
    }
    let stored = &before["relightDepth"]["geometry"];
    if cropped && *stored == crate::relight::geometry(before) {
        out["relightDepth"]["geometry"] = crate::relight::geometry(out);
    } else if out["relightDepth"]["geometry"].is_object() {
        out["relightDepth"]["geometry"][KEY] = json!(from);
    }
}

/// The lens blur's depth map covers the mask frame: drawn again over the
/// new one, its edge carried into what the frame gains.
fn move_lens_blur_depth(out: &mut Value, map: &FrameMap) {
    let Some(url) = out
        .get("lensBlurDepthMap")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let (from, _) = map.sizes();
    let then = WarpPoints::new(map.then_params(), from.0 as u32, from.1 as u32);
    if let Some(moved) = remap_bitmap(url, map, &then, Outside::Edge) {
        out["lensBlurDepthMap"] = json!(moved);
    }
}

/// What a remapped bitmap holds where the old frame had nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outside {
    /// Nothing (a mask covers nothing there).
    Zero,
    /// The nearest old pixel (a depth map).
    Edge,
}

/// A grey bitmap (PNG data URL or bare base64) kept over the old mask
/// frame, at any resolution, drawn again over the new one at the same
/// resolution per frame pixel: each new pixel takes the old one its centre
/// shows (`FrameMap::mask_back` through the bitmap's own warp `then`),
/// nearest, as the masks sample their bitmaps. None when it cannot be read.
pub(crate) fn remap_bitmap(
    data: &str,
    map: &FrameMap,
    then: &WarpPoints,
    outside: Outside,
) -> Option<String> {
    let bytes = general_purpose::STANDARD
        .decode(data.split_once(',').map_or(data, |(_, b)| b))
        .ok()?;
    let old = image::load_from_memory(&bytes).ok()?.to_luma8();
    let ((ow, oh), (nw, nh)) = map.sizes();
    let (sx, sy) = (f64::from(old.width()) / ow, f64::from(old.height()) / oh);
    let width = ((nw * sx).round() as u32).max(1);
    let height = ((nh * sy).round() as u32).max(1);
    let (bw, bh) = (old.width() as i64, old.height() as i64);
    let moved = GrayImage::from_fn(width, height, |x, y| {
        let centre = ((f64::from(x) + 0.5) / sx, (f64::from(y) + 0.5) / sy);
        let Some((px, py)) = map.mask_back(then, centre) else {
            return image::Luma([0]);
        };
        let (bx, by) = ((px * sx).floor() as i64, (py * sy).floor() as i64);
        let inside = (0..bw).contains(&bx) && (0..bh).contains(&by);
        match (inside, outside) {
            (true, _) => *old.get_pixel(bx as u32, by as u32),
            (false, Outside::Zero) => image::Luma([0]),
            (false, Outside::Edge) => {
                *old.get_pixel(bx.clamp(0, bw - 1) as u32, by.clamp(0, bh - 1) as u32)
            }
        }
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(moved)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    let encoded = general_purpose::STANDARD.encode(&png);
    Some(if data.starts_with("data:") {
        format!("data:image/png;base64,{encoded}")
    } else {
        encoded
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mask_generation::{PhotoGeometry, generate_mask_bitmap};
    use crate::masks::MaskDefinition;

    /// A frame that grows by `grow` columns on the sensor's right (the R5 II
    /// APS-C case), as developed (oriented) and before orientation.
    fn grows_right(from: (u32, u32), grow: u32) -> FrameChange {
        FrameChange {
            from,
            to: (from.0 + grow, from.1),
            offset: (0.0, 0.0),
            from_unrotated: from,
            to_unrotated: (from.0 + grow, from.1),
            offset_unrotated: (0.0, 0.0),
        }
    }

    fn png(image: &GrayImage) -> String {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageLuma8(image.clone())
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        format!(
            "data:image/png;base64,{}",
            general_purpose::STANDARD.encode(bytes)
        )
    }

    fn layer(sub_masks: Vec<Value>) -> Value {
        json!({"id": "l", "name": "l", "visible": true, "invert": false, "opacity": 100.0,
            "adjustments": {}, "subMasks": sub_masks})
    }

    fn sub(id: &str, kind: &str, parameters: Value) -> Value {
        json!({"id": id, "type": kind, "visible": true, "invert": false, "opacity": 100.0,
            "mode": "additive", "parameters": parameters})
    }

    /// The recipe's first layer's mask, shown at full resolution on a
    /// photo of `size` (before orientation steps).
    fn mask_of(recipe: &Value, size: (u32, u32)) -> GrayImage {
        let def: MaskDefinition = serde_json::from_value(recipe["masks"][0].clone()).unwrap();
        let steps = recipe["orientationSteps"].as_u64().unwrap_or(0);
        let (w, h) = if steps % 2 == 1 {
            (size.1, size.0)
        } else {
            size
        };
        let geometry = PhotoGeometry::from_adjustments(recipe)
            .with_photo_size(f64::from(size.0), f64::from(size.1));
        generate_mask_bitmap(&def, w, h, 1.0, (0.0, 0.0), &geometry, None).unwrap()
    }

    /// Masks of every kind the migration moves, on a photo turned half way
    /// round and flipped (the grown columns then come out on the left and
    /// at the top of what is shown): the mask of the new frame shows each
    /// pixel of the old one where its content went, and nothing new.
    #[test]
    fn masks_stay_on_their_content() {
        let (from, grow) = ((64, 48), 8);
        let change = grows_right(from, grow);
        let mut sky = GrayImage::new(64, 48);
        for (x, y, p) in sky.enumerate_pixels_mut() {
            if (10..30).contains(&x) && (5..20).contains(&y) {
                *p = image::Luma([255]);
            }
        }
        let recipe = json!({
            "orientationSteps": 2,
            "flipVertical": true,
            "masks": [layer(vec![
                // Drawn before frames were kept: as the photo is shown now.
                sub("r", "radial", json!({"centerX": 20.0, "centerY": 18.0, "radiusX": 9.0,
                    "radiusY": 6.0, "rotation": 20.0, "feather": 0.3})),
                // Drawn on this photo in another geometry, its size recorded.
                sub("g", "linear", json!({"startX": 40.0, "startY": 4.0, "endX": 50.0, "endY": 30.0,
                    "range": 6.0, "frame": {"width": 64.0, "height": 48.0, "rotation": 0.0,
                    "flipHorizontal": false, "flipVertical": false, "orientationSteps": 0}})),
                sub("b", "brush", json!({"lines": [{"tool": "brush", "brushSize": 6.0, "feather": 0.5,
                    "points": [{"x": 8.0, "y": 40.0}, {"x": 30.0, "y": 36.0}]}]})),
                sub("s", "ai-sky", json!({"maskDataBase64": png(&sky)})),
            ])],
        });
        let map = FrameMap::new(change, &recipe);
        let migrated = migrate(&recipe, Some(&change), &recipe, 1).adjustments;
        assert_eq!(migrated[KEY], 1);
        let old = mask_of(&recipe, from);
        let new = mask_of(&migrated, change.to);
        let geometry = SourceGeometry::from_adjustments(&recipe);
        assert!(old.pixels().any(|p| p.0[0] > 0));
        let mut covered = 0;
        for (x, y, p) in old.enumerate_pixels() {
            let (nx, ny) = map.shown(geometry, (f64::from(x), f64::from(y)));
            assert_eq!((nx.fract(), ny.fract()), (0.0, 0.0));
            let moved = new.get_pixel(nx as u32, ny as u32);
            assert!(
                (i32::from(moved.0[0]) - i32::from(p.0[0])).abs() <= 1,
                "({x}, {y}) -> ({nx}, {ny}): {} vs {}",
                p.0[0],
                moved.0[0]
            );
            covered += 1;
        }
        assert_eq!(covered, 64 * 48);
        // The sky bitmap is drawn again over the new frame: nothing where
        // the frame grew.
        let data = migrated["masks"][0]["subMasks"][3]["parameters"]["maskDataBase64"]
            .as_str()
            .unwrap();
        let bytes = general_purpose::STANDARD
            .decode(data.split_once(',').unwrap().1)
            .unwrap();
        let bitmap = image::load_from_memory(&bytes).unwrap().to_luma8();
        assert_eq!(bitmap.dimensions(), (64 + grow, 48));
        assert!((64..64 + grow).all(|x| (0..48).all(|y| bitmap.get_pixel(x, y).0[0] == 0)));
        assert_eq!(
            migrated["masks"][0]["subMasks"][3]["parameters"]["warp"],
            crate::mask_generation::mask_warp(&migrated)
        );
        // Every shape now records the new frame.
        for (i, path) in [
            (0, "/parameters/frame"),
            (1, "/parameters/frame"),
            (2, "/parameters/lines/0/frame"),
        ] {
            let frame = migrated["masks"][0]["subMasks"][i].pointer(path).unwrap();
            assert_eq!(frame["width"], 72.0, "{i}");
            assert_eq!(frame["height"], 48.0, "{i}");
        }
    }

    /// The crop, spots, red eyes, patches, guide lines and Lightroom's items
    /// not yet placed on a photo turned half way round: the grown columns
    /// show on the left, so what is shown moves right by them; what is kept
    /// in the unturned photo moves only in its units.
    #[test]
    fn retouching_and_the_crop_move_with_the_content() {
        let change = grows_right((4970, 3392), 118);
        let recipe = json!({
            "orientationSteps": 2,
            "crop": {"x": 100.0, "y": 50.0, "width": 2000.0, "height": 1500.0, "unit": "px"},
            "spots": [{"id": "s", "mode": "heal", "x": 300.0, "y": 200.0, "radius": 20.0,
                "sourceX": 360.0, "sourceY": 200.0, "points": [{"x": 300.0, "y": 200.0}, {"x": 310.0, "y": 210.0}]}],
            "redEyes": [{"id": "e", "x": 0.5, "y": 0.25, "radiusX": 0.01, "radiusY": 0.005, "angle": 0.0}],
            "aiPatches": [
                {"id": "p", "visible": true, "patchData": {"color": "c", "mask": "m", "offsetX": 40,
                    "offsetY": 30, "width": 100, "height": 80}, "subMasks": []},
                {"id": "w", "visible": true, "patchData": {"color": "c", "mask": "m"}, "subMasks": []}
            ],
            "guidedPerspective": {"enabled": true, "lines": [{"id": "l", "type": "vertical",
                "p1": {"x": 0.25, "y": 0.1}, "p2": {"x": 0.25, "y": 0.9}}]},
            "masks": [layer(vec![sub("lr", "radial", json!({PENDING_KEY: {"left": 0.5, "top": 0.2,
                "right": 0.6, "bottom": 0.4, "orientation": 1}}))])],
        });
        let moved = migrate(&recipe, Some(&change), &recipe, 1);
        assert_eq!(moved.hidden_patches, 0);
        let out = moved.adjustments;
        assert_eq!(out["crop"]["x"], 218.0);
        assert_eq!(out["crop"]["y"], 50.0);
        assert_eq!(out["crop"]["width"], 2000.0);
        // Uncropped, the photo's own aspect ratio follows the frame.
        let reset = json!({"aspectRatio": 4970.0 / 3392.0, KEY: 0});
        let moved = migrate(&reset, Some(&change), &reset, 1).adjustments;
        assert_eq!(moved["aspectRatio"], 5088.0 / 3392.0);
        assert_eq!(out["spots"][0]["x"], 418.0);
        assert_eq!(out["spots"][0]["sourceX"], 478.0);
        assert_eq!(out["spots"][0]["points"][1]["x"], 428.0);
        assert_eq!(out["spots"][0]["radius"], 20.0);
        // Long-edge units of the photo as decoded (unturned): the eye stays
        // at its decoded pixel, its size in pixels too.
        let eye_x = out["redEyes"][0]["x"].as_f64().unwrap() * 5088.0;
        assert!((eye_x - 0.5 * 4970.0).abs() < 1e-9);
        let radius = out["redEyes"][0]["radiusX"].as_f64().unwrap() * 5088.0;
        assert!((radius - 0.01 * 4970.0).abs() < 1e-9);
        assert_eq!(out["aiPatches"][0]["patchData"]["offsetX"], 40);
        assert_eq!(out["aiPatches"][1]["patchData"]["offsetX"], 0);
        assert_eq!(out["aiPatches"][1]["patchData"]["width"], 4970);
        let line_x = out["guidedPerspective"]["lines"][0]["p1"]["x"]
            .as_f64()
            .unwrap()
            * 5088.0;
        assert!((line_x - 0.25 * 4970.0).abs() < 1e-9);
        let left = out["masks"][0]["subMasks"][0]["parameters"][PENDING_KEY]["left"]
            .as_f64()
            .unwrap();
        assert!((left * 5088.0 - 0.5 * 4970.0).abs() < 1e-9);
    }

    const PENDING_KEY: &str = crate::lightroom_retouch::PENDING;

    /// A frame that does not hold the old one: AI patches cannot keep their
    /// pixels and are hidden (marked for regeneration), never moved wrongly.
    #[test]
    fn patches_that_do_not_fit_are_hidden_for_regeneration() {
        let change = FrameChange {
            from: (100, 80),
            to: (90, 80),
            offset: (-5.0, 0.0),
            from_unrotated: (100, 80),
            to_unrotated: (90, 80),
            offset_unrotated: (-5.0, 0.0),
        };
        assert!(!change.only_grows());
        let recipe = json!({"aiPatches": [
            {"id": "p", "visible": true, "generated": true,
                "patchData": {"color": "c", "mask": "m", "offsetX": 10, "offsetY": 10}},
            {"id": "q", "visible": false, "patchData": {"color": "c", "mask": "m"}}
        ]});
        let moved = migrate(&recipe, Some(&change), &recipe, 1);
        assert_eq!(moved.hidden_patches, 1);
        let patch = &moved.adjustments["aiPatches"][0];
        assert_eq!(patch["visible"], false);
        assert_eq!(patch[HIDDEN_PATCH], true);
        assert_eq!(patch["generated"], true);
        assert_eq!(patch["patchData"]["offsetX"], 10);
        // One hidden before stays as it was.
        assert!(
            moved.adjustments["aiPatches"][1]
                .get(HIDDEN_PATCH)
                .is_none()
        );
    }

    /// Without a frame change only the key changes.
    #[test]
    fn without_a_change_only_the_key_is_written() {
        let recipe = json!({"exposure": 1.0, "crop": {"x": 1.0, "y": 2.0, "width": 3.0, "height": 4.0},
            "spots": [{"x": 5.0, "y": 6.0}]});
        let moved = migrate(&recipe, None, &recipe, 1).adjustments;
        let mut expected = recipe.clone();
        expected[KEY] = json!(1);
        assert_eq!(moved, expected);
    }

    /// With a lens correction the warp is centred on the frame: a mask-frame
    /// point keeps the source pixel it shows.
    #[test]
    fn mask_frame_points_follow_the_warp() {
        let change = grows_right((400, 300), 24);
        let recipe = json!({"lensDistortionEnabled": true, "lensDistortionAmount": 100.0,
            "lensDistortionParams": {"k1": -0.08, "k2": 0.02, "k3": 0.0, "model": 0,
                "tca_vr": 1.0, "tca_vb": 1.0, "vig_k1": 0.0, "vig_k2": 0.0, "vig_k3": 0.0}});
        let map = FrameMap::new(change, &recipe);
        let params = get_geometry_params_from_json(&recipe);
        let (then, now) = (
            WarpPoints::new(&params, 400, 300),
            WarpPoints::new(&params, 424, 300),
        );
        for p in [(10.0, 10.0), (200.0, 150.0), (390.0, 280.0)] {
            let q = map.mask(p);
            let a = then.to_source(p.0 as f32, p.1 as f32).unwrap();
            let b = now.to_source(q.0 as f32, q.1 as f32).unwrap();
            assert!(
                (a.0 - b.0).abs() < 0.05 && (a.1 - b.1).abs() < 0.05,
                "{p:?}: {a:?} vs {b:?}"
            );
        }
        // The frame's centre moved, so the warp moves the content.
        let q = map.mask((10.0, 10.0));
        assert!((q.0 - 10.0).abs() > 0.1);
    }

    /// Relight depth stays valid where the shown frame stays (a crop), and
    /// is left to be analysed again where it changes.
    #[test]
    fn relight_depth_follows_the_shown_frame() {
        let change = grows_right((400, 300), 24);
        let depth =
            |recipe: &Value| json!({"data": "d", "geometry": crate::relight::geometry(recipe)});
        let mut uncropped = json!({"relightLights": [{"x": 50.0, "y": 50.0}]});
        uncropped["relightDepth"] = depth(&uncropped);
        let out = migrate(&uncropped, Some(&change), &uncropped, 1).adjustments;
        assert_ne!(
            out["relightDepth"]["geometry"],
            crate::relight::geometry(&out)
        );
        let light_x = out["relightLights"][0]["x"].as_f64().unwrap();
        assert!((light_x / 100.0 * 424.0 - 200.0).abs() < 1e-6);
        let mut cropped = json!({"crop": {"x": 10.0, "y": 10.0, "width": 100.0, "height": 100.0}});
        cropped["relightDepth"] = depth(&cropped);
        let out = migrate(&cropped, Some(&change), &cropped, 1).adjustments;
        assert_eq!(
            out["relightDepth"]["geometry"],
            crate::relight::geometry(&out)
        );
    }

    /// Guided perspective: its lines move with the frame and the rest moves
    /// through the warp they give on the new frame, so a spot keeps the
    /// source pixel it heals; faces that applied (any stamp rule) move and
    /// keep applying.
    #[test]
    fn points_follow_the_migrated_warp_and_faces_keep_applying() {
        let change = grows_right((400, 300), 24);
        let mut recipe = json!({
            "guidedPerspective": {"enabled": true, "autoCrop": false, "lines": [
                {"id": "a", "type": "vertical", "p1": {"x": 0.2, "y": 0.1}, "p2": {"x": 0.25, "y": 0.9}},
                {"id": "b", "type": "vertical", "p1": {"x": 0.8, "y": 0.1}, "p2": {"x": 0.74, "y": 0.9}}]},
            "spots": [{"id": "s", "x": 300.0, "y": 60.0, "radius": 5.0, "sourceX": 320.0, "sourceY": 60.0}],
            "faceRetouch": {"faces": [{"crop": [1.0, 0.0, 280.0, 0.0, 1.0, 40.0], "box": [280.0, 40.0, 40.0, 40.0],
                "blemishes": [{"id": 1, "x": 300.0, "y": 60.0, "r": 2.0}]}]},
        });
        for stamp in [
            crate::face_retouch::frame_key(&recipe),
            crate::face_retouch::frame_key_like("old", &recipe),
        ] {
            recipe["faceRetouch"]["frame"] = json!(stamp);
            let out = migrate(&recipe, Some(&change), &recipe, 1).adjustments;
            let then = WarpPoints::new(&get_geometry_params_from_json(&recipe), 400, 300);
            let now = WarpPoints::new(&get_geometry_params_from_json(&out), 424, 300);
            let (x, y) = (
                out["spots"][0]["x"].as_f64().unwrap(),
                out["spots"][0]["y"].as_f64().unwrap(),
            );
            let a = then.to_source(300.0, 60.0).unwrap();
            let b = now.to_source(x as f32, y as f32).unwrap();
            assert!(
                (a.0 - b.0).abs() < 0.05 && (a.1 - b.1).abs() < 0.05,
                "{a:?} vs {b:?}"
            );
            // The faces moved and are still applied by the renderer.
            let frame = out["faceRetouch"]["frame"].as_str().unwrap();
            assert!(crate::face_retouch::frame_matches(frame, &out), "{stamp}");
            assert_ne!(out["faceRetouch"]["faces"][0]["box"][0], 280.0);
            // Each blemish moves on its own, as a spot at its place does.
            assert_eq!(
                out["faceRetouch"]["faces"][0]["blemishes"][0]["x"],
                out["spots"][0]["x"]
            );
            assert_eq!(
                out["faceRetouch"]["faces"][0]["blemishes"][0]["y"],
                out["spots"][0]["y"]
            );
            // The crop map carries the weights' corner where the warp puts it.
            let crop = &out["faceRetouch"]["faces"][0]["crop"];
            let mapped = FrameMap::between(change, &recipe, &out)
                .shown(SourceGeometry::default(), (280.0, 40.0));
            assert!((crop[2].as_f64().unwrap() - mapped.0).abs() < 1e-9);
        }
    }

    #[test]
    fn the_requested_version() {
        assert_eq!(requested(&json!({})), VERSION);
        assert_eq!(requested(&Value::Null), VERSION);
        assert_eq!(requested(&json!({"exposure": 1.0})), 0);
        assert_eq!(requested(&json!({"exposure": 1.0, KEY: 1})), 1);
        assert_eq!(requested(&json!({"exposure": 1.0, KEY: 99})), VERSION);
        // Files no frame version can change develop version 0's frame.
        assert_eq!(effective("/p/A.NEF", 1), 0);
        assert_eq!(effective("/p/A.jpg", 1), 0);
    }
}
