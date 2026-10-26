use crate::image_processing::{GeometryParams, WarpPoints, get_geometry_params_from_json};
use crate::masks::{
    AiDepthMaskParameters, AiForegroundMaskParameters, AiSkyMaskParameters, AiSubjectMaskParameters,
};
use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, GenericImageView, GrayImage, Luma, Rgba};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::f32::consts::PI;
use std::hash::{Hash, Hasher};
#[cfg(test)]
use std::io::Cursor;
use std::io::Write;
use std::sync::{Arc, Weak};

pub use crate::masks::{MaskDefinition, SubMask, SubMaskMode, default_opacity};

mod precise;

/// Image before orientation, flip, rotation and crop, sampled in original-image
/// coordinates. Fast RAW decodes and background previews can use fewer pixels.
#[derive(Clone)]
pub struct ParametricMaskSource {
    pub image: Arc<DynamicImage>,
    coordinate_dimensions: (f64, f64),
    /// The photo's orientation, flips and straightening, for parametric masks
    /// that follow the photo (luma range) instead of storing their own.
    geometry: SourceGeometry,
    /// The image carries the CPU default raw curve
    /// (`apply_cpu_default_raw_processing`) rather than sRGB; Auto Mask
    /// version 2 inverts it to work in linear light.
    raw_curve: bool,
}

/// Orientation steps, flips and rotation of the photo a mask source belongs to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SourceGeometry {
    pub rotation: f32,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub orientation_steps: u8,
}

impl SourceGeometry {
    pub fn from_adjustments(adjustments: &Value) -> Self {
        Self {
            rotation: adjustments["rotation"].as_f64().unwrap_or(0.0) as f32,
            flip_horizontal: adjustments["flipHorizontal"].as_bool().unwrap_or(false),
            flip_vertical: adjustments["flipVertical"].as_bool().unwrap_or(false),
            orientation_steps: (adjustments["orientationSteps"].as_u64().unwrap_or(0) % 4) as u8,
        }
    }

    fn is_identity(&self) -> bool {
        self.rotation == 0.0
            && !self.flip_horizontal
            && !self.flip_vertical
            && self.orientation_steps.is_multiple_of(4)
    }
}

/// The photo's geometry when its masks are drawn. AI masks are bitmaps in the
/// photo's own frame (the lens- and transform-corrected photo before its
/// orientation, flips, straightening and crop); they are placed by this
/// geometry, not by the one they were made in, so they stay on the content
/// when the photo is turned, flipped, straightened or cropped later, as
/// Lightroom's and Capture One's masks do. A mask that records the lens and
/// transform warp it was made in (`warp`) is moved into the current one.
#[derive(Clone, Debug, Default)]
pub struct PhotoGeometry {
    pub frame: SourceGeometry,
    /// The lens and transform warp now; None: unknown (masks are taken as
    /// made in it).
    warp: Option<Arc<GeometryParams>>,
    /// `warp`'s identity for the mask cache.
    warp_key: u64,
    /// The photo's own size (full resolution, before orientation), for
    /// drawn masks whose frame records another photo's size or none (see
    /// `ShapeFrame`); None: unknown (shapes are placed with their frame's
    /// size, as before).
    size: Option<(f64, f64)>,
    /// Whether `size` is the full decode's own (`with_decoded_size`).
    decoded: bool,
}

impl PhotoGeometry {
    pub fn from_adjustments(adjustments: &Value) -> Self {
        if !adjustments.is_object() {
            return Self::default();
        }
        let warp = warp_geometry(get_geometry_params_from_json(adjustments));
        Self {
            frame: SourceGeometry::from_adjustments(adjustments),
            warp_key: warp_key(&warp),
            warp: Some(Arc::new(warp)),
            size: None,
            decoded: false,
        }
    }

    /// The photo's own size: full-resolution pixels before orientation (the
    /// frame drawn masks' coordinates and `frame` sizes are in). Sizes that
    /// are not finite and positive leave it unknown.
    pub fn with_photo_size(mut self, width: f64, height: f64) -> Self {
        self.size = (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
            .then_some((width, height));
        self.decoded = false;
        self
    }

    /// `with_photo_size` with the size of the photo's full decode, exactly as
    /// the editor knows it: Lightroom masks still in their imported form
    /// are then placed for the render (`lightroom_masks`), as the editor
    /// places them. Reduced copies, which know the size only roughly, use
    /// `with_photo_size` and leave them out.
    pub fn with_decoded_size(self, width: u32, height: u32) -> Self {
        let mut geometry = self.with_photo_size(f64::from(width), f64::from(height));
        geometry.decoded = geometry.size.is_some();
        geometry
    }

    /// The photo as drawn masks are placed on it now.
    fn shown(&self) -> Shown {
        Shown {
            geometry: self.frame,
            size: self.size,
        }
    }

    /// The geometry without its straightening, for frames that show the
    /// photo oriented and flipped but not yet straightened.
    pub fn unrotated(mut self) -> Self {
        self.frame.rotation = 0.0;
        self
    }

    /// From the warp a mask was made in to the current one, when they differ.
    fn warp_change(&self, params: &Value, width: u32, height: u32) -> Option<WarpChange> {
        let now = self.warp.as_deref()?;
        let then = warp_geometry(get_geometry_params_from_json(
            params.get("warp").filter(|w| w.is_object())?,
        ));
        (warp_key(&then) != self.warp_key).then(|| WarpChange {
            now: WarpPoints::new(now, width, height),
            then: WarpPoints::new(&then, width, height),
        })
    }

    fn hash_into(&self, hasher: &mut DefaultHasher) {
        self.frame.rotation.to_bits().hash(hasher);
        self.frame.flip_horizontal.hash(hasher);
        self.frame.flip_vertical.hash(hasher);
        self.frame.orientation_steps.hash(hasher);
        self.warp.is_some().hash(hasher);
        self.warp_key.hash(hasher);
        self.size
            .map(|(w, h)| (w.to_bits(), h.to_bits()))
            .hash(hasher);
        self.decoded.hash(hasher);
    }
}

/// The photo drawn masks are placed on: its orientation, flips and
/// straightening now, and its own size when known (see `ShapeFrame`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Shown {
    geometry: SourceGeometry,
    size: Option<(f64, f64)>,
}

impl From<SourceGeometry> for Shown {
    fn from(geometry: SourceGeometry) -> Self {
        Self {
            geometry,
            size: None,
        }
    }
}

/// The parts of a warp that move pixels (vignetting and colour fringe
/// corrections do not; nor does the profile's source, whose distortion
/// strength is the same either way).
fn warp_geometry(params: GeometryParams) -> GeometryParams {
    let defaults = GeometryParams::default();
    GeometryParams {
        lens_profile_exact: defaults.lens_profile_exact,
        lens_vignette_amount: defaults.lens_vignette_amount,
        lens_tca_amount: defaults.lens_tca_amount,
        lens_tca_enabled: defaults.lens_tca_enabled,
        lens_vignette_enabled: defaults.lens_vignette_enabled,
        tca_vr: defaults.tca_vr,
        tca_vb: defaults.tca_vb,
        tca_vr2: defaults.tca_vr2,
        tca_vb2: defaults.tca_vb2,
        vig_k1: defaults.vig_k1,
        vig_k2: defaults.vig_k2,
        vig_k3: defaults.vig_k3,
        auto_ca: defaults.auto_ca,
        auto_ca_version: defaults.auto_ca_version,
        auto_ca_versus_own: defaults.auto_ca_versus_own,
        ..params
    }
}

fn warp_key(params: &GeometryParams) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(params)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

/// A stored mask's pixel for a pixel of the current warped frame: through
/// the source to the frame of the warp the mask was made in.
struct WarpChange {
    now: WarpPoints,
    then: WarpPoints,
}

impl WarpChange {
    fn map(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let (source_x, source_y) = self.now.to_source(x, y)?;
        self.then.to_output(source_x, source_y)
    }
}

/// The lens and transform adjustments of `adjustments` (the warp an AI mask
/// is computed in), to store with the mask as its `warp`.
pub fn mask_warp(adjustments: &Value) -> Value {
    Value::Object(
        crate::cache_utils::GEOMETRY_KEYS
            .iter()
            .filter_map(|key| {
                adjustments
                    .get(*key)
                    .filter(|v| !v.is_null())
                    .map(|v| (key.to_string(), v.clone()))
            })
            .collect(),
    )
}

/// The frame a drawn mask (a radial or linear gradient, a brush or flow
/// stroke) was drawn in, stored as its `frame`: the photo's orientation
/// steps, flips and straightening then, and the photo's own size (full
/// resolution, before orientation). Drawn masks keep the coordinates of the
/// photo as it was shown (full-resolution pixels after orientation, flips
/// and straightening, before the crop); with their frame they are drawn
/// where that spot of the photo is shown now, so they stay on the content
/// when the photo is turned, flipped or straightened later, as Lightroom's
/// and Capture One's masks do. Without one (older recipes) they were drawn
/// as the photo is shown now.
///
/// A frame without a size (stamped where the photo's size was not at hand,
/// see `keep_drawn_masks_on_photo`) is in the size of the photo it belongs
/// to. On a photo of another size (a mask copied, synced or in a preset) the
/// shape is placed proportionally, as Lightroom keeps its masks in
/// coordinates relative to the photo: its points at the same fraction of the
/// photo's own width and height, its lengths (radii, ranges, brush sizes)
/// scaled with them.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ShapeFrame {
    #[serde(default)]
    width: Option<f64>,
    #[serde(default)]
    height: Option<f64>,
    #[serde(default)]
    rotation: f32,
    #[serde(default)]
    flip_horizontal: bool,
    #[serde(default)]
    flip_vertical: bool,
    #[serde(default)]
    orientation_steps: u8,
}

impl ShapeFrame {
    /// The usable frame stored under `frame` in `value`.
    fn of(value: &Value) -> Option<Self> {
        serde_json::from_value::<Self>(value.get("frame")?.clone())
            .ok()?
            .usable()
    }

    /// Readable: a finite rotation, and a positive size or none.
    fn usable(self) -> Option<Self> {
        let size_ok = match (self.width, self.height) {
            (None, None) => true,
            (Some(w), Some(h)) => w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0,
            _ => false,
        };
        (size_ok && self.rotation.is_finite()).then_some(self)
    }

    fn size(&self) -> Option<(f64, f64)> {
        self.width.zip(self.height)
    }

    fn geometry(&self) -> SourceGeometry {
        SourceGeometry {
            rotation: self.rotation,
            flip_horizontal: self.flip_horizontal,
            flip_vertical: self.flip_vertical,
            orientation_steps: self.orientation_steps % 4,
        }
    }

    /// From this frame to the photo `now`, rasterized at `scale`; none when
    /// the shape is shown as it was drawn (same geometry and size), or when
    /// neither the frame nor `now` knows the photo's size.
    fn map_to(&self, now: Shown, scale: f32) -> Option<ShapeMap> {
        let drawn = self.geometry();
        let scale = scale as f64;
        if !(scale.is_finite() && scale > 0.0) {
            return None;
        }
        let from = self.size().or(now.size)?;
        // Sizes less than a raster pixel apart are the same photo (a reduced
        // preview knows the photo's size only to its own pixels).
        let to = now
            .size
            .filter(|to| {
                (to.0 - from.0).abs() * scale >= 1.0 || (to.1 - from.1).abs() * scale >= 1.0
            })
            .unwrap_or(from);
        if drawn == now.geometry && from == to {
            return None;
        }
        Some(ShapeMap {
            from: Placement::new(drawn, (from.0 * scale, from.1 * scale)),
            to: Placement::new(now.geometry, (to.0 * scale, to.1 * scale)),
            resize: (from != to).then_some((to.0 / from.0, to.1 / from.1)),
            scale,
        })
    }
}

/// Mask kinds drawn as one shape with its own `frame`.
const FRAMED_SHAPES: [&str; 2] = ["radial", "linear"];
/// Mask kinds whose `lines` are strokes, each with its own `frame`.
const FRAMED_STROKES: [&str; 6] = ["brush", "flow", "clone", "heal", "liquify", "retouch"];

/// The `frame` (see `ShapeFrame`) of the photo shown in `adjustments`: its
/// orientation steps, flips and straightening, and its own `size` (full
/// resolution, before orientation) when known.
pub fn shape_frame(adjustments: &Value, size: Option<(f64, f64)>) -> Value {
    let mut frame = serde_json::json!({
        "rotation": adjustments["rotation"].as_f64().unwrap_or(0.0),
        "flipHorizontal": adjustments["flipHorizontal"] == true,
        "flipVertical": adjustments["flipVertical"] == true,
        "orientationSteps": adjustments["orientationSteps"].as_i64().unwrap_or(0).rem_euclid(4),
    });
    if let Some((width, height)) =
        size.filter(|(w, h)| w.is_finite() && h.is_finite() && *w > 0.0 && *h > 0.0)
    {
        frame["width"] = Value::from(width);
        frame["height"] = Value::from(height);
    }
    frame
}

/// Drawn masks without a frame (made before frames were kept) were drawn as
/// the photo is shown. When an edit changes the photo's orientation, flips
/// or straightening (`previous` → `next`), those the edit left as they were
/// get the frame they were shown in until then, so they stay on the photo's
/// content (as Lightroom's and Capture One's masks do) instead of on the
/// screen. Every path that changes a photo's geometry and keeps its masks
/// goes through this (pasted, synced and preset settings, auto settings);
/// the editor does the same in `src/utils/maskGeometry.ts`
/// (`keepDrawnMasksOnPhoto`). A sub-mask is left alone when its parameters
/// equal those of a sub-mask before the edit; masks the edit brings along
/// keep their own frames. `size` is the photo's own size when known; without
/// it the frame records none and the shape takes the photo's size when it
/// is drawn. Returns whether anything was stamped.
pub fn keep_drawn_masks_on_photo(
    previous: &Value,
    next: &mut Value,
    size: Option<(f64, f64)>,
) -> bool {
    if !previous.is_object()
        || !next.is_object()
        || SourceGeometry::from_adjustments(previous) == SourceGeometry::from_adjustments(next)
    {
        return false;
    }
    let containers = ["masks", "aiPatches"];
    let sub_masks = |adjustments: &Value| -> Vec<Value> {
        containers
            .iter()
            .filter_map(|key| adjustments.get(*key).and_then(Value::as_array))
            .flatten()
            .filter_map(|container| container.get("subMasks").and_then(Value::as_array))
            .flatten()
            .filter_map(|sub| sub.get("parameters").cloned())
            .collect()
    };
    let shown = sub_masks(previous);
    let frame = shape_frame(previous, size);
    let mut stamped = false;
    for key in containers {
        let Some(list) = next.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        for sub in list
            .iter_mut()
            .filter_map(|container| container.get_mut("subMasks").and_then(Value::as_array_mut))
            .flatten()
        {
            let kind = sub.get("type").and_then(Value::as_str).unwrap_or_default();
            let (shape, strokes) = (
                FRAMED_SHAPES.contains(&kind),
                FRAMED_STROKES.contains(&kind),
            );
            if !(shape || strokes) {
                continue;
            }
            let Some(parameters) = sub.get_mut("parameters").filter(|p| p.is_object()) else {
                continue;
            };
            if !shown.iter().any(|before| before == parameters) {
                continue;
            }
            if shape {
                if parameters.get("frame").is_none_or(Value::is_null) {
                    parameters["frame"] = frame.clone();
                    stamped = true;
                }
            } else if let Some(lines) = parameters.get_mut("lines").and_then(Value::as_array_mut) {
                for line in lines.iter_mut().filter(|line| line.is_object()) {
                    if line.get("frame").is_none_or(Value::is_null) {
                        line["frame"] = frame.clone();
                        stamped = true;
                    }
                }
            }
        }
    }
    stamped
}

/// A stroke's `frame`: an unreadable one is left out (the stroke is drawn
/// as stored) instead of failing the whole mask.
fn lenient_frame<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ShapeFrame>, D::Error> {
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value
        .and_then(|value| serde_json::from_value::<ShapeFrame>(value).ok())
        .and_then(ShapeFrame::usable))
}

/// Moves the points of strokes drawn in another frame to where the photo
/// shows them `now`, rasterized at `scale` (see `ShapeFrame`): each stroke
/// by its own frame, so one painted after a turn and those painted before
/// each keep their spot. Strokes drawn as the photo is shown are untouched;
/// on a photo of another size a stroke's brush size scales with it.
fn strokes_as_shown<'a>(
    strokes: impl Iterator<Item = (Option<ShapeFrame>, &'a mut Vec<Point>, &'a mut f32)>,
    now: Shown,
    scale: f32,
) {
    for (frame, points, brush_size) in strokes {
        if let Some(map) = frame.and_then(|frame| frame.map_to(now, scale)) {
            for point in points.iter_mut() {
                (point.x, point.y) = map.point(point.x, point.y);
            }
            if map.resize.is_some() {
                *brush_size = (*brush_size as f64 * map.area_scale().sqrt()) as f32;
            }
        }
    }
}

/// Where the photo's pixels are shown in one geometry at a raster scale, in
/// pixel indices as the image operations move them: `apply_coarse_rotation`,
/// `apply_flip` (index i of n to n − 1 − i), then `apply_rotation` about the
/// frame's centre. The mask rasterizers evaluate shapes at pixel indices, so
/// a shape moved by these maps covers the same pixels of the photo.
struct Placement {
    geometry: SourceGeometry,
    /// The photo's size before orientation, and as shown.
    photo: (f64, f64),
    shown: (f64, f64),
    sin_cos: (f64, f64),
}

impl Placement {
    fn new(geometry: SourceGeometry, photo: (f64, f64)) -> Self {
        Self {
            geometry,
            photo,
            shown: if geometry.orientation_steps % 2 == 1 {
                (photo.1, photo.0)
            } else {
                photo
            },
            sin_cos: (geometry.rotation as f64).to_radians().sin_cos(),
        }
    }

    /// The photo's pixel shown at (`x`, `y`).
    fn to_photo(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let (w, h) = self.shown;
        let (sin, cos) = self.sin_cos;
        let (dx, dy) = (x - w / 2.0, y - h / 2.0);
        let x = dx * cos + dy * sin + w / 2.0;
        let y = -dx * sin + dy * cos + h / 2.0;
        let x = if self.geometry.flip_horizontal {
            w - 1.0 - x
        } else {
            x
        };
        let y = if self.geometry.flip_vertical {
            h - 1.0 - y
        } else {
            y
        };
        match self.geometry.orientation_steps {
            1 => (y, w - 1.0 - x),
            2 => (w - 1.0 - x, h - 1.0 - y),
            3 => (h - 1.0 - y, x),
            _ => (x, y),
        }
    }

    /// Where the photo's pixel (`x`, `y`) is shown.
    fn to_shown(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let (pw, ph) = self.photo;
        let (x, y) = match self.geometry.orientation_steps {
            1 => (ph - 1.0 - y, x),
            2 => (pw - 1.0 - x, ph - 1.0 - y),
            3 => (y, pw - 1.0 - x),
            _ => (x, y),
        };
        let (w, h) = self.shown;
        let x = if self.geometry.flip_horizontal {
            w - 1.0 - x
        } else {
            x
        };
        let y = if self.geometry.flip_vertical {
            h - 1.0 - y
        } else {
            y
        };
        let (sin, cos) = self.sin_cos;
        let (dx, dy) = (x - w / 2.0, y - h / 2.0);
        (w / 2.0 + dx * cos - dy * sin, h / 2.0 + dx * sin + dy * cos)
    }
}

/// A drawn mask's points from the frame it was drawn in to the photo as
/// shown now (full-resolution pixels before the crop).
struct ShapeMap {
    from: Placement,
    to: Placement,
    /// From the frame's photo to a photo of another size: the factors of
    /// its own width and height.
    resize: Option<(f64, f64)>,
    scale: f64,
}

impl ShapeMap {
    fn point(&self, x: f64, y: f64) -> (f64, f64) {
        let (x, y) = self.from.to_photo((x * self.scale, y * self.scale));
        // The same fraction of the other photo, pixel index for pixel index
        // (index i of n covers (i + 0.5) / n of the side).
        let (x, y) = match self.resize {
            Some((sx, sy)) => ((x + 0.5) * sx - 0.5, (y + 0.5) * sy - 0.5),
            None => (x, y),
        };
        let (x, y) = self.to.to_shown((x, y));
        (x / self.scale, y / self.scale)
    }

    fn direction(&self, dx: f64, dy: f64) -> (f64, f64) {
        let (ox, oy) = self.point(0.0, 0.0);
        let (x, y) = self.point(dx, dy);
        (x - ox, y - oy)
    }

    /// How areas scale (1 unless the photo's size differs).
    fn area_scale(&self) -> f64 {
        self.resize.map_or(1.0, |(sx, sy)| sx * sy)
    }

    /// How a length along the unit direction (`dx`, `dy`) scales.
    fn length_along(&self, dx: f64, dy: f64) -> f64 {
        let (x, y) = self.direction(dx, dy);
        x.hypot(y)
    }

    /// One flip more on one side than on the other mirrors the shape.
    fn mirrors(&self) -> bool {
        let (a, b) = (self.from.geometry, self.to.geometry);
        a.flip_horizontal ^ a.flip_vertical ^ b.flip_horizontal ^ b.flip_vertical
    }
}

/// A radial's or linear's parameters as drawn on the photo `now`, rasterized
/// at `scale` (see `ShapeFrame`): borrowed when nothing moves, so shapes
/// drawn in the current geometry and older ones render as before.
fn shape_as_shown<'a>(kind: &str, params: &'a Value, now: Shown, scale: f32) -> Cow<'a, Value> {
    let num = |value: &Value, key: &str| value.get(key).and_then(Value::as_f64);
    let moved = match kind {
        "radial" => ShapeFrame::of(params)
            .and_then(|frame| frame.map_to(now, scale))
            .and_then(|map| {
                let (x, y) = map.point(num(params, "centerX")?, num(params, "centerY")?);
                let angle = num(params, "rotation").unwrap_or(0.0).to_radians();
                let (ax, ay) = map.direction(angle.cos(), angle.sin());
                let rotation = ay.atan2(ax).to_degrees();
                let mut shown = params.clone();
                shown["centerX"] = Value::from(x);
                shown["centerY"] = Value::from(y);
                shown["rotation"] = Value::from(rotation);
                let mut values = vec![x, y, rotation];
                if map.resize.is_some() {
                    // The first axis as it is carried along; the second
                    // keeps the ellipse's area in proportion (exact for an
                    // ellipse along the photo's sides).
                    let along = ax.hypot(ay);
                    let radius_x = num(params, "radiusX").unwrap_or(0.0) * along;
                    let radius_y = num(params, "radiusY").unwrap_or(0.0) * map.area_scale() / along;
                    shown["radiusX"] = Value::from(radius_x);
                    shown["radiusY"] = Value::from(radius_y);
                    values.extend([radius_x, radius_y]);
                }
                values.iter().all(|v| v.is_finite()).then_some(shown)
            }),
        "linear" => ShapeFrame::of(params)
            .and_then(|frame| frame.map_to(now, scale))
            .and_then(|map| {
                let (sx, sy) = (num(params, "startX")?, num(params, "startY")?);
                let (ex, ey) = (num(params, "endX")?, num(params, "endY")?);
                let start = map.point(sx, sy);
                let end = map.point(ex, ey);
                // Mirrored, the start (100%) side would fall on the other
                // side of the line through start and end: swapping them
                // keeps every side on its part of the photo.
                let (start, end) = if map.mirrors() {
                    (end, start)
                } else {
                    (start, end)
                };
                let mut shown = params.clone();
                shown["startX"] = Value::from(start.0);
                shown["startY"] = Value::from(start.1);
                shown["endX"] = Value::from(end.0);
                shown["endY"] = Value::from(end.1);
                let mut values = vec![start.0, start.1, end.0, end.1];
                if map.resize.is_some() {
                    // Distances from the central line scale as areas over
                    // lengths along it.
                    let length = (ex - sx).hypot(ey - sy);
                    let factor = if length > 0.0 {
                        map.area_scale() / map.length_along((ex - sx) / length, (ey - sy) / length)
                    } else {
                        map.area_scale().sqrt()
                    };
                    for key in ["range", "rangeEnd"] {
                        if let Some(range) = num(params, key) {
                            shown[key] = Value::from(range * factor);
                            values.push(range * factor);
                        }
                    }
                    if params.get("range").is_none() {
                        // The default range, carried along.
                        shown["range"] = Value::from(default_range() as f64 * factor);
                    }
                }
                values.iter().all(|v| v.is_finite()).then_some(shown)
            }),
        // Strokes move in `strokes_as_shown`, on their parsed points.
        _ => None,
    };
    moved.map_or(Cow::Borrowed(params), Cow::Owned)
}

impl ParametricMaskSource {
    pub fn new(
        image: Arc<DynamicImage>,
        coordinate_dimensions: (f64, f64),
    ) -> Result<Self, String> {
        validate_coordinate_dimensions(coordinate_dimensions)?;
        Ok(Self {
            image,
            coordinate_dimensions,
            geometry: SourceGeometry::default(),
            raw_curve: false,
        })
    }

    /// Whether the image carries the CPU default raw curve (see `raw_curve`).
    pub fn with_raw_curve(mut self, raw_curve: bool) -> Self {
        self.raw_curve = raw_curve;
        self
    }

    /// The geometry of the photo the source belongs to (see `SourceGeometry`).
    pub fn with_geometry(mut self, adjustments: &Value) -> Self {
        self.geometry = SourceGeometry::from_adjustments(adjustments);
        self
    }

    /// Restore the original coordinate domain when preparation starts with an
    /// image that was already resized using rounded integer dimensions.
    pub fn with_coordinate_dimensions(mut self, dimensions: (f64, f64)) -> Result<Self, String> {
        validate_coordinate_dimensions(dimensions)?;
        self.coordinate_dimensions = dimensions;
        Ok(self)
    }

    /// Resizing changes the sampling ratios independently on both axes, while
    /// saved picker coordinates and rotation centers stay in the same domain.
    pub fn with_resized_image(self, image: DynamicImage) -> Self {
        Self {
            image: Arc::new(image),
            coordinate_dimensions: self.coordinate_dimensions,
            geometry: self.geometry,
            raw_curve: self.raw_curve,
        }
    }

    fn sampling_scales(&self) -> (f64, f64) {
        (
            self.image.width() as f64 / self.coordinate_dimensions.0,
            self.image.height() as f64 / self.coordinate_dimensions.1,
        )
    }
}

pub struct FullWarpedMaskCache {
    source: Weak<DynamicImage>,
    is_raw: bool,
    geometry_hash: u64,
    image: Arc<DynamicImage>,
}

impl FullWarpedMaskCache {
    pub fn get(
        &self,
        source: &Arc<DynamicImage>,
        is_raw: bool,
        geometry_hash: u64,
    ) -> Option<Arc<DynamicImage>> {
        (Weak::ptr_eq(&self.source, &Arc::downgrade(source))
            && self.is_raw == is_raw
            && self.geometry_hash == geometry_hash)
            .then(|| Arc::clone(&self.image))
    }

    pub fn new(
        source: &Arc<DynamicImage>,
        is_raw: bool,
        geometry_hash: u64,
        image: Arc<DynamicImage>,
    ) -> Self {
        Self {
            source: Arc::downgrade(source),
            is_raw,
            geometry_hash,
            image,
        }
    }
}

fn prepare_warped_mask_image<'a>(
    source: &'a DynamicImage,
    is_raw: bool,
    adjustments: &Value,
) -> Result<Cow<'a, DynamicImage>, String> {
    let image = if is_raw {
        Cow::Owned(crate::image_processing::cpu_default_raw_processed(source))
    } else {
        Cow::Borrowed(source)
    };
    crate::image_processing::apply_geometry_warp(image, adjustments)
}

pub fn prepare_parametric_mask_source(
    source: &DynamicImage,
    is_raw: bool,
    source_scale: f32,
    adjustments: &Value,
) -> Result<ParametricMaskSource, String> {
    validate_source_scale(source_scale)?;
    ParametricMaskSource::new(
        Arc::new(prepare_warped_mask_image(source, is_raw, adjustments)?.into_owned()),
        (
            source.width() as f64 / source_scale as f64,
            source.height() as f64 / source_scale as f64,
        ),
    )
    .map(|source| source.with_geometry(adjustments).with_raw_curve(is_raw))
}

/// The warped image masks are computed on (fails only where the warp runs
/// on the GPU and the GPU fails).
pub fn prepare_shared_warped_mask_image(
    source: &Arc<DynamicImage>,
    is_raw: bool,
    adjustments: &Value,
) -> Result<Arc<DynamicImage>, String> {
    Ok(
        match prepare_warped_mask_image(source, is_raw, adjustments)? {
            Cow::Borrowed(_) => Arc::clone(source),
            Cow::Owned(image) => Arc::new(image),
        },
    )
}

pub fn validate_source_scale(scale: f32) -> Result<(), String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err("Parametric mask source scale must be finite and positive".into());
    }
    Ok(())
}

fn validate_coordinate_dimensions(dimensions: (f64, f64)) -> Result<(), String> {
    if !dimensions.0.is_finite()
        || !dimensions.1.is_finite()
        || dimensions.0 <= 0.0
        || dimensions.1 <= 0.0
    {
        return Err("Parametric mask source dimensions must be finite and positive".into());
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(crate = "serde")]
#[serde(rename_all = "camelCase")]
pub struct PatchData {
    pub color: String,
    pub mask: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
#[serde(crate = "serde")]
#[serde(rename_all = "camelCase")]
pub struct AiPatchDefinition {
    pub id: String,
    pub name: String,
    pub visible: bool,
    pub invert: bool,
    pub prompt: String,
    #[serde(default)]
    pub patch_data: Option<PatchData>,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    pub sub_masks: Vec<SubMask>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct GrowFeatherParameters {
    #[serde(default)]
    grow: f32,
    #[serde(default)]
    feather: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct RadialMaskParameters {
    center_x: f64,
    center_y: f64,
    radius_x: f64,
    radius_y: f64,
    rotation: f32,
    feather: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct LinearMaskParameters {
    start_x: f64,
    start_y: f64,
    end_x: f64,
    end_y: f64,
    /// Distance from the central (50%) line to the start (100%) line.
    #[serde(default = "default_range")]
    range: f32,
    /// Distance from the central line to the end (0%) line, when it differs
    /// from `range` (Capture One's Alt-drag). Absent: symmetric, as before.
    #[serde(default)]
    range_end: Option<f32>,
}

fn default_range() -> f32 {
    50.0
}

impl Default for LinearMaskParameters {
    fn default() -> Self {
        Self {
            start_x: 0.0,
            start_y: 0.0,
            end_x: 0.0,
            end_y: 0.0,
            range: default_range(),
            range_end: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Point {
    x: f64,
    y: f64,
    /// Pen pressure 0–1 at this point (pen strokes with a pressure mapping,
    /// see `BrushLine::pressure_size`); absent: 1, as for every mouse stroke.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    p: Option<f32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct BrushLine {
    tool: String,
    brush_size: f32,
    points: Vec<Point>,
    #[serde(default = "default_brush_feather")]
    feather: f32,
    /// Capture One's brush opacity and flow in percent; absent means 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flow: Option<f32>,
    /// Capture One's brush hardness in percent. Strokes that carry it use
    /// Capture One's tip and dab spacing and ignore `feather`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hardness: Option<f32>,
    /// Lightroom's brush (strokes from imported presets): its CenterWeight
    /// 0–1. Such strokes put one dab at every point, with Lightroom's tip,
    /// and `flow` is the coverage of one dab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lr_center_weight: Option<f32>,
    /// Lightroom's and Capture One's Auto Mask: each dab only reaches
    /// colours like the one under its centre (see `auto_mask_weight`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    auto_mask: bool,
    /// Auto Mask method. Absent: version 1, the Camera Raw-fitted colour
    /// distance of `auto_mask_weight` (old recipes and imported Lightroom
    /// strokes keep it). 2: the edge-aware mask of `AutoMaskModel`, one model
    /// per dab. 3: the same with the stroke's dabs modelled together
    /// (`stroke_models_v3`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auto_mask_version: Option<u32>,
    /// Dab placement of a dab-based stroke (not Lightroom's, which has a dab
    /// at every point). Absent: version 1, dabs `DabTip::step` apart ending
    /// at the last whole step, so a stroke stopped up to a step short of
    /// its last point (old recipes stay byte-identical). 2: a dab at the
    /// last point too, and a Capture One tip without Auto Mask lays its
    /// dabs closer (`DabTip::dab_version_2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dab_version: Option<u32>,
    /// Pen pressure (each point's `p`) scales the dab radius
    /// (`pressure_size`, Capture One's Pen Pressure) and/or the dab flow
    /// (`pressure_flow`) at that point. Absent: neither; a mouse stroke never
    /// carries them (user decision D3: a mouse paints at constant pressure).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pressure_size: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pressure_flow: bool,
    /// The photo's geometry the stroke was painted in (see `ShapeFrame`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_frame"
    )]
    frame: Option<ShapeFrame>,
}

/// Auto Mask, measured in Camera Raw (Lightroom stores such dabs as "M x y"
/// and computes the mask from the photo): a pixel keeps exp(−(ΔE/22.6)^1.36)
/// of a dab, ΔE the CIE Lab difference to the colour under the dab's
/// centre. Fitted on six strokes across two photographs (RMS 0.173 of the
/// kept share; normalising to the photo's colour spread fitted worse,
/// 0.199). notes: reference/auto-mask.md.
fn auto_mask_weight(pixel: [f32; 3], centre: [f32; 3]) -> f32 {
    let d = ((pixel[0] - centre[0]).powi(2)
        + (pixel[1] - centre[1]).powi(2)
        + (pixel[2] - centre[2]).powi(2))
    .sqrt();
    (-(d / 22.6).powf(1.36)).exp()
}

/// The photo's pixel a straightened frame shows at the unstraightened point
/// (`x`, `y`) (pixel indices of the oriented, flipped frame). Straightening
/// resamples the photo at that point (`apply_rotation`, bicubic, whose
/// weights peak at the nearest pixel), so a mask read from the photo's own
/// frame takes the nearest pixel too; rounding down put it half a pixel off
/// along each axis on average (a whole pixel wherever the point lies past a
/// pixel's middle). Without straightening the point is a pixel index
/// already and is kept as it is.
#[inline(always)]
fn nearest_when_straightened(x: f32, y: f32, rotation: f32) -> (f32, f32) {
    if rotation != 0.0 {
        (x.round(), y.round())
    } else {
        (x, y)
    }
}

/// The source pixel under each output mask pixel (the inverse of the photo's
/// orientation, flips and straightening), as `pixel_mask` maps them.
struct SourceSampler<'a> {
    image: &'a DynamicImage,
    source_size: (u32, u32),
    source_scale: (f64, f64),
    geometry: SourceGeometry,
    sin_cos: (f32, f32),
    scaled: (f32, f32),
    centre: (f32, f32),
    scale: f32,
    crop_offset: (f32, f32),
    raw_curve: bool,
}

impl<'a> SourceSampler<'a> {
    fn new(source: &'a ParametricMaskSource, scale: f32, crop_offset: (f32, f32)) -> Self {
        let geometry = source.geometry;
        let (full_w, full_h) = (
            source.coordinate_dimensions.0 as f32,
            source.coordinate_dimensions.1 as f32,
        );
        let (w, h) = if geometry.orientation_steps % 2 == 1 {
            (full_h, full_w)
        } else {
            (full_w, full_h)
        };
        let scaled = (w * scale, h * scale);
        Self {
            image: &source.image,
            source_size: source.image.dimensions(),
            source_scale: source.sampling_scales(),
            geometry,
            sin_cos: (geometry.rotation * PI / 180.0).sin_cos(),
            scaled,
            centre: (scaled.0 / 2.0, scaled.1 / 2.0),
            scale,
            crop_offset,
            raw_curve: source.raw_curve,
        }
    }

    fn at(&self, x_out: f32, y_out: f32) -> Option<Rgba<u8>> {
        let (sin_a, cos_a) = self.sin_cos;
        let x_centered = x_out + self.crop_offset.0 - self.centre.0;
        let y_centered = y_out + self.crop_offset.1 - self.centre.1;
        let (x_unrotated, y_unrotated) = nearest_when_straightened(
            x_centered * cos_a + y_centered * sin_a + self.centre.0,
            -x_centered * sin_a + y_centered * cos_a + self.centre.1,
            self.geometry.rotation,
        );
        // Pixel indices, as `apply_flip` and `apply_coarse_rotation` move
        // them: index i of n lands on n − 1 − i.
        let x = if self.geometry.flip_horizontal {
            self.scaled.0 - 1.0 - x_unrotated
        } else {
            x_unrotated
        };
        let y = if self.geometry.flip_vertical {
            self.scaled.1 - 1.0 - y_unrotated
        } else {
            y_unrotated
        };
        let (x, y) = match self.geometry.orientation_steps {
            1 => (y, self.scaled.0 - 1.0 - x),
            2 => (self.scaled.0 - 1.0 - x, self.scaled.1 - 1.0 - y),
            3 => (self.scaled.1 - 1.0 - y, x),
            _ => (x, y),
        };
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let x_src = ((x / self.scale) as f64 * self.source_scale.0) as u32;
        let y_src = ((y / self.scale) as f64 * self.source_scale.1) as u32;
        (x_src < self.source_size.0 && y_src < self.source_size.1)
            .then(|| self.image.get_pixel(x_src, y_src))
    }
}

/// CIE Lab of the source under each pixel of `bounds` (row-major), for Auto
/// Mask; pixels outside the photo are NaN and never match.
fn source_lab_region(sampler: &SourceSampler, bounds: (u32, u32, u32, u32)) -> Vec<[f32; 3]> {
    let (min_x, min_y, max_x, max_y) = bounds;
    (min_y..=max_y)
        .into_par_iter()
        .flat_map_iter(|y| {
            (min_x..=max_x).map(move |x| {
                sampler
                    .at(x as f32, y as f32)
                    .map_or([f32::NAN; 3], srgb8_to_lab)
            })
        })
        .collect()
}

/// Linear-light RGB of the source under each pixel of `bounds` (row-major),
/// for Auto Mask version 2; pixels outside the photo are NaN.
fn source_linear_region(sampler: &SourceSampler, bounds: (u32, u32, u32, u32)) -> Vec<[f32; 3]> {
    let lut = if sampler.raw_curve {
        raw8_linear_lut()
    } else {
        srgb8_linear_lut()
    };
    let (min_x, min_y, max_x, max_y) = bounds;
    (min_y..=max_y)
        .into_par_iter()
        .flat_map_iter(|y| {
            (min_x..=max_x).map(move |x| {
                sampler.at(x as f32, y as f32).map_or([f32::NAN; 3], |p| {
                    [lut[p[0] as usize], lut[p[1] as usize], lut[p[2] as usize]]
                })
            })
        })
        .collect()
}

fn srgb8_linear_lut() -> &'static [f32; 256] {
    static LINEAR: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LINEAR.get_or_init(|| {
        std::array::from_fn(|i| {
            let v = i as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
    })
}

/// The inverse of `apply_cpu_default_raw_processing`'s curve (gamma 2.38,
/// contrast 1.28 about 0.5) on 8-bit values.
fn raw8_linear_lut() -> &'static [f32; 256] {
    static LINEAR: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LINEAR.get_or_init(|| {
        std::array::from_fn(|i| {
            let v = i as f32 / 255.0;
            ((v - 0.5) / 1.28 + 0.5).clamp(0.0, 1.0).powf(2.38)
        })
    })
}

fn linear_to_lab(c: [f32; 3]) -> [f32; 3] {
    let x = (0.4124 * c[0] + 0.3576 * c[1] + 0.1805 * c[2]) / 0.950_47;
    let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let z = (0.0193 * c[0] + 0.1192 * c[1] + 0.9505 * c[2]) / 1.088_83;
    let f = |t: f32| {
        if t > 0.008_856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Share of `a` in `c` on the line from `b` to `a`, and whether `c` lies
/// within `AUTO_MASK_BAND` × |a − b| of that line.
fn share_on_line(c: [f32; 3], a: [f32; 3], b: [f32; 3]) -> Option<(f32, bool)> {
    let axis = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let len_sq = axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2];
    if len_sq <= 0.0 {
        return None;
    }
    let d = [c[0] - b[0], c[1] - b[1], c[2] - b[2]];
    let t = (d[0] * axis[0] + d[1] * axis[1] + d[2] * axis[2]) / len_sq;
    let r = [d[0] - t * axis[0], d[1] - t * axis[1], d[2] - t * axis[2]];
    let r_sq = r[0] * r[0] + r[1] * r[1] + r[2] * r[2];
    Some((t, r_sq < AUTO_MASK_BAND * AUTO_MASK_BAND * len_sq))
}

/// Colour clusters per dab for Auto Mask version 2.
const AUTO_MASK_CLUSTERS: usize = 3;
/// Samples per dab radius on the clustering grid (about π · 12² samples).
const AUTO_MASK_SAMPLES_PER_RADIUS: f32 = 12.0;
/// A background is fully one from this CIE Lab distance to the subject on
/// (fading in from the lower one): closer clusters are the subject's own
/// variation.
const AUTO_MASK_SEPARATION: (f32, f32) = (4.0, 10.0);
/// A cluster is a background only when it covers this share of the dab
/// (fading in from the lower one), not for a few stray pixels.
const AUTO_MASK_SHARE: (f32, f32) = (0.02, 0.06);
/// How far from the line between two clusters a third may lie to count as
/// their mix, relative to their distance.
const AUTO_MASK_BAND: f32 = 0.25;

/// The colour clusters under one Auto Mask dab (version 2 and 3), before
/// one of them is chosen as the subject.
#[derive(Clone, Copy)]
struct AutoMaskClusters {
    /// Cluster means in linear light and in CIE Lab.
    lins: [[f32; 3]; AUTO_MASK_CLUSTERS],
    labs: [[f32; 3]; AUTO_MASK_CLUSTERS],
    counts: [f32; AUTO_MASK_CLUSTERS],
    /// Mean sample offset from the centre (px) and mean sample distance.
    centroids: [[f32; 2]; AUTO_MASK_CLUSTERS],
    reach: [f32; AUTO_MASK_CLUSTERS],
    total: f32,
    centre_colour: [f32; 3],
    outer: f32,
}

impl AutoMaskClusters {
    /// The clusters of the dab at `centre` (pixels of the stroke's bounds,
    /// of which `colours` holds the linear-light colours, NaN outside the
    /// photo) that reaches `outer` pixels.
    fn of(
        colours: &[[f32; 3]],
        bb_w: usize,
        bb_h: usize,
        centre: (f32, f32),
        outer: f32,
    ) -> Option<Self> {
        let colour_at = |x: i64, y: i64| -> Option<[f32; 3]> {
            if x < 0 || y < 0 || x >= bb_w as i64 || y >= bb_h as i64 {
                return None;
            }
            let c = colours[y as usize * bb_w + x as usize];
            c[0].is_finite().then_some(c)
        };
        // The colour under the centre: its pixel and the four next to it.
        let (cx, cy) = (centre.0.round() as i64, centre.1.round() as i64);
        let mut sum = [0.0f32; 3];
        let mut n = 0.0f32;
        for (dx, dy) in [(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1)] {
            if let Some(c) = colour_at(cx + dx, cy + dy) {
                sum = [sum[0] + c[0], sum[1] + c[1], sum[2] + c[2]];
                n += 1.0;
            }
        }
        if n == 0.0 {
            return None;
        }
        let centre_colour = [sum[0] / n, sum[1] / n, sum[2] / n];

        let stride = (outer / AUTO_MASK_SAMPLES_PER_RADIUS).floor().max(1.0) as i64;
        let cells = outer.ceil() as i64 / stride + 1;
        let mut samples: Vec<([f32; 3], [f32; 3])> = Vec::new();
        let mut offsets: Vec<[f32; 2]> = Vec::new();
        for gy in -cells..=cells {
            for gx in -cells..=cells {
                let (x, y) = (cx + gx * stride, cy + gy * stride);
                let (ox, oy) = (x as f32 - centre.0, y as f32 - centre.1);
                if ox * ox + oy * oy > outer * outer {
                    continue;
                }
                if let Some(c) = colour_at(x, y) {
                    samples.push((c, linear_to_lab(c)));
                    offsets.push([ox, oy]);
                }
            }
        }
        if samples.len() < AUTO_MASK_CLUSTERS + 1 {
            return None;
        }

        // k-means seeded at the centre's colour, then each time at the
        // sample farthest from the seeds so far.
        let k = AUTO_MASK_CLUSTERS;
        let nearest_seed = |lab: [f32; 3], seeds: &[[f32; 3]]| {
            seeds
                .iter()
                .map(|seed| distance(lab, *seed))
                .fold(f32::INFINITY, f32::min)
        };
        let mut labs = vec![linear_to_lab(centre_colour)];
        while labs.len() < k {
            let farthest = samples
                .iter()
                .max_by(|a, b| nearest_seed(a.1, &labs).total_cmp(&nearest_seed(b.1, &labs)))
                .map_or(labs[0], |sample| sample.1);
            labs.push(farthest);
        }
        let mut lins = [[0.0f32; 3]; AUTO_MASK_CLUSTERS];
        let mut counts = [0.0f32; AUTO_MASK_CLUSTERS];
        let mut members = vec![0usize; samples.len()];
        for _ in 0..4 {
            let mut sums = [[0.0f32; 6]; AUTO_MASK_CLUSTERS];
            counts = [0.0; AUTO_MASK_CLUSTERS];
            for (i, &(lin, lab)) in samples.iter().enumerate() {
                let j = (0..k)
                    .min_by(|&a, &b| distance(lab, labs[a]).total_cmp(&distance(lab, labs[b])))
                    .unwrap_or(0);
                members[i] = j;
                for c in 0..3 {
                    sums[j][c] += lin[c];
                    sums[j][3 + c] += lab[c];
                }
                counts[j] += 1.0;
            }
            for j in 0..k {
                if counts[j] > 0.0 {
                    let s = sums[j];
                    lins[j] = [s[0] / counts[j], s[1] / counts[j], s[2] / counts[j]];
                    labs[j] = [s[3] / counts[j], s[4] / counts[j], s[5] / counts[j]];
                }
            }
        }
        let mut centroids = [[0.0f32; 2]; AUTO_MASK_CLUSTERS];
        let mut reach = [0.0f32; AUTO_MASK_CLUSTERS];
        for (offset, &j) in offsets.iter().zip(&members) {
            centroids[j] = [centroids[j][0] + offset[0], centroids[j][1] + offset[1]];
            reach[j] += offset[0].hypot(offset[1]);
        }
        for j in 0..k {
            if counts[j] > 0.0 {
                centroids[j] = [centroids[j][0] / counts[j], centroids[j][1] / counts[j]];
                reach[j] /= counts[j];
            }
        }
        Some(Self {
            lins,
            labs: labs.try_into().unwrap_or([[0.0; 3]; AUTO_MASK_CLUSTERS]),
            counts,
            centroids,
            reach,
            total: samples.len() as f32,
            centre_colour,
            outer,
        })
    }

    fn present(&self, j: usize) -> bool {
        self.counts[j] > 0.0
    }

    /// The cluster nearest the colour under the centre, in linear light.
    fn nearest_centre(&self) -> usize {
        (0..AUTO_MASK_CLUSTERS)
            .filter(|&j| self.present(j))
            .min_by(|&a, &b| {
                distance(self.centre_colour, self.lins[a])
                    .total_cmp(&distance(self.centre_colour, self.lins[b]))
            })
            .unwrap_or(0)
    }

    /// The model with cluster `subject` as the subject.
    fn model(&self, subject: usize) -> AutoMaskModel {
        let k = AUTO_MASK_CLUSTERS;
        let (lins, labs, counts) = (&self.lins, &self.labs, &self.counts);
        let present = |j: usize| self.present(j);
        let others = |j: usize| (0..k).filter(move |&m| m != j && m != subject && present(m));
        let purer_subject = |j: usize| {
            others(j).any(|m| {
                share_on_line(lins[subject], lins[j], lins[m])
                    .is_some_and(|(t, near)| near && (0.5..1.0).contains(&t))
            })
        };
        let soft_edge = |j: usize| {
            others(j).any(|m| {
                share_on_line(lins[j], lins[subject], lins[m])
                    .is_some_and(|(t, near)| near && (0.1..0.9).contains(&t))
            })
        };
        let mut model = AutoMaskModel::EMPTY;
        model.outer = self.outer;
        for j in (0..k).filter(|&j| present(j)) {
            let background = if j == subject || purer_subject(j) {
                None
            } else if soft_edge(j) {
                Some(0.0)
            } else {
                let (near, far) = AUTO_MASK_SEPARATION;
                let (few, many) = AUTO_MASK_SHARE;
                Some(
                    smoothstep(near, far, distance(labs[subject], labs[j]))
                        * smoothstep(few, many, counts[j] / self.total),
                )
            };
            model.colours[model.count] = lins[j];
            model.background[model.count] = background;
            model.centroids[model.count] = self.centroids[j];
            model.reach[model.count] = self.reach[j];
            model.counts[model.count] = counts[j];
            model.count += 1;
        }
        model
    }
}

/// Auto Mask version 2 for one dab. The colours under the dab fall into
/// `AUTO_MASK_CLUSTERS` clusters (k-means in CIE Lab); the one nearest the
/// colour under the centre (in linear light) is the subject. Another
/// cluster is
/// - the subject too when the subject's own colour lies on the line from it
///   to a third cluster, mostly towards it: the centre sits in a soft edge
///   and that cluster is the subject's purer colour;
/// - neither when it lies on the line between the subject and a third
///   cluster: it is their mix along a soft edge;
/// - a background otherwise, as strong as it is far from the subject and
///   large (`AUTO_MASK_SEPARATION`, `AUTO_MASK_SHARE`).
///
/// A pixel keeps the subject's share of its linear-light mix with each
/// background, so a soft or low-contrast edge keeps as much as the
/// subject's light covers there; the colour distance alone (version 1)
/// keeps most of a background that is close in colour.
/// notes: reference/local-masks-baseline.md.
#[derive(Clone, Copy)]
struct AutoMaskModel {
    /// Cluster colours in linear light.
    colours: [[f32; 3]; AUTO_MASK_CLUSTERS],
    /// Per cluster: `None` for the subject, else its strength as a
    /// background (0 = none).
    background: [Option<f32>; AUTO_MASK_CLUSTERS],
    count: usize,
    /// Version 3: the direction from the dab's centre towards the subject's
    /// side of the stroke times how far that side is trusted (0–1; zero
    /// when it is not known).
    inward: [f32; 2],
    outer: f32,
    /// Per cluster: mean sample offset from the centre (px), mean sample
    /// distance from it and sample count (as in `AutoMaskClusters`).
    centroids: [[f32; 2]; AUTO_MASK_CLUSTERS],
    reach: [f32; AUTO_MASK_CLUSTERS],
    counts: [f32; AUTO_MASK_CLUSTERS],
}

impl AutoMaskModel {
    const EMPTY: Self = Self {
        colours: [[0.0; 3]; AUTO_MASK_CLUSTERS],
        background: [None; AUTO_MASK_CLUSTERS],
        count: 0,
        inward: [0.0; 2],
        outer: 1.0,
        centroids: [[0.0; 2]; AUTO_MASK_CLUSTERS],
        reach: [0.0; AUTO_MASK_CLUSTERS],
        counts: [0.0; AUTO_MASK_CLUSTERS],
    };

    /// The model of the dab at `centre` (pixels of the stroke's bounds, of
    /// which `colours` holds the linear-light colours, NaN outside the
    /// photo) that reaches `outer` pixels.
    fn fit(colours: &[[f32; 3]], bb_w: usize, bb_h: usize, centre: (f32, f32), outer: f32) -> Self {
        AutoMaskClusters::of(colours, bb_w, bb_h, centre, outer).map_or(Self::EMPTY, |clusters| {
            clusters.model(clusters.nearest_centre())
        })
    }

    /// The share of the dab a pixel of linear-light colour `c` keeps: the
    /// best subject cluster's share against its nearest background.
    #[inline]
    fn weight(&self, c: [f32; 3]) -> f32 {
        if !c[0].is_finite() {
            return 0.0;
        }
        let clusters = || {
            self.colours[..self.count]
                .iter()
                .zip(&self.background[..self.count])
        };
        clusters()
            .filter(|(_, background)| background.is_none())
            .map(|(&subject, _)| {
                clusters()
                    .filter_map(|(&colour, background)| {
                        let strength = background.filter(|s| *s > 0.0)?;
                        let (t, _) = share_on_line(c, subject, colour)?;
                        Some(1.0 - strength * (1.0 - t.clamp(0.0, 1.0)))
                    })
                    .fold(1.0f32, f32::min)
            })
            .fold(0.0f32, f32::max)
    }
}

/// Version 3: how far along the stroke (in dab radii) the dabs' own
/// subjects are pooled into the subject's colour.
const AUTO_MASK_SUBJECT_REACH: f32 = 3.0;
/// Version 3: how far along the stroke (in dab radii) the dabs vote on the
/// side their backgrounds lie on.
const AUTO_MASK_SIDE_REACH: f32 = 6.0;
/// Version 3: a dab's backgrounds count as one-sided from this share of
/// their spread (fading in from the lower one).
const AUTO_MASK_ONE_SIDED: (f32, f32) = (0.35, 0.65);
/// Version 3: the subject's side is trusted fully from this agreement of the
/// dabs nearby (fading in from 0).
const AUTO_MASK_AGREEMENT: f32 = 0.3;
/// Version 3: the subject's side is kept from this distance behind the
/// centre (in dab radii; negative is towards the background), fully from
/// the second.
const AUTO_MASK_INSIDE: (f32, f32) = (-0.1, 0.3);
/// Version 3: a background cluster whose samples lie on average this far
/// behind the centre (in dab radii) is the subject's own, once the side is
/// trusted at least `AUTO_MASK_SIDE_TRUSTED`.
const AUTO_MASK_BEHIND: f32 = 0.1;
const AUTO_MASK_SIDE_TRUSTED: f32 = 0.3;

impl AutoMaskModel {
    /// Version 3: `weight`, with the subject's side of the dab kept. A pixel
    /// behind the centre, away from the stroke's backgrounds, is the
    /// subject's interior (dark gaps in fur, shaded leaves) whatever its
    /// colour.
    #[inline]
    fn weight_at(&self, c: [f32; 3], offset: (f32, f32)) -> f32 {
        let share = self.weight(c);
        if self.inward == [0.0, 0.0] || !c[0].is_finite() {
            return share;
        }
        let trust = self.inward[0].hypot(self.inward[1]);
        let behind = (offset.0 * self.inward[0] + offset.1 * self.inward[1]) / (trust * self.outer);
        let (from, to) = AUTO_MASK_INSIDE;
        let inside = smoothstep(0.0, AUTO_MASK_AGREEMENT, trust) * smoothstep(from, to, behind);
        share + (1.0 - share) * inside
    }

    /// The direction away from this dab's backgrounds, as long as they lie
    /// to one side: |∑ strength · offset| / ∑ strength · |offset| over the
    /// samples (0 = all around the centre, 1 = all on one side).
    fn away_from_backgrounds(&self) -> [f32; 2] {
        let mut towards = [0.0f32; 2];
        let mut spread = 0.0f32;
        for j in 0..self.count {
            let s = self.background[j].unwrap_or(0.0) * self.counts[j];
            towards = [
                towards[0] + s * self.centroids[j][0],
                towards[1] + s * self.centroids[j][1],
            ];
            spread += s * self.reach[j];
        }
        let len = towards[0].hypot(towards[1]);
        if len > 0.0 && spread > 0.0 {
            let one_sided = len / spread;
            [-towards[0] / len * one_sided, -towards[1] / len * one_sided]
        } else {
            [0.0, 0.0]
        }
    }
}

/// Auto Mask version 3: the dabs of one stroke modelled together.
/// 1. Subject: each dab takes its cluster nearest the medoid (CIE Lab) of
///    the subjects that the dabs within `AUTO_MASK_SUBJECT_REACH` radii
///    pick by their centre's colour, so a dab whose centre falls on a gap
///    or a highlight paints what the stroke paints.
/// 2. Side: the dabs within `AUTO_MASK_SIDE_REACH` radii vote on which side
///    of the stroke (left or right of its travel) their backgrounds lie; how
///    well they agree, and how one-sided each dab's own backgrounds are, is
///    how far the side is trusted.
/// 3. With the side trusted, a background cluster lying behind the centre is
///    the subject's own (shaded leaves), and pixels behind the centre are
///    kept whatever their colour (`AutoMaskModel::weight_at`).
fn stroke_models_v3(
    clusters: &[Option<AutoMaskClusters>],
    dabs: &[(f32, f32)],
    votes: &[f32],
) -> Vec<AutoMaskModel> {
    // How much a dab counts in its neighbours' votes: what it paints under
    // pen pressure (0 for a dab that paints nothing), exactly 1 otherwise.
    let vote = |j: usize| votes.get(j).copied().unwrap_or(1.0);
    let Some(outer) = clusters.iter().flatten().map(|c| c.outer).next() else {
        return vec![AutoMaskModel::EMPTY; clusters.len()];
    };
    let mut arc = vec![0.0f32; dabs.len()];
    for i in 1..dabs.len() {
        arc[i] = arc[i - 1] + (dabs[i].0 - dabs[i - 1].0).hypot(dabs[i].1 - dabs[i - 1].1);
    }
    // In radii of dab i (equal for every dab but under pen pressure).
    let outers: Vec<f32> = clusters
        .iter()
        .map(|c| c.map_or(outer, |c| c.outer))
        .collect();
    let within = |i: usize, reach: f32| {
        let (arc, outer) = (&arc, outers[i]);
        (0..dabs.len()).filter(move |&j| (arc[j] - arc[i]).abs() <= reach * outer)
    };

    let own: Vec<Option<[f32; 3]>> = clusters
        .iter()
        .map(|c| c.map(|c| c.labs[c.nearest_centre()]))
        .collect();
    let mut models: Vec<AutoMaskModel> = (0..clusters.len())
        .map(|i| {
            let Some(c) = clusters[i] else {
                return AutoMaskModel::EMPTY;
            };
            let near: Vec<([f32; 3], f32)> = within(i, AUTO_MASK_SUBJECT_REACH)
                .filter(|&j| vote(j) > 0.0)
                .filter_map(|j| own[j].map(|lab| (lab, vote(j))))
                .collect();
            let cost = |x: &[f32; 3]| {
                near.iter()
                    .map(|(y, weight)| weight * distance(*x, *y))
                    .sum::<f32>()
            };
            let Some((medoid, _)) = near.iter().min_by(|a, b| cost(&a.0).total_cmp(&cost(&b.0)))
            else {
                return c.model(c.nearest_centre());
            };
            let subject = (0..AUTO_MASK_CLUSTERS)
                .filter(|&j| c.present(j))
                .min_by(|&a, &b| {
                    distance(c.labs[a], *medoid).total_cmp(&distance(c.labs[b], *medoid))
                })
                .unwrap_or_else(|| c.nearest_centre());
            c.model(subject)
        })
        .collect();

    // The stroke's normal at each dab (to the left of its travel).
    let normal = |i: usize| {
        let a = dabs[i.saturating_sub(1)];
        let b = dabs[(i + 1).min(dabs.len() - 1)];
        let (tx, ty) = (b.0 - a.0, b.1 - a.1);
        let len = tx.hypot(ty);
        if len > 0.0 {
            [-ty / len, tx / len]
        } else {
            [0.0, 0.0]
        }
    };
    let away: Vec<[f32; 2]> = models
        .iter()
        .map(AutoMaskModel::away_from_backgrounds)
        .collect();
    let one_sided: Vec<f32> = away.iter().map(|a| a[0].hypot(a[1])).collect();
    // > 0: the backgrounds lie to the left of the travel.
    let leftwards: Vec<f32> = away
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let n = normal(i);
            -(a[0] * n[0] + a[1] * n[1])
        })
        .collect();
    for (i, model) in models.iter_mut().enumerate() {
        let (side, total) = within(i, AUTO_MASK_SIDE_REACH).fold((0.0f32, 0.0f32), |(s, t), j| {
            (s + leftwards[j] * vote(j), t + one_sided[j] * vote(j))
        });
        if total <= 0.0 {
            continue;
        }
        let (low, high) = AUTO_MASK_ONE_SIDED;
        let trust = side.abs() / total * smoothstep(low, high, one_sided[i]);
        if trust <= 0.0 {
            continue;
        }
        let n = normal(i);
        let sign = if side > 0.0 { -1.0 } else { 1.0 };
        let inward = [sign * n[0], sign * n[1]];
        model.inward = [inward[0] * trust, inward[1] * trust];
        if trust >= AUTO_MASK_SIDE_TRUSTED {
            for j in 0..model.count {
                let c = model.centroids[j];
                if model.background[j].is_some_and(|s| s > 0.0)
                    && (c[0] * inward[0] + c[1] * inward[1]) / model.outer > AUTO_MASK_BEHIND
                {
                    model.background[j] = None;
                }
            }
        }
    }
    models
}

fn default_brush_feather() -> f32 {
    0.5
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct BrushMaskParameters {
    #[serde(default)]
    lines: Vec<BrushLine>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct FlowLine {
    tool: String,
    brush_size: f32,
    points: Vec<Point>,
    #[serde(default = "default_brush_feather")]
    feather: f32,
    #[serde(default = "default_line_flow")]
    flow: f32,
    /// The photo's geometry the stroke was painted in (see `ShapeFrame`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_frame"
    )]
    frame: Option<ShapeFrame>,
}

fn default_line_flow() -> f32 {
    10.0
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct FlowMaskParameters {
    #[serde(default)]
    lines: Vec<FlowLine>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct ParametricMaskParameters {
    target_x: f64,
    target_y: f64,
    #[serde(default = "default_tolerance")]
    tolerance: f32,
    #[serde(default)]
    grow: f32,
    #[serde(default)]
    feather: f32,
    #[serde(default)]
    rotation: f32,
    #[serde(default)]
    flip_horizontal: bool,
    #[serde(default)]
    flip_vertical: bool,
    #[serde(default)]
    orientation_steps: u8,
    /// Colour range: Lightroom's Refine (0–100). Set by the measured model
    /// (`color_range_model`); recipes without it keep the inherited RGB
    /// tolerance.
    #[serde(default)]
    refine: Option<f32>,
    /// Colour range samples (points, or areas with a size), in the same
    /// coordinates as `target_x`/`target_y`; empty means the target alone.
    #[serde(default)]
    samples: Vec<ColorSample>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ColorSample {
    x: f64,
    y: f64,
    #[serde(default)]
    width: f64,
    #[serde(default)]
    height: f64,
    /// The photo's orientation, flips and straightening when the sample was
    /// taken (the point and area are in the photo as it was shown then).
    /// Samples without it were taken in the component's geometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rotation: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flip_horizontal: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flip_vertical: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    orientation_steps: Option<u8>,
}

impl ColorSample {
    /// The geometry the sample was taken in; `component` for older samples.
    fn geometry(&self, component: SourceGeometry) -> SourceGeometry {
        SourceGeometry {
            rotation: self.rotation.unwrap_or(component.rotation),
            flip_horizontal: self.flip_horizontal.unwrap_or(component.flip_horizontal),
            flip_vertical: self.flip_vertical.unwrap_or(component.flip_vertical),
            orientation_steps: self
                .orientation_steps
                .map_or(component.orientation_steps, |steps| steps % 4),
        }
    }
}

fn default_tolerance() -> f32 {
    20.0
}

impl Default for ParametricMaskParameters {
    fn default() -> Self {
        Self {
            target_x: 0.0,
            target_y: 0.0,
            tolerance: default_tolerance(),
            grow: 0.0,
            feather: 35.0,
            rotation: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
            orientation_steps: 0,
            refine: None,
            samples: Vec::new(),
        }
    }
}

fn grayscale_dilate(image: &GrayImage, k: u8) -> GrayImage {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return image.clone();
    }
    let w = width as usize;
    let h = height as usize;
    let r = k as i32;
    let src = image.as_raw();

    let mut temp = vec![0u8; w * h];
    let mut out = vec![0u8; w * h];

    for y in 0..h {
        let row_offset = y * w;
        for x in 0..w {
            let mut max_val = 0;
            let start = (x as i32 - r).max(0) as usize;
            let end = (x as i32 + r).min((w - 1) as i32) as usize;
            for xi in start..=end {
                max_val = max_val.max(src[row_offset + xi]);
            }
            temp[row_offset + x] = max_val;
        }
    }

    for x in 0..w {
        for y in 0..h {
            let mut max_val = 0;
            let start = (y as i32 - r).max(0) as usize;
            let end = (y as i32 + r).min((h - 1) as i32) as usize;
            for yi in start..=end {
                max_val = max_val.max(temp[yi * w + x]);
            }
            out[y * w + x] = max_val;
        }
    }

    GrayImage::from_raw(width, height, out).unwrap()
}

fn grayscale_erode(image: &GrayImage, k: u8) -> GrayImage {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return image.clone();
    }
    let w = width as usize;
    let h = height as usize;
    let r = k as i32;
    let src = image.as_raw();

    let mut temp = vec![0u8; w * h];
    let mut out = vec![0u8; w * h];

    for y in 0..h {
        let row_offset = y * w;
        for x in 0..w {
            let mut min_val = 255;
            let start = (x as i32 - r).max(0) as usize;
            let end = (x as i32 + r).min((w - 1) as i32) as usize;
            for xi in start..=end {
                min_val = min_val.min(src[row_offset + xi]);
            }
            temp[row_offset + x] = min_val;
        }
    }

    for x in 0..w {
        for y in 0..h {
            let mut min_val = 255;
            let start = (y as i32 - r).max(0) as usize;
            let end = (y as i32 + r).min((h - 1) as i32) as usize;
            for yi in start..=end {
                min_val = min_val.min(temp[yi * w + x]);
            }
            out[y * w + x] = min_val;
        }
    }

    GrayImage::from_raw(width, height, out).unwrap()
}

fn apply_grow_and_feather(mask: &mut GrayImage, grow: f32, feather: f32, width: u32, height: u32) {
    let base_dimension = width.min(height) as f32;

    if grow.abs() > 0.01 {
        const MAX_GROW_PERCENTAGE: f32 = 0.01;
        let grow_pixels = (grow / 100.0) * base_dimension * MAX_GROW_PERCENTAGE;

        let amount = grow_pixels.abs().round() as u8;

        if amount > 0 {
            if grow_pixels > 0.0 {
                *mask = grayscale_dilate(mask, amount);
            } else {
                *mask = grayscale_erode(mask, amount);
            }
        }
    }

    if feather > 0.0 {
        const MAX_FEATHER_SIGMA_PERCENTAGE: f32 = 0.005;
        let sigma = (feather / 100.0) * base_dimension * MAX_FEATHER_SIGMA_PERCENTAGE;

        if sigma > 0.01 {
            *mask = imageproc::filter::gaussian_blur_f32(mask, sigma);
        }
    }
}

fn stroke_bounds(
    points: &[Point],
    width: u32,
    height: u32,
    radius: f32,
    scale: f32,
    crop_offset: (f32, f32),
) -> Option<(u32, u32, u32, u32)> {
    if width == 0 || height == 0 || points.is_empty() {
        return None;
    }

    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let r_pad = radius.ceil() + 2.0;

    for p in points {
        let px = p.x as f32 * scale - crop_offset.0;
        let py = p.y as f32 * scale - crop_offset.1;

        min_x = min_x.min(px - r_pad);
        min_y = min_y.min(py - r_pad);
        max_x = max_x.max(px + r_pad);
        max_y = max_y.max(py + r_pad);
    }

    if max_x < 0.0 || max_y < 0.0 || min_x > (width - 1) as f32 || min_y > (height - 1) as f32 {
        return None;
    }

    let min_x = min_x.floor().max(0.0).min((width - 1) as f32) as u32;
    let min_y = min_y.floor().max(0.0).min((height - 1) as f32) as u32;
    let max_x = max_x.ceil().max(0.0).min((width - 1) as f32) as u32;
    let max_y = max_y.ceil().max(0.0).min((height - 1) as f32) as u32;

    if min_x > max_x || min_y > max_y {
        None
    } else {
        Some((min_x, min_y, max_x, max_y))
    }
}

struct StrokeSegment {
    x1: f32,
    y1: f32,
    dx: f32,
    dy: f32,
    len_sq: f32,
    bounds_left: i32,
    bounds_right: i32,
    bounds_top: i32,
    bounds_bottom: i32,
}

/// Rasterize one stroke and combine its coverage into `mask` within `bounds`
/// (inclusive, from `stroke_bounds`). `combine(destination, coverage)` runs only
/// for nonzero coverage. Coordinates stay relative to the bounds' origin, with
/// the same distances and 8-bit quantization as a separate stroke layer, but
/// each row only visits columns near its active segments and rows run in
/// parallel: a long diagonal stroke no longer costs its whole bounding box.
#[allow(clippy::too_many_arguments)]
fn blend_stroke(
    mask: &mut GrayImage,
    points: &[Point],
    radius: f32,
    feather: f32,
    opacity: f32,
    scale: f32,
    crop_offset: (f32, f32),
    bounds: (u32, u32, u32, u32),
    combine: impl Fn(u8, u8) -> u8 + Sync,
) {
    let (min_x, min_y, max_x, max_y) = bounds;
    if points.is_empty() || radius <= 0.0 {
        return;
    }
    let width = mask.width() as usize;
    let bb_w = max_x - min_x + 1;
    let bb_h = max_y - min_y + 1;
    let layer_offset = (min_x as f32, min_y as f32);

    let mut segments = Vec::with_capacity(points.len().saturating_sub(1));
    for pair in points.windows(2) {
        let x1 = pair[0].x as f32 * scale - crop_offset.0 - layer_offset.0;
        let y1 = pair[0].y as f32 * scale - crop_offset.1 - layer_offset.1;
        let x2 = pair[1].x as f32 * scale - crop_offset.0 - layer_offset.0;
        let y2 = pair[1].y as f32 * scale - crop_offset.1 - layer_offset.1;

        let left = ((x1.min(x2) - radius).floor() as i32).max(0);
        let right = ((x1.max(x2) + radius).ceil() as i32).min(bb_w as i32 - 1);
        let top = ((y1.min(y2) - radius).floor() as i32).max(0);
        let bottom = ((y1.max(y2) + radius).ceil() as i32).min(bb_h as i32 - 1);

        if left > right || top > bottom {
            continue;
        }

        let dx = x2 - x1;
        let dy = y2 - y1;
        segments.push(StrokeSegment {
            x1,
            y1,
            dx,
            dy,
            len_sq: dx * dx + dy * dy,
            bounds_left: left,
            bounds_right: right,
            bounds_top: top,
            bounds_bottom: bottom,
        });
    }

    let mut single_point = None;
    if segments.is_empty() {
        let x1 = points[0].x as f32 * scale - crop_offset.0 - layer_offset.0;
        let y1 = points[0].y as f32 * scale - crop_offset.1 - layer_offset.1;
        let left = ((x1 - radius).floor() as i32).max(0);
        let right = ((x1 + radius).ceil() as i32).min(bb_w as i32 - 1);
        let top = ((y1 - radius).floor() as i32).max(0);
        let bottom = ((y1 + radius).ceil() as i32).min(bb_h as i32 - 1);
        if left <= right && top <= bottom {
            single_point = Some((x1, y1, left, right, top, bottom));
        }
    }

    let feather_amount = feather.clamp(0.0, 1.0);
    let inner_radius = radius * (1.0 - feather_amount);
    let feather_range = (radius - inner_radius).max(0.01);
    let radius_sq = radius * radius;
    let inner_radius_sq = inner_radius * inner_radius;

    let rows = &mut mask.as_mut()[min_y as usize * width..(max_y as usize + 1) * width];
    rows.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let py = y as f32;
        let y_i32 = y as i32;
        let mut left = i32::MAX;
        let mut right = i32::MIN;
        let mut active_segments = Vec::new();
        for seg in &segments {
            if y_i32 >= seg.bounds_top && y_i32 <= seg.bounds_bottom {
                left = left.min(seg.bounds_left);
                right = right.max(seg.bounds_right);
                active_segments.push(seg);
            }
        }
        let point = single_point.filter(|pt| y_i32 >= pt.4 && y_i32 <= pt.5);
        if let Some(pt) = point {
            left = left.min(pt.2);
            right = right.max(pt.3);
        }
        // Columns outside every active segment's bounds keep zero coverage.
        for x_i32 in left.max(0)..=right {
            let px = x_i32 as f32;
            let mut min_dist_sq = radius_sq + 1.0;

            for seg in &active_segments {
                if x_i32 >= seg.bounds_left && x_i32 <= seg.bounds_right {
                    let dist_sq = if seg.len_sq < 0.0001 {
                        (px - seg.x1) * (px - seg.x1) + (py - seg.y1) * (py - seg.y1)
                    } else {
                        let t = (((px - seg.x1) * seg.dx + (py - seg.y1) * seg.dy) / seg.len_sq)
                            .clamp(0.0, 1.0);
                        let proj_x = seg.x1 + t * seg.dx;
                        let proj_y = seg.y1 + t * seg.dy;
                        (px - proj_x) * (px - proj_x) + (py - proj_y) * (py - proj_y)
                    };
                    if dist_sq < min_dist_sq {
                        min_dist_sq = dist_sq;
                    }
                }
            }

            if let Some(pt) = point
                && x_i32 >= pt.2
                && x_i32 <= pt.3
            {
                let dist_sq = (px - pt.0) * (px - pt.0) + (py - pt.1) * (py - pt.1);
                if dist_sq < min_dist_sq {
                    min_dist_sq = dist_sq;
                }
            }

            if min_dist_sq <= radius_sq {
                let intensity = if min_dist_sq <= inner_radius_sq {
                    1.0
                } else {
                    let dist = min_dist_sq.sqrt();
                    let t = ((dist - inner_radius) / feather_range).clamp(0.0, 1.0);
                    1.0 - (t * t * (3.0 - 2.0 * t))
                };
                let coverage = (intensity * opacity * 255.0).round() as u8;
                if coverage > 0 {
                    let pixel = &mut row[min_x as usize + x_i32 as usize];
                    *pixel = combine(*pixel, coverage);
                }
            }
        }
    });
}

/// The tip and spacing of a dab-based brush stroke, in raster pixels: full
/// coverage up to `inner`, a smoothstep falloff to zero at `outer`, one dab
/// every `step` along the path. A Lightroom tip instead follows Lightroom's
/// profile for its CenterWeight out to `outer`, with a dab at every point.
#[derive(Clone, Copy)]
struct DabTip {
    inner: f32,
    outer: f32,
    step: f32,
    lightroom: Option<f32>,
    /// Each dab's share of the coverage one dab `step` would add, as an
    /// exponent of what it keeps: (1 − flow · tip)^density. 1 in version 1.
    density: f32,
    /// Version 2: a dab at the path's last point, weighted by the share of
    /// a step the path covered since the dab before it.
    end_dab: bool,
}

/// Lightroom's brush tip: coverage at distance ρ = 0, 0.05, …, 1 of the
/// radius, for CenterWeight 0, 0.1, …, 1 (rows). Measured in Camera Raw 18.7
/// from single dabs (equivalent exposure of a +1 EV brush, rings of 3 px at
/// a radius of 120 px); CenterWeight 0 is close to a Gaussian of σ = 0.24 r,
/// 1 has its edge at 0.93 r. notes: reference/point-color.md.
const LIGHTROOM_TIP: [[f32; 21]; 11] = [
    [
        1.000, 0.977, 0.917, 0.825, 0.711, 0.584, 0.459, 0.344, 0.252, 0.175, 0.122, 0.085, 0.061,
        0.044, 0.032, 0.022, 0.014, 0.007, 0.003, 0.000, 0.000,
    ],
    [
        1.000, 0.986, 0.949, 0.890, 0.815, 0.727, 0.630, 0.533, 0.440, 0.352, 0.280, 0.216, 0.165,
        0.126, 0.095, 0.071, 0.051, 0.032, 0.016, 0.004, 0.000,
    ],
    [
        1.000, 0.992, 0.970, 0.936, 0.889, 0.833, 0.770, 0.699, 0.624, 0.548, 0.472, 0.396, 0.326,
        0.264, 0.206, 0.156, 0.114, 0.077, 0.042, 0.012, 0.000,
    ],
    [
        1.000, 0.996, 0.984, 0.965, 0.939, 0.907, 0.869, 0.825, 0.776, 0.720, 0.658, 0.590, 0.518,
        0.440, 0.360, 0.284, 0.211, 0.145, 0.083, 0.027, 0.000,
    ],
    [
        1.000, 0.998, 0.992, 0.983, 0.970, 0.953, 0.933, 0.909, 0.880, 0.845, 0.805, 0.757, 0.695,
        0.622, 0.537, 0.440, 0.337, 0.238, 0.141, 0.049, 0.000,
    ],
    [
        1.000, 0.999, 0.997, 0.993, 0.987, 0.979, 0.970, 0.959, 0.944, 0.926, 0.903, 0.872, 0.831,
        0.777, 0.701, 0.602, 0.484, 0.349, 0.216, 0.079, 0.000,
    ],
    [
        1.000, 1.000, 0.999, 0.997, 0.995, 0.993, 0.989, 0.984, 0.978, 0.970, 0.959, 0.943, 0.919,
        0.883, 0.828, 0.748, 0.628, 0.477, 0.304, 0.117, 0.000,
    ],
    [
        1.000, 1.000, 1.000, 0.999, 0.999, 0.998, 0.997, 0.996, 0.994, 0.991, 0.986, 0.979, 0.968,
        0.948, 0.913, 0.854, 0.758, 0.604, 0.404, 0.165, 0.000,
    ],
    [
        1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 0.999, 0.999, 0.999, 0.998, 0.997, 0.994, 0.990,
        0.981, 0.963, 0.926, 0.855, 0.724, 0.514, 0.222, 0.000,
    ],
    [
        1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 0.999, 0.998,
        0.995, 0.987, 0.968, 0.923, 0.821, 0.621, 0.286, 0.000,
    ],
    [
        1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
        0.999, 0.997, 0.989, 0.965, 0.896, 0.726, 0.357, 0.000,
    ],
];

/// Lightroom's tip coverage at `rho` (distance / radius) for `center_weight`.
fn lightroom_tip(center_weight: f32, rho: f32) -> f32 {
    if !(0.0..1.0).contains(&rho) {
        return if rho < 0.0 { 1.0 } else { 0.0 };
    }
    let row = center_weight.clamp(0.0, 1.0) * 10.0;
    let col = rho * 20.0;
    let (r0, c0) = (row.floor() as usize, col.floor() as usize);
    let (r1, c1) = ((r0 + 1).min(10), (c0 + 1).min(20));
    let (fr, fc) = (row - r0 as f32, col - c0 as f32);
    let at = |r: usize| LIGHTROOM_TIP[r][c0] * (1.0 - fc) + LIGHTROOM_TIP[r][c1] * fc;
    at(r0) * (1.0 - fr) + at(r1) * fr
}

impl DabTip {
    /// A feathered stroke with flow below 100%: the tip of `blend_stroke`, a
    /// dab every quarter radius.
    fn feathered(radius: f32, feather: f32) -> Self {
        Self {
            inner: radius * (1.0 - feather.clamp(0.0, 1.0)),
            outer: radius,
            step: (radius * 0.25).max(0.5),
            lightroom: None,
            density: 1.0,
            end_dab: false,
        }
    }

    /// Lightroom's brush: its tip for `center_weight`, a dab at every point.
    fn lightroom(radius: f32, center_weight: f32) -> Self {
        Self {
            inner: 0.0,
            outer: radius,
            step: 0.0,
            lightroom: Some(center_weight),
            density: 1.0,
            end_dab: false,
        }
    }

    /// Capture One's brush, measured in Capture One 16: with softness
    /// s = 1 − hardness the tip falls from r(1 − s) to r(1 + s), so coverage is
    /// 50% at the brush size, and dabs are r(0.2 + 0.8√s) apart.
    fn capture_one(radius: f32, hardness: f32) -> Self {
        let softness = 1.0 - hardness.clamp(0.0, 1.0);
        Self {
            inner: radius * (1.0 - softness),
            outer: radius * (1.0 + softness),
            step: (radius * (0.2 + 0.8 * softness.sqrt())).max(0.5),
            lightroom: None,
            density: 1.0,
            end_dab: false,
        }
    }

    /// `dabVersion` 2. Every dab-based tip but Lightroom's (a dab at every
    /// point already) ends with a dab at the last point, so the stroke
    /// reaches where it was released and, while it is drawn, the pointer
    /// (version 1 trailed it by up to a step: 0.77 r at hardness 50). A
    /// Capture One tip without Auto Mask (`dense`) also lays its dabs at
    /// most 0.4 of its falloff apart (at least 0.1 of its outer radius),
    /// each carrying the share of version 1's dab its spacing stands for, so
    /// the stroke keeps version 1's mean coverage across its width (within
    /// 1/255 in a model of straight strokes) without version 1's scallops
    /// along the edge (up to 7/255 at hardness 50, 10–25/255 at 75–90, and
    /// < 1/255 at 0.4 of the falloff up to hardness 75). Auto Mask strokes
    /// keep version 1's spacing: their per-dab colour models would cost as
    /// many times more and were measured at that spacing.
    /// notes: parity-gaps/G30-brush.md (2026-10-18, task Z2).
    fn dab_version_2(self, dense: bool) -> Self {
        if self.lightroom.is_some() {
            return self;
        }
        let mut tip = Self {
            end_dab: true,
            ..self
        };
        if dense {
            let step = (0.4 * (tip.outer - tip.inner))
                .max(0.1 * tip.outer)
                .max(0.5)
                .min(tip.step);
            tip.density = step / tip.step;
            tip.step = step;
        }
        tip
    }
}

/// Pen pressure of a stroke: what each point's `p` (0–1, absent 1) scales.
/// Linear in both, like Capture One's Pen Pressure on size, so the full
/// range from no dab to the set size and flow is reachable and the response
/// is monotonic without steps (bar: plan M7; tablet test open).
/// The dabs of a pen pressure stroke (`PenPressure::dabs`).
struct PenDabs {
    dabs: Vec<(f32, f32)>,
    /// Each dab's (radius, flow) factor.
    scales: Vec<(f32, f32)>,
    /// Each dab's share of a regular dab (as the end dab's weight).
    weights: Vec<f32>,
    /// The share of a step the path covered after the last dab.
    travelled: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PenPressure {
    size: bool,
    flow: bool,
}

impl PenPressure {
    /// Only strokes with a mapping and at least one point's pressure; the
    /// others (every mouse stroke, every saved recipe before) take the
    /// rasteriser's unchanged path.
    fn of(line: &BrushLine) -> Option<Self> {
        let pen = Self {
            size: line.pressure_size,
            flow: line.pressure_flow,
        };
        ((pen.size || pen.flow) && line.points.iter().any(|point| point.p.is_some())).then_some(pen)
    }

    /// A point's (radius, flow) factor.
    fn factors(self, p: Option<f32>) -> (f32, f32) {
        let p = p.unwrap_or(1.0);
        let p = if p.is_finite() {
            p.clamp(0.0, 1.0)
        } else {
            1.0
        };
        (
            if self.size { p } else { 1.0 },
            if self.flow { p } else { 1.0 },
        )
    }

    /// The dabs of a pressure stroke with their factors and weights, and
    /// the share of a step the path covered after its last dab (for the end
    /// dab). Each dab sits one step of its own radius after the one before
    /// (spacing as a share of the dab's size, as the references space a
    /// smaller tip closer); pressure is interpolated linearly between
    /// points. A point where the pressure peaks above the last dab (the pen
    /// pressed harder while barely moving) keeps that peak: within half a
    /// step of the last dab that dab is raised to it (at most half a step
    /// back, the density unchanged, and pen jitter only nudges it up);
    /// farther on, a dab is added there, weighted like the end dab by the
    /// share of a step since the dab before it, so the density along the
    /// path stays even. Lightroom tips keep a dab at every point.
    fn dabs(
        self,
        points: &[Point],
        local: impl Fn(&Point) -> (f32, f32),
        step: f32,
        lightroom: bool,
    ) -> PenDabs {
        if lightroom {
            return PenDabs {
                dabs: points.iter().map(&local).collect(),
                scales: points.iter().map(|point| self.factors(point.p)).collect(),
                weights: Vec::new(),
                travelled: 0.0,
            };
        }
        let pressure = |point: &Point| self.factors(point.p);
        let raised = |a: (f32, f32), b: (f32, f32)| b.0 > a.0 || b.1 > a.1;
        let spacing = |size: f32| (step * size).max(0.5);
        let first = pressure(&points[0]);
        let mut pen = PenDabs {
            dabs: vec![local(&points[0])],
            scales: vec![first],
            weights: vec![1.0],
            travelled: 0.0,
        };
        let mut remaining = spacing(first.0);
        let mut last_step = remaining;
        for (k, pair) in points.windows(2).enumerate() {
            let (x1, y1) = local(&pair[0]);
            let (x2, y2) = local(&pair[1]);
            let length = (x2 - x1).hypot(y2 - y1);
            if length > 0.0 {
                let (p1, p2) = (pair[0].p.unwrap_or(1.0), pair[1].p.unwrap_or(1.0));
                let mut at = 0.0;
                while remaining <= length - at {
                    at += remaining;
                    let t = at / length;
                    let factors = self.factors(Some(p1 + (p2 - p1) * t));
                    pen.dabs.push((x1 + (x2 - x1) * t, y1 + (y2 - y1) * t));
                    pen.scales.push(factors);
                    pen.weights.push(1.0);
                    remaining = spacing(factors.0);
                    last_step = remaining;
                }
                remaining -= length - at;
            }
            // A peak at this point above the last dab: the pressure falls
            // after it (or the stroke ends here without having moved on).
            let here = pressure(&pair[1]);
            let last = pen.scales.len() - 1;
            let share = 1.0 - remaining / last_step;
            let peak = match points.get(k + 2) {
                Some(next) => raised(pressure(next), here),
                None => share <= 0.5,
            };
            if !peak || !raised(pen.scales[last], here) {
                continue;
            }
            if share > 0.5 {
                pen.dabs.push((x2, y2));
                pen.scales.push(here);
                pen.weights.push(share.min(1.0));
                remaining = spacing(here.0);
                last_step = remaining;
            } else {
                // The spacing stays: the next dab comes a step after this
                // one, as before it was raised.
                pen.scales[last] = (
                    pen.scales[last].0.max(here.0),
                    pen.scales[last].1.max(here.1),
                );
            }
        }
        pen.travelled = 1.0 - remaining / last_step;
        pen
    }
}

/// Coverage builds up within one stroke over dabs spaced evenly along the
/// path, as 1 − Π(1 − flow · dab), so it depends on the path and not on how
/// densely the pointer was sampled (Capture One's and Photoshop's flow); the
/// stroke's opacity then scales the result. Pixel positions match
/// `blend_stroke`. Auto Mask uses the source's colours over `bounds`: CIE
/// Lab in `colours` for version 1, linear light in `linear_colours` for
/// version 2 (`blend_stroke_auto_mask_v2`).
#[allow(clippy::too_many_arguments)]
fn blend_stroke_dabs(
    mask: &mut GrayImage,
    points: &[Point],
    tip: DabTip,
    flow: f32,
    opacity: f32,
    scale: f32,
    crop_offset: (f32, f32),
    bounds: (u32, u32, u32, u32),
    colours: Option<&[[f32; 3]]>,
    linear_colours: Option<&[[f32; 3]]>,
    auto_mask_version: u32,
    pressure: Option<PenPressure>,
    combine: impl Fn(u8, u8) -> u8 + Sync,
) -> usize {
    let (min_x, min_y, max_x, max_y) = bounds;
    let DabTip {
        inner,
        outer,
        step,
        lightroom,
        density,
        end_dab,
    } = tip;
    if points.is_empty() || outer <= 0.0 {
        return 0;
    }
    let width = mask.width() as usize;
    let bb_w = (max_x - min_x + 1) as usize;
    let local = |point: &Point| {
        (
            point.x as f32 * scale - crop_offset.0 - min_x as f32,
            point.y as f32 * scale - crop_offset.1 - min_y as f32,
        )
    };

    let mut dabs = vec![local(&points[0])];
    let mut travelled = 0.0;
    // Pen pressure: each dab's (radius, flow) factor, with dabs spaced by
    // the step of their own radius (`PenPressure::dabs`). Empty without.
    let mut scales: Vec<(f32, f32)> = Vec::new();
    let mut weights: Vec<f32> = Vec::new();
    // Lightroom stores its dabs: one at every point.
    if lightroom.is_some() {
        dabs = points.iter().map(local).collect();
    }
    if let Some(pen) = pressure {
        let pen = pen.dabs(points, local, step, lightroom.is_some());
        (dabs, scales, weights, travelled) = (pen.dabs, pen.scales, pen.weights, pen.travelled);
    }
    for pair in points
        .windows(2)
        .filter(|_| lightroom.is_none() && pressure.is_none())
    {
        let (x1, y1) = local(&pair[0]);
        let (x2, y2) = local(&pair[1]);
        let length = (x2 - x1).hypot(y2 - y1);
        if length <= 0.0 {
            continue;
        }
        let mut at = step - travelled;
        while at <= length {
            let t = at / length;
            dabs.push((x1 + (x2 - x1) * t, y1 + (y2 - y1) * t));
            at += step;
        }
        travelled = length - (at - step);
    }
    // Version 2's end dab: the share of a step since the last dab.
    let mut end_weight = None;
    if end_dab && lightroom.is_none() && travelled > 1e-3 {
        let last = &points[points.len() - 1];
        dabs.push(local(last));
        // With pressure `travelled` is already the share of the last step.
        end_weight = Some(match pressure {
            Some(pen) => {
                scales.push(pen.factors(last.p));
                weights.push(1.0);
                travelled.min(1.0)
            }
            None => (travelled / step).min(1.0),
        });
    }
    // A dab's (radius, flow) factor: exactly 1 without pressure, so those
    // strokes compute what they did before.
    let scale_of = |dab: usize| scales.get(dab).copied().unwrap_or((1.0, 1.0));
    // A pressure peak's dab counts for its share of a step (1 otherwise).
    let weight_of = |dab: usize| weights.get(dab).copied().unwrap_or(1.0);
    // A dab's exponent on what it keeps (1 for every dab in version 1).
    let last_dab = dabs.len() - 1;
    let exponent = |dab: usize| match end_weight {
        Some(weight) if dab == last_dab => density * weight,
        _ => density * weight_of(dab),
    };

    let feather_range = (outer - inner).max(0.01);
    let radius_sq = outer * outer;
    let inner_radius_sq = inner * inner;
    let bb_h = (max_y - min_y + 1) as usize;
    // Auto Mask: the colour under each dab's centre (3×3 mean).
    let centres: Vec<[f32; 3]> = colours.map_or_else(Vec::new, |colours| {
        dabs.iter()
            .map(|&(dx, dy)| {
                let (cx, cy) = (dx.round() as i64, dy.round() as i64);
                let mut sum = [0.0f32; 3];
                let mut n = 0.0;
                for y in cy - 1..=cy + 1 {
                    for x in cx - 1..=cx + 1 {
                        if x < 0 || y < 0 || x >= bb_w as i64 || y >= bb_h as i64 {
                            continue;
                        }
                        let c = colours[y as usize * bb_w + x as usize];
                        if c[0].is_finite() {
                            sum = [sum[0] + c[0], sum[1] + c[1], sum[2] + c[2]];
                            n += 1.0;
                        }
                    }
                }
                if n > 0.0 {
                    [sum[0] / n, sum[1] / n, sum[2] / n]
                } else {
                    [f32::NAN; 3]
                }
            })
            .collect()
    });

    if let Some(linear_colours) = linear_colours {
        let tip_at = |dist_sq: f32| {
            if let Some(center_weight) = lightroom {
                lightroom_tip(center_weight, dist_sq.sqrt() / outer)
            } else if dist_sq <= inner_radius_sq {
                1.0
            } else {
                let t = ((dist_sq.sqrt() - inner) / feather_range).clamp(0.0, 1.0);
                1.0 - (t * t * (3.0 - 2.0 * t))
            }
        };
        blend_stroke_auto_mask_v2(
            mask,
            (min_x, min_y, bb_w, bb_h),
            &dabs,
            (&scales, &weights),
            end_weight,
            outer,
            tip_at,
            flow,
            opacity,
            linear_colours,
            auto_mask_version,
            combine,
        );
        return dabs.len();
    }

    // Version 2's closer dabs (no Auto Mask colours): what a dab keeps,
    // (1 − flow · tip)^density, tabulated over the falloff; the power per
    // pixel cost more than the rest of the dab.
    const KEPT_STEPS: usize = 4096;
    let kept_for = |flow: f32| -> Vec<f32> {
        (0..=KEPT_STEPS)
            .map(|i| {
                let t = i as f32 / KEPT_STEPS as f32;
                (1.0 - flow * (1.0 - t * t * (3.0 - 2.0 * t))).powf(density)
            })
            .collect()
    };
    let tabulated = density != 1.0 && lightroom.is_none() && colours.is_none();
    let varying_flow = pressure.is_some_and(|pen| pen.flow);
    let kept_table: Option<Vec<f32>> = (tabulated && !varying_flow).then(|| kept_for(flow));
    // Pressure on flow: a table per dab at its own flow, for dabs large
    // enough to repay it (a per-pixel power made such strokes 2.4× slower).
    let dab_tables: Vec<Option<Vec<f32>>> = if tabulated && varying_flow {
        (0..dabs.len())
            .into_par_iter()
            .map(|dab| {
                let (size, flow_factor) = scale_of(dab);
                let area = std::f32::consts::PI * (outer * size).powi(2);
                (area >= 4.0 * KEPT_STEPS as f32).then(|| kept_for(flow * flow_factor))
            })
            .collect()
    } else {
        Vec::new()
    };
    let rows = &mut mask.as_mut()[min_y as usize * width..(max_y as usize + 1) * width];
    rows.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let py = y as f32;
        let mut keep: Vec<f32> = Vec::new();
        for (dab, &(dx, dy)) in dabs.iter().enumerate() {
            // The tip is evaluated at the distance scaled to the full
            // radius; the factors are 1 without pressure.
            let (size, flow_factor) = scale_of(dab);
            if size <= 0.0 {
                continue;
            }
            let (dab_outer, dab_radius_sq) = (outer * size, radius_sq * (size * size));
            let (tip_scale, flow) = (1.0 / (size * size), flow * flow_factor);
            let offset_y = py - dy;
            if offset_y.abs() > dab_outer {
                continue;
            }
            if keep.is_empty() {
                keep.resize(bb_w, 1.0);
            }
            let half = (dab_radius_sq - offset_y * offset_y).max(0.0).sqrt();
            let left = ((dx - half).floor() as i32).max(0);
            let right = ((dx + half).ceil() as i32).min(bb_w as i32 - 1);
            let table = kept_table
                .as_deref()
                .or_else(|| dab_tables.get(dab).and_then(Option::as_deref))
                .filter(|_| (end_weight.is_none() || dab != last_dab) && weight_of(dab) == 1.0);
            for x in left..=right {
                let offset_x = x as f32 - dx;
                let dist_sq = offset_x * offset_x + offset_y * offset_y;
                if dist_sq > dab_radius_sq {
                    continue;
                }
                let dist_sq = dist_sq * tip_scale;
                if let Some(table) = table {
                    keep[x as usize] *= if dist_sq <= inner_radius_sq {
                        table[0]
                    } else {
                        let t = ((dist_sq.sqrt() - inner) / feather_range).clamp(0.0, 1.0)
                            * KEPT_STEPS as f32;
                        let i = (t as usize).min(KEPT_STEPS - 1);
                        table[i] + (table[i + 1] - table[i]) * (t - i as f32)
                    };
                    continue;
                }
                let intensity = if let Some(center_weight) = lightroom {
                    lightroom_tip(center_weight, dist_sq.sqrt() / outer)
                } else if dist_sq <= inner_radius_sq {
                    1.0
                } else {
                    let t = ((dist_sq.sqrt() - inner) / feather_range).clamp(0.0, 1.0);
                    1.0 - (t * t * (3.0 - 2.0 * t))
                };
                let intensity = match colours {
                    Some(colours) => {
                        let pixel = colours[y * bb_w + x as usize];
                        if pixel[0].is_finite() && centres[dab][0].is_finite() {
                            intensity * auto_mask_weight(pixel, centres[dab])
                        } else {
                            0.0
                        }
                    }
                    None => intensity,
                };
                let kept = 1.0 - flow * intensity;
                let power = exponent(dab);
                keep[x as usize] *= if power == 1.0 { kept } else { kept.powf(power) };
            }
        }
        for (x, kept) in keep.iter().enumerate() {
            let coverage = ((1.0 - kept) * opacity * 255.0).round() as u8;
            if coverage > 0 {
                let pixel = &mut row[min_x as usize + x];
                *pixel = combine(*pixel, coverage);
            }
        }
    });
    dabs.len()
}

/// Auto Mask version 2 over one stroke: the stroke's coverage without Auto
/// Mask times a matte, the tip-weighted mean over the dabs of each dab's
/// share (`AutoMaskModel`). Applying the share per dab instead would let
/// overlapping dabs build a half-kept edge pixel up to 75–88 %, moving the
/// edge into the background.
#[allow(clippy::too_many_arguments)]
fn blend_stroke_auto_mask_v2(
    mask: &mut GrayImage,
    (min_x, min_y, bb_w, bb_h): (u32, u32, usize, usize),
    dabs: &[(f32, f32)],
    (scales, weights): (&[(f32, f32)], &[f32]),
    end_weight: Option<f32>,
    outer: f32,
    tip_at: impl Fn(f32) -> f32 + Sync,
    flow: f32,
    opacity: f32,
    colours: &[[f32; 3]],
    version: u32,
    combine: impl Fn(u8, u8) -> u8 + Sync,
) {
    let scale_of = |dab: usize| scales.get(dab).copied().unwrap_or((1.0, 1.0));
    let models: Vec<AutoMaskModel> = if version >= 3 {
        let clusters: Vec<Option<AutoMaskClusters>> = dabs
            .par_iter()
            .enumerate()
            .map(|(dab, &centre)| {
                AutoMaskClusters::of(colours, bb_w, bb_h, centre, outer * scale_of(dab).0)
            })
            .collect();
        let votes: Vec<f32> = (0..scales.len())
            .map(|dab| match scale_of(dab) {
                (size, _) if size <= 0.0 => 0.0,
                (_, flow) => flow,
            })
            .collect();
        stroke_models_v3(&clusters, dabs, &votes)
    } else {
        dabs.par_iter()
            .enumerate()
            .map(|(dab, &centre)| {
                AutoMaskModel::fit(colours, bb_w, bb_h, centre, outer * scale_of(dab).0)
            })
            .collect()
    };
    let radius_sq = outer * outer;
    let width = mask.width() as usize;
    let rows = &mut mask.as_mut()[min_y as usize * width..(min_y as usize + bb_h) * width];
    rows.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let py = y as f32;
        let mut keep: Vec<f32> = Vec::new();
        let mut shares: Vec<(f32, f32)> = Vec::new();
        for (dab, &(dx, dy)) in dabs.iter().enumerate() {
            let (size, flow_factor) = scale_of(dab);
            if size <= 0.0 {
                continue;
            }
            let (dab_outer, dab_radius_sq) = (outer * size, radius_sq * (size * size));
            let (tip_scale, flow) = (1.0 / (size * size), flow * flow_factor);
            let offset_y = py - dy;
            if offset_y.abs() > dab_outer {
                continue;
            }
            if keep.is_empty() {
                keep.resize(bb_w, 1.0);
                shares.resize(bb_w, (0.0, 0.0));
            }
            let half = (dab_radius_sq - offset_y * offset_y).max(0.0).sqrt();
            let left = ((dx - half).floor() as i32).max(0);
            let right = ((dx + half).ceil() as i32).min(bb_w as i32 - 1);
            for x in left..=right {
                let offset_x = x as f32 - dx;
                let dist_sq = offset_x * offset_x + offset_y * offset_y;
                if dist_sq > dab_radius_sq {
                    continue;
                }
                let intensity = tip_at(dist_sq * tip_scale);
                let x = x as usize;
                // Version 2's end dab counts for its share of a step.
                // So does a pressure peak's dab (`PenPressure::dabs`).
                let weight = match end_weight {
                    Some(weight) if dab == dabs.len() - 1 => weight,
                    _ => weights.get(dab).copied().unwrap_or(1.0),
                };
                let (kept, intensity) = if weight == 1.0 {
                    (1.0 - flow * intensity, intensity)
                } else {
                    ((1.0 - flow * intensity).powf(weight), intensity * weight)
                };
                keep[x] *= kept;
                // A dab weighs in by what it paints: under pen pressure on
                // flow that is its flow factor too (1 otherwise, exactly).
                let intensity = intensity * flow_factor;
                let share = models[dab].weight_at(colours[y * bb_w + x], (offset_x, offset_y));
                shares[x].0 += intensity * share;
                shares[x].1 += intensity;
            }
        }
        for (x, (kept, (share, weight))) in keep.iter().zip(&shares).enumerate() {
            if *weight <= 0.0 {
                continue;
            }
            let coverage = ((1.0 - kept) * (share / weight) * opacity * 255.0).round() as u8;
            if coverage > 0 {
                let pixel = &mut row[min_x as usize + x];
                *pixel = combine(*pixel, coverage);
            }
        }
    });
}

/// The former separate-layer rasterizer, kept as the exact test reference.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn render_stroke_layer_parallel(
    points: &[Point],
    radius: f32,
    feather: f32,
    scale: f32,
    crop_offset: (f32, f32),
    layer_offset: (f32, f32),
    bb_w: u32,
    bb_h: u32,
) -> GrayImage {
    let mut out_pixels = vec![0u8; (bb_w * bb_h) as usize];
    if points.is_empty() || radius <= 0.0 {
        return GrayImage::from_raw(bb_w, bb_h, out_pixels).unwrap();
    }

    struct Segment {
        x1: f32,
        y1: f32,
        dx: f32,
        dy: f32,
        len_sq: f32,
        bounds_left: i32,
        bounds_right: i32,
        bounds_top: i32,
        bounds_bottom: i32,
    }

    let mut segments = Vec::with_capacity(points.len().saturating_sub(1));
    for pair in points.windows(2) {
        let x1 = pair[0].x as f32 * scale - crop_offset.0 - layer_offset.0;
        let y1 = pair[0].y as f32 * scale - crop_offset.1 - layer_offset.1;
        let x2 = pair[1].x as f32 * scale - crop_offset.0 - layer_offset.0;
        let y2 = pair[1].y as f32 * scale - crop_offset.1 - layer_offset.1;

        let left = ((x1.min(x2) - radius).floor() as i32).max(0);
        let right = ((x1.max(x2) + radius).ceil() as i32).min(bb_w as i32 - 1);
        let top = ((y1.min(y2) - radius).floor() as i32).max(0);
        let bottom = ((y1.max(y2) + radius).ceil() as i32).min(bb_h as i32 - 1);

        if left > right || top > bottom {
            continue;
        }

        let dx = x2 - x1;
        let dy = y2 - y1;
        let len_sq = dx * dx + dy * dy;

        segments.push(Segment {
            x1,
            y1,
            dx,
            dy,
            len_sq,
            bounds_left: left,
            bounds_right: right,
            bounds_top: top,
            bounds_bottom: bottom,
        });
    }

    let mut single_point = None;
    if segments.is_empty() && !points.is_empty() {
        let x1 = points[0].x as f32 * scale - crop_offset.0 - layer_offset.0;
        let y1 = points[0].y as f32 * scale - crop_offset.1 - layer_offset.1;
        let left = ((x1 - radius).floor() as i32).max(0);
        let right = ((x1 + radius).ceil() as i32).min(bb_w as i32 - 1);
        let top = ((y1 - radius).floor() as i32).max(0);
        let bottom = ((y1 + radius).ceil() as i32).min(bb_h as i32 - 1);
        if left <= right && top <= bottom {
            single_point = Some((x1, y1, left, right, top, bottom));
        }
    }

    let feather_amount = feather.clamp(0.0, 1.0);
    let inner_radius = radius * (1.0 - feather_amount);
    let feather_range = (radius - inner_radius).max(0.01);
    let radius_sq = radius * radius;
    let inner_radius_sq = inner_radius * inner_radius;

    out_pixels
        .par_chunks_mut(bb_w as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let py = y as f32;
            let y_i32 = y as i32;

            let mut active_segments = Vec::new();
            for seg in &segments {
                if y_i32 >= seg.bounds_top && y_i32 <= seg.bounds_bottom {
                    active_segments.push(seg);
                }
            }

            let is_point_active = if let Some(pt) = &single_point {
                y_i32 >= pt.4 && y_i32 <= pt.5
            } else {
                false
            };

            if active_segments.is_empty() && !is_point_active {
                return;
            }

            for (x, pixel) in row.iter_mut().enumerate() {
                let px = x as f32;
                let x_i32 = x as i32;

                let mut min_dist_sq = radius_sq + 1.0;

                for seg in &active_segments {
                    if x_i32 >= seg.bounds_left && x_i32 <= seg.bounds_right {
                        let dist_sq = if seg.len_sq < 0.0001 {
                            (px - seg.x1) * (px - seg.x1) + (py - seg.y1) * (py - seg.y1)
                        } else {
                            let t = (((px - seg.x1) * seg.dx + (py - seg.y1) * seg.dy)
                                / seg.len_sq)
                                .clamp(0.0, 1.0);
                            let proj_x = seg.x1 + t * seg.dx;
                            let proj_y = seg.y1 + t * seg.dy;
                            (px - proj_x) * (px - proj_x) + (py - proj_y) * (py - proj_y)
                        };
                        if dist_sq < min_dist_sq {
                            min_dist_sq = dist_sq;
                        }
                    }
                }

                if is_point_active {
                    let pt = single_point.as_ref().unwrap();
                    if x_i32 >= pt.2 && x_i32 <= pt.3 {
                        let dist_sq = (px - pt.0) * (px - pt.0) + (py - pt.1) * (py - pt.1);
                        if dist_sq < min_dist_sq {
                            min_dist_sq = dist_sq;
                        }
                    }
                }

                if min_dist_sq <= radius_sq {
                    let intensity = if min_dist_sq <= inner_radius_sq {
                        1.0
                    } else {
                        let dist = min_dist_sq.sqrt();
                        let t = ((dist - inner_radius) / feather_range).clamp(0.0, 1.0);
                        1.0 - (t * t * (3.0 - 2.0 * t))
                    };
                    *pixel = (intensity * 255.0).round() as u8;
                }
            }
        });

    GrayImage::from_raw(bb_w, bb_h, out_pixels).unwrap()
}

fn generate_radial_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
) -> GrayImage {
    let params: RadialMaskParameters =
        serde_json::from_value(params_value.clone()).unwrap_or_default();
    let mut mask = GrayImage::new(width, height);

    let center_x = (params.center_x as f32 * scale - crop_offset.0) as i32;
    let center_y = (params.center_y as f32 * scale - crop_offset.1) as i32;
    let radius_x = params.radius_x as f32 * scale;
    let radius_y = params.radius_y as f32 * scale;
    let rotation_rad = params.rotation * PI / 180.0;
    let cos_rot = rotation_rad.cos();
    let sin_rot = rotation_rad.sin();
    let inner_bound = 1.0 - params.feather.clamp(0.0, 1.0);

    // Rows in parallel; every pixel is computed exactly as before.
    if width > 0 {
        mask.par_chunks_mut(width as usize)
            .enumerate()
            .for_each(|(y, row)| {
                let dy = y as f32 - center_y as f32;
                for (x, pixel) in row.iter_mut().enumerate() {
                    let dx = x as f32 - center_x as f32;

                    let rot_dx = dx * cos_rot + dy * sin_rot;
                    let rot_dy = -dx * sin_rot + dy * cos_rot;

                    let norm_x = rot_dx / radius_x.max(0.01);
                    let norm_y = rot_dy / radius_y.max(0.01);

                    let dist = (norm_x.powi(2) + norm_y.powi(2)).sqrt();

                    let intensity = 1.0 - (dist - inner_bound) / (1.0 - inner_bound).max(0.01);
                    *pixel = (intensity.clamp(0.0, 1.0) * 255.0) as u8;
                }
            });
    }

    mask
}

fn generate_linear_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
) -> GrayImage {
    let params: LinearMaskParameters =
        serde_json::from_value(params_value.clone()).unwrap_or_default();
    let mut mask = GrayImage::new(width, height);

    let start_x = params.start_x as f32 * scale - crop_offset.0;
    let start_y = params.start_y as f32 * scale - crop_offset.1;
    let end_x = params.end_x as f32 * scale - crop_offset.0;
    let end_y = params.end_y as f32 * scale - crop_offset.1;
    let range = params.range * scale;
    let range_end = params.range_end.map(|range_end| range_end * scale);

    let line_vec_x = end_x - start_x;
    let line_vec_y = end_y - start_y;

    let len_sq = line_vec_x.powi(2) + line_vec_y.powi(2);

    if len_sq < 0.01 || width == 0 {
        return mask;
    }

    let perp_vec_x = -line_vec_y / len_sq.sqrt();
    let perp_vec_y = line_vec_x / len_sq.sqrt();

    let half_width = range.max(0.01);
    let end_half_width = range_end.map_or(half_width, |range_end| range_end.max(0.01));

    // Rows in parallel; every pixel is computed exactly as before.
    mask.par_chunks_mut(width as usize)
        .enumerate()
        .for_each(|(y_u, row)| {
            let pixel_vec_y = y_u as f32 - start_y;
            for (x_u, pixel) in row.iter_mut().enumerate() {
                let pixel_vec_x = x_u as f32 - start_x;

                let dist_perp = pixel_vec_x * perp_vec_x + pixel_vec_y * perp_vec_y;

                // Negative distances lie on the start (100%) side.
                let t = if dist_perp > 0.0 {
                    dist_perp / end_half_width
                } else {
                    dist_perp / half_width
                };

                let intensity = 0.5 - t * 0.5;
                *pixel = (intensity.clamp(0.0, 1.0) * 255.0) as u8;
            }
        });

    mask
}

/// Brush strokes blend in order onto an empty mask, so the mask after N strokes
/// depends only on strokes 1..=N. Recent results are extended by appended
/// strokes instead of re-rasterizing every stroke after each edit. While a
/// stroke is painted only its newest line changes, so each geometry keeps its
/// latest result plus the longest earlier state that is a prefix of it. Every
/// stroke is drawn at up to four geometries (live overlay, preview, library
/// thumbnail, the layer list's mask thumbnails).
const BRUSH_PREFIX_ENTRIES: usize = 8;
const BRUSH_PREFIX_BYTES: usize = 24 * 1024 * 1024;

/// Mask size, scale and crop offset, and for Auto Mask strokes the photo they
/// read: its pixels (a Weak pins the allocation's identity without retaining
/// them, as in `MaskBitmapKey`; an address alone could be reused by a later
/// photo) and how they are read (geometry, raw curve, coordinates).
#[derive(Clone)]
struct BrushGeometry {
    frame: (u32, u32, u32, u32, u32),
    photo: Option<BrushPhoto>,
}

#[derive(Clone)]
struct BrushPhoto {
    image: Weak<DynamicImage>,
    geometry: SourceGeometry,
    raw_curve: bool,
    coordinate_dimensions: (f64, f64),
}

impl PartialEq for BrushGeometry {
    fn eq(&self, other: &Self) -> bool {
        self.frame == other.frame
            && match (&self.photo, &other.photo) {
                (Some(left), Some(right)) => {
                    Weak::ptr_eq(&left.image, &right.image)
                        && left.geometry == right.geometry
                        && left.raw_curve == right.raw_curve
                        && left.coordinate_dimensions == right.coordinate_dimensions
                }
                (None, None) => true,
                _ => false,
            }
    }
}

struct BrushPrefix {
    geometry: BrushGeometry,
    lines: Vec<BrushLine>,
    mask: GrayImage,
}

#[cfg(not(test))]
static BRUSH_PREFIXES: std::sync::LazyLock<std::sync::Mutex<VecDeque<BrushPrefix>>> =
    std::sync::LazyLock::new(Default::default);

/// The process-wide cache. Tests run in parallel threads and count their own
/// rasterizations, so each test thread gets a private cache instead.
fn with_brush_prefixes<R>(f: impl FnOnce(&mut VecDeque<BrushPrefix>) -> R) -> R {
    #[cfg(not(test))]
    {
        f(&mut BRUSH_PREFIXES
            .lock()
            .unwrap_or_else(|error| error.into_inner()))
    }
    #[cfg(test)]
    {
        thread_local! {
            static PREFIXES: std::cell::RefCell<VecDeque<BrushPrefix>> = Default::default();
        }
        PREFIXES.with(|prefixes| f(&mut prefixes.borrow_mut()))
    }
}

fn clear_brush_prefixes() {
    with_brush_prefixes(VecDeque::clear);
}

/// Empties the brush prefix cache, so a benchmark times every brush
/// rasterization in full (`test-support`; crates/engine/benches/develop.rs).
#[cfg(feature = "test-support")]
pub fn clear_brush_prefix_cache() {
    clear_brush_prefixes();
}

/// Longest cached exact prefix of `lines` for this geometry, marked recent.
fn brush_prefix(geometry: &BrushGeometry, lines: &[BrushLine]) -> Option<(GrayImage, usize)> {
    with_brush_prefixes(|prefixes| brush_prefix_in(prefixes, geometry, lines))
}

fn brush_prefix_in(
    prefixes: &mut VecDeque<BrushPrefix>,
    geometry: &BrushGeometry,
    lines: &[BrushLine],
) -> Option<(GrayImage, usize)> {
    let index = prefixes
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            entry.geometry == *geometry
                && entry.lines.len() <= lines.len()
                && entry.lines[..] == lines[..entry.lines.len()]
        })
        .max_by_key(|(_, entry)| entry.lines.len())
        .map(|(index, _)| index)?;
    let entry = prefixes.remove(index)?;
    let found = (entry.mask.clone(), entry.lines.len());
    prefixes.push_front(entry);
    Some(found)
}

fn remember_brush_prefix(geometry: &BrushGeometry, lines: &[BrushLine], mask: &GrayImage) {
    if lines.is_empty() || mask.as_raw().len() > BRUSH_PREFIX_BYTES {
        return;
    }
    with_brush_prefixes(|prefixes| remember_brush_prefix_in(prefixes, geometry, lines, mask));
}

fn remember_brush_prefix_in(
    prefixes: &mut VecDeque<BrushPrefix>,
    geometry: &BrushGeometry,
    lines: &[BrushLine],
    mask: &GrayImage,
) {
    prefixes.retain(|entry| !(entry.geometry == *geometry && entry.lines[..] == lines[..]));
    // Keep one earlier state for this geometry: the longest prefix of these
    // lines (the state before the newest stroke), else the most recent entry.
    // States of a stroke that has since grown are never prefixes again.
    let keep = prefixes
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.geometry == *geometry)
        .max_by_key(|(index, entry)| {
            let prefix =
                entry.lines.len() < lines.len() && entry.lines[..] == lines[..entry.lines.len()];
            (
                prefix,
                if prefix { entry.lines.len() } else { 0 },
                std::cmp::Reverse(*index),
            )
        })
        .map(|(index, _)| index);
    let mut index = 0;
    prefixes.retain(|entry| {
        let current = index;
        index += 1;
        entry.geometry != *geometry || Some(current) == keep
    });
    prefixes.push_front(BrushPrefix {
        geometry: geometry.clone(),
        lines: lines.to_vec(),
        mask: mask.clone(),
    });
    while prefixes.len() > BRUSH_PREFIX_ENTRIES
        || prefixes
            .iter()
            .map(|entry| entry.mask.as_raw().len())
            .sum::<usize>()
            > BRUSH_PREFIX_BYTES
    {
        prefixes.pop_back();
    }
}

#[cfg(test)]
thread_local! {
    static RASTERIZED_BRUSH_LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn apply_brush_line(
    mask: &mut GrayImage,
    line: &BrushLine,
    scale: f32,
    crop_offset: (f32, f32),
    source: Option<&SourceSampler>,
) {
    #[cfg(test)]
    RASTERIZED_BRUSH_LINES.with(|count| count.set(count.get() + 1));
    if line.points.is_empty() {
        return;
    }
    let (width, height) = mask.dimensions();
    let is_eraser = line.tool == "eraser";
    let radius = (line.brush_size * scale / 2.0).max(0.0);
    let feather = line.feather.clamp(0.0, 1.0);
    let percent = |value: Option<f32>| value.map_or(1.0, |value| (value / 100.0).clamp(0.0, 1.0));
    let (opacity, flow) = (percent(line.opacity), percent(line.flow));
    if opacity <= 0.0 || flow <= 0.0 {
        return;
    }

    // Auto Mask needs the photo; without it the stroke paints as usual.
    let auto_mask = source.filter(|_| line.auto_mask && !is_eraser);
    let pressure = PenPressure::of(line);
    let tip = match (line.lr_center_weight, line.hardness) {
        (Some(center_weight), _) => Some(DabTip::lightroom(radius, center_weight)),
        (None, Some(hardness)) => Some(DabTip::capture_one(radius, hardness / 100.0)),
        (None, None) if flow < 1.0 || auto_mask.is_some() || pressure.is_some() => {
            Some(DabTip::feathered(radius, feather))
        }
        (None, None) => None,
    };
    let tip = match line.dab_version {
        Some(version) if version >= 2 => tip.map(|tip| {
            tip.dab_version_2(line.hardness.is_some() && !(line.auto_mask && !is_eraser))
        }),
        _ => tip,
    };
    let reach = tip.map_or(radius, |tip| tip.outer);
    let Some(bounds) = stroke_bounds(&line.points, width, height, reach, scale, crop_offset) else {
        return;
    };
    #[cfg(feature = "performance-profiling")]
    let started = std::time::Instant::now();

    // Strokes lay over earlier ones (and the eraser removes in proportion), so
    // two 50% strokes reach 75%, as in Capture One.
    let combine = |destination: u8, coverage: u8| {
        let src_val = coverage as f32 / 255.0;
        let dst_val = destination as f32 / 255.0;
        let blended = if is_eraser {
            dst_val * (1.0 - src_val)
        } else {
            dst_val + src_val - dst_val * src_val
        };
        (blended.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    if let Some(tip) = tip {
        let edge_aware = line.auto_mask_version.is_some_and(|version| version >= 2);
        let colours = auto_mask
            .filter(|_| !edge_aware)
            .map(|sampler| source_lab_region(sampler, bounds));
        let linear_colours = auto_mask
            .filter(|_| edge_aware)
            .map(|sampler| source_linear_region(sampler, bounds));
        #[cfg(feature = "performance-profiling")]
        let colours_ms = started.elapsed().as_secs_f64() * 1000.0;
        let _dabs = blend_stroke_dabs(
            mask,
            &line.points,
            tip,
            flow,
            opacity,
            scale,
            crop_offset,
            bounds,
            colours.as_deref(),
            linear_colours.as_deref(),
            line.auto_mask_version.unwrap_or(1),
            pressure,
            combine,
        );
        // Profiling only: dabs of this stroke, the rows and columns of its
        // bounding box, and the Auto Mask colour lookup's share of the time.
        #[cfg(feature = "performance-profiling")]
        crate::performance::record_stage(
            "brush_stroke_dabs",
            started,
            serde_json::json!({
                "dabs": _dabs,
                "points": line.points.len(),
                "rows": bounds.3 - bounds.1 + 1,
                "cols": bounds.2 - bounds.0 + 1,
                "radius_px": radius,
                "auto_mask": auto_mask.is_some(),
                "auto_mask_version": line.auto_mask_version.unwrap_or(1),
                "colours_ms": colours_ms,
            }),
        );
    } else {
        blend_stroke(
            mask,
            &line.points,
            radius,
            feather,
            opacity,
            scale,
            crop_offset,
            bounds,
            combine,
        );
        #[cfg(feature = "performance-profiling")]
        crate::performance::record_stage(
            "brush_stroke_capsules",
            started,
            serde_json::json!({
                "points": line.points.len(),
                "rows": bounds.3 - bounds.1 + 1,
                "cols": bounds.2 - bounds.0 + 1,
                "radius_px": radius,
            }),
        );
    }
}

#[cfg(feature = "performance-profiling")]
struct BrushBitmapSpan {
    started: std::time::Instant,
    lines: usize,
    cached_lines: usize,
    width: u32,
    height: u32,
}

#[cfg(feature = "performance-profiling")]
impl Drop for BrushBitmapSpan {
    fn drop(&mut self) {
        crate::performance::record_stage(
            "brush_bitmap",
            self.started,
            serde_json::json!({
                "lines": self.lines,
                "cached_lines": self.cached_lines,
                "width": self.width,
                "height": self.height,
            }),
        );
    }
}

fn generate_brush_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    now: Shown,
    source: Option<&ParametricMaskSource>,
) -> GrayImage {
    // Read in place: copying the strokes first cost as much again per frame.
    let mut params = BrushMaskParameters::deserialize(params_value).unwrap_or_default();
    strokes_as_shown(
        params
            .lines
            .iter_mut()
            .map(|line| (line.frame, &mut line.points, &mut line.brush_size)),
        now,
        scale,
    );
    let lines = &params.lines;
    let Some(newest) = lines.len().checked_sub(1) else {
        return GrayImage::new(width, height);
    };
    // Auto Mask strokes depend on the photo too: the cached prefixes are
    // keyed by it (see `BrushGeometry`).
    let source = source.filter(|_| lines.iter().any(|line| line.auto_mask));
    let sampler = source.map(|source| SourceSampler::new(source, scale, crop_offset));
    let geometry = BrushGeometry {
        frame: (
            width,
            height,
            scale.to_bits(),
            crop_offset.0.to_bits(),
            crop_offset.1.to_bits(),
        ),
        photo: source.map(|source| BrushPhoto {
            image: Arc::downgrade(&source.image),
            geometry: source.geometry,
            raw_curve: source.raw_curve,
            coordinate_dimensions: source.coordinate_dimensions,
        }),
    };
    #[cfg(feature = "performance-profiling")]
    let started = std::time::Instant::now();
    let (mut mask, cached_lines) =
        brush_prefix(&geometry, lines).unwrap_or_else(|| (GrayImage::new(width, height), 0));
    if cached_lines == lines.len() {
        return mask;
    }
    // Profiling only: the whole brush component, with how many of its strokes
    // came from the cached prefix (a live stroke redraws only its newest).
    #[cfg(feature = "performance-profiling")]
    let _span = BrushBitmapSpan {
        started,
        lines: lines.len(),
        cached_lines,
        width,
        height,
    };
    for line in &lines[cached_lines..newest] {
        apply_brush_line(&mut mask, line, scale, crop_offset, sampler.as_ref());
    }
    if newest > cached_lines {
        remember_brush_prefix(&geometry, &lines[..newest], &mask);
    }
    apply_brush_line(
        &mut mask,
        &lines[newest],
        scale,
        crop_offset,
        sampler.as_ref(),
    );
    remember_brush_prefix(&geometry, lines, &mask);
    mask
}

fn generate_flow_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    now: Shown,
) -> GrayImage {
    let mut params = FlowMaskParameters::deserialize(params_value).unwrap_or_default();
    strokes_as_shown(
        params
            .lines
            .iter_mut()
            .map(|line| (line.frame, &mut line.points, &mut line.brush_size)),
        now,
        scale,
    );
    let mut final_mask = GrayImage::new(width, height);

    for line in &params.lines {
        if line.points.is_empty() {
            continue;
        }

        let is_eraser = line.tool == "eraser";
        let flow_per_stroke = (line.flow.clamp(0.0, 100.0) / 100.0) * 255.0;
        let radius = (line.brush_size * scale / 2.0).max(0.0);
        let feather = line.feather.clamp(0.0, 1.0);

        let Some((min_x, min_y, max_x, max_y)) =
            stroke_bounds(&line.points, width, height, radius, scale, crop_offset)
        else {
            continue;
        };

        blend_stroke(
            &mut final_mask,
            &line.points,
            radius,
            feather,
            1.0,
            scale,
            crop_offset,
            (min_x, min_y, max_x, max_y),
            |destination, coverage| {
                let stroke_pixel = coverage as f32;
                let c_norm = destination as f32 / 255.0;
                let delta = ((stroke_pixel / 255.0) * flow_per_stroke).round();
                let d_norm = (delta / 255.0).clamp(0.0, 1.0);
                let next = if is_eraser {
                    c_norm * (1.0 - d_norm)
                } else {
                    c_norm + d_norm - c_norm * d_norm
                };
                (next.clamp(0.0, 1.0) * 255.0).round() as u8
            },
        );
    }

    final_mask
}

struct TransformParams {
    pub rotation: f32,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub orientation_steps: u8,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub crop_offset: (f32, f32),
}

fn generate_ai_bitmap_from_full_mask(
    full_mask_image: &GrayImage,
    tf: &TransformParams,
    warp_change: Option<&WarpChange>,
) -> GrayImage {
    let (full_mask_w, full_mask_h) = full_mask_image.dimensions();
    let mut final_mask = GrayImage::new(tf.width, tf.height);

    let angle_rad = tf.rotation.to_radians();
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();

    let (coarse_rotated_w, coarse_rotated_h) = if tf.orientation_steps % 2 == 1 {
        (full_mask_h, full_mask_w)
    } else {
        (full_mask_w, full_mask_h)
    };

    let scaled_coarse_rotated_w = coarse_rotated_w as f32 * tf.scale;
    let scaled_coarse_rotated_h = coarse_rotated_h as f32 * tf.scale;
    let center_x = scaled_coarse_rotated_w / 2.0;
    let center_y = scaled_coarse_rotated_h / 2.0;

    for y_out in 0..tf.height {
        for x_out in 0..tf.width {
            let x_uncrop = x_out as f32 + tf.crop_offset.0;
            let y_uncrop = y_out as f32 + tf.crop_offset.1;

            let x_centered = x_uncrop - center_x;
            let y_centered = y_uncrop - center_y;

            let (x_unrotated, y_unrotated) = nearest_when_straightened(
                x_centered * cos_a + y_centered * sin_a + center_x,
                -x_centered * sin_a + y_centered * cos_a + center_y,
                tf.rotation,
            );

            // Pixel indices, as `apply_flip` and `apply_coarse_rotation` move
            // them: index i of n lands on n − 1 − i.
            let x_unflipped = if tf.flip_horizontal {
                scaled_coarse_rotated_w - 1.0 - x_unrotated
            } else {
                x_unrotated
            };
            let y_unflipped = if tf.flip_vertical {
                scaled_coarse_rotated_h - 1.0 - y_unrotated
            } else {
                y_unrotated
            };

            let (x_unrotated_coarse, y_unrotated_coarse) = match tf.orientation_steps {
                0 => (x_unflipped, y_unflipped),
                1 => (y_unflipped, scaled_coarse_rotated_w - 1.0 - x_unflipped),
                2 => (
                    scaled_coarse_rotated_w - 1.0 - x_unflipped,
                    scaled_coarse_rotated_h - 1.0 - y_unflipped,
                ),
                3 => (scaled_coarse_rotated_h - 1.0 - y_unflipped, x_unflipped),
                _ => (x_unflipped, y_unflipped),
            };

            let x_src = x_unrotated_coarse / tf.scale;
            let y_src = y_unrotated_coarse / tf.scale;
            // A mask made in another lens or transform warp: its pixel for
            // this point of the current warped frame.
            let Some((x_src, y_src)) = (match warp_change {
                Some(change) => change.map(x_src, y_src),
                None => Some((x_src, y_src)),
            }) else {
                continue;
            };

            if x_src >= 0.0
                && x_src < full_mask_w as f32
                && y_src >= 0.0
                && y_src < full_mask_h as f32
            {
                let pixel = full_mask_image.get_pixel(x_src as u32, y_src as u32);
                final_mask.put_pixel(x_out, y_out, *pixel);
            }
        }
    }

    final_mask
}

fn decode_ai_bitmap(data_url: &str) -> Option<GrayImage> {
    let b64_data = if let Some(idx) = data_url.find(',') {
        &data_url[idx + 1..]
    } else {
        data_url
    };
    let decoded_bytes = general_purpose::STANDARD.decode(b64_data).ok()?;
    Some(image::load_from_memory(&decoded_bytes).ok()?.to_luma8())
}

/// An AI mask's stored bitmap (`maskDataBase64`, in the photo's own frame)
/// in the frame `width` × `height` at `scale` with `crop_offset`, placed by
/// the photo's current `geometry` (see `PhotoGeometry`).
fn ai_bitmap_in_frame(
    params_value: &Value,
    data_url: &str,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
) -> Option<GrayImage> {
    let full_mask = decode_ai_bitmap(data_url)?;
    let frame = geometry.frame;
    let tf = TransformParams {
        rotation: frame.rotation,
        flip_horizontal: frame.flip_horizontal,
        flip_vertical: frame.flip_vertical,
        orientation_steps: frame.orientation_steps,
        width,
        height,
        scale,
        crop_offset,
    };
    let change = geometry.warp_change(params_value, full_mask.width(), full_mask.height());
    Some(generate_ai_bitmap_from_full_mask(
        &full_mask,
        &tf,
        change.as_ref(),
    ))
}

fn generate_ai_sky_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
) -> Option<GrayImage> {
    let params: AiSkyMaskParameters = serde_json::from_value(params_value.clone()).ok()?;
    let grow_feather: GrowFeatherParameters =
        serde_json::from_value(params_value.clone()).unwrap_or_default();
    let data_url = params.mask_data_base64?;

    let mut mask = ai_bitmap_in_frame(
        params_value,
        &data_url,
        width,
        height,
        scale,
        crop_offset,
        geometry,
    )?;

    apply_grow_and_feather(
        &mut mask,
        grow_feather.grow,
        grow_feather.feather,
        width,
        height,
    );

    Some(mask)
}

fn generate_ai_depth_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
) -> Option<GrayImage> {
    let params: AiDepthMaskParameters = serde_json::from_value(params_value.clone()).ok()?;
    let grow_feather: GrowFeatherParameters =
        serde_json::from_value(params_value.clone()).unwrap_or_default();
    let data_url = params.mask_data_base64?;

    let depth_map = ai_bitmap_in_frame(
        params_value,
        &data_url,
        width,
        height,
        scale,
        crop_offset,
        geometry,
    )?;

    let (w, h) = depth_map.dimensions();
    let mut mask = GrayImage::new(w, h);

    fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
        let t = ((x - edge0) / (edge1 - edge0).max(0.0001)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    let min_fade = params.min_fade;
    let max_fade = params.max_fade;

    for (x, y, p) in depth_map.enumerate_pixels() {
        let val_pct = (p[0] as f32 / 255.0) * 100.0;

        let lower_bound = smoothstep(params.min_depth - min_fade, params.min_depth, val_pct);
        let upper_bound = 1.0 - smoothstep(params.max_depth, params.max_depth + max_fade, val_pct);
        // Full coverage inside the range, as Lightroom's Depth Range (the
        // inherited mask was also scaled by the depth, so far parts of the
        // range were barely selected).
        let final_intensity = lower_bound * upper_bound;

        mask.put_pixel(x, y, Luma([(final_intensity * 255.0).round() as u8]));
    }

    if params.feather > 0.0 {
        mask = image::imageops::blur(&mask, params.feather * 0.1);
    }

    apply_grow_and_feather(
        &mut mask,
        grow_feather.grow,
        grow_feather.feather,
        width,
        height,
    );

    Some(mask)
}

fn generate_ai_foreground_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
) -> Option<GrayImage> {
    let params: AiForegroundMaskParameters = serde_json::from_value(params_value.clone()).ok()?;
    let grow_feather: GrowFeatherParameters =
        serde_json::from_value(params_value.clone()).unwrap_or_default();
    let data_url = params.mask_data_base64?;

    let mut mask = ai_bitmap_in_frame(
        params_value,
        &data_url,
        width,
        height,
        scale,
        crop_offset,
        geometry,
    )?;

    apply_grow_and_feather(
        &mut mask,
        grow_feather.grow,
        grow_feather.feather,
        width,
        height,
    );

    Some(mask)
}

fn generate_ai_subject_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
) -> Option<GrayImage> {
    let params: AiSubjectMaskParameters = serde_json::from_value(params_value.clone()).ok()?;
    let grow_feather: GrowFeatherParameters =
        serde_json::from_value(params_value.clone()).unwrap_or_default();
    let data_url = params.mask_data_base64?;

    let mut mask = ai_bitmap_in_frame(
        params_value,
        &data_url,
        width,
        height,
        scale,
        crop_offset,
        geometry,
    )?;

    apply_grow_and_feather(
        &mut mask,
        grow_feather.grow,
        grow_feather.feather,
        width,
        height,
    );

    Some(mask)
}

/// Object mask coverage from which Remove's Detect objects removes a pixel
/// completely (out of 255).
const QUICK_ERASE_THRESHOLD: u8 = 32;

/// Remove's Detect objects (`quick-eraser`): the object mask made binary, so
/// that thin or soft parts (a crane's lattice, hair) are filled completely
/// instead of showing through the fill at partial coverage, then grown and
/// feathered so the fill takes the object's edge along.
fn generate_quick_erase_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
) -> Option<GrayImage> {
    let params: AiSubjectMaskParameters = serde_json::from_value(params_value.clone()).ok()?;
    let grow_feather: GrowFeatherParameters =
        serde_json::from_value(params_value.clone()).unwrap_or_default();
    let data_url = params.mask_data_base64?;
    let mut mask = ai_bitmap_in_frame(
        params_value,
        &data_url,
        width,
        height,
        scale,
        crop_offset,
        geometry,
    )?;
    mask.as_mut()
        .par_chunks_mut(MASK_PASS_CHUNK)
        .for_each(|chunk| {
            chunk.iter_mut().for_each(|value| {
                *value = if *value >= QUICK_ERASE_THRESHOLD {
                    255
                } else {
                    0
                }
            })
        });
    grow_and_feather_within_bounds(
        &mut mask,
        grow_feather.grow,
        grow_feather.feather,
        width,
        height,
    );
    Some(mask)
}

/// `apply_grow_and_feather` on the box around the mask's coverage (with the
/// margin the grow and the blur kernel reach) instead of the whole frame:
/// the same result, since everything outside stays zero, in a fraction of
/// the time for an object much smaller than a 40 MP photo (both filters
/// are single-threaded and scale with the area). Growing only (a negative
/// grow, which erodes from the frame's edges, takes the whole frame).
fn grow_and_feather_within_bounds(
    mask: &mut GrayImage,
    grow: f32,
    feather: f32,
    width: u32,
    height: u32,
) {
    let (w, h) = mask.dimensions();
    let raw = mask.as_raw();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0, 0);
    for (y, row) in raw.chunks_exact(w.max(1) as usize).enumerate() {
        if let (Some(first), Some(last)) = (
            row.iter().position(|&v| v > 0),
            row.iter().rposition(|&v| v > 0),
        ) {
            min_x = min_x.min(first as u32);
            max_x = max_x.max(last as u32);
            min_y = min_y.min(y as u32);
            max_y = y as u32;
        }
    }
    if min_x == u32::MAX || grow < 0.0 {
        if min_x != u32::MAX {
            apply_grow_and_feather(mask, grow, feather, width, height);
        }
        return;
    }
    // The reach of apply_grow_and_feather: the dilation's radius plus the
    // blur kernel's (imageproc: ceil(2 sigma)), and a pixel for rounding.
    let base_dimension = width.min(height) as f32;
    let grow_pixels = ((grow / 100.0) * base_dimension * 0.01).round().min(255.0);
    let sigma = (feather.max(0.0) / 100.0) * base_dimension * 0.005;
    let margin = grow_pixels as u32 + (2.0 * sigma).ceil() as u32 + 2;
    let x0 = min_x.saturating_sub(margin);
    let y0 = min_y.saturating_sub(margin);
    let x1 = (max_x + margin).min(w - 1);
    let y1 = (max_y + margin).min(h - 1);
    let mut region = image::imageops::crop_imm(mask, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image();
    apply_grow_and_feather(&mut region, grow, feather, width, height);
    image::imageops::replace(mask, &region, i64::from(x0), i64::from(y0));
}

/// Where a point of the photo shown in `geometry` (full-resolution pixels
/// after orientation, flips and straightening, before the crop) lies in the
/// source's coordinates: the inverse of `DisplayToSource`'s mapping for one
/// point, exact for a photo shown as it is.
fn display_point_to_source(
    source: &ParametricMaskSource,
    geometry: SourceGeometry,
    x: f64,
    y: f64,
) -> (f64, f64) {
    if geometry.is_identity() {
        return (x, y);
    }
    let (full_w, full_h) = source.coordinate_dimensions;
    let (w, h) = if geometry.orientation_steps % 2 == 1 {
        (full_h, full_w)
    } else {
        (full_w, full_h)
    };
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (sin_a, cos_a) = (geometry.rotation as f64).to_radians().sin_cos();
    let (dx, dy) = (x - cx, y - cy);
    let x_unrotated = dx * cos_a + dy * sin_a + cx;
    let y_unrotated = -dx * sin_a + dy * cos_a + cy;
    let x_unflipped = if geometry.flip_horizontal {
        w - x_unrotated
    } else {
        x_unrotated
    };
    let y_unflipped = if geometry.flip_vertical {
        h - y_unrotated
    } else {
        y_unrotated
    };
    match geometry.orientation_steps {
        1 => (y_unflipped, w - x_unflipped),
        2 => (w - x_unflipped, h - y_unflipped),
        3 => (h - y_unflipped, x_unflipped),
        _ => (x_unflipped, y_unflipped),
    }
}

/// The geometry a component's own point (`targetX`/`targetY`) was set in.
fn parametric_geometry(params: &ParametricMaskParameters) -> SourceGeometry {
    SourceGeometry {
        rotation: params.rotation,
        flip_horizontal: params.flip_horizontal,
        flip_vertical: params.flip_vertical,
        orientation_steps: params.orientation_steps % 4,
    }
}

/// The source pixel under the component's point, which was set on the photo
/// as shown then (its stored geometry), whatever its geometry now.
fn parametric_reference_pixel(
    source: &ParametricMaskSource,
    params: &ParametricMaskParameters,
) -> Option<Rgba<u8>> {
    let (width, height) = source.image.dimensions();
    let (scale_x, scale_y) = source.sampling_scales();
    if !params.target_x.is_finite() || !params.target_y.is_finite() {
        return None;
    }
    let (target_x, target_y) = display_point_to_source(
        source,
        parametric_geometry(params),
        params.target_x,
        params.target_y,
    );
    if width == 0
        || height == 0
        || target_x < 0.0
        || target_y < 0.0
        || target_x >= source.coordinate_dimensions.0
        || target_y >= source.coordinate_dimensions.1
    {
        return None;
    }
    // Resolve the full-resolution reference first, then use the same floor
    // sampling as output pixels. Rounding after scaling could move an odd
    // original coordinate into the next reduced pixel or beyond the image edge.
    let x = (target_x.round() * scale_x).floor().min((width - 1) as f64) as u32;
    let y = (target_y.round() * scale_y)
        .floor()
        .min((height - 1) as f64) as u32;
    Some(source.image.get_pixel(x, y))
}

fn generate_color_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    warped_image: Option<&ParametricMaskSource>,
) -> Option<GrayImage> {
    let params: ParametricMaskParameters = serde_json::from_value(params_value.clone()).ok()?;
    let source = warped_image?;
    if let Some(refine) = params.refine {
        return color_range_mask(&params, refine, width, height, scale, crop_offset, source);
    }
    let warped = &source.image;
    let (source_w, source_h) = warped.dimensions();
    let full_w = source.coordinate_dimensions.0 as f32;
    let full_h = source.coordinate_dimensions.1 as f32;
    let (source_scale_x, source_scale_y) = source.sampling_scales();

    let ref_pixel = parametric_reference_pixel(source, &params)?;
    let ref_r = ref_pixel[0] as f32;
    let ref_g = ref_pixel[1] as f32;
    let ref_b = ref_pixel[2] as f32;

    let mut mask = GrayImage::new(width, height);

    // The mask follows the photo's geometry now (its point was read in the
    // geometry it was set in).
    let now = source.geometry;
    let angle_rad = now.rotation * PI / 180.0;
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();

    let (coarse_rotated_w, coarse_rotated_h) = if now.orientation_steps % 2 == 1 {
        (full_h, full_w)
    } else {
        (full_w, full_h)
    };

    let scaled_coarse_rotated_w = coarse_rotated_w * scale;
    let scaled_coarse_rotated_h = coarse_rotated_h * scale;
    let center_x = scaled_coarse_rotated_w / 2.0;
    let center_y = scaled_coarse_rotated_h / 2.0;

    let tolerance_sq = (params.tolerance * 2.55).max(1.0).powi(2) * 3.0;
    let inv_scale = 1.0 / scale;

    for y_out in 0..height {
        let y_uncrop = y_out as f32 + crop_offset.1;
        let y_centered = y_uncrop - center_y;
        let y_sin = y_centered * sin_a;
        let y_cos = y_centered * cos_a;

        for x_out in 0..width {
            let x_uncrop = x_out as f32 + crop_offset.0;
            let x_centered = x_uncrop - center_x;

            let (x_unrotated, y_unrotated) = nearest_when_straightened(
                x_centered * cos_a + y_sin + center_x,
                -x_centered * sin_a + y_cos + center_y,
                now.rotation,
            );

            // Pixel indices, as `apply_flip` and `apply_coarse_rotation` move
            // them: index i of n lands on n − 1 − i.
            let x_unflipped = if now.flip_horizontal {
                scaled_coarse_rotated_w - 1.0 - x_unrotated
            } else {
                x_unrotated
            };
            let y_unflipped = if now.flip_vertical {
                scaled_coarse_rotated_h - 1.0 - y_unrotated
            } else {
                y_unrotated
            };

            let (x_unrotated_coarse, y_unrotated_coarse) = match now.orientation_steps {
                0 => (x_unflipped, y_unflipped),
                1 => (y_unflipped, scaled_coarse_rotated_w - 1.0 - x_unflipped),
                2 => (
                    scaled_coarse_rotated_w - 1.0 - x_unflipped,
                    scaled_coarse_rotated_h - 1.0 - y_unflipped,
                ),
                3 => (scaled_coarse_rotated_h - 1.0 - y_unflipped, x_unflipped),
                _ => (x_unflipped, y_unflipped),
            };

            if x_unrotated_coarse >= 0.0 && y_unrotated_coarse >= 0.0 {
                let x_src = ((x_unrotated_coarse * inv_scale) as f64 * source_scale_x) as u32;
                let y_src = ((y_unrotated_coarse * inv_scale) as f64 * source_scale_y) as u32;

                if x_src < source_w && y_src < source_h {
                    let pixel = warped.get_pixel(x_src, y_src);
                    let dist_sq = (pixel[0] as f32 - ref_r).powi(2)
                        + (pixel[1] as f32 - ref_g).powi(2)
                        + (pixel[2] as f32 - ref_b).powi(2);

                    if dist_sq <= tolerance_sq {
                        let intensity = 1.0 - (dist_sq.sqrt() / tolerance_sq.sqrt());
                        mask.put_pixel(x_out, y_out, Luma([(intensity * 255.0) as u8]));
                    }
                }
            }
        }
    }

    apply_grow_and_feather(&mut mask, params.grow, params.feather, width, height);
    Some(mask)
}

/// Lightroom's Color Range, measured in Camera Raw on two photographs with
/// 80 renders (notes: reference/color-range.md). A pixel's colour is taken
/// relative to the photo's own spread: lightness as its percentile rank,
/// a* and b* divided by the span between the 1st and 99th percentiles.
/// Coverage is exp(−(d/σ)^2.03) with d² = 0.5015 Δrank² + 0.2566 (Δa'² +
/// Δb'²) and σ = exp(−2.246 + 1.3685 r), r = (Refine − 1) / 99 (Lightroom
/// stores Refine 50 as 0.4949); several samples take the strongest
/// coverage. An area sample counts as five colours, one per lightness fifth
/// of the area. Mean error against Camera Raw's masks 0.106.
fn mean_feature(features: &[[f32; 3]]) -> [f32; 3] {
    let n = features.len().max(1) as f32;
    let sum = features.iter().fold([0.0; 3], |acc, f| {
        [acc[0] + f[0], acc[1] + f[1], acc[2] + f[2]]
    });
    [sum[0] / n, sum[1] / n, sum[2] / n]
}

struct ColorRangeModel {
    /// Lightness rank by L* (0–100 in 0.1 steps).
    rank: Vec<f32>,
    inv_span_a: f32,
    inv_span_b: f32,
    samples: Vec<[f32; 3]>,
    inv_sigma: f32,
}

const COLOR_RANGE_WEIGHT_RANK: f32 = 0.708_166; // sqrt(0.5015)
const COLOR_RANGE_WEIGHT_AB: f32 = 0.506_557; // sqrt(0.2566)
const COLOR_RANGE_SHAPE: f32 = 2.03;

fn srgb8_to_lab(pixel: Rgba<u8>) -> [f32; 3] {
    let lut = srgb8_linear_lut();
    linear_to_lab([
        lut[pixel[0] as usize],
        lut[pixel[1] as usize],
        lut[pixel[2] as usize],
    ])
}

impl ColorRangeModel {
    fn new(image: &DynamicImage, refine: f32) -> Self {
        let (w, h) = image.dimensions();
        let step = (((w as u64 * h as u64) as f64 / 250_000.0).sqrt().ceil() as u32).max(1);
        let mut lightness = Vec::new();
        let mut a = Vec::new();
        let mut b = Vec::new();
        for y in (0..h).step_by(step as usize) {
            for x in (0..w).step_by(step as usize) {
                let [l, aa, bb] = srgb8_to_lab(image.get_pixel(x, y));
                lightness.push(l);
                a.push(aa);
                b.push(bb);
            }
        }
        let span = |values: &mut Vec<f32>| -> f32 {
            if values.is_empty() {
                return 1.0;
            }
            values.sort_by(f32::total_cmp);
            let at = |q: f32| values[((values.len() - 1) as f32 * q).round() as usize];
            (at(0.99) - at(0.01)).max(1.0)
        };
        let (span_a, span_b) = (span(&mut a), span(&mut b));
        lightness.sort_by(f32::total_cmp);
        let n = lightness.len().max(1) as f32;
        let rank = (0..=1000)
            .map(|i| lightness.partition_point(|&l| l < i as f32 / 10.0) as f32 / n)
            .collect();
        let amount = ((refine - 1.0) / 99.0).clamp(0.0, 1.0);
        Self {
            rank,
            inv_span_a: 1.0 / span_a,
            inv_span_b: 1.0 / span_b,
            samples: Vec::new(),
            inv_sigma: (2.246 - 1.3685 * amount).exp(),
        }
    }

    fn feature(&self, pixel: Rgba<u8>) -> [f32; 3] {
        let [l, a, b] = srgb8_to_lab(pixel);
        let rank = self.rank[((l * 10.0).round() as usize).min(1000)];
        [
            rank * COLOR_RANGE_WEIGHT_RANK,
            a * self.inv_span_a * COLOR_RANGE_WEIGHT_AB,
            b * self.inv_span_b * COLOR_RANGE_WEIGHT_AB,
        ]
    }

    /// The colours a sample stands for: the mean around a point (0.3 % of
    /// the long side), or five lightness groups of an area.
    fn add_sample(&mut self, image: &DynamicImage, x: f64, y: f64, width: f64, height: f64) {
        let (w, h) = image.dimensions();
        let collect = |x0: f64, y0: f64, x1: f64, y1: f64, limit: u64| -> Vec<[f32; 3]> {
            let (x0, x1) = (x0.max(0.0) as u32, (x1.min(w as f64 - 1.0)).max(0.0) as u32);
            let (y0, y1) = (y0.max(0.0) as u32, (y1.min(h as f64 - 1.0)).max(0.0) as u32);
            let count = (x1.saturating_sub(x0) as u64 + 1) * (y1.saturating_sub(y0) as u64 + 1);
            let step = ((count as f64 / limit as f64).sqrt().ceil() as usize).max(1);
            (y0..=y1)
                .step_by(step)
                .flat_map(|yy| (x0..=x1).step_by(step).map(move |xx| (xx, yy)))
                .map(|(xx, yy)| self.feature(image.get_pixel(xx, yy)))
                .collect()
        };
        if width > 0.0 && height > 0.0 {
            let features = collect(x, y, x + width, y + height, 4096);
            if features.len() >= 5 {
                self.add_area_features(features);
                return;
            }
        }
        let r = (0.003 * w.max(h) as f64).max(1.0);
        let features = collect(x - r, y - r, x + r, y + r, 1024);
        if !features.is_empty() {
            self.samples.push(mean_feature(&features));
        }
    }

    /// `add_sample` for a sample taken on the photo shown in `geometry`
    /// (turned, flipped or straightened): the colours under its point or
    /// area as shown then, read through that geometry (an area is a turned
    /// rectangle of the source).
    fn add_turned_sample(
        &mut self,
        source: &ParametricMaskSource,
        geometry: SourceGeometry,
        sample: &ColorSample,
    ) {
        let mapping = DisplayToSource::new(source, geometry, 1.0);
        let collect = |x0: f64, y0: f64, x1: f64, y1: f64, limit: f64| -> Vec<[f32; 3]> {
            let count = ((x1 - x0 + 1.0) * (y1 - y0 + 1.0)).max(1.0);
            let step = (count / limit).sqrt().ceil().max(1.0);
            let mut features = Vec::new();
            let mut y = y0;
            while y <= y1 {
                let row = mapping.row(y as f32);
                let mut x = x0;
                while x <= x1 {
                    if let Some((sx, sy)) = mapping.pixel(x as f32, row) {
                        features.push(self.feature(source.image.get_pixel(sx, sy)));
                    }
                    x += step;
                }
                y += step;
            }
            features
        };
        let (x, y) = (sample.x, sample.y);
        let (width, height) = (sample.width.max(0.0), sample.height.max(0.0));
        if width > 0.0 && height > 0.0 {
            let features = collect(x, y, x + width, y + height, 4096.0);
            if features.len() >= 5 {
                self.add_area_features(features);
                return;
            }
        }
        let (full_w, full_h) = source.coordinate_dimensions;
        let r = (0.003 * full_w.max(full_h)).max(1.0);
        let features = collect(x - r, y - r, x + r, y + r, 1024.0);
        if !features.is_empty() {
            self.samples.push(mean_feature(&features));
        }
    }

    /// An area's colours as five lightness groups.
    fn add_area_features(&mut self, mut features: Vec<[f32; 3]>) {
        features.sort_by(|p, q| p[0].total_cmp(&q[0]));
        let chunk = features.len().div_ceil(5);
        let groups: Vec<[f32; 3]> = features.chunks(chunk).map(mean_feature).collect();
        self.samples.extend(groups);
    }

    fn coverage(&self, pixel: Rgba<u8>) -> f32 {
        let f = self.feature(pixel);
        let nearest = self
            .samples
            .iter()
            .map(|s| (f[0] - s[0]).powi(2) + (f[1] - s[1]).powi(2) + (f[2] - s[2]).powi(2))
            .fold(f32::INFINITY, f32::min);
        if !nearest.is_finite() {
            return 0.0;
        }
        (-(nearest.sqrt() * self.inv_sigma).powf(COLOR_RANGE_SHAPE)).exp()
    }
}

fn color_range_mask(
    params: &ParametricMaskParameters,
    refine: f32,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    source: &ParametricMaskSource,
) -> Option<GrayImage> {
    let image = &source.image;
    let (scale_x, scale_y) = source.sampling_scales();
    let mut model = ColorRangeModel::new(image, refine);
    let samples: Vec<ColorSample> = if params.samples.is_empty() {
        vec![ColorSample {
            x: params.target_x,
            y: params.target_y,
            width: 0.0,
            height: 0.0,
            rotation: None,
            flip_horizontal: None,
            flip_vertical: None,
            orientation_steps: None,
        }]
    } else {
        params.samples.clone()
    };
    let component = parametric_geometry(params);
    for sample in samples.iter().take(5) {
        if !(sample.x.is_finite() && sample.y.is_finite()) {
            continue;
        }
        // Samples are points and areas of the photo as it was shown when
        // they were taken; the mask follows the photo's geometry now.
        let geometry = sample.geometry(component);
        if geometry.is_identity() {
            model.add_sample(
                image,
                sample.x * scale_x,
                sample.y * scale_y,
                sample.width.max(0.0) * scale_x,
                sample.height.max(0.0) * scale_y,
            );
        } else {
            model.add_turned_sample(source, geometry, sample);
        }
    }
    if model.samples.is_empty() {
        return None;
    }
    let mut mask = pixel_mask(
        width,
        height,
        scale,
        crop_offset,
        source,
        source.geometry,
        |pixel| Some(model.coverage(pixel)),
    );
    apply_grow_and_feather(&mut mask, params.grow, params.feather, width, height);
    Some(mask)
}

fn generate_luminance_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    warped_image: Option<&ParametricMaskSource>,
) -> Option<GrayImage> {
    let params: ParametricMaskParameters = serde_json::from_value(params_value.clone()).ok()?;
    let source = warped_image?;
    let ref_pixel = parametric_reference_pixel(source, &params)?;
    let ref_luma =
        0.299 * ref_pixel[0] as f32 + 0.587 * ref_pixel[1] as f32 + 0.114 * ref_pixel[2] as f32;
    let tolerance_val = (params.tolerance * 2.55).max(1.0);
    let mut mask = luma_mask(
        width,
        height,
        scale,
        crop_offset,
        source,
        source.geometry,
        |luma| {
            let dist = (luma - ref_luma).abs();
            (dist <= tolerance_val).then(|| 1.0 - dist / tolerance_val)
        },
    );
    apply_grow_and_feather(&mut mask, params.grow, params.feather, width, height);
    Some(mask)
}

/// Capture One's Luma Range / Lightroom's Luminance Range: full coverage for
/// tones between `rangeLow` and `rangeHigh` (percent of white), fading out over
/// `falloff` percent on either side. Tones come from the photo's default
/// rendering before edits; the mask follows the photo's orientation.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct LumaRangeParameters {
    #[serde(default)]
    range_low: f32,
    #[serde(default = "default_range_high")]
    range_high: f32,
    #[serde(default = "default_luma_falloff")]
    falloff: f32,
    /// Lightroom's Luminance Range from an imported preset: zero, full,
    /// full, zero points (0–1) in the photo's luminance percentiles, which is
    /// what Camera Raw selects by (the share of the photo covered follows
    /// them on a dark still life and a landscape alike). Replaces the
    /// range above when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lr_range: Option<[f32; 4]>,
}

/// Coverage of a luminance percentile `p` in Lightroom's four-point range,
/// with smoothstep ramps.
fn lightroom_range_coverage(p: f32, [a, b, c, d]: [f32; 4]) -> f32 {
    let smooth = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    if p < a || p > d {
        0.0
    } else if p < b {
        smooth((p - a) / (b - a).max(1e-6))
    } else if p <= c {
        1.0
    } else {
        smooth((d - p) / (d - c).max(1e-6))
    }
}

/// Luminance percentile of every luma level (0–255 in quarter steps) of an
/// image, the mid-rank of the level among the pixels.
fn luma_percentiles(image: &DynamicImage) -> Vec<f32> {
    let rgb = image.to_rgb8();
    let mut histogram = vec![0u64; 1024];
    let step = ((rgb.width() as u64 * rgb.height() as u64) / 2_000_000).max(1) as usize;
    for pixel in rgb.pixels().step_by(step) {
        let luma = 0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32;
        histogram[((luma * 4.0) as usize).min(1023)] += 1;
    }
    let total = histogram.iter().sum::<u64>().max(1) as f32;
    let mut below = 0u64;
    histogram
        .iter()
        .map(|count| {
            let percentile = (below as f32 + *count as f32 / 2.0) / total;
            below += count;
            percentile
        })
        .collect()
}

fn default_range_high() -> f32 {
    100.0
}

fn default_luma_falloff() -> f32 {
    20.0
}

/// Coverage of a tone (percent of white) in a luma range.
fn luma_range_coverage(tone: f32, low: f32, high: f32, falloff: f32) -> f32 {
    let (low, high) = if low <= high {
        (low, high)
    } else {
        (high, low)
    };
    let distance = if tone < low {
        low - tone
    } else if tone > high {
        tone - high
    } else {
        return 1.0;
    };
    if falloff <= 0.0 || distance >= falloff {
        return 0.0;
    }
    let t = 1.0 - distance / falloff;
    t * t * (3.0 - 2.0 * t)
}

fn generate_luma_range_bitmap(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    warped_image: Option<&ParametricMaskSource>,
) -> Option<GrayImage> {
    let params: LumaRangeParameters = serde_json::from_value(params_value.clone()).ok()?;
    let source = warped_image?;
    let (low, high) = (
        params.range_low.clamp(0.0, 100.0),
        params.range_high.clamp(0.0, 100.0),
    );
    let falloff = params.falloff.clamp(0.0, 100.0);
    let percentiles = params.lr_range.map(|_| luma_percentiles(&source.image));
    Some(luma_mask(
        width,
        height,
        scale,
        crop_offset,
        source,
        source.geometry,
        |luma| {
            let coverage = match (params.lr_range, &percentiles) {
                (Some(range), Some(table)) => lightroom_range_coverage(
                    table[((luma * 4.0) as usize).min(1023)],
                    range.map(|v| v.clamp(0.0, 1.0)),
                ),
                _ => luma_range_coverage(luma / 2.55, low, high, falloff),
            };
            (coverage > 0.0).then_some(coverage)
        },
    ))
}

/// A mask in display geometry from the source's luma (0–255) per pixel.
fn luma_mask(
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    source: &ParametricMaskSource,
    geometry: SourceGeometry,
    intensity: impl Fn(f32) -> Option<f32>,
) -> GrayImage {
    pixel_mask(
        width,
        height,
        scale,
        crop_offset,
        source,
        geometry,
        |pixel| {
            intensity(0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32)
        },
    )
}

/// Where a pixel of the displayed photo (after orientation, flips and
/// straightening, before the crop; at `scale` of the full resolution) comes
/// from in a mask source. Shared by the parametric masks and the luma range
/// eyedropper, so a sampled tone is the one the mask compares.
struct DisplayToSource {
    cos_a: f32,
    sin_a: f32,
    center_x: f32,
    center_y: f32,
    rotated_w: f32,
    rotated_h: f32,
    inv_scale: f32,
    geometry: SourceGeometry,
    scales: (f64, f64),
    source_size: (u32, u32),
}

impl DisplayToSource {
    fn new(source: &ParametricMaskSource, geometry: SourceGeometry, scale: f32) -> Self {
        let full_w = source.coordinate_dimensions.0 as f32;
        let full_h = source.coordinate_dimensions.1 as f32;
        let angle_rad = geometry.rotation * PI / 180.0;
        let (coarse_rotated_w, coarse_rotated_h) = if geometry.orientation_steps % 2 == 1 {
            (full_h, full_w)
        } else {
            (full_w, full_h)
        };
        let rotated_w = coarse_rotated_w * scale;
        let rotated_h = coarse_rotated_h * scale;
        Self {
            cos_a: angle_rad.cos(),
            sin_a: angle_rad.sin(),
            center_x: rotated_w / 2.0,
            center_y: rotated_h / 2.0,
            rotated_w,
            rotated_h,
            inv_scale: 1.0 / scale,
            geometry,
            scales: source.sampling_scales(),
            source_size: source.image.dimensions(),
        }
    }

    /// The row terms of `pixel` for display row `y` (uncropped).
    #[inline]
    fn row(&self, y: f32) -> (f32, f32) {
        let y_centered = y - self.center_y;
        (y_centered * self.sin_a, y_centered * self.cos_a)
    }

    /// The source pixel under display pixel `x` of a row, if inside it.
    #[inline]
    fn pixel(&self, x: f32, (y_sin, y_cos): (f32, f32)) -> Option<(u32, u32)> {
        let x_centered = x - self.center_x;

        let (x_unrotated, y_unrotated) = nearest_when_straightened(
            x_centered * self.cos_a + y_sin + self.center_x,
            -x_centered * self.sin_a + y_cos + self.center_y,
            self.geometry.rotation,
        );

        // Pixel indices, as `apply_flip` and `apply_coarse_rotation` move
        // them: index i of n lands on n − 1 − i.
        let x_unflipped = if self.geometry.flip_horizontal {
            self.rotated_w - 1.0 - x_unrotated
        } else {
            x_unrotated
        };
        let y_unflipped = if self.geometry.flip_vertical {
            self.rotated_h - 1.0 - y_unrotated
        } else {
            y_unrotated
        };

        let (x_unrotated_coarse, y_unrotated_coarse) = match self.geometry.orientation_steps {
            0 => (x_unflipped, y_unflipped),
            1 => (y_unflipped, self.rotated_w - 1.0 - x_unflipped),
            2 => (
                self.rotated_w - 1.0 - x_unflipped,
                self.rotated_h - 1.0 - y_unflipped,
            ),
            3 => (self.rotated_h - 1.0 - y_unflipped, x_unflipped),
            _ => (x_unflipped, y_unflipped),
        };

        if x_unrotated_coarse >= 0.0 && y_unrotated_coarse >= 0.0 {
            let x_src = ((x_unrotated_coarse * self.inv_scale) as f64 * self.scales.0) as u32;
            let y_src = ((y_unrotated_coarse * self.inv_scale) as f64 * self.scales.1) as u32;
            (x_src < self.source_size.0 && y_src < self.source_size.1).then_some((x_src, y_src))
        } else {
            None
        }
    }
}

/// A mask from each output pixel's source pixel (after orientation, flips and
/// straightening); `intensity` returns its coverage 0–1, or none.
fn pixel_mask(
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    source: &ParametricMaskSource,
    geometry: SourceGeometry,
    intensity: impl Fn(Rgba<u8>) -> Option<f32>,
) -> GrayImage {
    let warped = &source.image;
    let mapping = DisplayToSource::new(source, geometry, scale);
    let mut mask = GrayImage::new(width, height);
    for y_out in 0..height {
        let row = mapping.row(y_out as f32 + crop_offset.1);
        for x_out in 0..width {
            if let Some((x_src, y_src)) = mapping.pixel(x_out as f32 + crop_offset.0, row)
                && let Some(value) = intensity(warped.get_pixel(x_src, y_src))
            {
                mask.put_pixel(x_out, y_out, Luma([(value * 255.0) as u8]));
            }
        }
    }
    mask
}

/// The tones under a sample on the photo, in percent of white as the luma
/// range compares them (Rec. 601 luma of the mask source: the photo's default
/// rendering before edits): the 2nd percentile, the median and the 98th
/// percentile, so a stray pixel does not widen the range.
#[derive(Serialize, Debug, Clone, Copy, PartialEq)]
pub struct LumaSample {
    pub low: f32,
    pub median: f32,
    pub high: f32,
}

/// Samples the luma range's tones for its eyedropper (Lightroom's Luminance
/// Range picker). `x`, `y`, `width` and `height` are full-resolution pixels of
/// the displayed photo (after orientation, flips and straightening, before
/// the crop), as the viewer's mask tools use them; a point (no size) takes
/// the neighbourhood the colour range takes, 0.3 % of the long side. Pixels
/// are mapped exactly as `generate_luma_range_bitmap` maps them.
pub fn sample_luma_tones(
    source: &ParametricMaskSource,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Option<LumaSample> {
    if !(x.is_finite() && y.is_finite() && width.is_finite() && height.is_finite()) {
        return None;
    }
    let mapping = DisplayToSource::new(source, source.geometry, 1.0);
    let (full_w, full_h) = source.coordinate_dimensions;
    let (x0, y0, x1, y1, limit) = if width > 0.0 && height > 0.0 {
        (x, y, x + width, y + height, 4096.0)
    } else {
        let r = (0.003 * full_w.max(full_h) as f32).max(1.0);
        (x - r, y - r, x + r, y + r, 1024.0)
    };
    let count = ((x1 - x0 + 1.0) * (y1 - y0 + 1.0)).max(1.0);
    let step = (count / limit).sqrt().ceil().max(1.0);
    let mut tones = Vec::new();
    let mut yy = y0;
    while yy <= y1 {
        let row = mapping.row(yy);
        let mut xx = x0;
        while xx <= x1 {
            if let Some((sx, sy)) = mapping.pixel(xx, row) {
                let pixel = source.image.get_pixel(sx, sy);
                let luma =
                    0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32;
                tones.push(luma / 2.55);
            }
            xx += step;
        }
        yy += step;
    }
    if tones.is_empty() {
        return None;
    }
    tones.sort_by(f32::total_cmp);
    let at = |q: f32| tones[((tones.len() - 1) as f32 * q).round() as usize];
    Some(LumaSample {
        low: at(0.02),
        median: at(0.5),
        high: at(0.98),
    })
}

fn generate_all_bitmap(width: u32, height: u32) -> GrayImage {
    GrayImage::from_pixel(width, height, Luma([255]))
}

fn generate_sub_mask_bitmap(
    sub_mask: &SubMask,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> Option<GrayImage> {
    if !sub_mask.visible {
        return None;
    }
    // Drawn masks where the photo shows the spot they were drawn on.
    let drawn = |kind: &str| shape_as_shown(kind, &sub_mask.parameters, geometry.shown(), scale);

    match sub_mask.mask_type.as_str() {
        "radial" => Some(generate_radial_bitmap(
            &drawn("radial"),
            width,
            height,
            scale,
            crop_offset,
        )),
        "linear" => Some(generate_linear_bitmap(
            &drawn("linear"),
            width,
            height,
            scale,
            crop_offset,
        )),
        "brush" | "clone" | "heal" | "liquify" | "retouch" => Some(generate_brush_bitmap(
            &sub_mask.parameters,
            width,
            height,
            scale,
            crop_offset,
            geometry.shown(),
            warped_image,
        )),
        "flow" => Some(generate_flow_bitmap(
            &sub_mask.parameters,
            width,
            height,
            scale,
            crop_offset,
            geometry.shown(),
        )),
        "color" => generate_color_bitmap(
            &sub_mask.parameters,
            width,
            height,
            scale,
            crop_offset,
            warped_image,
        ),
        "luma-range" => generate_luma_range_bitmap(
            &sub_mask.parameters,
            width,
            height,
            scale,
            crop_offset,
            warped_image,
        ),
        "luminance" => generate_luminance_bitmap(
            &sub_mask.parameters,
            width,
            height,
            scale,
            crop_offset,
            warped_image,
        ),
        "ai-subject" => generate_ai_subject_bitmap(
            &sub_mask.parameters,
            width,
            height,
            scale,
            crop_offset,
            geometry,
        ),
        "ai-foreground" => generate_ai_foreground_bitmap(
            &sub_mask.parameters,
            width,
            height,
            scale,
            crop_offset,
            geometry,
        ),
        // A People mask is a stored bitmap like the sky's.
        "ai-sky" | "ai-people" => generate_ai_sky_bitmap(
            &sub_mask.parameters,
            width,
            height,
            scale,
            crop_offset,
            geometry,
        ),
        "ai-depth" => generate_ai_depth_bitmap(
            &sub_mask.parameters,
            width,
            height,
            scale,
            crop_offset,
            geometry,
        ),
        "quick-eraser" => generate_quick_erase_bitmap(
            &sub_mask.parameters,
            width,
            height,
            scale,
            crop_offset,
            geometry,
        ),
        "all" => Some(generate_all_bitmap(width, height)),
        _ => None,
    }
}

fn invert_mask(mask: &mut GrayImage) {
    mask.as_mut()
        .iter_mut()
        .for_each(|value| *value = 255 - *value);
}

/// Same per-value arithmetic as before, evaluated once per byte value.
fn scale_mask_opacity(mask: &mut GrayImage, opacity: f32) {
    let multiplier = (opacity / 100.0).clamp(0.0, 1.0);
    if multiplier >= 1.0 {
        return;
    }
    let table: [u8; 256] = std::array::from_fn(|value| (value as f32 * multiplier) as u8);
    mask.as_mut()
        .par_chunks_mut(MASK_PASS_CHUNK)
        .for_each(|chunk| {
            chunk
                .iter_mut()
                .for_each(|value| *value = table[*value as usize])
        });
}

/// Bytes per parallel task of whole-mask passes (a few rows of a preview).
const MASK_PASS_CHUNK: usize = 64 * 1024;

/// A layer's mask: 8-bit coverage (overlays, thumbnails, and the develop
/// pass of 8-bit layers) and, for `maskVersion` 2 layers whose coverage is
/// finer than 8 bits, the 16-bit coverage the develop pass uses instead.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskBitmap {
    bytes: GrayImage,
    wide: Option<Vec<u16>>,
}

impl MaskBitmap {
    /// A 16-bit mask from coverage in 0–1 (row-major, `width` × `height`).
    pub fn from_coverage(width: u32, height: u32, coverage: &[f32]) -> Self {
        debug_assert_eq!(coverage.len(), width as usize * height as usize);
        let bytes = coverage
            .par_iter()
            .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect();
        let wide = coverage
            .par_iter()
            .map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
            .collect();
        Self {
            bytes: GrayImage::from_raw(width, height, bytes).unwrap(),
            wide: Some(wide),
        }
    }

    /// The 16-bit coverage (row-major) of a 16-bit mask.
    pub fn wide(&self) -> Option<&[u16]> {
        self.wide.as_deref()
    }

    pub fn into_bytes(self) -> GrayImage {
        self.bytes
    }

    /// The 8-bit pixels to edit in place; a 16-bit mask becomes an 8-bit one.
    pub fn bytes_mut(&mut self) -> &mut GrayImage {
        self.wide = None;
        &mut self.bytes
    }

    /// The part `x`, `y`, `width` × `height` of the mask.
    pub fn crop(&self, x: u32, y: u32, width: u32, height: u32) -> Self {
        let bytes = image::imageops::crop_imm(&self.bytes, x, y, width, height).to_image();
        let wide = self.wide.as_ref().map(|wide| {
            let stride = self.bytes.width() as usize;
            (y..y + height)
                .flat_map(|row| {
                    let start = row as usize * stride + x as usize;
                    wide[start..start + width as usize].iter().copied()
                })
                .collect()
        });
        Self { bytes, wide }
    }

    /// The full-opacity mask scaled by a layer opacity, as the layer's own
    /// mask is made: `scale_mask_opacity` on 8-bit layers, in floating point
    /// on `maskVersion` 2 layers (`precise`).
    fn with_opacity(&self, opacity: f32, precise: bool) -> Self {
        let multiplier = (opacity / 100.0).clamp(0.0, 1.0);
        match &self.wide {
            None if !precise => {
                let mut bytes = self.bytes.clone();
                scale_mask_opacity(&mut bytes, opacity);
                Self { bytes, wide: None }
            }
            None => {
                // `from_coverage` of each byte value, looked up: a brush layer
                // below full opacity costs no more per stroke than before.
                let coverage = |value: usize| value as f32 / 255.0 * multiplier;
                let bytes: [u8; 256] =
                    std::array::from_fn(|v| (coverage(v).clamp(0.0, 1.0) * 255.0).round() as u8);
                let wide: [u16; 256] =
                    std::array::from_fn(|v| (coverage(v).clamp(0.0, 1.0) * 65535.0).round() as u16);
                let raw = self.bytes.as_raw();
                Self {
                    bytes: GrayImage::from_raw(
                        self.bytes.width(),
                        self.bytes.height(),
                        raw.par_iter().map(|v| bytes[*v as usize]).collect(),
                    )
                    .unwrap(),
                    wide: Some(raw.par_iter().map(|v| wide[*v as usize]).collect()),
                }
            }
            Some(wide) => {
                let coverage: Vec<f32> = wide
                    .par_iter()
                    .map(|v| *v as f32 / 65535.0 * multiplier)
                    .collect();
                Self::from_coverage(self.bytes.width(), self.bytes.height(), &coverage)
            }
        }
    }

    /// Bytes the mask holds.
    pub fn byte_len(&self) -> usize {
        self.bytes.as_raw().len() + self.wide.as_ref().map_or(0, |wide| wide.len() * 2)
    }
}

impl From<GrayImage> for MaskBitmap {
    fn from(bytes: GrayImage) -> Self {
        Self { bytes, wide: None }
    }
}

impl std::ops::Deref for MaskBitmap {
    type Target = GrayImage;

    fn deref(&self) -> &GrayImage {
        &self.bytes
    }
}

/// A layer's 8-bit mask (`generate_layer_mask`'s 8-bit coverage).
pub fn generate_mask_bitmap(
    mask_def: &MaskDefinition,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> Option<GrayImage> {
    generate_layer_mask(
        mask_def,
        width,
        height,
        scale,
        crop_offset,
        geometry,
        warped_image,
    )
    .map(MaskBitmap::into_bytes)
}

/// Whether `generate_layer_mask` gives this layer a 16-bit mask, known
/// before generating it.
pub fn layer_mask_is_wide(mask_def: &MaskDefinition) -> bool {
    mask_def.produces_bitmap() && mask_def.precise() && precise::needs_wide(mask_def)
}

/// A layer's mask. Layers before `maskVersion` 2, and later layers whose
/// every step is exact at 8 bits (see `precise::needs_wide`), keep the 8-bit
/// mask; other layers of version 2 and up are computed in floating point.
pub fn generate_layer_mask(
    mask_def: &MaskDefinition,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> Option<MaskBitmap> {
    if !mask_def.produces_bitmap() {
        return None;
    }
    if geometry.decoded
        && let Some(size) = geometry.size
        && let Some(placed) = crate::lightroom_masks::placed_layer(mask_def, size)
    {
        return generate_layer_mask(
            &placed,
            width,
            height,
            scale,
            crop_offset,
            geometry,
            warped_image,
        );
    }
    if mask_def.precise() && precise::needs_wide(mask_def) {
        return Some(precise::layer_mask(
            mask_def,
            width,
            height,
            scale,
            crop_offset,
            geometry,
            warped_image,
        ));
    }
    Some(MaskBitmap::from(eight_bit_mask(
        mask_def,
        mask_def.opacity,
        width,
        height,
        scale,
        crop_offset,
        geometry,
        warped_image,
    )))
}

/// The 8-bit mask of a layer that produces one, with the layer at `opacity`.
#[allow(clippy::too_many_arguments)]
fn eight_bit_mask(
    mask_def: &MaskDefinition,
    opacity: f32,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> GrayImage {
    // Starts empty: an additive first sub-mask is the result itself, while
    // subtracting from or intersecting with zero coverage stays zero.
    let mut final_mask: Option<GrayImage> = None;

    for sub_mask in &mask_def.sub_masks {
        if let Some(mut sub_bitmap) = generate_sub_mask_bitmap(
            sub_mask,
            width,
            height,
            scale,
            crop_offset,
            geometry,
            warped_image,
        ) {
            if sub_mask.invert {
                invert_mask(&mut sub_bitmap);
            }
            scale_mask_opacity(&mut sub_bitmap, sub_mask.opacity);

            let Some(target) = final_mask.as_mut() else {
                final_mask = Some(match sub_mask.mode {
                    SubMaskMode::Additive => sub_bitmap,
                    SubMaskMode::Subtractive | SubMaskMode::Intersect => {
                        GrayImage::new(width, height)
                    }
                });
                continue;
            };
            let pixels = target.as_mut().iter_mut().zip(sub_bitmap.as_raw());
            match sub_mask.mode {
                SubMaskMode::Additive => {
                    pixels.for_each(|(pixel, sub)| *pixel = (*pixel).max(*sub))
                }
                SubMaskMode::Subtractive => {
                    pixels.for_each(|(pixel, sub)| *pixel = pixel.saturating_sub(*sub))
                }
                SubMaskMode::Intersect => {
                    pixels.for_each(|(pixel, sub)| *pixel = (*pixel).min(*sub))
                }
            }
        }
    }
    let mut final_mask = final_mask.unwrap_or_else(|| GrayImage::new(width, height));

    if mask_def.invert {
        invert_mask(&mut final_mask);
    }
    scale_mask_opacity(&mut final_mask, opacity);

    final_mask
}

/// How the overlay draws the mask: Capture One's Mask Color (a colour with an
/// opacity, red at 50% by default) or its opaque grayscale view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayStyle {
    Colored { rgb: [u8; 3], opacity: u8 },
    Grayscale,
}

impl OverlayStyle {
    pub const DEFAULT_RGB: [u8; 3] = [255, 0, 0];
    pub const DEFAULT_OPACITY: u8 = 50;
}

// The default overlay: red at 50%.
#[cfg(test)]
pub fn encode_mask_overlay(mask: &GrayImage) -> Result<Vec<u8>, String> {
    encode_mask_png(
        mask,
        OverlayStyle::Colored {
            rgb: OverlayStyle::DEFAULT_RGB,
            opacity: OverlayStyle::DEFAULT_OPACITY,
        },
    )
}

/// The coloured overlay, or (Capture One's grayscale view) an opaque image
/// with the mask in white and unmasked pixels in black. PNG stores the
/// existing one-byte mask directly, without a four-byte RGBA expansion: every
/// palette entry is the overlay colour and coverage selects its alpha.
pub fn encode_mask_png(mask: &GrayImage, style: OverlayStyle) -> Result<Vec<u8>, String> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, mask.width(), mask.height());
        encoder.set_depth(png::BitDepth::Eight);
        match style {
            OverlayStyle::Grayscale => encoder.set_color(png::ColorType::Grayscale),
            OverlayStyle::Colored { rgb, opacity } => {
                let opacity = u32::from(opacity.min(100));
                encoder.set_color(png::ColorType::Indexed);
                encoder.set_palette((0..256).flat_map(|_| rgb).collect::<Vec<u8>>());
                encoder.set_trns(
                    (0..256u32)
                        .map(|coverage| (coverage * opacity / 100) as u8)
                        .collect::<Vec<u8>>(),
                );
            }
        }
        encoder.set_compression(png::Compression::Fast);
        encoder.set_filter(png::Filter::NoFilter);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer
            .write_image_data(mask.as_raw())
            .map_err(|e| e.to_string())?;
    }
    Ok(encoded)
}

#[cfg(test)]
mod quick_erase_tests {
    use super::*;

    fn object_mask(width: u32, height: u32) -> GrayImage {
        // A soft blob and a thin, faint line (a lattice strut).
        GrayImage::from_fn(width, height, |x, y| {
            let d = ((x as f32 - 60.0).powi(2) + (y as f32 - 50.0).powi(2)).sqrt();
            let blob = (255.0 * (1.0 - (d - 12.0) / 4.0).clamp(0.0, 1.0)) as u8;
            let line = if y == 80 && (20..140).contains(&x) {
                90
            } else {
                0
            };
            Luma([blob.max(line)])
        })
    }

    #[test]
    fn grows_and_feathers_within_bounds_as_on_the_whole_frame() {
        let (width, height) = (400, 300);
        for (grow, feather) in [(50.0, 50.0), (30.0, 0.0), (0.0, 40.0), (100.0, 100.0)] {
            let mut whole = object_mask(width, height);
            apply_grow_and_feather(&mut whole, grow, feather, width, height);
            let mut bounded = object_mask(width, height);
            grow_and_feather_within_bounds(&mut bounded, grow, feather, width, height);
            assert_eq!(
                whole.as_raw(),
                bounded.as_raw(),
                "grow {grow} feather {feather}"
            );
        }
        // An object at the frame's corner.
        let corner = || {
            GrayImage::from_fn(width, height, |x, y| {
                Luma([if x < 5 && y < 5 { 255 } else { 0 }])
            })
        };
        let mut whole = corner();
        apply_grow_and_feather(&mut whole, 50.0, 50.0, width, height);
        let mut bounded = corner();
        grow_and_feather_within_bounds(&mut bounded, 50.0, 50.0, width, height);
        assert_eq!(whole.as_raw(), bounded.as_raw());
        // No coverage stays empty.
        let mut empty = GrayImage::new(width, height);
        grow_and_feather_within_bounds(&mut empty, 50.0, 50.0, width, height);
        assert!(empty.as_raw().iter().all(|&v| v == 0));
    }

    #[test]
    fn removes_thin_and_soft_parts_completely() {
        let (width, height) = (200, 120);
        let mut png = Cursor::new(Vec::new());
        object_mask(width, height)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let data = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png.into_inner())
        );
        let params = serde_json::json!({
            "startX": 0.0, "startY": 0.0, "endX": 200.0, "endY": 120.0,
            "maskDataBase64": data, "grow": 0.0, "feather": 0.0,
        });
        let mask = generate_quick_erase_bitmap(
            &params,
            width,
            height,
            1.0,
            (0.0, 0.0),
            &PhotoGeometry::default(),
        )
        .unwrap();
        // The faint strut (90 of 255) is removed whole, not at 35 %.
        assert_eq!(mask.get_pixel(100, 80)[0], 255);
        assert_eq!(mask.get_pixel(60, 50)[0], 255);
        assert_eq!(mask.get_pixel(100, 20)[0], 0);
        // The object mask itself keeps its soft coverage.
        let subject = generate_ai_subject_bitmap(
            &params,
            width,
            height,
            1.0,
            (0.0, 0.0),
            &PhotoGeometry::default(),
        )
        .unwrap();
        assert_eq!(subject.get_pixel(100, 80)[0], 90);
    }
}

#[cfg(test)]
mod linear_tests {
    use super::*;

    // The linear mask before `rangeEnd` existed.
    fn former_linear(
        params: &Value,
        width: u32,
        height: u32,
        scale: f32,
        crop: (f32, f32),
    ) -> GrayImage {
        let start_x = params["startX"].as_f64().unwrap() as f32 * scale - crop.0;
        let start_y = params["startY"].as_f64().unwrap() as f32 * scale - crop.1;
        let end_x = params["endX"].as_f64().unwrap() as f32 * scale - crop.0;
        let end_y = params["endY"].as_f64().unwrap() as f32 * scale - crop.1;
        let range = params["range"].as_f64().unwrap_or(50.0) as f32 * scale;
        let (vx, vy) = (end_x - start_x, end_y - start_y);
        let len = (vx * vx + vy * vy).sqrt();
        let (px, py) = (-vy / len, vx / len);
        GrayImage::from_fn(width, height, |x, y| {
            let d = (x as f32 - start_x) * px + (y as f32 - start_y) * py;
            Luma([((0.5 - d / range.max(0.01) * 0.5).clamp(0.0, 1.0) * 255.0) as u8])
        })
    }

    #[test]
    fn symmetric_linear_masks_are_unchanged() {
        for (params, scale, crop) in [
            (
                serde_json::json!({"startX": 10.0, "startY": 40.0, "endX": 90.0, "endY": 25.0, "range": 18.0}),
                1.0,
                (0.0, 0.0),
            ),
            (
                serde_json::json!({"startX": -5.0, "startY": 3.0, "endX": 7.0, "endY": 60.0, "range": 0.0}),
                0.5,
                (2.0, 1.0),
            ),
            (
                serde_json::json!({"startX": 50.0, "startY": 0.0, "endX": 50.0, "endY": 80.0}),
                1.5,
                (10.0, 4.0),
            ),
        ] {
            assert_eq!(
                generate_linear_bitmap(&params, 97, 61, scale, crop).as_raw(),
                former_linear(&params, 97, 61, scale, crop).as_raw()
            );
        }
    }

    #[test]
    fn range_end_sets_the_transition_beyond_the_central_line() {
        // Horizontal central line at y = 50; the start (100%) side is above it.
        let params = serde_json::json!({"startX": 0.0, "startY": 50.0, "endX": 10.0, "endY": 50.0,
            "range": 10.0, "rangeEnd": 30.0});
        let mask = generate_linear_bitmap(&params, 4, 101, 1.0, (0.0, 0.0));
        let at = |y: u32| mask.get_pixel(1, y)[0];
        assert_eq!(at(39), 255); // past the start line
        assert_eq!(at(45), 191); // 100% → 50% over 10 px
        assert_eq!(at(50), 127);
        assert_eq!(at(65), 63); // 50% → 0% over 30 px
        assert_eq!(at(80), 0);
        assert_eq!(at(81), 0);
    }
}

#[cfg(test)]
mod depth_range_tests {
    use super::*;

    #[test]
    fn a_depth_range_covers_its_whole_range() {
        // Depth rising from 0 (far, left) to 255 (near, right).
        let depth = GrayImage::from_fn(256, 4, |x, _| Luma([x as u8]));
        let mut png = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageLuma8(depth)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let params = serde_json::json!({
            "maskDataBase64": format!("data:image/png;base64,{}", general_purpose::STANDARD.encode(png.into_inner())),
            "minDepth": 20.0, "maxDepth": 60.0, "minFade": 0.0, "maxFade": 0.0, "feather": 0.0,
        });
        let mask =
            generate_ai_depth_bitmap(&params, 256, 4, 1.0, (0.0, 0.0), &PhotoGeometry::default())
                .unwrap();
        let at = |x: u32| mask.get_pixel(x, 1)[0];
        // Inside the range fully selected, whatever the depth.
        assert_eq!(at(60), 255);
        assert_eq!(at(140), 255);
        // Outside nothing.
        assert_eq!(at(30), 0);
        assert_eq!(at(200), 0);
    }
}

#[cfg(test)]
mod people_mask_tests {
    use super::*;

    #[test]
    fn a_people_mask_renders_its_stored_bitmap() {
        let stored = GrayImage::from_fn(8, 4, |x, _| Luma([if x < 4 { 255 } else { 0 }]));
        let mut png = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageLuma8(stored)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let sub_mask: SubMask = serde_json::from_value(serde_json::json!({
            "id": "p", "type": "ai-people", "visible": true, "mode": "additive",
            "parameters": {
                "parts": ["faceSkin", "lips"], "person": 0,
                "maskDataBase64": format!("data:image/png;base64,{}", general_purpose::STANDARD.encode(png.into_inner())),
            },
        }))
        .unwrap();
        let mask = generate_sub_mask_bitmap(
            &sub_mask,
            8,
            4,
            1.0,
            (0.0, 0.0),
            &PhotoGeometry::default(),
            None,
        )
        .unwrap();
        assert_eq!(mask.get_pixel(1, 1)[0], 255);
        assert_eq!(mask.get_pixel(6, 1)[0], 0);
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    #[test]
    fn palette_overlay_preserves_every_coverage_and_alpha() {
        let mask = GrayImage::from_fn(257, 3, |x, y| Luma([((x + y) % 256) as u8]));
        let decoded = image::load_from_memory(&encode_mask_overlay(&mask).unwrap())
            .unwrap()
            .into_rgba8();
        assert_eq!(decoded.dimensions(), mask.dimensions());
        for (coverage, pixel) in mask.as_raw().iter().zip(decoded.pixels()) {
            assert_eq!(pixel.0, [255, 0, 0, (f32::from(*coverage) * 0.5) as u8]);
        }
    }

    #[test]
    fn grayscale_overlay_is_the_opaque_mask() {
        let mask = GrayImage::from_fn(257, 3, |x, y| Luma([((x * 3 + y) % 256) as u8]));
        let decoded =
            image::load_from_memory(&encode_mask_png(&mask, OverlayStyle::Grayscale).unwrap())
                .unwrap()
                .into_rgba8();
        assert_eq!(decoded.dimensions(), mask.dimensions());
        for (coverage, pixel) in mask.as_raw().iter().zip(decoded.pixels()) {
            assert_eq!(pixel.0, [*coverage, *coverage, *coverage, 255]);
        }
    }

    #[test]
    fn colored_overlay_uses_the_chosen_colour_and_opacity() {
        let mask = GrayImage::from_fn(256, 2, |x, _| Luma([x as u8]));
        for (rgb, opacity) in [([0, 255, 0], 100), ([255, 255, 255], 25), ([0, 0, 0], 0)] {
            let style = OverlayStyle::Colored { rgb, opacity };
            let decoded = image::load_from_memory(&encode_mask_png(&mask, style).unwrap())
                .unwrap()
                .into_rgba8();
            for (coverage, pixel) in mask.as_raw().iter().zip(decoded.pixels()) {
                let alpha = (u32::from(*coverage) * u32::from(opacity) / 100) as u8;
                assert_eq!(pixel.0, [rgb[0], rgb[1], rgb[2], alpha]);
            }
        }
        // Out-of-range opacity is clamped rather than wrapping alpha.
        let over = OverlayStyle::Colored {
            rgb: [255, 0, 0],
            opacity: 200,
        };
        let full = OverlayStyle::Colored {
            rgb: [255, 0, 0],
            opacity: 100,
        };
        assert_eq!(encode_mask_png(&mask, over), encode_mask_png(&mask, full));
    }

    #[test]
    #[ignore = "native CPU timing probe for full-resolution mask overlay encoding"]
    fn overlay_encoding_timing_probe() {
        for (width, height) in [(1536, 1152), (2304, 1728)] {
            let mask = GrayImage::from_fn(width, height, |x, y| {
                let radius = ((x as f32 - width as f32 / 2.0).powi(2)
                    + (y as f32 - height as f32 / 2.0).powi(2))
                .sqrt();
                Luma([((1.0 - radius / 450.0).clamp(0.0, 1.0) * 255.0) as u8])
            });
            for frame in 0..14 {
                for palette in if frame % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                } {
                    let started = std::time::Instant::now();
                    let bytes = if palette {
                        encode_mask_overlay(&mask).unwrap()
                    } else {
                        let mut rgba = image::RgbaImage::new(width, height);
                        for (x, y, value) in mask.enumerate_pixels() {
                            rgba.put_pixel(
                                x,
                                y,
                                image::Rgba([255, 0, 0, (value[0] as f32 * 0.5) as u8]),
                            );
                        }
                        let mut output = Cursor::new(Vec::new());
                        rgba.write_to(&mut output, image::ImageFormat::Png).unwrap();
                        // Include the former native Base64 response construction.
                        general_purpose::STANDARD
                            .encode(output.into_inner())
                            .into_bytes()
                    };
                    if frame >= 2 {
                        println!(
                            "{}",
                            serde_json::json!({"stage":"mask_overlay_encode","palette":palette,
                            "width":width,"height":height,"frame":frame - 2,
                            "bytes":bytes.len(),"ms":started.elapsed().as_secs_f64()*1000.0})
                        );
                    }
                }
            }
        }
    }
}

const MASK_CACHE_BYTE_BUDGET: usize = 128 * 1024 * 1024;
const MASK_CACHE_ENTRY_LIMIT: usize = 64;

struct MaskBitmapKey {
    bitmap_hash: u64,
    // A Weak pins the allocation's identity without retaining its image pixels.
    // Keeping only the address would let an unrelated later allocation alias it.
    source: Option<Weak<DynamicImage>>,
}

impl PartialEq for MaskBitmapKey {
    fn eq(&self, other: &Self) -> bool {
        self.bitmap_hash == other.bitmap_hash
            && match (&self.source, &other.source) {
                (Some(left), Some(right)) => Weak::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for MaskBitmapKey {}

struct MaskBitmapEntry {
    key: MaskBitmapKey,
    image: Arc<MaskBitmap>,
}

/// Keeps immutable mask pixels shared between preview jobs and bounded in memory.
pub struct MaskBitmapCache {
    entries: VecDeque<MaskBitmapEntry>,
    retained_bytes: usize,
    byte_budget: usize,
    entry_limit: usize,
}

impl Default for MaskBitmapCache {
    fn default() -> Self {
        Self::new()
    }
}

impl MaskBitmapCache {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            retained_bytes: 0,
            byte_budget: MASK_CACHE_BYTE_BUDGET,
            entry_limit: MASK_CACHE_ENTRY_LIMIT,
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.retained_bytes = 0;
        clear_brush_prefixes();
    }

    fn get(&mut self, key: &MaskBitmapKey) -> Option<Arc<MaskBitmap>> {
        let index = self.entries.iter().position(|entry| entry.key == *key)?;
        let entry = self.entries.remove(index)?;
        let image = Arc::clone(&entry.image);
        self.entries.push_back(entry);
        Some(image)
    }

    fn insert(&mut self, key: MaskBitmapKey, image: Arc<MaskBitmap>) {
        let bytes = image.byte_len();
        if self.entry_limit == 0 || self.byte_budget == 0 || bytes > self.byte_budget {
            return;
        }

        if let Some(index) = self.entries.iter().position(|entry| entry.key == key) {
            let old = self.entries.remove(index).unwrap();
            self.retained_bytes -= old.image.byte_len();
        }

        // Least recently used first, but never a mask someone else still
        // holds (the GPU's mask array, the frame being prepared): dropping
        // it frees nothing, and a stack of layers larger than the budget
        // would then miss on every layer of every frame (notes: WP6 of
        // plans/local-adjustments-g29-g31.md). Held masks may exceed the
        // budget until they are released.
        while self.entries.len() >= self.entry_limit
            || self.retained_bytes + bytes > self.byte_budget
        {
            let Some(index) = self
                .entries
                .iter()
                .position(|entry| Arc::strong_count(&entry.image) == 1)
            else {
                break;
            };
            let old = self.entries.remove(index).unwrap();
            self.retained_bytes -= old.image.byte_len();
        }

        self.retained_bytes += bytes;
        self.entries.push_back(MaskBitmapEntry { key, image });
    }
}

/// Serialize borrowed fields directly into the hasher, without copying raster
/// payloads or serializing local color adjustments that cannot change the mask.
fn mask_bitmap_key(
    def: &MaskDefinition,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> MaskBitmapKey {
    #[derive(Serialize)]
    struct BitmapDefinition<'a> {
        visible: bool,
        invert: bool,
        opacity: f32,
        sub_masks: Vec<SubMaskFields<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        mask_version: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        refine_edge: Option<f32>,
    }

    /// A component without its parameters, which are hashed as values: a
    /// brush layer's thousands of points written as JSON text were most of
    /// a many-layer preview's mask stage.
    #[derive(Serialize)]
    struct SubMaskFields<'a> {
        id: &'a str,
        mask_type: &'a str,
        visible: bool,
        invert: bool,
        opacity: f32,
        mode: SubMaskMode,
    }

    struct HashWriter<'a>(&'a mut DefaultHasher);

    impl Write for HashWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.write(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut hasher = DefaultHasher::new();
    serde_json::to_writer(
        HashWriter(&mut hasher),
        &BitmapDefinition {
            visible: def.visible,
            invert: def.invert,
            opacity: def.opacity,
            sub_masks: def
                .sub_masks
                .iter()
                .map(|sub| SubMaskFields {
                    id: &sub.id,
                    mask_type: &sub.mask_type,
                    visible: sub.visible,
                    invert: sub.invert,
                    opacity: sub.opacity,
                    mode: sub.mode,
                })
                .collect(),
            mask_version: def.mask_version,
            refine_edge: def.refine_edge,
        },
    )
    .expect("writing mask fields into an infallible hasher cannot fail");
    for sub in &def.sub_masks {
        crate::mask_parameters::content_hash(&sub.parameters).hash(&mut hasher);
    }

    width.hash(&mut hasher);
    height.hash(&mut hasher);
    scale.to_bits().hash(&mut hasher);
    crop_offset.0.to_bits().hash(&mut hasher);
    crop_offset.1.to_bits().hash(&mut hasher);
    geometry.hash_into(&mut hasher);

    let source = if def.requires_warped_image() {
        warped_image.map(|source| {
            source.coordinate_dimensions.0.to_bits().hash(&mut hasher);
            source.coordinate_dimensions.1.to_bits().hash(&mut hasher);
            let geometry = source.geometry;
            geometry.rotation.to_bits().hash(&mut hasher);
            geometry.flip_horizontal.hash(&mut hasher);
            geometry.flip_vertical.hash(&mut hasher);
            geometry.orientation_steps.hash(&mut hasher);
            Arc::downgrade(&source.image)
        })
    } else {
        None
    };

    MaskBitmapKey {
        bitmap_hash: hasher.finish(),
        source,
    }
}

#[cfg(test)]
thread_local! {
    /// Cache misses that rasterized, per test thread.
    static RASTERIZED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[allow(clippy::too_many_arguments)]
pub fn cached_or_generated_mask(
    cache: &std::sync::Mutex<MaskBitmapCache>,
    def: &MaskDefinition,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> Result<Option<Arc<MaskBitmap>>, String> {
    validate_mask_source(def, warped_image)?;
    // A hidden or empty layer has no mask; its key would hash every stroke.
    if !def.produces_bitmap() {
        return Ok(None);
    }
    let key = mask_bitmap_key(
        def,
        width,
        height,
        scale,
        crop_offset,
        geometry,
        warped_image,
    );

    if let Some(img) = cache.lock().unwrap().get(&key) {
        return Ok(Some(img));
    }

    // The layer opacity is applied last, so a layer below full opacity is its
    // full-opacity mask scaled. The mask overlay keeps that one cached for the
    // edited layer; derive from it instead of rasterizing the shapes again.
    let full_opacity = (def.opacity < 100.0 && def.produces_bitmap())
        .then(|| {
            let full = MaskDefinition {
                opacity: 100.0,
                ..def.clone()
            };
            let key = mask_bitmap_key(
                &full,
                width,
                height,
                scale,
                crop_offset,
                geometry,
                warped_image,
            );
            cache.lock().unwrap().get(&key)
        })
        .flatten();
    let resolved;
    let generated = match full_opacity {
        Some(full) => Some(Arc::new(full.with_opacity(def.opacity, def.precise()))),
        None => generate_layer_mask(
            match with_resolved_parameters(def) {
                Some(def) => {
                    resolved = def;
                    &resolved
                }
                None => def,
            },
            width,
            height,
            scale,
            crop_offset,
            geometry,
            warped_image,
        )
        .map(Arc::new),
    };

    if let Some(img) = &generated {
        cache.lock().unwrap().insert(key, Arc::clone(img));
    }

    Ok(generated)
}

/// `def` with the brush strokes its references stand for
/// (`mask_parameters`), if it has any.
fn with_resolved_parameters(def: &MaskDefinition) -> Option<MaskDefinition> {
    let resolved: Vec<_> = def
        .sub_masks
        .iter()
        .map(|sub| crate::mask_parameters::resolve(&sub.parameters))
        .collect();
    resolved
        .iter()
        .any(Option::is_some)
        .then(|| MaskDefinition {
            sub_masks: def
                .sub_masks
                .iter()
                .zip(resolved)
                .map(|(sub, parameters)| SubMask {
                    parameters: parameters.map_or_else(|| sub.parameters.clone(), |p| (*p).clone()),
                    ..sub.clone()
                })
                .collect(),
            ..def.clone()
        })
}

/// The mask `cached_or_generated_mask` keeps for these arguments, without
/// rasterizing it on a miss (the layer list's thumbnails reuse the preview's).
#[allow(clippy::too_many_arguments)]
pub fn peek_cached_mask(
    cache: &std::sync::Mutex<MaskBitmapCache>,
    def: &MaskDefinition,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> Option<Arc<MaskBitmap>> {
    let key = mask_bitmap_key(
        def,
        width,
        height,
        scale,
        crop_offset,
        geometry,
        warped_image,
    );
    cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key)
}

fn validate_mask_source(
    def: &MaskDefinition,
    warped_image: Option<&ParametricMaskSource>,
) -> Result<(), String> {
    if def.produces_bitmap() && def.requires_warped_image() && warped_image.is_none() {
        return Err("A source image is required for color and luminance masks".into());
    }
    if let Some(source) = warped_image {
        validate_coordinate_dimensions(source.coordinate_dimensions)?;
    }
    Ok(())
}

#[cfg(test)]
mod mask_bitmap_cache_tests {
    use super::*;
    use serde_json::json;

    fn stroke(tool: &str, size: f64, y: f64, feather: f64) -> Value {
        json!({"tool":tool,"brushSize":size,"feather":feather,
            "points":(0..12).map(|i| json!({"x": 40.0 + i as f64 * 37.5, "y": y + (i % 3) as f64 * 9.0})).collect::<Vec<_>>()})
    }

    fn brush(lines: &[Value], width: u32, height: u32, scale: f32) -> GrayImage {
        generate_brush_bitmap(
            &json!({"lines": lines}),
            width,
            height,
            scale,
            (3.5, -2.0),
            SourceGeometry::default().into(),
            None,
        )
    }

    #[test]
    #[ignore = "timing probe; set RAWTONIC_PREVIEW_RECIPE to an editor adjustments JSON"]
    fn preview_mask_timing_probe() {
        let Ok(path) = std::env::var("RAWTONIC_PREVIEW_RECIPE") else {
            println!("SKIP preview_mask_timing_probe: RAWTONIC_PREVIEW_RECIPE is not set");
            return;
        };
        let recipe: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let (width, height, scale, offset) = (2304, 1728, 0.5714286, (0.0, 0.0));
        let time = |label: &str, f: &mut dyn FnMut()| {
            let mut samples: Vec<f64> = (0..12)
                .map(|_| {
                    let start = std::time::Instant::now();
                    f();
                    start.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            samples.sort_by(f64::total_cmp);
            println!(
                "{}",
                json!({"probe":"preview_masks","stage":label,"median_ms":samples[6],"min_ms":samples[0],"max_ms":samples[11]})
            );
        };
        let defs: Vec<MaskDefinition> = serde_json::from_value(recipe["masks"].clone()).unwrap();
        time("parse_definitions", &mut || {
            let _: Vec<MaskDefinition> = serde_json::from_value(recipe["masks"].clone()).unwrap();
        });
        time("bitmap_keys", &mut || {
            for def in &defs {
                std::hint::black_box(mask_bitmap_key(
                    def,
                    width,
                    height,
                    scale,
                    offset,
                    &PhotoGeometry::default(),
                    None,
                ));
            }
        });
        let brush_def = defs
            .iter()
            .find(|d| d.sub_masks.iter().any(|s| s.mask_type == "brush"))
            .unwrap();
        let mut appended = brush_def.clone();
        let lines = appended.sub_masks[0].parameters["lines"]
            .as_array_mut()
            .unwrap();
        let stroke = lines.last().unwrap().clone();
        lines.push(stroke);
        time("brush_cold", &mut || {
            clear_brush_prefixes();
            std::hint::black_box(generate_mask_bitmap(
                &appended,
                width,
                height,
                scale,
                offset,
                &PhotoGeometry::default(),
                None,
            ));
        });
        let mut samples: Vec<f64> = (0..12)
            .map(|_| {
                clear_brush_prefixes();
                generate_mask_bitmap(
                    brush_def,
                    width,
                    height,
                    scale,
                    offset,
                    &PhotoGeometry::default(),
                    None,
                );
                let start = std::time::Instant::now();
                std::hint::black_box(generate_mask_bitmap(
                    &appended,
                    width,
                    height,
                    scale,
                    offset,
                    &PhotoGeometry::default(),
                    None,
                ));
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        samples.sort_by(f64::total_cmp);
        println!(
            "{}",
            json!({"probe":"preview_masks","stage":"brush_after_prefix","median_ms":samples[6],"min_ms":samples[0],"max_ms":samples[11]})
        );
        let radial = defs
            .iter()
            .find(|d| d.sub_masks.iter().any(|s| s.mask_type == "radial"))
            .unwrap();
        time("radial", &mut || {
            std::hint::black_box(generate_mask_bitmap(
                radial,
                width,
                height,
                scale,
                offset,
                &PhotoGeometry::default(),
                None,
            ));
        });
    }

    /// The original per-pixel composition, kept as an exact reference.
    fn reference_bitmap(def: &MaskDefinition, width: u32, height: u32) -> GrayImage {
        let mut final_mask = GrayImage::new(width, height);
        for sub_mask in &def.sub_masks {
            let Some(mut sub) = generate_sub_mask_bitmap(
                sub_mask,
                width,
                height,
                0.8,
                (3.5, -2.0),
                &PhotoGeometry::default(),
                None,
            ) else {
                continue;
            };
            if sub_mask.invert {
                for p in sub.pixels_mut() {
                    p[0] = 255 - p[0];
                }
            }
            let m = (sub_mask.opacity / 100.0).clamp(0.0, 1.0);
            if m < 1.0 {
                for p in sub.pixels_mut() {
                    p[0] = (p[0] as f32 * m) as u8;
                }
            }
            for (x, y, pixel) in final_mask.enumerate_pixels_mut() {
                let v = sub.get_pixel(x, y)[0];
                pixel[0] = match sub_mask.mode {
                    SubMaskMode::Additive => pixel[0].max(v),
                    SubMaskMode::Subtractive => pixel[0].saturating_sub(v),
                    SubMaskMode::Intersect => pixel[0].min(v),
                };
            }
        }
        if def.invert {
            for p in final_mask.pixels_mut() {
                p[0] = 255 - p[0];
            }
        }
        let m = (def.opacity / 100.0).clamp(0.0, 1.0);
        if m < 1.0 {
            for p in final_mask.pixels_mut() {
                p[0] = (p[0] as f32 * m) as u8;
            }
        }
        final_mask
    }

    #[test]
    fn mask_composition_matches_original_per_pixel_loops() {
        let radial = json!({"centerX":200,"centerY":120,"radiusX":150,"radiusY":90,"rotation":20,"feather":0.6});
        let linear = json!({"startX":30,"startY":40,"endX":400,"endY":220,"range":80});
        let brush = json!({"lines":[stroke("brush", 70.0, 100.0, 0.4), stroke("eraser", 40.0, 110.0, 0.8)]});
        let sub = |kind: &str, parameters: &Value, mode: &str, invert: bool, opacity: f32| {
            serde_json::from_value::<SubMask>(json!({"id":kind,"type":kind,"visible":true,
                "invert":invert,"opacity":opacity,"mode":mode,"parameters":parameters}))
            .unwrap()
        };
        let mut compared = 0;
        for first_mode in ["additive", "subtractive", "intersect"] {
            for second_mode in ["additive", "subtractive", "intersect"] {
                for (invert, opacity) in [(false, 100.0), (true, 37.5), (false, 0.0), (true, 100.0)]
                {
                    let def = MaskDefinition {
                        id: "m".into(),
                        name: "m".into(),
                        visible: true,
                        invert,
                        opacity: 100.0 - opacity / 2.0,
                        mask_version: None,
                        refine_edge: None,
                        adjustments: json!({}),
                        sub_masks: vec![
                            sub("radial", &radial, first_mode, !invert, opacity),
                            sub("brush", &brush, second_mode, false, 61.0),
                            sub("linear", &linear, "additive", invert, 100.0),
                        ],
                    };
                    for count in 1..=3 {
                        let mut partial = def.clone();
                        partial.sub_masks.truncate(count);
                        assert_eq!(
                            generate_mask_bitmap(
                                &partial,
                                451,
                                277,
                                0.8,
                                (3.5, -2.0),
                                &PhotoGeometry::default(),
                                None
                            )
                            .unwrap(),
                            reference_bitmap(&partial, 451, 277),
                            "{first_mode}/{second_mode}, invert {invert}, opacity {opacity}, {count} sub-masks"
                        );
                        compared += 1;
                    }
                }
            }
        }
        assert_eq!(compared, 108);
    }

    #[test]
    fn appended_brush_strokes_match_full_rasterization() {
        let lines = [
            stroke("brush", 60.0, 80.0, 0.5),
            stroke("eraser", 30.0, 95.0, 0.2),
            stroke("brush", 45.0, 140.0, 0.9),
            stroke("brush", 20.0, 60.0, 0.0),
        ];
        let fresh = |count: usize, width, height, scale| {
            clear_brush_prefixes();
            brush(&lines[..count], width, height, scale)
        };
        let expected: Vec<_> = (1..=lines.len()).map(|n| fresh(n, 517, 263, 0.8)).collect();
        clear_brush_prefixes();
        for (count, expected) in (1..=lines.len()).zip(&expected) {
            assert_eq!(
                &brush(&lines[..count], 517, 263, 0.8),
                expected,
                "{count} strokes"
            );
        }
        // Undo to a shorter prefix, then paint again from it.
        assert_eq!(&brush(&lines[..2], 517, 263, 0.8), &expected[1]);
        assert_eq!(&brush(&lines[..3], 517, 263, 0.8), &expected[2]);

        // An edited earlier stroke must not reuse the cached later state.
        let mut edited = lines.clone();
        edited[1] = stroke("eraser", 31.0, 95.0, 0.2);
        let reused = brush(&edited, 517, 263, 0.8);
        clear_brush_prefixes();
        assert_eq!(reused, brush(&edited, 517, 263, 0.8));
        assert_ne!(reused, expected[3]);

        // An overlay painting the next stroke must not evict the preview's
        // state: the preview then rasterizes only that new stroke.
        clear_brush_prefixes();
        brush(&lines[..3], 517, 263, 0.8);
        let growing = lines[3]["points"].as_array().unwrap().clone();
        for count in 1..=growing.len() {
            let mut partial = lines[..4].to_vec();
            partial[3]["points"] = json!(growing[..count]);
            brush(&partial, 401, 204, 0.62);
        }
        RASTERIZED_BRUSH_LINES.with(|count| count.set(0));
        assert_eq!(brush(&lines, 517, 263, 0.8), expected[3]);
        assert_eq!(RASTERIZED_BRUSH_LINES.with(std::cell::Cell::get), 1);

        // The app's order per stroke: overlay updates while the stroke grows,
        // the final preview, an overlay of the committed stroke, then the
        // thumbnail at its own size. Each must rasterize only the new stroke.
        clear_brush_prefixes();
        let geometries = [(401, 204, 0.62), (517, 263, 0.8), (240, 122, 0.37)];
        let rasterized = |f: &mut dyn FnMut()| {
            RASTERIZED_BRUSH_LINES.with(|count| count.set(0));
            f();
            RASTERIZED_BRUSH_LINES.with(std::cell::Cell::get)
        };
        let (overlay, preview, thumbnail) = (geometries[0], geometries[1], geometries[2]);
        for count in 1..=lines.len() {
            let growing = lines[count - 1]["points"].as_array().unwrap().clone();
            for points in 1..=growing.len() {
                let mut partial = lines[..count].to_vec();
                partial[count - 1]["points"] = json!(growing[..points]);
                let painted = rasterized(&mut || {
                    brush(&partial, overlay.0, overlay.1, overlay.2);
                });
                if count > 1 {
                    assert_eq!(painted, 1, "overlay move {points} of stroke {count}");
                }
            }
            for (label, geometry) in [
                ("preview", preview),
                ("overlay", overlay),
                ("thumbnail", thumbnail),
            ] {
                let painted = rasterized(&mut || {
                    let mask = brush(&lines[..count], geometry.0, geometry.1, geometry.2);
                    if label == "preview" {
                        assert_eq!(mask, expected[count - 1]);
                    }
                });
                if count > 1 {
                    let state: Vec<_> = with_brush_prefixes(|prefixes| {
                        prefixes
                            .iter()
                            .map(|entry| (entry.geometry.frame.0, entry.lines.len()))
                            .collect()
                    });
                    assert!(
                        painted <= 1,
                        "{label} of stroke {count} rasterized {painted} lines; {state:?}"
                    );
                }
            }
        }

        // Another geometry (overlay size or zoom) never borrows these pixels.
        brush(&lines[..3], 517, 263, 0.8);
        assert_eq!(brush(&lines, 400, 263, 0.8), fresh(4, 400, 263, 0.8));
        brush(&lines[..3], 517, 263, 0.8);
        assert_eq!(brush(&lines, 517, 263, 0.7), fresh(4, 517, 263, 0.7));
    }

    fn definition(mask_type: &str) -> MaskDefinition {
        MaskDefinition {
            id: "mask".into(),
            name: "Mask".into(),
            visible: true,
            invert: false,
            opacity: 100.0,
            mask_version: None,
            refine_edge: None,
            adjustments: json!({"exposure": 0.0}),
            sub_masks: vec![SubMask {
                id: "sub-mask".into(),
                mask_type: mask_type.into(),
                visible: true,
                invert: false,
                opacity: 100.0,
                mode: SubMaskMode::Additive,
                parameters: json!({}),
            }],
        }
    }

    fn key(id: u64) -> MaskBitmapKey {
        MaskBitmapKey {
            bitmap_hash: id,
            source: None,
        }
    }

    fn definition_key(
        def: &MaskDefinition,
        source: Option<&ParametricMaskSource>,
    ) -> MaskBitmapKey {
        mask_bitmap_key(
            def,
            8,
            6,
            0.5,
            (1.0, 2.0),
            &PhotoGeometry::default(),
            source,
        )
    }

    #[test]
    fn cache_shares_pixels_and_clear_keeps_borrowers_alive() {
        let mut cache = MaskBitmapCache::new();
        let image = Arc::new(MaskBitmap::from(GrayImage::from_pixel(2, 2, Luma([17]))));
        cache.insert(key(1), Arc::clone(&image));
        let first = cache.get(&key(1)).unwrap();
        let second = cache.get(&key(1)).unwrap();

        assert!(Arc::ptr_eq(&image, &first));
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(cache.retained_bytes, 4);

        cache.clear();
        assert!(cache.get(&key(1)).is_none());
        assert_eq!(cache.retained_bytes, 0);
        assert_eq!(first.as_raw(), &[17, 17, 17, 17]);
    }

    #[test]
    fn cache_evicts_least_recently_used_pixels_to_fit_byte_budget() {
        let mut cache = MaskBitmapCache::new();
        cache.byte_budget = 8;
        let image = || Arc::new(MaskBitmap::from(GrayImage::new(2, 2)));
        cache.insert(key(1), image());
        cache.insert(key(2), image());
        assert!(cache.get(&key(1)).is_some());
        cache.insert(key(3), image());

        assert!(cache.get(&key(2)).is_none());
        assert!(cache.get(&key(1)).is_some());
        assert!(cache.get(&key(3)).is_some());
        assert_eq!(cache.retained_bytes, 8);

        let replacement = Arc::new(MaskBitmap::from(GrayImage::new(3, 2)));
        cache.insert(key(1), Arc::clone(&replacement));
        assert!(cache.get(&key(3)).is_none());
        assert!(Arc::ptr_eq(&cache.get(&key(1)).unwrap(), &replacement));
        assert_eq!(cache.retained_bytes, 6);
    }

    #[test]
    fn cache_keeps_masks_still_held_elsewhere_over_its_budget() {
        // A frame holds every layer's mask (and the GPU's mask array keeps
        // the previous frame's): evicting them frees nothing, and with more
        // layers than the budget holds every layer would miss every frame.
        let mut cache = MaskBitmapCache::new();
        cache.byte_budget = 8;
        let held: Vec<_> = (0..3)
            .map(|_| Arc::new(MaskBitmap::from(GrayImage::new(2, 2))))
            .collect();
        for (id, image) in held.iter().enumerate() {
            cache.insert(key(id as u64), Arc::clone(image));
        }
        for (id, image) in held.iter().enumerate() {
            assert!(Arc::ptr_eq(&cache.get(&key(id as u64)).unwrap(), image));
        }
        assert_eq!(cache.retained_bytes, 12);

        // Once released, the least recently used make room again.
        drop(held);
        cache.insert(key(3), Arc::new(MaskBitmap::from(GrayImage::new(2, 2))));
        assert!(cache.get(&key(0)).is_none());
        assert!(cache.get(&key(1)).is_none());
        assert!(cache.get(&key(2)).is_some());
        assert!(cache.get(&key(3)).is_some());
        assert_eq!(cache.retained_bytes, 8);
    }

    #[test]
    fn cache_rejects_oversized_masks_without_evicting_useful_entries() {
        let mut cache = MaskBitmapCache::new();
        cache.byte_budget = 4;
        let image = Arc::new(MaskBitmap::from(GrayImage::new(2, 2)));
        cache.insert(key(1), Arc::clone(&image));
        cache.insert(key(2), Arc::new(MaskBitmap::from(GrayImage::new(3, 2))));

        assert!(Arc::ptr_eq(&cache.get(&key(1)).unwrap(), &image));
        assert!(cache.get(&key(2)).is_none());
        assert_eq!(cache.retained_bytes, 4);

        cache.clear();
        cache.byte_budget = 0;
        cache.insert(key(1), Arc::clone(&image));
        assert!(cache.get(&key(1)).is_none());
        cache.byte_budget = 4;
        cache.entry_limit = 0;
        cache.insert(key(1), image);
        assert!(cache.get(&key(1)).is_none());
    }

    #[test]
    fn cache_entry_limit_also_bounds_tiny_masks() {
        let mut cache = MaskBitmapCache::new();
        cache.entry_limit = 2;
        for id in 0..3 {
            cache.insert(key(id), Arc::new(MaskBitmap::from(GrayImage::new(0, 0))));
        }

        assert!(cache.get(&key(0)).is_none());
        assert!(cache.get(&key(1)).is_some());
        assert!(cache.get(&key(2)).is_some());
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.retained_bytes, 0);
    }

    #[test]
    fn bitmap_key_reuses_adjustment_edits_but_tracks_mask_and_sampling_changes() {
        let def = definition("ai-subject");
        let base_key = definition_key(&def, None);
        let mut edited = def.clone();
        edited.adjustments = json!({"exposure": 2.0, "contrast": 70});
        edited.name = "Renamed".into();
        assert!(base_key == definition_key(&edited, None));

        edited.sub_masks[0].parameters = json!({"maskDataBase64": "updated raster bytes"});
        assert!(base_key != definition_key(&edited, None));
        edited = def.clone();
        edited.opacity = 75.0;
        assert!(base_key != definition_key(&edited, None));
        edited = def.clone();
        edited.sub_masks[0].invert = true;
        assert!(base_key != definition_key(&edited, None));

        for (width, height, scale, offset) in [
            (9, 6, 0.5, (1.0, 2.0)),
            (8, 7, 0.5, (1.0, 2.0)),
            (8, 6, 0.6, (1.0, 2.0)),
            (8, 6, 0.5, (2.0, 2.0)),
            (8, 6, 0.5, (1.0, 3.0)),
        ] {
            assert!(
                base_key
                    != mask_bitmap_key(
                        &def,
                        width,
                        height,
                        scale,
                        offset,
                        &PhotoGeometry::default(),
                        None
                    )
            );
        }
    }

    #[test]
    fn parametric_bitmap_keys_track_source_identity_and_sampling_scale() {
        let source = ParametricMaskSource {
            image: Arc::new(DynamicImage::new_rgb8(8, 6)),
            coordinate_dimensions: (8.0, 6.0),
            geometry: SourceGeometry::default(),
            raw_curve: false,
        };
        let other_source = ParametricMaskSource {
            image: Arc::new(source.image.as_ref().clone()),
            coordinate_dimensions: (8.0, 6.0),
            geometry: SourceGeometry::default(),
            raw_curve: false,
        };
        for mask_type in ["color", "luminance", "luma-range"] {
            let def = definition(mask_type);
            let base_key = definition_key(&def, Some(&source));
            // A luma range follows the photo's orientation: a rotated photo
            // must not reuse the bitmap.
            let rotated = source
                .clone()
                .with_geometry(&json!({"orientationSteps": 1}));
            assert!(base_key != definition_key(&def, Some(&rotated)));
            assert!(base_key == definition_key(&def, Some(&source.clone())));
            assert!(base_key != definition_key(&def, Some(&other_source)));
            assert!(base_key != definition_key(&def, None));
            for dimensions in [(16.0, 6.0), (8.0, 12.0)] {
                let resized_domain = source
                    .clone()
                    .with_coordinate_dimensions(dimensions)
                    .unwrap();
                assert!(base_key != definition_key(&def, Some(&resized_domain)));
            }
        }

        let independent = definition("radial");
        assert!(
            definition_key(&independent, Some(&source))
                == definition_key(&independent, Some(&other_source))
        );
    }

    #[test]
    fn cached_source_identity_does_not_keep_warped_pixels_alive() {
        let mut cache = MaskBitmapCache::new();
        let def = definition("color");
        let source = Arc::new(DynamicImage::new_rgb8(8, 6));
        let weak_source = Arc::downgrade(&source);
        let cache_key = definition_key(
            &def,
            Some(&ParametricMaskSource {
                image: Arc::clone(&source),
                coordinate_dimensions: (8.0, 6.0),
                geometry: SourceGeometry::default(),
                raw_curve: false,
            }),
        );
        let image = Arc::new(MaskBitmap::from(GrayImage::new(8, 6)));
        cache.insert(cache_key, Arc::clone(&image));

        assert_eq!(Arc::strong_count(&source), 1);
        assert_eq!(Arc::weak_count(&source), 2);
        drop(source);
        assert!(weak_source.upgrade().is_none());
        assert!(Weak::ptr_eq(
            cache.entries.front().unwrap().key.source.as_ref().unwrap(),
            &weak_source,
        ));

        let new_source = Arc::new(DynamicImage::new_rgb8(8, 6));
        let new_key = definition_key(
            &def,
            Some(&ParametricMaskSource {
                image: new_source,
                coordinate_dimensions: (8.0, 6.0),
                geometry: SourceGeometry::default(),
                raw_curve: false,
            }),
        );
        assert!(cache.get(&new_key).is_none());
    }

    fn parametric_definition(kind: &str, target: (f64, f64)) -> MaskDefinition {
        let mut def = definition(kind);
        def.sub_masks[0].parameters =
            json!({"targetX": target.0, "targetY": target.1, "tolerance": 1.0, "feather": 0.0});
        def
    }

    fn block_source() -> DynamicImage {
        DynamicImage::ImageRgb8(image::RgbImage::from_fn(8, 6, |x, y| {
            image::Rgb([((x / 2) * 60) as u8, ((y / 2) * 70) as u8, 0])
        }))
    }

    #[test]
    fn reduced_parametric_sources_preserve_original_coordinates_and_crop() {
        let image = block_source();
        let full = prepare_parametric_mask_source(&image, false, 1.0, &json!({})).unwrap();
        let reduced_image = image.resize_exact(4, 3, image::imageops::FilterType::Nearest);
        let reduced =
            prepare_parametric_mask_source(&reduced_image, false, 0.5, &json!({})).unwrap();
        for kind in ["color", "luminance"] {
            let def = parametric_definition(kind, (2.0, 2.0));
            for (width, height, scale, offset) in [
                (8, 6, 1.0, (0.0, 0.0)),
                (4, 3, 0.5, (0.0, 0.0)),
                (4, 2, 1.0, (2.0, 2.0)),
            ] {
                let expected = generate_mask_bitmap(
                    &def,
                    width,
                    height,
                    scale,
                    offset,
                    &PhotoGeometry::default(),
                    Some(&full),
                )
                .unwrap();
                let actual = generate_mask_bitmap(
                    &def,
                    width,
                    height,
                    scale,
                    offset,
                    &PhotoGeometry::default(),
                    Some(&reduced),
                )
                .unwrap();
                assert_eq!(actual, expected, "{kind}, scale {scale}, crop {offset:?}");
                assert!(actual.as_raw().contains(&255));
            }
            // The last full-resolution coordinate maps to the last reduced
            // pixel, including when round(target * scale) equals its width.
            let edge = parametric_definition(kind, (7.0, 5.0));
            let expected = generate_mask_bitmap(
                &edge,
                8,
                6,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::default(),
                Some(&full),
            )
            .unwrap();
            let actual = generate_mask_bitmap(
                &edge,
                8,
                6,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::default(),
                Some(&reduced),
            )
            .unwrap();
            assert_eq!(actual, expected, "{kind} edge reference");
            assert_eq!(actual.get_pixel(7, 5)[0], 255);
            for target in [(1.0, 1.0), (3.0, 3.0), (5.0, 1.0)] {
                let def = parametric_definition(kind, target);
                let expected = generate_mask_bitmap(
                    &def,
                    8,
                    6,
                    1.0,
                    (0.0, 0.0),
                    &PhotoGeometry::default(),
                    Some(&full),
                )
                .unwrap();
                let actual = generate_mask_bitmap(
                    &def,
                    8,
                    6,
                    1.0,
                    (0.0, 0.0),
                    &PhotoGeometry::default(),
                    Some(&reduced),
                )
                .unwrap();
                assert_eq!(actual, expected, "{kind} odd reference {target:?}");
            }
        }
    }

    #[test]
    fn rounded_resize_preserves_both_coordinate_axes_and_reference_bounds() {
        // Both floor and ceil of the aspect-preserving height occur in image
        // pipelines. A single ratio from the width cannot recover either domain.
        for reduced_height in [8, 9] {
            let image = DynamicImage::new_rgb8(60, 40);
            let reduced =
                DynamicImage::ImageRgb8(image::RgbImage::from_fn(13, reduced_height, |_, y| {
                    image::Rgb(if y == reduced_height - 1 {
                        [255; 3]
                    } else {
                        [0; 3]
                    })
                }));
            let source = prepare_parametric_mask_source(&image, false, 1.0, &json!({}))
                .unwrap()
                .with_resized_image(reduced.clone());
            let already_reduced =
                prepare_parametric_mask_source(&reduced, false, 13.0 / 60.0, &json!({}))
                    .unwrap()
                    .with_coordinate_dimensions((60.0, 40.0))
                    .unwrap();
            for kind in ["color", "luminance"] {
                let def = parametric_definition(kind, (59.0, 39.0));
                let actual = generate_mask_bitmap(
                    &def,
                    60,
                    40,
                    1.0,
                    (0.0, 0.0),
                    &PhotoGeometry::default(),
                    Some(&source),
                )
                .unwrap();
                let from_existing = generate_mask_bitmap(
                    &def,
                    60,
                    40,
                    1.0,
                    (0.0, 0.0),
                    &PhotoGeometry::default(),
                    Some(&already_reduced),
                )
                .unwrap();
                assert_eq!(actual, from_existing);
                for (x, y, pixel) in actual.enumerate_pixels() {
                    let expected = if y * reduced_height / 40 == reduced_height - 1 {
                        255
                    } else {
                        0
                    };
                    assert_eq!(
                        pixel[0], expected,
                        "{kind} resize 13x{reduced_height}, pixel {x},{y}"
                    );
                }
                let mut params: ParametricMaskParameters =
                    serde_json::from_value(def.sub_masks[0].parameters.clone()).unwrap();
                assert!(parametric_reference_pixel(&source, &params).is_some());
                params.target_y = 40.0;
                assert!(parametric_reference_pixel(&source, &params).is_none());
                params.target_y = 39.0;
                params.target_x = 60.0;
                assert!(parametric_reference_pixel(&source, &params).is_none());
            }
        }
    }

    #[test]
    fn explicit_parametric_source_changes_mask_pixels_and_preserves_inputs() {
        let first = block_source();
        let second =
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 6, image::Rgb([200, 50, 20])));
        let first_before = first.as_bytes().to_vec();
        let second_before = second.as_bytes().to_vec();
        let first_source = prepare_parametric_mask_source(&first, false, 1.0, &json!({})).unwrap();
        let second_source =
            prepare_parametric_mask_source(&second, false, 1.0, &json!({})).unwrap();
        for kind in ["color", "luminance"] {
            let def = parametric_definition(kind, (0.0, 0.0));
            let first_mask = generate_mask_bitmap(
                &def,
                8,
                6,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::default(),
                Some(&first_source),
            )
            .unwrap();
            let second_mask = generate_mask_bitmap(
                &def,
                8,
                6,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::default(),
                Some(&second_source),
            )
            .unwrap();
            assert_ne!(
                first_mask, second_mask,
                "{kind} must use its requested source"
            );
            assert!(second_mask.as_raw().iter().all(|value| *value == 255));
        }
        assert_eq!(first.as_bytes(), first_before);
        assert_eq!(second.as_bytes(), second_before);
    }

    #[test]
    fn warp_cache_checks_source_geometry_and_raw_mode_and_background_is_transient() {
        let editor = Arc::new(block_source());
        let background = Arc::new(DynamicImage::new_rgb8(8, 6));
        let adjustments = json!({"transformDistortion": 10.0});
        let hash = crate::cache_utils::calculate_geometry_hash(&adjustments);
        let warped = prepare_shared_warped_mask_image(&editor, false, &adjustments).unwrap();
        let cache = FullWarpedMaskCache::new(&editor, false, hash, Arc::clone(&warped));
        assert!(Arc::ptr_eq(
            &cache.get(&editor, false, hash).unwrap(),
            &warped
        ));
        assert!(cache.get(&background, false, hash).is_none());
        assert!(cache.get(&editor, true, hash).is_none());
        assert!(cache.get(&editor, false, hash.wrapping_add(1)).is_none());
        let _transient =
            prepare_parametric_mask_source(&background, false, 1.0, &adjustments).unwrap();
        assert!(Arc::ptr_eq(
            &cache.get(&editor, false, hash).unwrap(),
            &warped
        ));
        let identity = prepare_shared_warped_mask_image(&editor, false, &json!({})).unwrap();
        assert!(Arc::ptr_eq(&identity, &editor));
    }

    #[test]
    fn parametric_preparation_matches_raw_then_geometry_without_changing_original() {
        let image = DynamicImage::ImageRgb32F(image::Rgb32FImage::from_fn(8, 6, |x, y| {
            image::Rgb([x as f32 / 8.0, y as f32 / 6.0, 0.15])
        }));
        let before = image.as_bytes().to_vec();
        let adjustments = json!({"transformDistortion": 12.0});
        let actual = prepare_parametric_mask_source(&image, true, 1.0, &adjustments).unwrap();
        let mut expected = image.clone();
        crate::image_processing::apply_cpu_default_raw_processing(&mut expected);
        let expected =
            crate::image_processing::apply_geometry_warp(expected, &adjustments).unwrap();
        assert_eq!(actual.image.as_bytes(), expected.as_bytes());
        assert_eq!(image.as_bytes(), before);
    }

    #[test]
    fn raw_curve_from_borrowed_source_matches_in_place_curve() {
        let value = |x: u32, y: u32, c: u32| (x * 7 + y * 13 + c * 5) as f32 / 40.0 - 0.3;
        let rgb = DynamicImage::ImageRgb32F(image::Rgb32FImage::from_fn(9, 5, |x, y| {
            image::Rgb([value(x, y, 0), value(x, y, 1), value(x, y, 2)])
        }));
        let rgba = DynamicImage::ImageRgba32F(image::Rgba32FImage::from_fn(9, 5, |x, y| {
            image::Rgba([value(x, y, 0), value(x, y, 1), value(x, y, 2), 0.5])
        }));
        let bytes = DynamicImage::ImageRgb8(image::RgbImage::from_fn(9, 5, |x, y| {
            image::Rgb([(x * 20) as u8, (y * 40) as u8, 200])
        }));
        for image in [rgb, rgba, bytes] {
            let mut expected = image.clone();
            crate::image_processing::apply_cpu_default_raw_processing(&mut expected);
            let actual = crate::image_processing::cpu_default_raw_processed(&image);
            assert_eq!(actual.as_bytes(), expected.as_bytes());
        }
    }

    #[test]
    fn parametric_sources_reject_invalid_scale_and_required_missing_images() {
        let image = block_source();
        for scale in [0.0, -1.0, f32::INFINITY, f32::NAN] {
            assert!(prepare_parametric_mask_source(&image, false, scale, &json!({})).is_err());
        }
        for dimensions in [
            (0.0, 40.0),
            (60.0, -1.0),
            (f64::NAN, 40.0),
            (60.0, f64::INFINITY),
        ] {
            assert!(ParametricMaskSource::new(Arc::new(image.clone()), dimensions).is_err());
        }
        for kind in ["color", "luminance"] {
            let mut def = parametric_definition(kind, (0.0, 0.0));
            assert!(validate_mask_source(&def, None).is_err());
            def.visible = false;
            assert!(validate_mask_source(&def, None).is_ok());
        }
        assert!(validate_mask_source(&definition("radial"), None).is_ok());
    }
}

#[cfg(test)]
mod stroke_rasterization_tests {
    use super::*;

    /// The former two-pass brush application: a separate stroke layer, then a
    /// serial blend over its whole bounding box.
    fn reference_apply_brush_line(
        mask: &mut GrayImage,
        line: &BrushLine,
        scale: f32,
        crop_offset: (f32, f32),
    ) {
        if line.points.is_empty() {
            return;
        }
        let (width, height) = mask.dimensions();
        let is_eraser = line.tool == "eraser";
        let radius = (line.brush_size * scale / 2.0).max(0.0);
        let feather = line.feather.clamp(0.0, 1.0);
        let Some((min_x, min_y, max_x, max_y)) =
            stroke_bounds(&line.points, width, height, radius, scale, crop_offset)
        else {
            return;
        };
        let (bb_w, bb_h) = (max_x - min_x + 1, max_y - min_y + 1);
        let line_mask = render_stroke_layer_parallel(
            &line.points,
            radius,
            feather,
            scale,
            crop_offset,
            (min_x as f32, min_y as f32),
            bb_w,
            bb_h,
        );
        for y in 0..bb_h {
            for x in 0..bb_w {
                let src_val = line_mask.get_pixel(x, y)[0] as f32 / 255.0;
                if src_val <= 0.0 {
                    continue;
                }
                let dst_pixel = mask.get_pixel_mut(min_x + x, min_y + y);
                let dst_val = dst_pixel[0] as f32 / 255.0;
                let blended = if is_eraser {
                    dst_val * (1.0 - src_val)
                } else {
                    dst_val + src_val - dst_val * src_val
                };
                dst_pixel[0] = (blended.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }

    /// The former flow-brush loop over each stroke layer.
    fn reference_flow_bitmap(lines: &[FlowLine], width: u32, height: u32, scale: f32) -> GrayImage {
        let crop_offset = (3.5, -2.0);
        let mut final_mask = GrayImage::new(width, height);
        for line in lines {
            if line.points.is_empty() {
                continue;
            }
            let is_eraser = line.tool == "eraser";
            let flow_per_stroke = (line.flow.clamp(0.0, 100.0) / 100.0) * 255.0;
            let radius = (line.brush_size * scale / 2.0).max(0.0);
            let Some((min_x, min_y, max_x, max_y)) =
                stroke_bounds(&line.points, width, height, radius, scale, crop_offset)
            else {
                continue;
            };
            let (bb_w, bb_h) = (max_x - min_x + 1, max_y - min_y + 1);
            let line_mask = render_stroke_layer_parallel(
                &line.points,
                radius,
                line.feather.clamp(0.0, 1.0),
                scale,
                crop_offset,
                (min_x as f32, min_y as f32),
                bb_w,
                bb_h,
            );
            for y in 0..bb_h {
                for x in 0..bb_w {
                    let stroke_pixel = line_mask.get_pixel(x, y)[0] as f32;
                    if stroke_pixel <= 0.0 {
                        continue;
                    }
                    let pixel = final_mask.get_pixel_mut(min_x + x, min_y + y);
                    let c_norm = pixel[0] as f32 / 255.0;
                    let delta = ((stroke_pixel / 255.0) * flow_per_stroke).round();
                    let d_norm = (delta / 255.0).clamp(0.0, 1.0);
                    let next = if is_eraser {
                        c_norm * (1.0 - d_norm)
                    } else {
                        c_norm + d_norm - c_norm * d_norm
                    };
                    pixel[0] = (next.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
        final_mask
    }

    fn xorshift(mut seed: u32) -> impl FnMut() -> u32 {
        move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        }
    }

    fn random_points(next: &mut impl FnMut() -> u32, width: u32, height: u32) -> Vec<Point> {
        let count = match next() % 6 {
            0 => 1,
            1 => 2,
            _ => 3 + next() as usize % 22,
        };
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            // Coordinates reach beyond the image; some points repeat.
            if !points.is_empty() && next().is_multiple_of(7) {
                let last: &Point = points.last().unwrap();
                points.push(Point {
                    x: last.x,
                    y: last.y,
                    p: None,
                });
                continue;
            }
            points.push(Point {
                x: (next() % (width * 3)) as f64 * 0.8 - width as f64 * 0.4
                    + (next() % 100) as f64 / 37.0,
                y: (next() % (height * 3)) as f64 * 0.8 - height as f64 * 0.4
                    + (next() % 100) as f64 / 41.0,
                p: None,
            });
        }
        points
    }

    #[test]
    fn fused_strokes_match_the_two_pass_rasterizer() {
        let mut next = xorshift(0x51ed_270b);
        let mut checked = 0;
        for (width, height) in [(1, 1), (97, 61), (300, 7), (211, 233)] {
            for scale in [0.5714286_f32, 1.0, 2.25] {
                let mut actual = GrayImage::from_fn(width, height, |x, y| {
                    Luma([((x * 31 + y * 17) % 256) as u8])
                });
                let mut expected = actual.clone();
                for _ in 0..60 {
                    let line = BrushLine {
                        tool: if next().is_multiple_of(4) {
                            "eraser"
                        } else {
                            "brush"
                        }
                        .into(),
                        brush_size: [0.0, 0.4, 3.0, 17.5, 111.5, 260.0][next() as usize % 6],
                        points: random_points(&mut next, width, height),
                        feather: [0.0, 0.25, 0.5, 1.0, 1.7][next() as usize % 5],
                        opacity: None,
                        flow: None,
                        hardness: None,
                        lr_center_weight: None,
                        auto_mask: false,
                        auto_mask_version: None,
                        dab_version: None,
                        pressure_size: false,
                        pressure_flow: false,
                        frame: None,
                    };
                    let offset = ((next() % 9) as f32 - 4.5, (next() % 7) as f32 - 2.0);
                    apply_brush_line(&mut actual, &line, scale, offset, None);
                    reference_apply_brush_line(&mut expected, &line, scale, offset);
                    assert!(actual == expected, "{width}x{height} at {scale}: {line:?}");
                    checked += 1;
                }
            }
            let lines: Vec<_> = (0..40)
                .map(|_| FlowLine {
                    tool: if next().is_multiple_of(4) {
                        "eraser"
                    } else {
                        "brush"
                    }
                    .into(),
                    brush_size: [0.4, 9.0, 111.5, 260.0][next() as usize % 4],
                    points: random_points(&mut next, width, height),
                    feather: [0.0, 0.5, 1.0][next() as usize % 3],
                    flow: [0.0, 3.5, 10.0, 55.0, 100.0, 140.0][next() as usize % 6],
                    frame: None,
                })
                .collect();
            let value = serde_json::to_value(FlowMaskParameters {
                lines: lines.clone(),
            })
            .unwrap();
            assert!(
                generate_flow_bitmap(
                    &value,
                    width,
                    height,
                    0.8,
                    (3.5, -2.0),
                    SourceGeometry::default().into(),
                ) == reference_flow_bitmap(&lines, width, height, 0.8),
                "flow {width}x{height}"
            );
            checked += 1;
        }
        println!("{checked} fused stroke rasterizations match the two-pass reference");
    }

    fn painted(lines: &[BrushLine], width: u32, height: u32) -> GrayImage {
        let mut mask = GrayImage::new(width, height);
        for line in lines {
            apply_brush_line(&mut mask, line, 1.0, (0.0, 0.0), None);
        }
        mask
    }

    fn brush(
        tool: &str,
        points: &[(f64, f64)],
        opacity: Option<f32>,
        flow: Option<f32>,
    ) -> BrushLine {
        BrushLine {
            tool: tool.into(),
            brush_size: 20.0,
            points: points
                .iter()
                .map(|&(x, y)| Point { x, y, p: None })
                .collect(),
            feather: 0.5,
            opacity,
            flow,
            hardness: None,
            lr_center_weight: None,
            auto_mask: false,
            auto_mask_version: None,
            dab_version: None,
            pressure_size: false,
            pressure_flow: false,
            frame: None,
        }
    }

    #[test]
    fn lightroom_brush_puts_its_tip_at_every_dab() {
        // Camera Raw's single dabs: CenterWeight 0 at 0.3 r is 0.46, a hard
        // tip at 0.9 r 0.73; nothing at the radius.
        assert!((lightroom_tip(0.0, 0.3) - 0.459).abs() < 1e-3);
        assert!((lightroom_tip(1.0, 0.9) - 0.726).abs() < 1e-3);
        assert!((lightroom_tip(0.5, 0.0) - 1.0).abs() < 1e-6);
        assert_eq!(lightroom_tip(0.5, 1.0), 0.0);
        let line = |points: &[(f64, f64)], flow: f32| BrushLine {
            tool: "brush".into(),
            brush_size: 40.0,
            points: points
                .iter()
                .map(|&(x, y)| Point { x, y, p: None })
                .collect(),
            feather: 0.5,
            opacity: None,
            flow: Some(flow),
            hardness: None,
            lr_center_weight: Some(1.0),
            auto_mask: false,
            auto_mask_version: None,
            dab_version: None,
            pressure_size: false,
            pressure_flow: false,
            frame: None,
        };
        // One dab at 50 % coverage; two dabs on the same spot reach 75 %
        // (Camera Raw: two Flow 0.5 dabs 0.208 from 0.109 each).
        let one = painted(&[line(&[(50.0, 30.0)], 50.0)], 100, 60);
        let two = painted(&[line(&[(50.0, 30.0), (50.0, 30.0)], 50.0)], 100, 60);
        assert!((one.get_pixel(50, 30)[0] as i32 - 128).abs() <= 1);
        assert!((two.get_pixel(50, 30)[0] as i32 - 191).abs() <= 1);
        // Dabs stay where they are: nothing is painted between distant dabs.
        let apart = painted(&[line(&[(10.0, 30.0), (90.0, 30.0)], 100.0)], 100, 60);
        assert_eq!(apart.get_pixel(50, 30)[0], 0);
        assert_eq!(apart.get_pixel(90, 30)[0], 255);
    }

    #[test]
    fn lightroom_luminance_ranges_select_percentiles() {
        let range = [0.0, 0.0, 0.25, 0.4];
        assert_eq!(lightroom_range_coverage(0.1, range), 1.0);
        assert!((lightroom_range_coverage(0.325, range) - 0.5).abs() < 1e-6);
        assert_eq!(lightroom_range_coverage(0.5, range), 0.0);
        // A dark photo: three quarters of it at luma 20, a quarter at 200.
        let mut image = image::RgbImage::new(4, 1);
        for (x, v) in [(0, 20u8), (1, 20), (2, 20), (3, 200)] {
            image.put_pixel(x, 0, image::Rgb([v, v, v]));
        }
        let table = luma_percentiles(&DynamicImage::ImageRgb8(image));
        assert!((table[80] - 0.375).abs() < 1e-6);
        assert!((table[800] - 0.875).abs() < 1e-6);
    }

    #[test]
    fn brush_opacity_and_flow_follow_capture_one() {
        let (w, h) = (100, 60);
        let near = |actual: u8, expected: f32| (actual as f32 - expected).abs() <= 1.0;
        let path = [(10.0, 30.0), (47.0, 33.0), (90.0, 30.0)];
        // Full opacity and flow render exactly like strokes without the fields.
        assert!(
            painted(&[brush("brush", &path, None, None)], w, h)
                == painted(&[brush("brush", &path, Some(100.0), Some(100.0))], w, h)
        );

        // A 50% stroke covers half; a second stroke over it reaches 75%.
        let dot = [(50.0, 30.0)];
        let half = brush("brush", &dot, Some(50.0), None);
        assert!(near(
            painted(std::slice::from_ref(&half), w, h).get_pixel(50, 30)[0],
            127.5
        ));
        let twice = painted(&[half.clone(), half.clone()], w, h);
        assert!(
            near(twice.get_pixel(50, 30)[0], 191.25),
            "{}",
            twice.get_pixel(50, 30)[0]
        );
        // Erasing at 50% removes half of the coverage.
        let erased = painted(
            &[
                brush("brush", &dot, None, None),
                brush("eraser", &dot, Some(50.0), None),
            ],
            w,
            h,
        );
        assert!(near(erased.get_pixel(50, 30)[0], 127.5));
        assert_eq!(
            painted(&[brush("brush", &dot, Some(0.0), None)], w, h).get_pixel(50, 30)[0],
            0
        );

        // Flow: one dab gives flow × opacity; along a stroke the coverage
        // builds up, never beyond the stroke's opacity.
        let flow_dot = painted(&[brush("brush", &dot, None, Some(25.0))], w, h);
        assert!(near(flow_dot.get_pixel(50, 30)[0], 63.75));
        let sparse = [(10.0, 30.0), (90.0, 30.0)];
        let built = painted(&[brush("brush", &sparse, Some(80.0), Some(25.0))], w, h);
        let centre = built.get_pixel(50, 30)[0];
        assert!(
            centre > 150 && centre as f32 <= 0.8 * 255.0 + 0.5,
            "{centre}"
        );
        // The edge of the brush builds up less than its core.
        assert!(built.get_pixel(50, 38)[0] < centre);

        // Build-up depends on the path, not on how densely it was sampled.
        let dense: Vec<_> = (10..=90).map(|x| (x as f64, 30.0)).collect();
        let resampled = painted(&[brush("brush", &dense, Some(80.0), Some(25.0))], w, h);
        let worst = built
            .as_raw()
            .iter()
            .zip(resampled.as_raw())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(worst <= 1, "sampling changed coverage by {worst}");
    }

    /// Values measured in Capture One 16 (mask overlay with and without the
    /// mask, brush size 100 px): stroke centres, the edge of strokes and dabs.
    #[test]
    fn capture_one_brush_matches_reference_measurements() {
        let (w, h) = (240, 120);
        let r = 20.0;
        let line = |points: &[(f64, f64)], hardness: f32, flow: f32| BrushLine {
            tool: "brush".into(),
            brush_size: 2.0 * r,
            points: points
                .iter()
                .map(|&(x, y)| Point { x, y, p: None })
                .collect(),
            feather: 0.5,
            opacity: None,
            flow: Some(flow),
            hardness: Some(hardness),
            lr_center_weight: None,
            auto_mask: false,
            auto_mask_version: None,
            dab_version: None,
            pressure_size: false,
            pressure_flow: false,
            frame: None,
        };
        let stroke = [(120.0, -150.0), (120.0, 270.0)];
        let at = |hardness: f32, flow: f32, distance: f32| {
            let mask = painted(&[line(&stroke, hardness, flow)], w, h);
            mask.get_pixel((120.0 + distance * r).round() as u32, 60)[0] as f32 / 255.0
        };
        let close = |actual: f32, expected: f32, tolerance: f32, case: &str| {
            assert!(
                (actual - expected).abs() <= tolerance,
                "{case}: {actual:.3} vs C1 {expected}"
            );
        };
        close(at(100.0, 10.0, 0.0), 0.65, 0.04, "hardness 100, flow 10");
        close(at(100.0, 25.0, 0.0), 0.948, 0.03, "hardness 100, flow 25");
        close(at(100.0, 100.0, 0.0), 1.0, 0.0, "hardness 100, flow 100");
        close(at(75.0, 10.0, 0.0), 0.30, 0.04, "hardness 75, flow 10");
        close(at(50.0, 10.0, 0.0), 0.25, 0.04, "hardness 50, flow 10");
        close(
            at(50.0, 10.0, 0.82),
            0.12,
            0.05,
            "hardness 50, flow 10, 0.82 r",
        );
        close(at(25.0, 10.0, 0.0), 0.23, 0.04, "hardness 25, flow 10");
        close(
            at(50.0, 100.0, 1.09),
            0.51,
            0.08,
            "hardness 50, flow 100, 1.09 r",
        );
        close(
            at(0.0, 100.0, 1.23),
            0.45,
            0.08,
            "hardness 0, flow 100, 1.23 r",
        );

        // One dab: the tip itself, 50% at the brush size.
        let dab = painted(&[line(&[(120.0, 60.0)], 50.0, 100.0)], w, h);
        let tip = |distance: f32| {
            dab.get_pixel((120.0 + distance * r).round() as u32, 60)[0] as f32 / 255.0
        };
        close(tip(0.4), 1.0, 0.0, "dab 0.4 r");
        close(tip(1.0), 0.50, 0.04, "dab at r");
        close(tip(1.2), 0.21, 0.05, "dab 1.2 r");
        close(tip(1.6), 0.0, 0.01, "dab 1.6 r");
        let dab = painted(&[line(&[(120.0, 60.0)], 50.0, 10.0)], w, h);
        close(
            dab.get_pixel(120, 60)[0] as f32 / 255.0,
            0.10,
            0.01,
            "one flow 10 dab",
        );
    }

    /// `dabVersion` 2 on a straight stroke: no scallops along the edge, the
    /// same mean coverage across the stroke as version 1, the stroke's end at
    /// its last point; without the field the stroke is version 1's.
    #[test]
    fn dab_version_2_ends_at_the_last_point_without_scallops() {
        let (w, h, r) = (900u32, 260u32, 40.0f32);
        let (x0, x1, cy) = (120.0, 120.0 + 8.37 * r as f64, 130.0);
        let line = |hardness: f32, flow: f32, version: Option<u32>| BrushLine {
            tool: "brush".into(),
            brush_size: 2.0 * r,
            points: vec![
                Point {
                    x: x0,
                    y: cy,
                    p: None,
                },
                Point {
                    x: x1,
                    y: cy,
                    p: None,
                },
            ],
            feather: 0.5,
            opacity: None,
            flow: Some(flow),
            hardness: Some(hardness),
            lr_center_weight: None,
            auto_mask: false,
            auto_mask_version: None,
            dab_version: version,
            pressure_size: false,
            pressure_flow: false,
            frame: None,
        };
        // Per row across the stroke's middle: the largest swing along it and
        // the mean; the end's coverage on the centre line.
        let measure = |mask: &GrayImage| {
            let (from, to) = ((x0 + 2.0 * r as f64) as u32, (x1 - 2.0 * r as f64) as u32);
            let (mut ripple, mut means) = (0u8, Vec::new());
            for y in 0..h {
                let row: Vec<u8> = (from..to).map(|x| mask.get_pixel(x, y)[0]).collect();
                ripple = ripple.max(row.iter().max().unwrap() - row.iter().min().unwrap());
                means.push(row.iter().map(|&v| v as f32).sum::<f32>() / row.len() as f32);
            }
            (ripple, means, mask.get_pixel(x1 as u32, cy as u32)[0])
        };
        for (hardness, flow) in [(50.0, 100.0), (75.0, 100.0), (50.0, 30.0), (25.0, 100.0)] {
            let v1 = painted(&[line(hardness, flow, None)], w, h);
            assert_eq!(v1, painted(&[line(hardness, flow, Some(1))], w, h));
            let v2 = painted(&[line(hardness, flow, Some(2))], w, h);
            let ((ripple1, means1, end1), (ripple2, means2, end2)) = (measure(&v1), measure(&v2));
            let width = means1
                .iter()
                .zip(&means2)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            println!(
                "hardness {hardness} flow {flow}: ripple {ripple1} -> {ripple2}, mean across {width:.2}, end {end1} -> {end2}"
            );
            assert!(
                ripple2 <= 2,
                "hardness {hardness} flow {flow}: ripple {ripple2}"
            );
            assert!(
                ripple1 >= 4,
                "hardness {hardness} flow {flow}: v1 ripple {ripple1}"
            );
            assert!(
                width <= 1.5,
                "hardness {hardness} flow {flow}: mean moved {width}"
            );
            assert!(
                end2 >= end1,
                "hardness {hardness} flow {flow}: end {end1} -> {end2}"
            );
        }
        // Hardness 50 at full flow: version 1 stopped short of the last point.
        let end = |version| measure(&painted(&[line(50.0, 100.0, version)], w, h)).2;
        assert!(
            end(None) < 240 && end(Some(2)) == 255,
            "{} {}",
            end(None),
            end(Some(2))
        );
    }

    fn stroke_line(points: &[(f64, f64)]) -> BrushLine {
        BrushLine {
            tool: "brush".into(),
            brush_size: 111.5,
            points: points
                .iter()
                .map(|&(x, y)| Point { x, y, p: None })
                .collect(),
            feather: 0.5,
            opacity: None,
            flow: None,
            hardness: None,
            lr_center_weight: None,
            auto_mask: false,
            auto_mask_version: None,
            dab_version: None,
            pressure_size: false,
            pressure_flow: false,
            frame: None,
        }
    }

    #[test]
    #[ignore = "timing probe; synthetic preview-sized brush strokes, fused vs two-pass"]
    fn stroke_rasterization_timing_probe() {
        // The in-app fixture geometry: 4032x3024 source, 2304x1728 preview.
        let (width, height, scale) = (2304, 1728, 0.5714286_f32);
        let short = stroke_line(
            &(0..13)
                .map(|i| (1300.0 + i as f64 * 36.0, 1000.0 + (i % 3) as f64 * 10.0))
                .collect::<Vec<_>>(),
        );
        let diagonal = stroke_line(
            &(0..21)
                .map(|i| (150.0 + i as f64 * 185.0, 2840.0 - i as f64 * 68.5))
                .collect::<Vec<_>>(),
        );
        let base = GrayImage::from_fn(width, height, |x, y| {
            Luma([((x / 64 + y / 64) % 3 * 90) as u8])
        });
        for (name, line) in [("short", &short), ("diagonal", &diagonal)] {
            for frame in 0..14 {
                let order = if frame % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                };
                for fused in order {
                    let mut mask = base.clone();
                    let start = std::time::Instant::now();
                    if fused {
                        apply_brush_line(&mut mask, line, scale, (0.0, 0.0), None);
                    } else {
                        reference_apply_brush_line(&mut mask, line, scale, (0.0, 0.0));
                    }
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    std::hint::black_box(&mask);
                    if frame >= 2 {
                        println!(
                            "{}",
                            serde_json::json!({"probe":"stroke_rasterization","stroke":name,
                                "path":if fused {"fused"} else {"two_pass"},"frame":frame-2,"ms":ms})
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "timing probe; synthetic preview-sized brush strokes, flow 100% vs 50%"]
    fn flow_stroke_timing_probe() {
        let (width, height, scale) = (2304, 1728, 0.5714286_f32);
        let short = stroke_line(
            &(0..13)
                .map(|i| (1300.0 + i as f64 * 36.0, 1000.0 + (i % 3) as f64 * 10.0))
                .collect::<Vec<_>>(),
        );
        let diagonal = stroke_line(
            &(0..21)
                .map(|i| (150.0 + i as f64 * 185.0, 2840.0 - i as f64 * 68.5))
                .collect::<Vec<_>>(),
        );
        for (name, line) in [("short", short), ("diagonal", diagonal)] {
            for flow in [None, Some(50.0)] {
                let line = BrushLine {
                    flow,
                    ..line.clone()
                };
                let mut times = Vec::new();
                for _ in 0..14 {
                    let mut mask = GrayImage::new(width, height);
                    let start = std::time::Instant::now();
                    apply_brush_line(&mut mask, &line, scale, (0.0, 0.0), None);
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                    std::hint::black_box(&mask);
                }
                times.sort_by(f64::total_cmp);
                println!(
                    "{}",
                    serde_json::json!({"probe":"flow_stroke","stroke":name,
                        "flow":flow.unwrap_or(100.0),"median_ms":times[7],"max_ms":times[13]})
                );
            }
        }
    }
}

#[cfg(test)]
mod shape_bitmap_timing {
    use super::*;
    use std::time::Instant;

    fn definition(sub_mask: Value, opacity: f32) -> MaskDefinition {
        serde_json::from_value(serde_json::json!({
            "id": "shape", "name": "shape", "visible": true, "invert": false,
            "opacity": opacity, "adjustments": {}, "subMasks": [sub_mask]
        }))
        .unwrap()
    }

    #[test]
    #[ignore = "timing probe; run with --release --ignored --nocapture"]
    fn shape_drag_mask_timing_probe() {
        // Per pointer move of a layer at 25% opacity with the mask overlay:
        // its 100% mask for the overlay, then its own (derived) bitmap.
        let (width, height) = (1808, 1356);
        let scale = width as f32 / 4032.0;
        let cache = std::sync::Mutex::new(MaskBitmapCache::new());
        for shape in ["radial", "linear"] {
            let mut times = Vec::new();
            for step in 0..24 {
                let t = step as f64;
                let sub_mask = if shape == "radial" {
                    serde_json::json!({"id":"r","type":"radial","visible":true,"invert":false,
                        "opacity":100,"mode":"additive","parameters":{"centerX":2016.0 + 9.0 * t,
                        "centerY":1512.0 + 5.0 * t,"radiusX":1008,"radiusY":756,"rotation":26,"feather":0.5}})
                } else {
                    serde_json::json!({"id":"l","type":"linear","visible":true,"invert":false,
                        "opacity":100,"mode":"additive","parameters":{"startX":328.0 + 6.0 * t,
                        "startY":11.0 + 14.0 * t,"endX":2092.0 + 6.0 * t,"endY":745.0 + 14.0 * t,"range":500}})
                };
                let def = definition(sub_mask, 25.0);
                let full = MaskDefinition {
                    opacity: 100.0,
                    ..def.clone()
                };
                let start = Instant::now();
                cached_or_generated_mask(
                    &cache,
                    &full,
                    width,
                    height,
                    scale,
                    (0.0, 0.0),
                    &PhotoGeometry::default(),
                    None,
                )
                .unwrap();
                cached_or_generated_mask(
                    &cache,
                    &def,
                    width,
                    height,
                    scale,
                    (0.0, 0.0),
                    &PhotoGeometry::default(),
                    None,
                )
                .unwrap();
                times.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            times.sort_by(f64::total_cmp);
            println!(
                "{}",
                serde_json::json!({"probe":"shape_drag_masks","shape":shape,
                    "median_ms":times[times.len() / 2],"max_ms":times[times.len() - 1]})
            );
        }
    }

    #[test]
    #[ignore = "timing probe; run with --release --ignored --nocapture"]
    fn shape_bitmap_timing_probe() {
        let (width, height) = (1808, 1356);
        let scale = width as f32 / 4032.0;
        let shapes = [
            (
                "radial",
                serde_json::json!({"id":"r","type":"radial","visible":true,"invert":false,
                "opacity":100,"mode":"additive","parameters":{"centerX":2016,"centerY":1512,
                "radiusX":1008,"radiusY":756,"rotation":26,"feather":0.5}}),
            ),
            (
                "linear",
                serde_json::json!({"id":"l","type":"linear","visible":true,"invert":false,
                "opacity":100,"mode":"additive","parameters":{"startX":328,"startY":11,
                "endX":2092,"endY":745,"range":500}}),
            ),
        ];
        for (name, sub_mask) in shapes {
            for opacity in [100.0, 25.0] {
                let def = definition(sub_mask.clone(), opacity);
                let mut times = Vec::new();
                for _ in 0..12 {
                    let start = Instant::now();
                    std::hint::black_box(generate_mask_bitmap(
                        &def,
                        width,
                        height,
                        scale,
                        (0.0, 0.0),
                        &PhotoGeometry::default(),
                        None,
                    ));
                    times.push(start.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(f64::total_cmp);
                println!(
                    "{}",
                    serde_json::json!({"probe":"shape_bitmap","shape":name,"opacity":opacity,
                        "median_ms":times[times.len() / 2],"min_ms":times[0]})
                );
            }
        }
    }
}

/// The serial radial and linear rasterizers before rows ran in parallel,
/// kept as byte-exact references.
#[cfg(test)]
mod serial_shapes {
    use super::*;

    pub(super) fn serial_radial_bitmap(
        params_value: &Value,
        width: u32,
        height: u32,
        scale: f32,
        crop_offset: (f32, f32),
    ) -> GrayImage {
        let params: RadialMaskParameters =
            serde_json::from_value(params_value.clone()).unwrap_or_default();
        let mut mask = GrayImage::new(width, height);

        let center_x = (params.center_x as f32 * scale - crop_offset.0) as i32;
        let center_y = (params.center_y as f32 * scale - crop_offset.1) as i32;
        let radius_x = params.radius_x as f32 * scale;
        let radius_y = params.radius_y as f32 * scale;
        let rotation_rad = params.rotation * PI / 180.0;

        for y in 0..height {
            for x in 0..width {
                let dx = x as f32 - center_x as f32;
                let dy = y as f32 - center_y as f32;

                let cos_rot = rotation_rad.cos();
                let sin_rot = rotation_rad.sin();

                let rot_dx = dx * cos_rot + dy * sin_rot;
                let rot_dy = -dx * sin_rot + dy * cos_rot;

                let norm_x = rot_dx / radius_x.max(0.01);
                let norm_y = rot_dy / radius_y.max(0.01);

                let dist = (norm_x.powi(2) + norm_y.powi(2)).sqrt();

                let inner_bound = 1.0 - params.feather.clamp(0.0, 1.0);
                let intensity = 1.0 - (dist - inner_bound) / (1.0 - inner_bound).max(0.01);
                let clamped_intensity = intensity.clamp(0.0, 1.0);

                mask.put_pixel(x, y, Luma([(clamped_intensity * 255.0) as u8]));
            }
        }

        mask
    }

    pub(super) fn serial_linear_bitmap(
        params_value: &Value,
        width: u32,
        height: u32,
        scale: f32,
        crop_offset: (f32, f32),
    ) -> GrayImage {
        let params: LinearMaskParameters =
            serde_json::from_value(params_value.clone()).unwrap_or_default();
        let mut mask = GrayImage::new(width, height);

        let start_x = params.start_x as f32 * scale - crop_offset.0;
        let start_y = params.start_y as f32 * scale - crop_offset.1;
        let end_x = params.end_x as f32 * scale - crop_offset.0;
        let end_y = params.end_y as f32 * scale - crop_offset.1;
        let range = params.range * scale;
        let range_end = params.range_end.map(|range_end| range_end * scale);

        let line_vec_x = end_x - start_x;
        let line_vec_y = end_y - start_y;

        let len_sq = line_vec_x.powi(2) + line_vec_y.powi(2);

        if len_sq < 0.01 {
            return mask;
        }

        let perp_vec_x = -line_vec_y / len_sq.sqrt();
        let perp_vec_y = line_vec_x / len_sq.sqrt();

        let half_width = range.max(0.01);
        let end_half_width = range_end.map_or(half_width, |range_end| range_end.max(0.01));

        for y_u in 0..height {
            for x_u in 0..width {
                let x = x_u as f32;
                let y = y_u as f32;

                let pixel_vec_x = x - start_x;
                let pixel_vec_y = y - start_y;

                let dist_perp = pixel_vec_x * perp_vec_x + pixel_vec_y * perp_vec_y;

                // Negative distances lie on the start (100%) side.
                let t = if dist_perp > 0.0 {
                    dist_perp / end_half_width
                } else {
                    dist_perp / half_width
                };

                let intensity = 0.5 - t * 0.5;

                let clamped_intensity = intensity.clamp(0.0, 1.0);

                mask.put_pixel(x_u, y_u, Luma([(clamped_intensity * 255.0) as u8]));
            }
        }

        mask
    }
}

#[cfg(test)]
mod derived_opacity_tests {
    use super::*;

    #[test]
    fn partial_opacity_masks_derive_exactly_from_full_opacity() {
        let cache = std::sync::Mutex::new(MaskBitmapCache::new());
        for (opacity, invert) in [(25.0, false), (60.0, true), (0.0, false), (99.5, true)] {
            let def: MaskDefinition = serde_json::from_value(serde_json::json!({
                "id": "layer", "name": "layer", "visible": true, "invert": invert,
                "opacity": opacity, "adjustments": {}, "subMasks": [
                    {"id":"r","type":"radial","visible":true,"invert":false,"opacity":80,
                     "mode":"additive","parameters":{"centerX":300,"centerY":200,"radiusX":250,
                     "radiusY":160,"rotation":17,"feather":0.6}},
                    {"id":"l","type":"linear","visible":true,"invert":true,"opacity":100,
                     "mode":"subtractive","parameters":{"startX":20,"startY":30,"endX":500,
                     "endY":260,"range":90}}]
            }))
            .unwrap();
            let full = MaskDefinition {
                opacity: 100.0,
                ..def.clone()
            };
            let geometry = (517, 389, 0.8, (12.0, -7.0));
            let expected = generate_mask_bitmap(
                &def,
                geometry.0,
                geometry.1,
                geometry.2,
                geometry.3,
                &PhotoGeometry::default(),
                None,
            )
            .unwrap();
            // Cached full opacity first, as the mask overlay leaves it.
            cached_or_generated_mask(
                &cache,
                &full,
                geometry.0,
                geometry.1,
                geometry.2,
                geometry.3,
                &PhotoGeometry::default(),
                None,
            )
            .unwrap();
            let derived = cached_or_generated_mask(
                &cache,
                &def,
                geometry.0,
                geometry.1,
                geometry.2,
                geometry.3,
                &PhotoGeometry::default(),
                None,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                *derived,
                expected.into(),
                "opacity {opacity}, invert {invert}"
            );
        }
    }
}

#[cfg(test)]
mod parallel_shape_tests {
    use super::*;

    #[test]
    fn parallel_shapes_match_the_serial_rasterizers() {
        let mut state = 0x2545_f491_u32;
        let mut next = move |range: f64| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state as f64 / u32::MAX as f64) * range
        };
        let mut checked = 0;
        for (width, height) in [(1, 1), (7, 3), (517, 389), (1808, 1356)] {
            for _ in 0..6 {
                let scale = 0.2 + next(1.2) as f32;
                let crop = (next(300.0) as f32, next(200.0) as f32);
                let radial = serde_json::json!({"centerX": next(4000.0), "centerY": next(3000.0),
                    "radiusX": next(2000.0), "radiusY": next(1500.0), "rotation": next(360.0) - 180.0,
                    "feather": next(1.2) - 0.1});
                assert_eq!(
                    generate_radial_bitmap(&radial, width, height, scale, crop),
                    serial_shapes::serial_radial_bitmap(&radial, width, height, scale, crop),
                    "radial {radial}"
                );
                let mut linear = serde_json::json!({"startX": next(4000.0), "startY": next(3000.0),
                    "endX": next(4000.0), "endY": next(3000.0), "range": next(800.0)});
                if next(1.0) > 0.5 {
                    linear["rangeEnd"] = serde_json::json!(next(800.0));
                }
                assert_eq!(
                    generate_linear_bitmap(&linear, width, height, scale, crop),
                    serial_shapes::serial_linear_bitmap(&linear, width, height, scale, crop),
                    "linear {linear}"
                );
                checked += 2;
            }
        }
        assert_eq!(checked, 48);
    }

    #[test]
    fn luma_range_coverage_is_full_inside_and_fades_smoothly_outside() {
        assert_eq!(luma_range_coverage(50.0, 40.0, 60.0, 10.0), 1.0);
        assert_eq!(luma_range_coverage(40.0, 40.0, 60.0, 10.0), 1.0);
        assert_eq!(luma_range_coverage(29.0, 40.0, 60.0, 10.0), 0.0);
        assert_eq!(luma_range_coverage(35.0, 40.0, 60.0, 10.0), 0.5);
        assert!(
            luma_range_coverage(62.0, 40.0, 60.0, 10.0)
                > luma_range_coverage(68.0, 40.0, 60.0, 10.0)
        );
        // Swapped bounds mean the same range; no falloff is a hard edge.
        assert_eq!(luma_range_coverage(50.0, 60.0, 40.0, 0.0), 1.0);
        assert_eq!(luma_range_coverage(39.9, 40.0, 60.0, 0.0), 0.0);
    }

    #[test]
    fn luma_range_selects_tones_and_follows_the_photos_orientation() {
        // Left half black, right half white.
        let mut image = image::RgbImage::new(8, 4);
        for (x, _, pixel) in image.enumerate_pixels_mut() {
            *pixel = image::Rgb(if x < 4 { [0, 0, 0] } else { [255, 255, 255] });
        }
        let source =
            ParametricMaskSource::new(Arc::new(DynamicImage::ImageRgb8(image)), (8.0, 4.0))
                .unwrap();
        let highlights = serde_json::json!({"rangeLow": 70, "rangeHigh": 100, "falloff": 0});
        let mask =
            generate_luma_range_bitmap(&highlights, 8, 4, 1.0, (0.0, 0.0), Some(&source)).unwrap();
        assert_eq!(mask.get_pixel(1, 1)[0], 0);
        assert_eq!(mask.get_pixel(6, 1)[0], 255);
        // Turned 180°, the white half is on the left of the displayed photo.
        let turned = source.with_geometry(&serde_json::json!({"orientationSteps": 2}));
        let mask =
            generate_luma_range_bitmap(&highlights, 8, 4, 1.0, (0.0, 0.0), Some(&turned)).unwrap();
        assert_eq!(mask.get_pixel(1, 1)[0], 255);
        assert_eq!(mask.get_pixel(6, 1)[0], 0);
        // Without a source the mask cannot be made (callers report it).
        assert!(generate_luma_range_bitmap(&highlights, 8, 4, 1.0, (0.0, 0.0), None).is_none());
    }

    #[test]
    fn sampled_tones_are_covered_by_the_luma_range_on_turned_photos() {
        // Grey levels rising to the right of the source (0–255 over 512 px).
        let image = image::RgbImage::from_fn(512, 256, |x, _| {
            let v = (x / 2) as u8;
            image::Rgb([v, v, v])
        });
        let source =
            ParametricMaskSource::new(Arc::new(DynamicImage::ImageRgb8(image)), (512.0, 256.0))
                .unwrap();
        for geometry in [
            serde_json::json!({}),
            serde_json::json!({"orientationSteps": 1, "flipHorizontal": true}),
            serde_json::json!({"orientationSteps": 2, "rotation": 7.5}),
            serde_json::json!({"orientationSteps": 3, "flipVertical": true}),
        ] {
            let turned = source.clone().with_geometry(&geometry);
            let steps = geometry["orientationSteps"].as_u64().unwrap_or(0);
            let (w, h) = if steps % 2 == 1 {
                (256, 512)
            } else {
                (512, 256)
            };
            // A click: a narrow spread around the tone under the pointer, which
            // a luma range around it covers in the displayed photo.
            let (px, py) = (w / 3, h / 2 + 5);
            let point = sample_luma_tones(&turned, px as f32, py as f32, 0.0, 0.0).unwrap();
            assert!(point.low <= point.median && point.median <= point.high);
            assert!(point.high - point.low < 2.0, "{geometry}: {point:?}");
            let range = serde_json::json!({
                "rangeLow": point.median - 0.5,
                "rangeHigh": point.median + 0.5,
                "falloff": 0,
            });
            let mask =
                generate_luma_range_bitmap(&range, w, h, 1.0, (0.0, 0.0), Some(&turned)).unwrap();
            assert_eq!(mask.get_pixel(px, py)[0], 255, "{geometry}: {point:?}");
            // An area: its darkest to brightest tones (robust), wider than a click.
            let area = sample_luma_tones(&turned, 20.0, 20.0, 200.0, 200.0).unwrap();
            assert!(area.high - area.low > 10.0, "{geometry}: {area:?}");
        }
        // Outside the photo nothing is sampled.
        assert!(sample_luma_tones(&source, -50.0, -50.0, 0.0, 0.0).is_none());
        assert!(sample_luma_tones(&source, f32::NAN, 1.0, 0.0, 0.0).is_none());
    }
}

#[cfg(test)]
mod color_range_tools {
    use super::*;

    fn colour_range(samples: serde_json::Value, refine: f32, image: &DynamicImage) -> GrayImage {
        let (w, h) = image.dimensions();
        let source =
            ParametricMaskSource::new(Arc::new(image.clone()), (w as f64, h as f64)).unwrap();
        let def: MaskDefinition = serde_json::from_value(serde_json::json!({
            "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
            "adjustments": {}, "subMasks": [{"id": "c", "type": "color", "visible": true,
            "mode": "additive", "parameters": {"targetX": 2.0, "targetY": 2.0, "samples": samples,
            "refine": refine, "feather": 0.0}}]
        }))
        .unwrap();
        generate_mask_bitmap(
            &def,
            w,
            h,
            1.0,
            (0.0, 0.0),
            &PhotoGeometry::default(),
            Some(&source),
        )
        .unwrap()
    }

    #[test]
    fn colour_range_selects_the_sampled_colours() {
        // Red, orange, blue and grey quarters.
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(64, 64, |x, y| {
            match (x < 32, y < 32) {
                (true, true) => Rgba([200, 40, 30, 255]),
                (false, true) => Rgba([210, 110, 40, 255]),
                (true, false) => Rgba([30, 60, 190, 255]),
                (false, false) => Rgba([120, 120, 120, 255]),
            }
        }));
        let at = |m: &GrayImage, x, y| m.get_pixel(x, y)[0];
        let red = colour_range(serde_json::json!([{"x": 10.0, "y": 10.0}]), 50.0, &image);
        assert_eq!(at(&red, 10, 10), 255);
        assert!(at(&red, 10, 50) < 10, "blue {}", at(&red, 10, 50));
        assert!(at(&red, 50, 50) < 10, "grey {}", at(&red, 50, 50));
        // More samples take the strongest coverage.
        let both = colour_range(
            serde_json::json!([{"x": 10.0, "y": 10.0}, {"x": 10.0, "y": 50.0}]),
            50.0,
            &image,
        );
        assert!(at(&both, 10, 50) > 250 && at(&both, 10, 10) > 250);
        // An area sample covers the colours inside it.
        let area = colour_range(
            serde_json::json!([{"x": 20.0, "y": 0.0, "width": 24.0, "height": 20.0}]),
            50.0,
            &image,
        );
        assert!(at(&area, 10, 10) > 200 && at(&area, 50, 10) > 200);
        // Refine widens the range: orange next to red.
        let narrow = colour_range(serde_json::json!([{"x": 10.0, "y": 10.0}]), 0.0, &image);
        let wide = colour_range(serde_json::json!([{"x": 10.0, "y": 10.0}]), 100.0, &image);
        assert!(at(&wide, 50, 10) > at(&narrow, 50, 10));
    }

    /// Tool: colour range masks of `$CR_IMAGE` (an sRGB rendering) for the
    /// samples in `$CR_SAMPLES` ("name:x,y;…", normalised) at Refine 1, 26,
    /// 50.5, 75.25 and 100 (Lightroom's stored 0, .25, .5, .75, 1), written
    /// as PNGs to `$CR_OUT/<name>_<amount>.png`.
    #[test]
    #[ignore = "tool: needs CR_IMAGE, CR_SAMPLES, CR_OUT"]
    fn write_color_range_masks() {
        let image = image::open(std::env::var("CR_IMAGE").unwrap()).unwrap();
        let image = DynamicImage::ImageRgba8(image.to_rgba8());
        let (w, h) = image.dimensions();
        let source = ParametricMaskSource::new(Arc::new(image), (w as f64, h as f64)).unwrap();
        let out = std::path::PathBuf::from(std::env::var("CR_OUT").unwrap());
        for entry in std::env::var("CR_SAMPLES").unwrap().split(';') {
            let (name, xy) = entry.split_once(':').unwrap();
            let (x, y) = xy.split_once(',').unwrap();
            let (x, y): (f64, f64) = (x.parse().unwrap(), y.parse().unwrap());
            for amount in [0, 25, 50, 75, 100] {
                let refine = 1.0 + 99.0 * amount as f32 / 100.0;
                let mut parameters = serde_json::json!({"targetX": x * w as f64,
                    "targetY": y * h as f64, "refine": refine, "feather": 0.0});
                // CR_LEGACY: the inherited RGB tolerance (5–50) at its default feather.
                if std::env::var_os("CR_LEGACY").is_some() {
                    let tolerance = [5.0, 10.0, 20.0, 35.0, 50.0][amount / 25];
                    parameters = serde_json::json!({"targetX": x * w as f64, "targetY": y * h as f64,
                        "tolerance": tolerance, "feather": 35.0});
                }
                let def: MaskDefinition = serde_json::from_value(serde_json::json!({
                    "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
                    "adjustments": {}, "subMasks": [{"id": "c", "type": "color", "visible": true,
                    "mode": "additive", "parameters": parameters}]
                }))
                .unwrap();
                let mask = generate_mask_bitmap(
                    &def,
                    w,
                    h,
                    1.0,
                    (0.0, 0.0),
                    &PhotoGeometry::default(),
                    Some(&source),
                )
                .unwrap();
                mask.save(out.join(format!("{name}_{amount}.png"))).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod auto_mask_tools {
    use super::*;

    #[test]
    fn auto_mask_keeps_a_stroke_to_the_colour_under_it() {
        // Red above y = 32, blue below; a stroke along y = 28 (on red)
        // whose dabs reach well into the blue.
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(96, 64, |_, y| {
            if y < 32 {
                Rgba([200, 40, 30, 255])
            } else {
                Rgba([30, 60, 190, 255])
            }
        }));
        let source = ParametricMaskSource::new(Arc::new(image), (96.0, 64.0)).unwrap();
        let stroke = |auto: bool| {
            let def: MaskDefinition = serde_json::from_value(serde_json::json!({
                "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
                "adjustments": {}, "subMasks": [{"id": "b", "type": "brush", "visible": true,
                "mode": "additive", "parameters": {"lines": [{"tool": "brush", "brushSize": 24.0,
                "points": [{"x": 20.0, "y": 28.0}, {"x": 76.0, "y": 28.0}], "feather": 0.5,
                "hardness": 80.0, "autoMask": auto}]}}]
            }))
            .unwrap();
            assert_eq!(def.requires_warped_image(), auto);
            generate_mask_bitmap(
                &def,
                96,
                64,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::default(),
                Some(&source),
            )
            .unwrap()
        };
        let (plain, auto) = (stroke(false), stroke(true));
        assert!(
            plain.get_pixel(48, 34)[0] > 200,
            "plain {}",
            plain.get_pixel(48, 34)[0]
        );
        assert!(
            auto.get_pixel(48, 34)[0] < 10,
            "auto {}",
            auto.get_pixel(48, 34)[0]
        );
        assert!(auto.get_pixel(48, 28)[0] > 240 && auto.get_pixel(48, 24)[0] > 200);
    }

    /// One stroke along y = `y` over `image` (96 × 64), Capture One's tip at
    /// 80 % hardness, with the given Auto Mask setting.
    fn auto_mask_stroke(image: DynamicImage, y: f64, auto_mask: Value) -> GrayImage {
        let source = ParametricMaskSource::new(Arc::new(image), (96.0, 64.0)).unwrap();
        let mut line = serde_json::json!({"tool": "brush", "brushSize": 24.0,
            "points": [{"x": 20.0, "y": y}, {"x": 76.0, "y": y}], "feather": 0.5,
            "hardness": 80.0});
        for (key, value) in auto_mask.as_object().unwrap() {
            line[key] = value.clone();
        }
        let def: MaskDefinition = serde_json::from_value(serde_json::json!({
            "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
            "adjustments": {}, "subMasks": [{"id": "b", "type": "brush", "visible": true,
            "mode": "additive", "parameters": {"lines": [line]}}]
        }))
        .unwrap();
        generate_mask_bitmap(
            &def,
            96,
            64,
            1.0,
            (0.0, 0.0),
            &PhotoGeometry::default(),
            Some(&source),
        )
        .unwrap()
    }

    fn rows(top: Rgba<u8>, bottom: Rgba<u8>, split: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(96, 64, |_, y| {
            if y < split { top } else { bottom }
        }))
    }

    #[test]
    fn edge_aware_auto_mask_separates_a_low_contrast_edge() {
        // Two greys about 11 ΔE apart: version 1 keeps most of the other
        // one, version 2 keeps to the side under the stroke.
        let image = rows(Rgba([128, 128, 128, 255]), Rgba([100, 106, 116, 255]), 32);
        let v1 = auto_mask_stroke(image.clone(), 28.0, serde_json::json!({"autoMask": true}));
        let v2 = auto_mask_stroke(
            image,
            28.0,
            serde_json::json!({"autoMask": true, "autoMaskVersion": 2}),
        );
        assert!(
            v1.get_pixel(48, 35)[0] > 150,
            "v1 {}",
            v1.get_pixel(48, 35)[0]
        );
        assert!(
            v2.get_pixel(48, 35)[0] < 5,
            "v2 {}",
            v2.get_pixel(48, 35)[0]
        );
        assert!(v2.get_pixel(48, 28)[0] > 245 && v2.get_pixel(48, 31)[0] > 240);
    }

    #[test]
    fn edge_aware_auto_mask_keeps_a_soft_edges_coverage_in_linear_light() {
        // A defocused edge: red mixing into blue in linear light over rows
        // 26..34 (sRGB-encoded), within the dabs' reach. The mask follows the
        // red's share.
        let encode = |linear: f32| {
            let v = if linear <= 0.003_130_8 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            (v * 255.0).round() as u8
        };
        let (red, blue) = ([0.6f32, 0.05, 0.03], [0.03f32, 0.08, 0.5]);
        let share = |y: u32| 1.0 - ((y as f32 - 26.0) / 8.0).clamp(0.0, 1.0);
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(96, 64, |_, y| {
            let a = share(y);
            let c = |i: usize| encode(red[i] * a + blue[i] * (1.0 - a));
            Rgba([c(0), c(1), c(2), 255])
        }));
        let mask = auto_mask_stroke(
            image,
            24.0,
            serde_json::json!({"autoMask": true, "autoMaskVersion": 2}),
        );
        for y in [26u32, 28, 30, 32] {
            let kept = mask.get_pixel(48, y)[0] as f32 / 255.0;
            assert!(
                (kept - share(y)).abs() < 0.08,
                "row {y}: {kept} vs {}",
                share(y)
            );
        }
        assert!(mask.get_pixel(48, 36)[0] < 5);
    }

    #[test]
    fn edge_aware_auto_mask_drops_two_backgrounds() {
        // Brown roof (rows 24..36) between sky above and a white wall below;
        // a stroke along the roof reaches both.
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(96, 64, |_, y| match y {
            0..24 => Rgba([170, 200, 235, 255]),
            24..36 => Rgba([110, 50, 35, 255]),
            _ => Rgba([235, 232, 225, 255]),
        }));
        let mask = auto_mask_stroke(
            image,
            30.0,
            serde_json::json!({"autoMask": true, "autoMaskVersion": 2}),
        );
        assert!(mask.get_pixel(48, 30)[0] > 245);
        assert!(mask.get_pixel(48, 21)[0] < 5 && mask.get_pixel(48, 39)[0] < 5);
    }

    #[test]
    fn auto_mask_version_stays_out_of_strokes_without_it() {
        // Old recipes and Lightroom imports carry no version and keep it so.
        let line: BrushLine = serde_json::from_value(serde_json::json!({"tool": "brush",
            "brushSize": 24.0, "points": [], "autoMask": true}))
        .unwrap();
        assert_eq!(line.auto_mask_version, None);
        assert!(
            serde_json::to_value(&line)
                .unwrap()
                .get("autoMaskVersion")
                .is_none()
        );
        let image = rows(Rgba([200, 40, 30, 255]), Rgba([30, 60, 190, 255]), 32);
        assert_eq!(
            auto_mask_stroke(image.clone(), 28.0, serde_json::json!({"autoMask": true})),
            auto_mask_stroke(
                image,
                28.0,
                serde_json::json!({"autoMask": true, "autoMaskVersion": 1})
            )
        );
    }

    #[test]
    fn cached_auto_mask_strokes_follow_the_photo_they_read() {
        // The same strokes over the same pixels, read the other way up or
        // through the raw curve: the brush prefix cache must not hand back
        // the mask of the photo as it was read before.
        let image = Arc::new(DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(
            96,
            64,
            |x, y| Rgba([(y * 4) as u8, 120, (x * 2) as u8, 255]),
        )));
        let def: MaskDefinition = serde_json::from_value(serde_json::json!({
            "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
            "adjustments": {}, "subMasks": [{"id": "b", "type": "brush", "visible": true,
            "mode": "additive", "parameters": {"lines": [{"tool": "brush", "brushSize": 24.0,
            "points": [{"x": 20.0, "y": 28.0}, {"x": 76.0, "y": 28.0}], "feather": 0.5,
            "hardness": 80.0, "autoMask": true, "autoMaskVersion": 3}]}}]
        }))
        .unwrap();
        let render = |source: &ParametricMaskSource| {
            generate_mask_bitmap(
                &def,
                96,
                64,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::default(),
                Some(source),
            )
            .unwrap()
        };
        let upright = ParametricMaskSource::new(Arc::clone(&image), (96.0, 64.0)).unwrap();
        let changed = [
            upright
                .clone()
                .with_geometry(&serde_json::json!({"flipVertical": true})),
            upright.clone().with_raw_curve(true),
        ];
        for source in &changed {
            clear_brush_prefixes();
            let before = render(&upright);
            let cached = render(source);
            clear_brush_prefixes();
            let fresh = render(source);
            assert_ne!(before, fresh, "the change must alter what the stroke keeps");
            assert_eq!(cached, fresh);
        }
    }

    /// A stroke 10 px inside an edge: a dark background above it, below it a
    /// pale subject with dark green gaps (15 % of its pixels).
    fn fur_stroke(version: u32) -> GrayImage {
        let (w, h) = (320u32, 160u32);
        let edge = 60u32;
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(w, h, |x, y| {
            if y < edge {
                Rgba([30, 30, 34, 255])
            } else if (x * 7 + y * 3) % 13 < 2 {
                Rgba([20, 70, 40, 255])
            } else {
                Rgba([200, 170, 140, 255])
            }
        }));
        let source = ParametricMaskSource::new(Arc::new(image), (w as f64, h as f64)).unwrap();
        let points: Vec<Value> = (0..=12)
            .map(|i| serde_json::json!({"x": 40.0 + i as f64 * 20.0, "y": edge as f64 + 10.0}))
            .collect();
        let def: MaskDefinition = serde_json::from_value(serde_json::json!({
            "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
            "adjustments": {}, "subMasks": [{"id": "b", "type": "brush", "visible": true,
            "mode": "additive", "parameters": {"lines": [{"tool": "brush", "brushSize": 80.0,
            "points": points, "feather": 0.5, "hardness": 50.0, "autoMask": true,
            "autoMaskVersion": version}]}}]
        }))
        .unwrap();
        generate_mask_bitmap(
            &def,
            w,
            h,
            1.0,
            (0.0, 0.0),
            &PhotoGeometry::default(),
            Some(&source),
        )
        .unwrap()
    }

    #[test]
    fn auto_mask_v3_keeps_gaps_inside_the_subject() {
        // Version 2 lets the gaps go with the background they look like;
        // version 3 keeps the subject's side of the stroke, gaps and all, and
        // still leaves the background above the edge.
        let mean = |mask: &GrayImage, rows: std::ops::Range<u32>| {
            let mut sum = 0.0;
            let mut n = 0.0;
            for y in rows {
                for x in 100..220 {
                    sum += mask.get_pixel(x, y)[0] as f64 / 255.0;
                    n += 1.0;
                }
            }
            sum / n
        };
        let (v2, v3) = (fur_stroke(2), fur_stroke(3));
        let (inside_v2, inside_v3) = (mean(&v2, 64..90), mean(&v3, 64..90));
        assert!(inside_v2 < 0.9, "version 2 inside {inside_v2}");
        assert!(inside_v3 > 0.98, "version 3 inside {inside_v3}");
        let outside = mean(&v3, 30..56);
        assert!(outside < 0.02, "version 3 outside {outside}");
    }

    /// M3 of notes plans/local-adjustments-g29-g31.md: the brush stage per
    /// pointer move while a stroke grows (the newest stroke is redrawn on
    /// every move), on a 1536 × 1024 preview of a 7728 × 5152 photo, as in
    /// the WP0 baseline (reference/local-masks-baseline.md): 60 points over
    /// the width, Capture One's tip at 50 %, brush diameters of 300 and 1500
    /// photo pixels, Auto Mask off and versions 1 to 3. `$AM_IMAGE`
    /// (any photo, resized to the preview) replaces the synthetic one.
    #[test]
    #[ignore = "timing: run with --release"]
    fn measures_auto_mask_stroke_cost() {
        let (w, h) = (1536u32, 1024u32);
        let scale = w as f32 / 7728.0;
        let image = match std::env::var("AM_IMAGE") {
            Ok(path) => {
                image::open(path)
                    .unwrap()
                    .resize_exact(w, h, image::imageops::FilterType::Triangle)
            }
            Err(_) => DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(w, h, |x, y| {
                let edge = (y as f32) < 500.0 + 120.0 * (x as f32 / 90.0).sin();
                let n = ((x * 7919 + y * 104_729) % 37) as u8;
                if edge {
                    Rgba([150 + n, 170 + n, 200, 255])
                } else {
                    Rgba([90 + n, 60, 40 + n, 255])
                }
            })),
        };
        let source = ParametricMaskSource::new(Arc::new(image), (7728.0, 5152.0)).unwrap();
        for size in [300.0f64, 1500.0] {
            for (label, extra) in [
                ("off", serde_json::json!({})),
                ("v1", serde_json::json!({"autoMask": true})),
                (
                    "v2",
                    serde_json::json!({"autoMask": true, "autoMaskVersion": 2}),
                ),
                (
                    "v3",
                    serde_json::json!({"autoMask": true, "autoMaskVersion": 3}),
                ),
            ] {
                let mut times = Vec::new();
                for stroke in 0..5 {
                    clear_brush_prefixes();
                    let y = 2000.0 + stroke as f64 * 300.0;
                    for n in 2..=60usize {
                        let points: Vec<Value> = (0..n)
                            .map(|i| serde_json::json!({"x": 800.0 + i as f64 * 100.0, "y": y + 40.0 * (i as f64 / 6.0).sin()}))
                            .collect();
                        let mut line = serde_json::json!({"tool": "brush", "brushSize": size,
                            "points": points, "feather": 0.5, "hardness": 50.0});
                        for (key, value) in extra.as_object().unwrap() {
                            line[key] = value.clone();
                        }
                        let params = serde_json::json!({"lines": [line]});
                        let started = std::time::Instant::now();
                        let _ = generate_brush_bitmap(
                            &params,
                            w,
                            h,
                            scale,
                            (0.0, 0.0),
                            Shown::default(),
                            Some(&source),
                        );
                        times.push(started.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                times.sort_by(f64::total_cmp);
                let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
                println!(
                    "diameter {size} px, auto mask {label}: brush stage per move p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms ({} moves)",
                    at(0.5),
                    at(0.95),
                    at(1.0),
                    times.len()
                );
            }
        }
    }

    /// Tool: brush masks of `$AM_IMAGE` for the strokes in `$AM_STROKES`
    /// (JSON {"name": [[x, y], …]}, normalised; Lightroom dabs with radius
    /// `$AM_RADIUS` of the long side, CenterWeight 0), plain and with Auto
    /// Mask, written to `$AM_OUT/<name>_{d,M}.png`.
    #[test]
    #[ignore = "tool: needs AM_IMAGE, AM_STROKES, AM_RADIUS, AM_OUT"]
    fn write_auto_mask_strokes() {
        let image = image::open(std::env::var("AM_IMAGE").unwrap()).unwrap();
        let image = DynamicImage::ImageRgba8(image.to_rgba8());
        let (w, h) = image.dimensions();
        let source = ParametricMaskSource::new(Arc::new(image), (w as f64, h as f64)).unwrap();
        let strokes: serde_json::Map<String, Value> = serde_json::from_str(
            &std::fs::read_to_string(std::env::var("AM_STROKES").unwrap()).unwrap(),
        )
        .unwrap();
        let radius: f64 = std::env::var("AM_RADIUS").unwrap().parse().unwrap();
        let out = std::path::PathBuf::from(std::env::var("AM_OUT").unwrap());
        for (name, points) in strokes {
            let points: Vec<Value> = points
                .as_array()
                .unwrap()
                .iter()
                .map(|p| serde_json::json!({"x": p[0].as_f64().unwrap() * w as f64, "y": p[1].as_f64().unwrap() * h as f64}))
                .collect();
            for (code, auto) in [("d", false), ("M", true)] {
                let def: MaskDefinition = serde_json::from_value(serde_json::json!({
                    "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
                    "adjustments": {}, "subMasks": [{"id": "b", "type": "brush", "visible": true,
                    "mode": "additive", "parameters": {"lines": [{"tool": "brush",
                    "brushSize": 2.0 * radius * w.max(h) as f64, "points": points, "feather": 0.5,
                    "flow": 100.0, "lrCenterWeight": 0.0, "autoMask": auto}]}}]
                }))
                .unwrap();
                let mask = generate_mask_bitmap(
                    &def,
                    w,
                    h,
                    1.0,
                    (0.0, 0.0),
                    &PhotoGeometry::default(),
                    Some(&source),
                )
                .unwrap();
                mask.save(out.join(format!("{name}_{code}.png"))).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod geometry_follow_tests {
    use super::*;
    use crate::image_processing::{apply_coarse_rotation, apply_flip, warp_image_geometry};
    use serde_json::json;

    const KINDS: [&str; 6] = [
        "ai-sky",
        "ai-subject",
        "ai-foreground",
        "ai-people",
        "ai-depth",
        "quick-eraser",
    ];

    fn data_url(mask: &GrayImage) -> String {
        let mut png = Cursor::new(Vec::new());
        mask.write_to(&mut png, image::ImageFormat::Png).unwrap();
        format!(
            "data:image/png;base64,{}",
            general_purpose::STANDARD.encode(png.into_inner())
        )
    }

    /// One AI component of `kind` with `params` (stored geometry, warp).
    fn ai_layer(kind: &str, mask: &GrayImage, params: Value) -> MaskDefinition {
        let mut parameters = json!({
            "maskDataBase64": data_url(mask),
            "startX": 0.0, "startY": 0.0, "endX": 1.0, "endY": 1.0,
            "minDepth": 0.0, "maxDepth": 100.0, "minFade": 0.0, "maxFade": 0.0, "feather": 0.0,
        });
        parameters
            .as_object_mut()
            .unwrap()
            .extend(params.as_object().unwrap().clone());
        serde_json::from_value(json!({
            "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
            "adjustments": {}, "subMasks": [{"id": "a", "type": kind, "visible": true,
            "mode": "additive", "parameters": parameters}]
        }))
        .unwrap()
    }

    /// A mask whose every pixel differs from its neighbours (a misplaced
    /// pixel shows), 60 × 40.
    fn pattern() -> GrayImage {
        GrayImage::from_fn(60, 40, |x, y| Luma([((x * 4 + y * 61) % 251) as u8]))
    }

    /// `shown` is `expected` (the mask turned by the image operations that
    /// turn the photo), pixel for pixel.
    fn assert_turned(shown: &GrayImage, expected: &GrayImage, what: &str) {
        assert_eq!(shown.dimensions(), expected.dimensions(), "{what}");
        assert!(shown == expected, "{what}");
    }

    fn render(
        def: &MaskDefinition,
        adjustments: &Value,
        size: (u32, u32),
        crop: (f32, f32),
    ) -> GrayImage {
        generate_mask_bitmap(
            def,
            size.0,
            size.1,
            1.0,
            crop,
            &PhotoGeometry::from_adjustments(adjustments),
            None,
        )
        .unwrap()
    }

    #[test]
    fn ai_masks_turn_and_flip_with_the_photo() {
        let mask = pattern();
        for kind in KINDS {
            // Made while the photo was shown as it is, or turned: the stored
            // geometry never matters, the bitmap is in the photo's own frame.
            for made_in in [
                json!({}),
                json!({"orientationSteps": 1, "rotation": 4.0, "flipHorizontal": true}),
            ] {
                let def = ai_layer(kind, &mask, made_in.clone());
                let upright = render(&def, &json!({}), (60, 40), (0.0, 0.0));
                for (steps, flip_h, flip_v) in [
                    (0, false, false),
                    (1, false, false),
                    (2, true, false),
                    (3, false, true),
                    (1, true, true),
                ] {
                    let now = json!({"orientationSteps": steps, "flipHorizontal": flip_h, "flipVertical": flip_v});
                    let size = if steps % 2 == 1 { (40, 60) } else { (60, 40) };
                    let shown = render(&def, &now, size, (0.0, 0.0));
                    let expected = apply_flip(
                        apply_coarse_rotation(DynamicImage::ImageLuma8(upright.clone()), steps),
                        flip_h,
                        flip_v,
                    )
                    .into_owned()
                    .into_luma8();
                    assert_turned(
                        &shown,
                        &expected,
                        &format!("{kind} made in {made_in} shown in {now}"),
                    );
                }
            }
        }
    }

    /// For each pixel of the photo of `size` shown in `g` (turned, flipped
    /// and straightened by the image operations themselves), the point of the
    /// photo's own frame it is resampled at, when the resampling read only
    /// pixels of the photo: a photo whose pixels hold their own coordinates
    /// through `apply_coarse_rotation`, `apply_flip` and `apply_rotation`
    /// (Catmull-Rom reproduces a linear ramp exactly).
    fn resampled_at(size: (u32, u32), g: &Value) -> (u32, Vec<Option<(f32, f32)>>) {
        let coordinates =
            DynamicImage::ImageRgba32F(image::ImageBuffer::from_fn(size.0, size.1, |x, y| {
                image::Rgba([x as f32, y as f32, 1.0, 1.0])
            }));
        let steps = g["orientationSteps"].as_u64().unwrap_or(0) as u8;
        let turned = apply_flip(
            apply_coarse_rotation(coordinates, steps),
            g["flipHorizontal"] == true,
            g["flipVertical"] == true,
        );
        let shown = crate::image_processing::apply_rotation(
            turned,
            g["rotation"].as_f64().unwrap_or(0.0) as f32,
            false,
        )
        .to_rgba32f();
        let points = shown
            .pixels()
            .map(|p| ((p[2] - 1.0).abs() < 1e-4).then_some((p[0], p[1])))
            .collect();
        (shown.width(), points)
    }

    /// The photo's pixel nearest a resampled point, unless the point lies
    /// halfway between two (where either is as near).
    fn nearest(point: (f32, f32)) -> Option<(u32, u32)> {
        let tie = |v: f32| (v.fract() - 0.5).abs() < 1e-3;
        (!tie(point.0) && !tie(point.1)).then(|| (point.0.round() as u32, point.1.round() as u32))
    }

    const STRAIGHTENED: [&str; 5] = [
        r#"{"rotation": -6.5, "orientationSteps": 1, "flipHorizontal": true}"#,
        r#"{"rotation": 3.0}"#,
        r#"{"rotation": 17.25, "orientationSteps": 2, "flipVertical": true}"#,
        r#"{"rotation": -0.4, "orientationSteps": 3}"#,
        r#"{"rotation": 44.0, "flipHorizontal": true, "flipVertical": true}"#,
    ];

    /// AI masks on a straightened photo take, for every pixel, the photo's
    /// pixel the straightened photo takes most from (the one nearest the
    /// point it is resampled at), with and without a crop. They rounded the
    /// point down before, so about three pixels in four read a neighbour.
    #[test]
    fn ai_masks_on_straightened_photos_sit_on_the_photos_pixels() {
        let mask = pattern();
        let (mut compared, mut floor_differs) = (0usize, 0usize);
        for g in STRAIGHTENED {
            let g: Value = serde_json::from_str(g).unwrap();
            let (width, points) = resampled_at(mask.dimensions(), &g);
            let height = points.len() as u32 / width;
            for kind in ["ai-sky", "ai-subject", "ai-foreground", "ai-people"] {
                let def = ai_layer(kind, &mask, json!({}));
                let full = render(&def, &g, (width, height), (0.0, 0.0));
                let cropped = render(&def, &g, (width - 9, height - 6), (5.0, 4.0));
                for (i, point) in points.iter().enumerate() {
                    let Some((px, py)) = point.and_then(nearest) else {
                        continue;
                    };
                    let (x, y) = (i as u32 % width, i as u32 / width);
                    let expected = mask.get_pixel(px, py)[0];
                    assert_eq!(
                        full.get_pixel(x, y)[0],
                        expected,
                        "{kind} in {g} at {x},{y}"
                    );
                    if (5..width - 4).contains(&x) && (4..height - 2).contains(&y) {
                        assert_eq!(
                            cropped.get_pixel(x - 5, y - 4)[0],
                            expected,
                            "{kind} cropped"
                        );
                    }
                    compared += 1;
                    let (fx, fy) = point.unwrap();
                    floor_differs +=
                        usize::from((fx.floor(), fy.floor()) != (fx.round(), fy.round()));
                }
            }
        }
        assert!(compared > 20_000, "{compared} pixels compared");
        // The share of pixels that read a neighbour before.
        let share = floor_differs as f64 / compared as f64;
        assert!(share > 0.5, "{share}");
        println!(
            "straightened AI masks: {compared} pixels, {:.1} % moved",
            share * 100.0
        );
    }

    /// Masks read from the photo's pixels (colour and luminance ranges, the
    /// luma range, the colour samples, Auto Mask strokes) on a straightened
    /// photo read the pixel the straightened photo takes most from.
    #[test]
    fn photo_sampled_masks_on_straightened_photos_read_the_shown_pixel() {
        let size = (60u32, 40u32);
        // Every pixel holds its own position.
        let photo = DynamicImage::ImageRgb8(image::RgbImage::from_fn(size.0, size.1, |x, y| {
            image::Rgb([x as u8, y as u8, 0])
        }));
        for g in STRAIGHTENED {
            let g: Value = serde_json::from_str(g).unwrap();
            let source = ParametricMaskSource::new(Arc::new(photo.clone()), (60.0, 40.0))
                .unwrap()
                .with_geometry(&g);
            let mapping = DisplayToSource::new(&source, source.geometry, 1.0);
            let (width, points) = resampled_at(size, &g);
            let mut compared = 0;
            for (i, point) in points.iter().enumerate() {
                let Some(expected) = point.and_then(nearest) else {
                    continue;
                };
                let (x, y) = ((i as u32 % width) as f32, (i as u32 / width) as f32);
                assert_eq!(
                    mapping.pixel(x, mapping.row(y)),
                    Some(expected),
                    "{g} at {x},{y}: {point:?}"
                );
                // Auto Mask reads its colours the same way, cropped too.
                let sampler = SourceSampler::new(&source, 1.0, (3.0, 2.0));
                assert_eq!(
                    sampler.at(x - 3.0, y - 2.0),
                    Some(photo.get_pixel(expected.0, expected.1)),
                    "{g}"
                );
                compared += 1;
            }
            assert!(compared > 1000, "{g}: {compared}");
        }
    }

    #[test]
    fn ai_masks_follow_straightening_and_crop() {
        let mask = pattern();
        let now = json!({"orientationSteps": 1, "rotation": -6.5, "flipHorizontal": true});
        let (width, points) = resampled_at(mask.dimensions(), &now);
        for kind in KINDS {
            let made_upright = ai_layer(kind, &mask, json!({}));
            let made_turned = ai_layer(
                kind,
                &mask,
                json!({"orientationSteps": 3, "rotation": 12.0, "flipVertical": true}),
            );
            let full = render(&made_upright, &now, (40, 60), (0.0, 0.0));
            assert_eq!(
                full,
                render(&made_turned, &now, (40, 60), (0.0, 0.0)),
                "{kind}"
            );
            // The straightened photo: a pixel of the mask lands where the
            // photo's pixel is shown (the kinds that keep values as stored).
            if !matches!(kind, "quick-eraser" | "ai-depth") {
                for (x, y) in [(20u32, 30u32), (10, 12), (30, 45), (25, 8)] {
                    let (px, py) = points[(y * width + x) as usize].and_then(nearest).unwrap();
                    assert_eq!(
                        full.get_pixel(x, y)[0],
                        mask.get_pixel(px, py)[0],
                        "{kind} at {x},{y}"
                    );
                }
            }
            // A crop shows the same pixels, offset.
            let cropped = render(&made_turned, &now, (20, 30), (11.0, 17.0));
            for (x, y, v) in cropped.enumerate_pixels() {
                assert_eq!(v, full.get_pixel(x + 11, y + 17), "{kind} crop at {x},{y}");
            }
        }
    }

    /// A smooth ramp in the sensor's frame.
    fn ramp(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(width, height, |x, y| {
            let v = 0.15 + 0.5 * (x as f32 / width as f32) + 0.3 * (y as f32 / height as f32);
            image::Rgb([v, v, v])
        }))
    }

    /// The ramp's luma as the photo warped by `adjustments` shows it.
    fn warped_luma(sensor: &DynamicImage, adjustments: &Value) -> GrayImage {
        let params = warp_geometry(get_geometry_params_from_json(adjustments));
        warp_image_geometry(sensor, params).unwrap().into_luma8()
    }

    #[test]
    fn ai_masks_move_with_a_later_lens_or_transform_change() {
        let (w, h) = (300, 200);
        let sensor = ramp(w, h);
        let then = json!({"transformVertical": 25.0, "transformRotate": 3.0});
        for now in [
            json!({}),
            json!({"lensDistortionParams": {"k1": -0.08, "k2": 0.0, "k3": 0.0, "model": 0}}),
            json!({"transformHorizontal": -15.0, "transformScale": 110.0}),
        ] {
            // The model saw the photo in the old warp; it is now shown in
            // the new one.
            let made = warped_luma(&sensor, &then);
            let expected = warped_luma(&sensor, &now);
            let def = ai_layer("ai-sky", &made, json!({"warp": mask_warp(&then)}));
            let shown = render(&def, &now, (w, h), (0.0, 0.0));
            let legacy = render(
                &ai_layer("ai-sky", &made, json!({})),
                &now,
                (w, h),
                (0.0, 0.0),
            );
            // Inside the frame both warps cover, away from the edges.
            let (mut moved, mut stayed, mut n) = (0.0, 0.0, 0.0);
            for y in 30..h - 30 {
                for x in 40..w - 40 {
                    let e = expected.get_pixel(x, y)[0] as f64;
                    let a = shown.get_pixel(x, y)[0] as f64;
                    let b = legacy.get_pixel(x, y)[0] as f64;
                    if e == 0.0 || a == 0.0 || b == 0.0 {
                        continue;
                    }
                    moved += (a - e).abs();
                    stayed += (b - e).abs();
                    n += 1.0;
                }
            }
            let (moved, stayed) = (moved / n, stayed / n);
            assert!(n > 10_000.0, "{now}: {n} pixels compared");
            assert!(moved < 1.5, "{now}: mean error {moved:.2} after the move");
            assert!(
                stayed > 3.0 * moved,
                "{now}: {stayed:.2} without the move vs {moved:.2}"
            );
        }
        // The same warp: the mask as stored, bit for bit.
        let made = warped_luma(&sensor, &then);
        let same = ai_layer("ai-sky", &made, json!({"warp": mask_warp(&then)}));
        let mut adjustments = then.clone();
        adjustments["exposure"] = json!(1.0);
        assert_eq!(render(&same, &adjustments, (w, h), (0.0, 0.0)), made);
    }

    #[test]
    fn the_mask_cache_tells_geometries_apart() {
        let def = ai_layer("ai-sky", &pattern(), json!({}));
        let cache = std::sync::Mutex::new(MaskBitmapCache::new());
        let at = |adjustments: Value| {
            cached_or_generated_mask(
                &cache,
                &def,
                60,
                40,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::from_adjustments(&adjustments),
                None,
            )
            .unwrap()
            .unwrap()
        };
        let upright = at(json!({}));
        let flipped = at(json!({"flipHorizontal": true}));
        assert_ne!(*upright, *flipped);
        // A mask without a recorded warp is taken as made in the current one.
        assert_eq!(*at(json!({"transformVertical": 10.0})), *upright);
    }

    /// Masks read from the photo's pixels (luma range, the inherited colour
    /// tolerance, Auto Mask strokes) on a turned or flipped photo are the
    /// upright mask turned like the photo, pixel for pixel: each output
    /// pixel reads the source pixel the photo shows there (they sat one
    /// pixel over on every flipped or turned axis before).
    #[test]
    fn photo_sampled_masks_land_on_the_photos_pixels_when_turned() {
        // Every pixel a different grey (a misplaced pixel shows).
        let photo = DynamicImage::ImageRgb8(image::RgbImage::from_fn(60, 40, |x, y| {
            let v = ((x * 4 + y * 61) % 251) as u8;
            image::Rgb([v, v, v])
        }));
        let layer = |kind: &str, parameters: Value| -> MaskDefinition {
            serde_json::from_value(json!({
                "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
                "adjustments": {}, "subMasks": [{"id": "a", "type": kind, "visible": true,
                "mode": "additive", "parameters": parameters}]
            }))
            .unwrap()
        };
        let layers = [
            layer(
                "luma-range",
                json!({"rangeLow": 30.0, "rangeHigh": 55.0, "falloff": 30.0}),
            ),
            layer(
                "color",
                json!({"targetX": 21.0, "targetY": 13.0, "tolerance": 60.0}),
            ),
        ];
        let source = |adjustments: &Value| {
            ParametricMaskSource::new(Arc::new(photo.clone()), (60.0, 40.0))
                .unwrap()
                .with_geometry(adjustments)
        };
        let render = |def: &MaskDefinition, adjustments: &Value, size: (u32, u32)| {
            generate_mask_bitmap(
                def,
                size.0,
                size.1,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::from_adjustments(adjustments),
                Some(&source(adjustments)),
            )
            .unwrap()
        };
        for def in &layers {
            let upright = render(def, &json!({}), (60, 40));
            assert!(upright.pixels().any(|p| p[0] > 0 && p[0] < 255));
            for (steps, flip_h, flip_v) in [
                (1, false, false),
                (2, false, false),
                (3, false, false),
                (0, true, false),
                (0, false, true),
                (1, true, true),
                (3, true, false),
            ] {
                let now = json!({"orientationSteps": steps, "flipHorizontal": flip_h, "flipVertical": flip_v});
                let size = if steps % 2 == 1 { (40, 60) } else { (60, 40) };
                let expected = apply_flip(
                    apply_coarse_rotation(DynamicImage::ImageLuma8(upright.clone()), steps),
                    flip_h,
                    flip_v,
                )
                .into_owned()
                .into_luma8();
                assert_turned(
                    &render(def, &now, size),
                    &expected,
                    &format!("{} in {now}", def.sub_masks[0].mask_type),
                );
            }
        }
        // Auto Mask: a dab keeps to the colour under its centre, read from
        // the pixel shown under it.
        let stroke = |x: f64, y: f64| {
            json!({"lines": [{"tool": "brush", "brushSize": 30.0, "feather": 0.0, "autoMask": true,
                "points": [{"x": x, "y": y}]}]})
        };
        let upright = generate_brush_bitmap(
            &stroke(30.0, 20.0),
            60,
            40,
            1.0,
            (0.0, 0.0),
            SourceGeometry::default().into(),
            Some(&source(&json!({}))),
        );
        // Turned 180°: the dab's centre on the same photo pixel.
        let turned = json!({"orientationSteps": 2});
        let shown = generate_brush_bitmap(
            &stroke(29.0, 19.0),
            60,
            40,
            1.0,
            (0.0, 0.0),
            SourceGeometry::from_adjustments(&turned).into(),
            Some(&source(&turned)),
        );
        let expected = apply_coarse_rotation(DynamicImage::ImageLuma8(upright), 2)
            .into_owned()
            .into_luma8();
        assert_turned(&shown, &expected, "auto mask turned 180°");
    }

    /// Where a point of the unturned `w` × `h` photo is shown in `g` (the
    /// forward mapping, written out apart from the engine's inverse).
    fn shown_at(g: &Value, (w, h): (f64, f64), (x, y): (f64, f64)) -> (f64, f64) {
        let steps = g["orientationSteps"].as_u64().unwrap_or(0);
        let (sw, sh) = if steps % 2 == 1 { (h, w) } else { (w, h) };
        let (mut px, mut py) = match steps {
            1 => (h - y, x),
            2 => (w - x, h - y),
            3 => (y, w - x),
            _ => (x, y),
        };
        if g["flipHorizontal"] == true {
            px = sw - px;
        }
        if g["flipVertical"] == true {
            py = sh - py;
        }
        let (sin, cos) = g["rotation"].as_f64().unwrap_or(0.0).to_radians().sin_cos();
        let (dx, dy) = (px - sw / 2.0, py - sh / 2.0);
        (
            sw / 2.0 + dx * cos - dy * sin,
            sh / 2.0 + dx * sin + dy * cos,
        )
    }

    /// Red, orange, blue and grey quarters of a 96 × 64 photo.
    fn quarters() -> DynamicImage {
        DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(96, 64, |x, y| {
            match (x < 48, y < 32) {
                (true, true) => Rgba([200, 40, 30, 255]),
                (false, true) => Rgba([210, 110, 40, 255]),
                (true, false) => Rgba([30, 60, 190, 255]),
                (false, false) => Rgba([120, 120, 120, 255]),
            }
        }))
    }

    fn colour_layer(kind: &str, params: Value) -> MaskDefinition {
        serde_json::from_value(json!({
            "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
            "adjustments": {}, "subMasks": [{"id": "c", "type": kind, "visible": true,
            "mode": "additive", "parameters": params}]
        }))
        .unwrap()
    }

    fn with_geometry(mut params: Value, g: &Value) -> Value {
        params["orientationSteps"] = g.get("orientationSteps").cloned().unwrap_or(json!(0));
        params["rotation"] = g.get("rotation").cloned().unwrap_or(json!(0.0));
        params["flipHorizontal"] = g.get("flipHorizontal").cloned().unwrap_or(json!(false));
        params["flipVertical"] = g.get("flipVertical").cloned().unwrap_or(json!(false));
        params
    }

    #[test]
    fn colour_samples_on_turned_photos_take_the_colour_under_the_pointer() {
        let source = ParametricMaskSource::new(Arc::new(quarters()), (96.0, 64.0)).unwrap();
        let at = |m: &GrayImage, x, y| m.get_pixel(x, y)[0];
        let size = (96.0, 64.0);
        let render = |def: &MaskDefinition| {
            generate_mask_bitmap(
                def,
                96,
                64,
                1.0,
                (0.0, 0.0),
                &PhotoGeometry::default(),
                Some(&source),
            )
            .unwrap()
        };
        for taken in [
            json!({"orientationSteps": 1}),
            json!({"orientationSteps": 2, "flipHorizontal": true}),
            json!({"orientationSteps": 3, "flipVertical": true, "rotation": 7.5}),
            json!({"orientationSteps": 1, "rotation": -10.0}),
        ] {
            // A click on blue, stored with the component's geometry (as
            // samples before per-sample geometry were).
            let (px, py) = shown_at(&taken, size, (20.0, 50.0));
            let point = with_geometry(
                json!({"targetX": px, "targetY": py, "refine": 50.0, "feather": 0.0,
                    "samples": [{"x": px, "y": py}]}),
                &taken,
            );
            // An area drawn on the turned photo over blue (its own geometry),
            // and a point on red taken later on the photo shown as it is.
            let (c0, c1) = (
                shown_at(&taken, size, (8.0, 42.0)),
                shown_at(&taken, size, (18.0, 52.0)),
            );
            let area_sample = with_geometry(
                json!({"x": c0.0.min(c1.0), "y": c0.1.min(c1.1),
                    "width": (c0.0 - c1.0).abs(), "height": (c0.1 - c1.1).abs()}),
                &taken,
            );
            let red_sample = with_geometry(json!({"x": 10.0, "y": 10.0}), &json!({}));
            let area = with_geometry(
                json!({"targetX": 0.0, "targetY": 0.0, "refine": 50.0, "feather": 0.0,
                    "samples": [area_sample, red_sample]}),
                &json!({"orientationSteps": 2}),
            );
            // The point components of the inherited tools read their point
            // the same way.
            let luminance = with_geometry(
                json!({"targetX": px, "targetY": py, "tolerance": 5.0, "feather": 0.0}),
                &taken,
            );
            let mut rgb = luminance.clone();
            rgb["tolerance"] = json!(10.0);

            let point = render(&colour_layer("color", point));
            assert!(at(&point, 20, 50) > 250, "{taken}: point on blue");
            assert!(
                at(&point, 70, 50) < 10 && at(&point, 20, 10) < 10,
                "{taken}: point"
            );
            let area = render(&colour_layer("color", area));
            assert!(
                at(&area, 20, 50) > 200 && at(&area, 20, 10) > 200,
                "{taken}: area and point"
            );
            assert!(at(&area, 70, 50) < 10, "{taken}: grey left out");
            let luminance = render(&colour_layer("luminance", luminance));
            assert_eq!(at(&luminance, 20, 50), 255, "{taken}: luminance");
            assert_eq!(at(&luminance, 70, 50), 0, "{taken}: luminance on grey");
            let rgb = render(&colour_layer("color", rgb));
            assert_eq!(at(&rgb, 20, 50), 255, "{taken}: colour");
            assert_eq!(at(&rgb, 20, 10), 0, "{taken}: colour on red");
        }
    }

    #[test]
    fn colour_masks_follow_the_photo_turned_after_sampling() {
        let source = ParametricMaskSource::new(Arc::new(quarters()), (96.0, 64.0)).unwrap();
        let now = json!({"orientationSteps": 1, "flipVertical": true});
        let turned = source.clone().with_geometry(&now);
        for params in [
            json!({"targetX": 20.0, "targetY": 50.0, "refine": 50.0, "feather": 0.0,
                "samples": [{"x": 20.0, "y": 50.0}]}),
            json!({"targetX": 20.0, "targetY": 50.0, "tolerance": 10.0, "feather": 0.0}),
        ] {
            for kind in ["color", "luminance"] {
                let def = colour_layer(kind, params.clone());
                let upright = generate_mask_bitmap(
                    &def,
                    96,
                    64,
                    1.0,
                    (0.0, 0.0),
                    &PhotoGeometry::default(),
                    Some(&source),
                )
                .unwrap();
                let shown = generate_mask_bitmap(
                    &def,
                    64,
                    96,
                    1.0,
                    (0.0, 0.0),
                    &PhotoGeometry::from_adjustments(&now),
                    Some(&turned),
                )
                .unwrap();
                let expected = apply_flip(
                    apply_coarse_rotation(DynamicImage::ImageLuma8(upright), 1),
                    false,
                    true,
                )
                .into_owned()
                .into_luma8();
                assert_turned(&shown, &expected, &format!("{kind} {params}"));
            }
        }
    }
}

#[cfg(test)]
mod drawn_mask_frame_tests {
    use super::*;
    use crate::image_processing::{apply_coarse_rotation, apply_flip, apply_rotation};
    use serde_json::json;

    /// The photo's own size (before orientation).
    const PHOTO: (u32, u32) = (60, 40);

    fn layer(kind: &str, parameters: Value) -> MaskDefinition {
        serde_json::from_value(json!({
            "id": "m", "name": "m", "visible": true, "invert": false, "opacity": 100.0,
            "adjustments": {}, "subMasks": [{"id": "d", "type": kind, "visible": true,
            "mode": "additive", "parameters": parameters}]
        }))
        .unwrap()
    }

    /// The `frame` a mask drawn on the photo shown in `g` records.
    fn frame(g: &Value) -> Value {
        json!({
            "width": PHOTO.0, "height": PHOTO.1,
            "rotation": g["rotation"].as_f64().unwrap_or(0.0),
            "flipHorizontal": g["flipHorizontal"] == true,
            "flipVertical": g["flipVertical"] == true,
            "orientationSteps": g["orientationSteps"].as_u64().unwrap_or(0),
        })
    }

    /// A radial, a linear with a different range on either side, brush
    /// strokes with an eraser stroke, and a flow stroke, all within the
    /// 40 × 40 square shown in any orientation; `frame` as given.
    fn shapes(frame: Option<Value>) -> Vec<MaskDefinition> {
        let with = |mut parameters: Value| {
            if let Some(frame) = &frame {
                parameters["frame"] = frame.clone();
            }
            parameters
        };
        let points = |points: &[(f64, f64)]| {
            points
                .iter()
                .map(|(x, y)| json!({"x": x, "y": y}))
                .collect::<Vec<_>>()
        };
        vec![
            layer(
                "radial",
                with(
                    json!({"centerX": 22.0, "centerY": 15.0, "radiusX": 14.0, "radiusY": 7.0,
                    "rotation": 30.0, "feather": 0.6}),
                ),
            ),
            layer(
                "linear",
                with(
                    json!({"startX": 10.0, "startY": 8.0, "endX": 33.0, "endY": 30.0,
                    "range": 9.0, "rangeEnd": 4.0}),
                ),
            ),
            layer(
                "brush",
                json!({"lines": [
                    with(json!({"tool": "brush", "brushSize": 12.0, "feather": 0.5,
                        "points": points(&[(5.0, 5.0), (20.0, 9.0), (33.0, 25.0)])})),
                    with(json!({"tool": "eraser", "brushSize": 6.0, "feather": 0.0,
                        "points": points(&[(18.0, 8.0), (22.0, 14.0)])})),
                ]}),
            ),
            layer(
                "flow",
                json!({"lines": [with(json!({"tool": "brush", "brushSize": 10.0, "flow": 40.0,
                    "points": points(&[(30.0, 4.0), (8.0, 30.0)])}))]}),
            ),
        ]
    }

    fn shown_size(g: &Value) -> (u32, u32) {
        if g["orientationSteps"].as_u64().unwrap_or(0) % 2 == 1 {
            (PHOTO.1, PHOTO.0)
        } else {
            PHOTO
        }
    }

    fn render(def: &MaskDefinition, g: &Value, size: (u32, u32), crop: (f32, f32)) -> GrayImage {
        generate_mask_bitmap(
            def,
            size.0,
            size.1,
            1.0,
            crop,
            &PhotoGeometry::from_adjustments(g),
            None,
        )
        .unwrap()
    }

    /// A mask shown in `g` (turns and flips only) brought to the photo's own
    /// frame by the image operations.
    fn to_photo(mask: &GrayImage, g: &Value) -> GrayImage {
        let steps = g["orientationSteps"].as_u64().unwrap_or(0) as u8;
        let unflipped = apply_flip(
            DynamicImage::ImageLuma8(mask.clone()),
            g["flipHorizontal"] == true,
            g["flipVertical"] == true,
        );
        apply_coarse_rotation(unflipped, (4 - steps) % 4)
            .into_owned()
            .into_luma8()
    }

    /// A mask in the photo's own frame as the photo is shown in `g`.
    fn to_shown(mask: &GrayImage, g: &Value) -> GrayImage {
        let steps = g["orientationSteps"].as_u64().unwrap_or(0) as u8;
        apply_flip(
            apply_coarse_rotation(DynamicImage::ImageLuma8(mask.clone()), steps),
            g["flipHorizontal"] == true,
            g["flipVertical"] == true,
        )
        .into_owned()
        .into_luma8()
    }

    /// The largest difference between two masks. Points move exactly, but
    /// a turned shape's distances are summed in another order (and a
    /// radial's angle goes through sines and cosines), so a value may round
    /// to its neighbour.
    fn largest_difference(a: &GrayImage, b: &GrayImage, what: &str) -> u8 {
        assert_eq!(a.dimensions(), b.dimensions(), "{what}");
        a.as_raw()
            .iter()
            .zip(b.as_raw())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0)
    }

    const GEOMETRIES: [&str; 8] = [
        r#"{}"#,
        r#"{"orientationSteps": 1}"#,
        r#"{"orientationSteps": 2}"#,
        r#"{"orientationSteps": 3}"#,
        r#"{"flipHorizontal": true}"#,
        r#"{"flipVertical": true}"#,
        r#"{"orientationSteps": 1, "flipHorizontal": true}"#,
        r#"{"orientationSteps": 3, "flipHorizontal": true, "flipVertical": true}"#,
    ];

    #[test]
    fn drawn_masks_turn_and_flip_with_the_photo() {
        for drawn_in in GEOMETRIES {
            let a: Value = serde_json::from_str(drawn_in).unwrap();
            for def in shapes(Some(frame(&a))) {
                let kind = &def.sub_masks[0].mask_type;
                let drawn = render(&def, &a, shown_size(&a), (0.0, 0.0));
                assert!(
                    drawn.pixels().filter(|p| p[0] > 0).count() > 100,
                    "{kind} drawn in {a}"
                );
                let on_photo = to_photo(&drawn, &a);
                for shown_in in GEOMETRIES {
                    let b: Value = serde_json::from_str(shown_in).unwrap();
                    let shown = render(&def, &b, shown_size(&b), (0.0, 0.0));
                    let expected = to_shown(&on_photo, &b);
                    let what = format!("{kind} drawn in {a} shown in {b}");
                    assert!(largest_difference(&shown, &expected, &what) <= 1, "{what}");
                }
            }
        }
    }

    #[test]
    fn masks_drawn_as_shown_render_as_before() {
        let turned = json!({"orientationSteps": 1, "rotation": 6.0, "flipVertical": true});
        let plain = shapes(None);
        // A frame equal to the geometry now changes nothing, byte for byte.
        for (def, framed) in plain.iter().zip(shapes(Some(frame(&turned)))) {
            for crop in [(0.0, 0.0), (3.0, 5.0)] {
                assert_eq!(
                    render(def, &turned, (30, 50), crop),
                    render(&framed, &turned, (30, 50), crop),
                    "{}",
                    def.sub_masks[0].mask_type
                );
            }
        }
        // Without a frame (older recipes) a drawn mask was drawn as the
        // photo is shown: it stays where it is drawn on screen, as before.
        for def in &plain {
            assert_eq!(
                render(def, &json!({}), (40, 40), (0.0, 0.0)),
                render(def, &turned, (40, 40), (0.0, 0.0)),
                "{}",
                def.sub_masks[0].mask_type
            );
        }
        // An unreadable frame, or one without a size where the photo's size
        // is unknown, leaves the shape as stored instead of losing the mask.
        for broken in [json!("turned"), json!({"orientationSteps": 1})] {
            for (def, framed) in plain.iter().zip(shapes(Some(broken.clone()))) {
                assert_eq!(
                    render(def, &turned, (30, 50), (0.0, 0.0)),
                    render(&framed, &turned, (30, 50), (0.0, 0.0)),
                    "{} with frame {broken}",
                    def.sub_masks[0].mask_type
                );
            }
        }
    }

    /// Centre of a mask's coverage.
    fn centroid(mask: &GrayImage) -> (f64, f64) {
        let (mut sx, mut sy, mut sum) = (0.0, 0.0, 0.0);
        for (x, y, p) in mask.enumerate_pixels() {
            let v = p[0] as f64;
            sx += x as f64 * v;
            sy += y as f64 * v;
            sum += v;
        }
        assert!(sum > 0.0);
        (sx / sum, sy / sum)
    }

    /// Centre of a float image's brightness.
    fn centroid_f32(image: &image::Rgba32FImage) -> (f64, f64) {
        let (mut sx, mut sy, mut sum) = (0.0, 0.0, 0.0);
        for (x, y, p) in image.enumerate_pixels() {
            let v = p[0] as f64;
            sx += x as f64 * v;
            sy += y as f64 * v;
            sum += v;
        }
        (sx / sum, sy / sum)
    }

    /// A drawn mask stays on the photo's pixels under straightening too: a
    /// dab put where a spot of the photo is shown in one geometry is on that
    /// spot in every other one, as the photo itself is turned, flipped and
    /// straightened by the image operations.
    #[test]
    fn drawn_masks_follow_straightening_onto_the_same_pixels() {
        // A soft spot on the photo, off its centre.
        let photo =
            DynamicImage::ImageRgba32F(image::ImageBuffer::from_fn(PHOTO.0, PHOTO.1, |x, y| {
                let d2 = (x as f32 - 17.3).powi(2) + (y as f32 - 11.6).powi(2);
                let v = (-d2 / 8.0).exp();
                image::Rgba([v, v, v, 1.0])
            }));
        let spot_shown = |g: &Value| {
            let steps = g["orientationSteps"].as_u64().unwrap_or(0) as u8;
            let turned = apply_flip(
                apply_coarse_rotation(&photo, steps),
                g["flipHorizontal"] == true,
                g["flipVertical"] == true,
            );
            let straightened =
                apply_rotation(turned, g["rotation"].as_f64().unwrap_or(0.0) as f32, false);
            centroid_f32(&straightened.to_rgba32f())
        };
        let geometries = [
            json!({}),
            json!({"rotation": 9.0}),
            json!({"rotation": -14.0, "orientationSteps": 1}),
            json!({"rotation": 5.0, "flipHorizontal": true}),
            json!({"rotation": 21.0, "orientationSteps": 2, "flipVertical": true}),
            json!({"rotation": -3.5, "orientationSteps": 3, "flipHorizontal": true}),
        ];
        for a in &geometries {
            let (x, y) = spot_shown(a);
            let dab = layer(
                "brush",
                json!({"lines": [{"tool": "brush", "brushSize": 8.0, "feather": 0.8,
                    "points": [{"x": x, "y": y}], "frame": frame(a)}]}),
            );
            for b in &geometries {
                let spot_now = spot_shown(b);
                let dab_now = centroid(&render(&dab, b, shown_size(b), (0.0, 0.0)));
                let off = (spot_now.0 - dab_now.0).hypot(spot_now.1 - dab_now.1);
                assert!(off < 0.2, "drawn in {a}, shown in {b}: {off} px off");
            }
        }
        // The straightened, cropped photo shows the same pixels, offset.
        let a = json!({"rotation": 9.0});
        let b = json!({"rotation": -14.0, "orientationSteps": 1, "flipHorizontal": true});
        for def in shapes(Some(frame(&a))) {
            let full = render(&def, &b, (40, 60), (0.0, 0.0));
            let cropped = render(&def, &b, (25, 30), (7.0, 13.0));
            for (x, y, v) in cropped.enumerate_pixels() {
                assert_eq!(
                    v,
                    full.get_pixel(x + 7, y + 13),
                    "{}",
                    def.sub_masks[0].mask_type
                );
            }
        }
    }

    /// Cost of moving drawn strokes into the geometry now while a stroke is
    /// painted after a turn: 40 earlier strokes of 250 points each, painted
    /// before the turn, and the stroke being painted, at a preview size.
    #[test]
    #[ignore = "timing probe; run with --release --ignored --nocapture"]
    fn drawn_mask_mapping_timing_probe() {
        let before = json!({});
        let after = json!({"orientationSteps": 1, "flipHorizontal": true});
        // A 6000 x 4000 photo.
        let stroke = |g: &Value, i: usize| {
            let mut frame = frame(g);
            frame["width"] = json!(6000.0);
            frame["height"] = json!(4000.0);
            json!({"tool": "brush", "brushSize": 120.0, "feather": 0.5, "frame": frame,
                "points": (0..250).map(|k| json!({"x": 100.0 + 13.0 * k as f64,
                    "y": 80.0 + 90.0 * i as f64 + (k as f64 * 0.1).sin() * 40.0})).collect::<Vec<_>>()})
        };
        let earlier: Vec<Value> = (0..40).map(|i| stroke(&before, i)).collect();
        let (width, height) = (1356, 1808);
        let scale = width as f32 / 4000.0;
        for (label, g) in [("turned (strokes moved)", &after), ("as painted", &before)] {
            let size = if g == &after {
                (width, height)
            } else {
                (height, width)
            };
            let mut lines = earlier.clone();
            let mut times = Vec::new();
            clear_brush_prefixes();
            for step in 0..30 {
                // The live stroke grows by eight points a frame.
                let mut live = stroke(g, 41);
                live["points"]
                    .as_array_mut()
                    .unwrap()
                    .truncate(10 + step * 8);
                if step > 0 {
                    lines.pop();
                }
                lines.push(live);
                let params = json!({"lines": lines});
                let def = layer("brush", params);
                let start = std::time::Instant::now();
                std::hint::black_box(generate_mask_bitmap(
                    &def,
                    size.0,
                    size.1,
                    scale,
                    (0.0, 0.0),
                    &PhotoGeometry::from_adjustments(g),
                    None,
                ));
                times.push(start.elapsed().as_secs_f64() * 1e3);
            }
            times.sort_by(f64::total_cmp);
            println!(
                "{label}: mask {:.2} ms median (p90 {:.2})",
                times[15], times[27]
            );
        }
    }

    /// A stroke painted after a turn keeps its own frame next to the earlier
    /// strokes' frames, and the brush's cached earlier strokes still give
    /// the full rasterization.
    #[test]
    fn strokes_keep_their_own_frames_and_the_cached_prefix() {
        let before = json!({});
        let after = json!({"orientationSteps": 1, "flipVertical": true});
        let stroke = |g: &Value, points: &[(f64, f64)], tool: &str| {
            json!({"tool": tool, "brushSize": 8.0, "feather": 0.3, "frame": frame(g),
                "points": points.iter().map(|(x, y)| json!({"x": x, "y": y})).collect::<Vec<_>>()})
        };
        let first = stroke(&before, &[(6.0, 6.0), (30.0, 12.0)], "brush");
        let second = stroke(&after, &[(10.0, 40.0), (25.0, 20.0)], "brush");
        let third = stroke(&after, &[(15.0, 30.0), (22.0, 28.0)], "eraser");
        let mask = |lines: Vec<&Value>| layer("brush", json!({"lines": lines}));
        let size = shown_size(&after);
        // Painted one after the other at the geometry now (cached prefixes).
        clear_brush_prefixes();
        render(&mask(vec![&first]), &after, size, (0.0, 0.0));
        render(&mask(vec![&first, &second]), &after, size, (0.0, 0.0));
        let painted = render(
            &mask(vec![&first, &second, &third]),
            &after,
            size,
            (0.0, 0.0),
        );
        clear_brush_prefixes();
        let whole = render(
            &mask(vec![&first, &second, &third]),
            &after,
            size,
            (0.0, 0.0),
        );
        assert_eq!(painted, whole);
        // The first stroke where it was painted on the photo: its own
        // rendering before the turn, turned like the photo.
        clear_brush_prefixes();
        let first_then = to_photo(
            &render(
                &mask(vec![&first]),
                &before,
                shown_size(&before),
                (0.0, 0.0),
            ),
            &before,
        );
        let first_now = render(&mask(vec![&first]), &after, size, (0.0, 0.0));
        assert_eq!(first_now, to_shown(&first_then, &after));
        // The later strokes as painted, on top.
        assert_ne!(whole, first_now);
        // At another geometry the prefixes of this one are not reused.
        let elsewhere = render(
            &mask(vec![&first, &second, &third]),
            &before,
            PHOTO,
            (0.0, 0.0),
        );
        clear_brush_prefixes();
        assert_eq!(
            elsewhere,
            render(
                &mask(vec![&first, &second, &third]),
                &before,
                PHOTO,
                (0.0, 0.0)
            )
        );
        assert!(
            largest_difference(
                &to_photo(&whole, &after),
                &to_photo(&elsewhere, &before),
                "the whole brush stays on the photo"
            ) <= 1
        );
    }

    /// `render` on a photo whose own size is `photo` (known to the engine).
    fn render_on(def: &MaskDefinition, g: &Value, photo: (u32, u32)) -> GrayImage {
        let size = if g["orientationSteps"].as_u64().unwrap_or(0) % 2 == 1 {
            (photo.1, photo.0)
        } else {
            photo
        };
        generate_mask_bitmap(
            def,
            size.0,
            size.1,
            1.0,
            (0.0, 0.0),
            &PhotoGeometry::from_adjustments(g).with_photo_size(photo.0 as f64, photo.1 as f64),
            None,
        )
        .unwrap()
    }

    /// Coverage of a mask: its sum.
    fn coverage(mask: &GrayImage) -> f64 {
        mask.as_raw().iter().map(|&v| v as f64).sum()
    }

    /// A mask copied to a photo of another size lands on the same part of
    /// it: its centre at the same fraction of the photo's own width and
    /// height, its coverage grown with the photo's area, in any geometry.
    #[test]
    fn drawn_masks_scale_to_a_photo_of_another_size() {
        for (target, what) in [((120u32, 80u32), "twice"), ((90, 40), "wider")] {
            let (sx, sy) = (
                target.0 as f64 / PHOTO.0 as f64,
                target.1 as f64 / PHOTO.1 as f64,
            );
            for drawn_in in [r#"{}"#, r#"{"orientationSteps": 1, "flipVertical": true}"#] {
                let a: Value = serde_json::from_str(drawn_in).unwrap();
                for def in shapes(Some(frame(&a))) {
                    let kind = def.sub_masks[0].mask_type.clone();
                    let drawn = to_photo(&render_on(&def, &a, PHOTO), &a);
                    let (cx, cy) = centroid(&drawn);
                    for shown_in in GEOMETRIES {
                        let b: Value = serde_json::from_str(shown_in).unwrap();
                        let on_target = to_photo(&render_on(&def, &b, target), &b);
                        assert_eq!(on_target.dimensions(), target);
                        let (tx, ty) = centroid(&on_target);
                        let label = format!("{kind} drawn in {a}, {what}, shown in {b}");
                        // Pixel index i of n covers (i + 0.5) / n of the side.
                        // A radial's centre is rasterized at a whole pixel;
                        // strokes keep round dabs when the photo is only
                        // wider, which moves their centre a little.
                        let tolerance = if sx == sy { 1.0 } else { 2.0 };
                        let off_x = (tx + 0.5) / target.0 as f64 - (cx + 0.5) / PHOTO.0 as f64;
                        let off_y = (ty + 0.5) / target.1 as f64 - (cy + 0.5) / PHOTO.1 as f64;
                        assert!(
                            off_x.abs() * (target.0 as f64) < tolerance
                                && off_y.abs() * (target.1 as f64) < tolerance,
                            "{label}: centre off by {off_x:.4}, {off_y:.4}"
                        );
                        let grown = coverage(&on_target) / coverage(&drawn);
                        assert!(
                            (grown / (sx * sy) - 1.0).abs() < 0.12,
                            "{label}: coverage x{grown:.3}, area x{:.3}",
                            sx * sy
                        );
                    }
                }
            }
        }
    }

    /// The photo's own size known to the engine changes nothing for masks
    /// drawn on a photo of that size, byte for byte; a frame stamped
    /// without a size takes the photo's.
    #[test]
    fn frames_of_the_same_size_or_none_render_as_on_the_photo() {
        let turned = json!({"orientationSteps": 1, "rotation": 6.0, "flipVertical": true});
        let sizeless = |g: &Value| {
            let mut frame = frame(g);
            let frame_map = frame.as_object_mut().unwrap();
            frame_map.remove("width");
            frame_map.remove("height");
            frame
        };
        for g in [json!({}), turned.clone()] {
            for (def, unsized_def) in shapes(Some(frame(&g)))
                .iter()
                .zip(shapes(Some(sizeless(&g))))
            {
                for b in [json!({}), turned.clone(), json!({"orientationSteps": 2})] {
                    let kind = &def.sub_masks[0].mask_type;
                    let size = shown_size(&b);
                    let known = render_on(def, &b, PHOTO);
                    assert_eq!(known, render(def, &b, size, (0.0, 0.0)), "{kind} in {b}");
                    assert_eq!(
                        known,
                        render_on(&unsized_def, &b, PHOTO),
                        "{kind} sizeless in {b}"
                    );
                }
            }
        }
        // Without frames: as stored, whatever the photo's size.
        for def in shapes(None) {
            assert_eq!(
                render_on(&def, &turned, (90, 70)).dimensions(),
                (70, 90),
                "{}",
                def.sub_masks[0].mask_type
            );
            let size = shown_size(&turned);
            assert_eq!(
                render_on(&def, &turned, PHOTO),
                render(&def, &turned, size, (0.0, 0.0))
            );
        }
    }

    /// The viewer moves shapes as the engine does: the same numbers come
    /// out of `shapeAsShown` in src/utils/maskGeometry.test.ts ("places
    /// shapes as the engine does").
    #[test]
    fn shapes_move_as_the_viewer_moves_them() {
        let drawn = json!({"width": 60.0, "height": 40.0, "rotation": 7.0,
            "flipHorizontal": true, "flipVertical": false, "orientationSteps": 1});
        let now =
            PhotoGeometry::from_adjustments(&json!({"rotation": -3.0, "orientationSteps": 2}))
                .with_photo_size(90.0, 50.0)
                .shown();
        let radial = json!({"centerX": 12.0, "centerY": 31.0, "radiusX": 9.0, "radiusY": 4.0,
            "rotation": 20.0, "feather": 0.5, "frame": drawn});
        let linear = json!({"startX": 5.0, "startY": 8.0, "endX": 30.0, "endY": 50.0,
            "range": 6.0, "rangeEnd": 2.0, "frame": drawn});
        let radial = shape_as_shown("radial", &radial, now, 1.0).into_owned();
        let linear = shape_as_shown("linear", &linear, now, 1.0).into_owned();
        let mut points = vec![Point {
            x: 12.0,
            y: 31.0,
            p: None,
        }];
        let mut size = 10.0f32;
        strokes_as_shown(
            [(
                ShapeFrame::of(&json!({"frame": drawn})),
                &mut points,
                &mut size,
            )]
            .into_iter(),
            now,
            1.0,
        );
        let expected = [
            (&radial["centerX"], 41.257_114),
            (&radial["centerY"], 33.856_149),
            (&radial["radiusX"], 11.374_553),
            (&radial["radiusY"], 5.934_299),
            (&radial["rotation"], -108.484_957),
            (&linear["startX"], 14.974_016),
            (&linear["startY"], 9.972_283),
            (&linear["endX"], 74.813_051),
            (&linear["endY"], 44.302_807),
            (&linear["range"], 7.970_568),
            (&linear["rangeEnd"], 2.656_856),
        ];
        for (i, (value, want)) in expected.iter().enumerate() {
            let got = value.as_f64().unwrap();
            assert!((got - want).abs() < 1e-5, "value {i}: {got:.6} vs {want}");
        }
        // A stroke's point moves as the radial's centre; its brush scales
        // with the square root of the area: 90 × 50 over 60 × 40.
        assert!((points[0].x - 41.257_114).abs() < 1e-5 && (points[0].y - 33.856_149).abs() < 1e-5);
        assert!(
            (size as f64 - 10.0 * (1.875f64).sqrt()).abs() < 1e-4,
            "{size}"
        );
    }

    fn sub_masks(adjustments: &Value) -> Vec<Value> {
        ["masks", "aiPatches"]
            .iter()
            .filter_map(|key| adjustments[*key].as_array())
            .flatten()
            .flat_map(|container| container["subMasks"].as_array().unwrap().clone())
            .collect()
    }

    /// The backend's twin of the editor's `keepDrawnMasksOnPhoto`: a change
    /// of geometry gives the drawn masks it left alone the frame they were
    /// shown in; masks it brought along, AI masks and framed shapes keep
    /// theirs, and nothing changes without a change of geometry.
    #[test]
    fn a_geometry_change_stamps_the_masks_it_keeps() {
        let turned = json!({"width": 60.0, "height": 40.0, "rotation": 0.0,
            "flipHorizontal": false, "flipVertical": false, "orientationSteps": 1});
        let previous = json!({
            "rotation": 3.0,
            "masks": [{"id": "m", "subMasks": [
                {"id": "r", "type": "radial", "parameters": {"centerX": 1.0, "centerY": 2.0}},
                {"id": "b", "type": "brush", "parameters": {"lines": [
                    {"tool": "brush", "points": [], "frame": turned},
                    {"tool": "brush", "points": []}]}},
                {"id": "a", "type": "ai-sky", "parameters": {"maskDataBase64": "x"}},
            ]}],
            "aiPatches": [{"id": "p", "subMasks": [
                {"id": "q", "type": "brush", "parameters": {"lines": [{"tool": "brush", "points": []}]}}]}],
        });
        let was = json!({"rotation": 3.0, "flipHorizontal": false, "flipVertical": false,
            "orientationSteps": 0, "width": 60.0, "height": 40.0});
        let mut next = previous.clone();
        next["orientationSteps"] = json!(1);
        assert!(keep_drawn_masks_on_photo(
            &previous,
            &mut next,
            Some((60.0, 40.0))
        ));
        let subs = sub_masks(&next);
        assert_eq!(subs[0]["parameters"]["frame"], was);
        assert_eq!(subs[1]["parameters"]["lines"][0]["frame"], turned);
        assert_eq!(subs[1]["parameters"]["lines"][1]["frame"], was);
        assert_eq!(subs[2], sub_masks(&previous)[2]);
        assert_eq!(subs[3]["parameters"]["lines"][0]["frame"], was);
        // Stamped, the masks render exactly as they were shown before.
        let shown_before = PhotoGeometry::from_adjustments(&previous).with_photo_size(60.0, 40.0);
        for (old, new) in sub_masks(&previous).iter().zip(&subs).take(2) {
            let def = |sub: &Value| layer(sub["type"].as_str().unwrap(), sub["parameters"].clone());
            let at = |sub: &Value| {
                generate_mask_bitmap(&def(sub), 60, 40, 1.0, (0.0, 0.0), &shown_before, None)
            };
            assert_eq!(at(old), at(new));
        }
        // Without the size the frame records none.
        let mut sizeless = previous.clone();
        sizeless["flipHorizontal"] = json!(true);
        keep_drawn_masks_on_photo(&previous, &mut sizeless, None);
        let frame = &sub_masks(&sizeless)[0]["parameters"]["frame"];
        assert_eq!(frame["rotation"], json!(3.0));
        assert!(frame.get("width").is_none());
        // No change of geometry: nothing to do.
        let mut same = previous.clone();
        same["exposure"] = json!(1.0);
        assert!(!keep_drawn_masks_on_photo(
            &previous,
            &mut same,
            Some((60.0, 40.0))
        ));
        assert_eq!(sub_masks(&same), sub_masks(&previous));
        // Masks the edit brings along keep their own frames (or none).
        let mut pasted = previous.clone();
        pasted["orientationSteps"] = json!(2);
        pasted["masks"] = json!([{"id": "n", "subMasks": [
            {"id": "r2", "type": "radial", "parameters": {"centerX": 9.0, "centerY": 2.0}}]}]);
        keep_drawn_masks_on_photo(&previous, &mut pasted, Some((60.0, 40.0)));
        assert!(
            pasted["masks"][0]["subMasks"][0]["parameters"]
                .get("frame")
                .is_none()
        );
    }
}

#[cfg(test)]
mod pen_pressure_tests {
    use super::*;

    /// A horizontal stroke along y = 60 from x = 40 to 360 as a recipe line,
    /// one point every 8 px with pressure `pressure(t)` (t = 0..1 along it)
    /// or none, plus the extra recipe keys in `extra`.
    fn line(pressure: Option<fn(f64) -> f64>, extra: Value) -> BrushLine {
        let points: Vec<Value> = (0..=40)
            .map(|i| {
                let t = i as f64 / 40.0;
                let mut point = serde_json::json!({"x": 40.0 + 320.0 * t, "y": 60.0});
                if let Some(pressure) = pressure {
                    point["p"] = pressure(t).into();
                }
                point
            })
            .collect();
        let mut line = serde_json::json!({"tool": "brush", "brushSize": 60.0,
            "points": points, "feather": 0.5, "hardness": 80.0, "dabVersion": 2});
        for (key, value) in extra.as_object().unwrap() {
            line[key] = value.clone();
        }
        serde_json::from_value(line).unwrap()
    }

    fn paint(line: &BrushLine) -> GrayImage {
        let mut mask = GrayImage::new(400, 120);
        apply_brush_line(&mut mask, line, 1.0, (0.0, 0.0), None);
        mask
    }

    /// Rows painted at least half at column `x`: the stroke's width there.
    fn width_at(mask: &GrayImage, x: u32) -> u32 {
        (0..mask.height())
            .filter(|&y| mask.get_pixel(x, y)[0] >= 128)
            .count() as u32
    }

    fn ramp(t: f64) -> f64 {
        t
    }

    #[test]
    fn mouse_strokes_keep_their_recipe_and_their_pixels() {
        // No `p` (every mouse stroke): no new keys, and the pressure keys
        // alone or `p` alone change no pixel.
        let mouse = line(None, serde_json::json!({}));
        let saved = serde_json::to_value(&mouse).unwrap();
        assert!(saved["points"][0].get("p").is_none());
        assert!(saved.get("pressureSize").is_none() && saved.get("pressureFlow").is_none());
        let plain = paint(&mouse);
        let flags = serde_json::json!({"pressureSize": true, "pressureFlow": true});
        assert!(paint(&line(None, flags.clone())) == plain);
        assert!(paint(&line(Some(ramp), serde_json::json!({}))) == plain);
        // Pressure 1 throughout paints the mouse stroke (within one level:
        // dabs are placed by the pressure walk).
        let full = paint(&line(
            Some(|_| 1.0),
            serde_json::json!({"pressureSize": true}),
        ));
        let most = plain
            .pixels()
            .zip(full.pixels())
            .map(|(a, b)| a[0].abs_diff(b[0]))
            .max()
            .unwrap();
        assert!(
            most <= 1,
            "pressure 1 differs from the mouse stroke by {most}"
        );
        let round_trip: BrushLine =
            serde_json::from_value(serde_json::to_value(line(Some(ramp), flags.clone())).unwrap())
                .unwrap();
        assert_eq!(round_trip, line(Some(ramp), flags));
    }

    #[test]
    fn pressure_scales_the_width_monotonically() {
        let mask = paint(&line(Some(ramp), serde_json::json!({"pressureSize": true})));
        let full = width_at(&paint(&line(None, serde_json::json!({}))), 200);
        // Columns 80..=320 (pressure 0.125..0.875): never narrower further on.
        let widths: Vec<u32> = (80..=320).step_by(8).map(|x| width_at(&mask, x)).collect();
        assert!(
            widths
                .windows(2)
                .all(|w| w[1] + 1 >= w[0] && w[1] <= w[0] + 3),
            "{widths:?}"
        );
        // Linear in pressure: half the width at half pressure, about.
        let half = width_at(&mask, 200) as f32;
        assert!(
            (half / full as f32 - 0.5).abs() < 0.08,
            "{half} of {full} at pressure 0.5"
        );
        assert!(width_at(&mask, 44) <= 4, "{}", width_at(&mask, 44));
    }

    #[test]
    fn pressure_scales_the_flow_monotonically() {
        let flow = |extra: Value, p: Option<fn(f64) -> f64>| {
            let mut extra = extra;
            extra["flow"] = 40.0.into();
            paint(&line(p, extra))
        };
        let mask = flow(serde_json::json!({"pressureFlow": true}), Some(ramp));
        let centre: Vec<u8> = (80..=320)
            .step_by(4)
            .map(|x| mask.get_pixel(x, 60)[0])
            .collect();
        assert!(centre.windows(2).all(|w| w[1] >= w[0]), "{centre:?}");
        assert!(centre[0] < centre[centre.len() - 1] / 2, "{centre:?}");
        // Width unchanged: the edge stays where the full-flow stroke's is.
        let plain = flow(serde_json::json!({}), None);
        assert_eq!(width_at(&mask, 300) > 0, width_at(&plain, 300) > 0);
        // Pressure 0.5 is the stroke at half the flow.
        let half_p = flow(serde_json::json!({"pressureFlow": true}), Some(|_| 0.5));
        let mut half_flow = line(None, serde_json::json!({}));
        half_flow.flow = Some(20.0);
        let half_flow = paint(&half_flow);
        let most = half_p
            .pixels()
            .zip(half_flow.pixels())
            .map(|(a, b)| a[0].abs_diff(b[0]))
            .max()
            .unwrap();
        assert!(most <= 1, "{most}");
    }

    /// A stroke along y = 60 through `points` (x, pressure) as a recipe line.
    fn through(points: &[(f64, f64)], extra: Value) -> BrushLine {
        let points: Vec<Value> = points
            .iter()
            .map(|&(x, p)| serde_json::json!({"x": x, "y": 60.0, "p": p}))
            .collect();
        let mut line = serde_json::json!({"tool": "brush", "brushSize": 60.0,
            "points": points, "feather": 0.5, "hardness": 80.0, "dabVersion": 2});
        for (key, value) in extra.as_object().unwrap() {
            line[key] = value.clone();
        }
        serde_json::from_value(line).unwrap()
    }

    #[test]
    fn auto_mask_dabs_without_flow_leave_the_matte_alone() {
        // Red left of x = 200, blue right of it. A stroke on the red at full
        // pressure whose tail goes on over the blue at zero pressure: the
        // tail paints nothing, so it must not halve the red's coverage
        // where its dabs (subject blue, share 0 on red) overlap.
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(400, 120, |x, _| {
            if x < 200 {
                Rgba([200, 40, 30, 255])
            } else {
                Rgba([30, 60, 190, 255])
            }
        }));
        let source = ParametricMaskSource::new(Arc::new(image), (400.0, 120.0)).unwrap();
        let sampler = SourceSampler::new(&source, 1.0, (0.0, 0.0));
        for version in [2, 3] {
            let extra = serde_json::json!({"pressureFlow": true, "autoMask": true,
                "autoMaskVersion": version, "flow": 60.0});
            let paint = |points: &[(f64, f64)]| {
                let mut mask = GrayImage::new(400, 120);
                let line = through(points, extra.clone());
                apply_brush_line(&mut mask, &line, 1.0, (0.0, 0.0), Some(&sampler));
                mask
            };
            // The same dabs up to x = 177 in both.
            let red = paint(&[(60.0, 1.0), (176.0, 1.0), (177.0, 0.0)]);
            let tail = paint(&[(60.0, 1.0), (176.0, 1.0), (177.0, 0.0), (330.0, 0.0)]);
            for x in [160u32, 175, 185] {
                let (a, b) = (red.get_pixel(x, 60)[0], tail.get_pixel(x, 60)[0]);
                assert!(
                    b + 3 >= a,
                    "v{version} x {x}: {b} with the tail, {a} without"
                );
            }
        }
    }

    #[test]
    fn pressing_harder_in_place_widens_the_dab() {
        // A press at x = 200 that grows from 0.3 to 1 before the pen moves
        // paints the dab at the pressure it reached.
        let size = serde_json::json!({"pressureSize": true});
        let mut pressed = GrayImage::new(400, 120);
        let line = through(&[(200.0, 0.3), (200.0, 0.6), (200.0, 1.0)], size.clone());
        apply_brush_line(&mut pressed, &line, 1.0, (0.0, 0.0), None);
        let mut full = GrayImage::new(400, 120);
        apply_brush_line(
            &mut full,
            &through(&[(200.0, 1.0)], size),
            1.0,
            (0.0, 0.0),
            None,
        );
        assert_eq!(width_at(&pressed, 200), width_at(&full, 200));
    }

    #[test]
    fn a_pressure_peak_where_the_pen_stopped_stays() {
        // The pen moves 2 px (no dab yet), presses to 1 and eases off again:
        // the painted width at the peak stays.
        let size = serde_json::json!({"pressureSize": true});
        let paint = |points: &[(f64, f64)]| {
            let mut mask = GrayImage::new(400, 120);
            apply_brush_line(
                &mut mask,
                &through(points, size.clone()),
                1.0,
                (0.0, 0.0),
                None,
            );
            mask
        };
        let eased = paint(&[(200.0, 0.3), (202.0, 0.3), (202.0, 1.0), (202.0, 0.3)]);
        let full = paint(&[(202.0, 1.0)]);
        assert!(width_at(&eased, 202).abs_diff(width_at(&full, 202)) <= 1);
        // The same with the pen still moving a little.
        let wobbled = paint(&[(200.0, 0.3), (202.0, 0.3), (202.1, 1.0), (202.2, 0.3)]);
        assert!(width_at(&wobbled, 202).abs_diff(width_at(&full, 202)) <= 1);
        // Easing off alone paints no wider (the end dab takes the release
        // pressure).
        let moved = paint(&[(200.0, 0.3), (202.0, 0.3)]);
        let eased_only = paint(&[(200.0, 0.3), (202.0, 0.3), (202.0, 0.2)]);
        assert!(width_at(&eased_only, 202) <= width_at(&moved, 202));
    }

    #[test]
    fn pen_jitter_does_not_build_up_coverage() {
        // Pressure alternating 0.50 / 0.53 every pixel (above the editor's
        // 0.02 step, so every sample is kept) paints about what a steady
        // 0.515 paints: the peaks raise dabs rather than add them.
        let flow = serde_json::json!({"pressureFlow": true, "flow": 50.0});
        let jitter: Vec<(f64, f64)> = (0..=300)
            .map(|i| (50.0 + i as f64, if i % 2 == 0 { 0.5 } else { 0.53 }))
            .collect();
        let steady: Vec<(f64, f64)> = jitter.iter().map(|&(x, _)| (x, 0.515)).collect();
        let mean = |points: &[(f64, f64)]| {
            let mut mask = GrayImage::new(400, 120);
            apply_brush_line(
                &mut mask,
                &through(points, flow.clone()),
                1.0,
                (0.0, 0.0),
                None,
            );
            (100..300)
                .map(|x| mask.get_pixel(x, 60)[0] as f32)
                .sum::<f32>()
                / 200.0
        };
        let (jittered, steady) = (mean(&jitter), mean(&steady));
        assert!((jittered - steady).abs() < 4.0, "{jittered} vs {steady}");
    }

    #[test]
    fn auto_mask_v3_votes_leave_out_dabs_that_paint_nothing() {
        // As above: red left of x = 200. A short stroke on the red whose
        // zero-pressure tail runs over the blue would outvote the red
        // subjects near its start.
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(400, 120, |x, _| {
            if x < 200 {
                Rgba([200, 40, 30, 255])
            } else {
                Rgba([30, 60, 190, 255])
            }
        }));
        let source = ParametricMaskSource::new(Arc::new(image), (400.0, 120.0)).unwrap();
        let sampler = SourceSampler::new(&source, 1.0, (0.0, 0.0));
        let extra = serde_json::json!({"pressureFlow": true, "autoMask": true,
            "autoMaskVersion": 3});
        let paint = |points: &[(f64, f64)]| {
            let mut mask = GrayImage::new(400, 120);
            let line = through(points, extra.clone());
            apply_brush_line(&mut mask, &line, 1.0, (0.0, 0.0), Some(&sampler));
            mask
        };
        let short = paint(&[(180.0, 1.0), (181.0, 0.0)]);
        let tail = paint(&[(180.0, 1.0), (181.0, 0.0), (270.0, 0.0)]);
        assert!(short == tail);
    }

    #[test]
    fn pressure_reaches_lightroom_and_auto_mask_tips() {
        // Lightroom's tip (a dab at every point) and Auto Mask v2 both
        // shrink with pressure.
        let lightroom = paint(&line(
            Some(ramp),
            serde_json::json!({"pressureSize": true, "lrCenterWeight": 0.5, "flow": 50.0}),
        ));
        assert!(width_at(&lightroom, 100) < width_at(&lightroom, 300));
        let image = DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(400, 120, |_, _| {
            Rgba([128, 128, 128, 255])
        }));
        let source = ParametricMaskSource::new(Arc::new(image), (400.0, 120.0)).unwrap();
        let sampler = SourceSampler::new(&source, 1.0, (0.0, 0.0));
        for version in [2, 3] {
            let stroke = line(
                Some(ramp),
                serde_json::json!({"pressureSize": true, "autoMask": true,
                    "autoMaskVersion": version}),
            );
            let mut mask = GrayImage::new(400, 120);
            apply_brush_line(&mut mask, &stroke, 1.0, (0.0, 0.0), Some(&sampler));
            let (thin, wide) = (width_at(&mask, 100), width_at(&mask, 300));
            assert!(thin > 0 && thin < wide, "v{version}: {thin} vs {wide}");
        }
    }
}
