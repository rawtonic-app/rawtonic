//! Text watermarks: a line or lines of text in one of the computer's
//! installed fonts, with a colour and an optional soft shadow (Lightroom's
//! and Capture One's text watermarks), rendered at the size they are placed
//! at so they stay sharp.
//!
//! Fonts are read with skrifa (outlines, metrics, `kern` pairs) and the
//! outlines filled with ab_glyph_rasterizer's coverage rasterizer.

use ab_glyph_rasterizer::{Point, Rasterizer, point};
use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlineGlyphCollection, OutlinePen};
use skrifa::raw::TableProvider;
use skrifa::raw::tables::kern::SubtableKind;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use std::sync::OnceLock;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextWatermark {
    pub text: String,
    /// An installed font family; missing or unknown falls back to the
    /// system's sans-serif font.
    #[serde(default)]
    pub font_family: Option<String>,
    /// "#rrggbb".
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub shadow: bool,
}

fn default_color() -> String {
    "#ffffff".to_string()
}

fn database() -> &'static fontdb::Database {
    static DATABASE: OnceLock<fontdb::Database> = OnceLock::new();
    DATABASE.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db
    })
}

/// The installed font families, sorted and without duplicates.
pub fn font_families() -> Vec<String> {
    let mut families: Vec<String> = database()
        .faces()
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .filter(|name| !name.starts_with('.'))
        .collect();
    families.sort_by_key(|name| name.to_lowercase());
    families.dedup();
    families
}

/// An installed font file's data and the face's index in it (collections).
struct FontData {
    /// Shared with the shaper's copy of the face (`Typeface`).
    data: std::sync::Arc<Vec<u8>>,
    index: u32,
}

/// What the watermark needs of a face, in font units.
struct Face<'a> {
    charmap: skrifa::charmap::Charmap<'a>,
    glyph_metrics: skrifa::metrics::GlyphMetrics<'a>,
    outlines: OutlineGlyphCollection<'a>,
    /// Horizontal, non-variable `kern` subtables; the first with a value for
    /// a pair wins.
    kern: Vec<SubtableKind<'a>>,
    ascent: f32,
    descent: f32,
    line_gap: f32,
}

impl<'a> Face<'a> {
    fn new(font: &'a FontData) -> Option<Self> {
        let font = FontRef::from_index(&font.data, font.index).ok()?;
        let metrics =
            skrifa::metrics::Metrics::new(&font, Size::unscaled(), LocationRef::default());
        let kern = font
            .kern()
            .map(|kern| {
                kern.subtables()
                    .filter_map(Result::ok)
                    .filter(|table| table.is_horizontal() && !table.is_variable())
                    .filter_map(|table| table.kind().ok())
                    .collect()
            })
            .unwrap_or_default();
        Some(Face {
            charmap: font.charmap(),
            glyph_metrics: font.glyph_metrics(Size::unscaled(), LocationRef::default()),
            outlines: font.outline_glyphs(),
            kern,
            ascent: metrics.ascent,
            descent: metrics.descent,
            line_gap: metrics.leading,
        })
    }

    /// The glyph for `c`, or the missing-glyph box.
    fn glyph_id(&self, c: char) -> GlyphId {
        self.charmap.map(c).unwrap_or(GlyphId::NOTDEF)
    }

    fn advance(&self, id: GlyphId) -> f32 {
        self.glyph_metrics.advance_width(id).unwrap_or(0.0)
    }

    fn kern(&self, left: GlyphId, right: GlyphId) -> f32 {
        self.kern
            .iter()
            .find_map(|table| match table {
                SubtableKind::Format0(t) => t.kerning(left, right),
                SubtableKind::Format2(t) => t.kerning(left, right),
                SubtableKind::Format3(t) => t.kerning(left, right),
                SubtableKind::Format1(_) => None,
            })
            .unwrap_or(0) as f32
    }

    /// Pixels per font unit for a font `size` pixels from descender to
    /// ascender.
    fn scale(&self, size: f32) -> f32 {
        size / (self.ascent - self.descent)
    }

    /// The glyph's outline in font units, with its bounds (min, max), or none
    /// for glyphs without ink.
    fn outline(&self, id: GlyphId) -> Option<(Vec<Curve>, Point, Point)> {
        let glyph = self.outlines.get(id)?;
        let mut pen = CurvePen::default();
        glyph
            .draw(
                DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                &mut pen,
            )
            .ok()?;
        pen.close();
        let mut min = point(f32::INFINITY, f32::INFINITY);
        let mut max = point(f32::NEG_INFINITY, f32::NEG_INFINITY);
        for p in pen.curves.iter().flat_map(Curve::points) {
            min = point(min.x.min(p.x), min.y.min(p.y));
            max = point(max.x.max(p.x), max.y.max(p.y));
        }
        (min.x < max.x && min.y < max.y).then_some((pen.curves, min, max))
    }
}

enum Curve {
    Line(Point, Point),
    Quad(Point, Point, Point),
    Cubic(Point, Point, Point, Point),
}

impl Curve {
    fn points(&self) -> Vec<Point> {
        match *self {
            Curve::Line(a, b) => vec![a, b],
            Curve::Quad(a, b, c) => vec![a, b, c],
            Curve::Cubic(a, b, c, d) => vec![a, b, c, d],
        }
    }
}

/// Collects an outline as curves, each contour closed back to its start.
#[derive(Default)]
struct CurvePen {
    last: Point,
    last_move: Option<Point>,
    curves: Vec<Curve>,
}

impl OutlinePen for CurvePen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.last = point(x, y);
        self.last_move = Some(self.last);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = point(x, y);
        self.curves.push(Curve::Line(self.last, p));
        self.last = p;
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let p = point(x, y);
        self.curves.push(Curve::Quad(self.last, point(cx0, cy0), p));
        self.last = p;
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let p = point(x, y);
        self.curves
            .push(Curve::Cubic(self.last, point(cx0, cy0), point(cx1, cy1), p));
        self.last = p;
    }

    fn close(&mut self) {
        if let Some(start) = self.last_move.take() {
            self.curves.push(Curve::Line(self.last, start));
        }
    }
}

/// Rasterises glyph `id` with its origin at `position` (y down) at `scale`
/// pixels per font unit, calling `put(x, y, coverage)` for each pixel of its
/// whole-pixel bounding box.
fn draw_glyph(
    face: &Face,
    id: GlyphId,
    scale: f32,
    position: Point,
    mut put: impl FnMut(i64, i64, f32),
) {
    let Some((curves, min, max)) = face.outline(id) else {
        return;
    };
    // Pixel bounds (font y up, image y down), rounded outwards; the
    // subpixel fraction is applied before rounding.
    let (x_trunc, x_fract) = (position.x.trunc(), position.x.fract());
    let (y_trunc, y_fract) = (position.y.trunc(), position.y.fract());
    let left = (min.x * scale + x_fract).floor() + x_trunc;
    let right = (max.x * scale + x_fract).ceil() + x_trunc;
    let top = (max.y * -scale + y_fract).floor() + y_trunc;
    let bottom = (min.y * -scale + y_fract).ceil() + y_trunc;
    let offset = point(position.x - left, position.y - top);
    let to_px = |p: Point| point(p.x * scale + offset.x, p.y * -scale + offset.y);
    let mut rasterizer = Rasterizer::new((right - left) as usize, (bottom - top) as usize);
    for curve in &curves {
        match *curve {
            Curve::Line(a, b) => rasterizer.draw_line(to_px(a), to_px(b)),
            Curve::Quad(a, b, c) => rasterizer.draw_quad(to_px(a), to_px(b), to_px(c)),
            Curve::Cubic(a, b, c, d) => {
                rasterizer.draw_cubic(to_px(a), to_px(b), to_px(c), to_px(d))
            }
        }
    }
    rasterizer.for_each_pixel_2d(|x, y, coverage| {
        put(left as i64 + x as i64, top as i64 + y as i64, coverage)
    });
}

fn load_font(family: Option<&str>) -> Option<FontData> {
    let db = database();
    let name;
    let mut families = Vec::new();
    if let Some(family) = family.filter(|f| !f.trim().is_empty()) {
        name = family.to_string();
        families.push(fontdb::Family::Name(&name));
    }
    families.push(fontdb::Family::SansSerif);
    // fontdb maps the generic sans-serif to Arial, which most Linux systems
    // lack: try common sans families there, then any installed face.
    families.extend(
        ["DejaVu Sans", "Liberation Sans", "Noto Sans", "Helvetica"].map(fontdb::Family::Name),
    );
    let id = db
        .query(&fontdb::Query {
            families: &families,
            ..Default::default()
        })
        .or_else(|| db.faces().next().map(|face| face.id))?;
    let font = db.with_face_data(id, |data, index| FontData {
        data: std::sync::Arc::new(data.to_vec()),
        index,
    })?;
    // Only faces skrifa can read.
    Face::new(&font).is_some().then_some(font)
}

fn parse_color(hex: &str) -> [u8; 3] {
    crate::export_processing::parse_hex_color(hex).unwrap_or([255, 255, 255])
}

/// Width of each line at `scale` pixels per font unit, with kerning.
fn line_widths(face: &Face, scale: f32, lines: &[&str]) -> Vec<f32> {
    lines
        .iter()
        .map(|line| {
            let mut width = 0.0;
            let mut previous = None;
            for c in line.chars() {
                let id = face.glyph_id(c);
                if let Some(prev) = previous {
                    width += face.kern(prev, id) * scale;
                }
                width += face.advance(id) * scale;
                previous = Some(id);
            }
            width
        })
        .collect()
}

/// The watermark `target_width` pixels wide (lines left-aligned), or none
/// for empty text or when no font can be loaded.
pub fn render(watermark: &TextWatermark, target_width: u32) -> Option<RgbaImage> {
    let text = watermark.text.trim_end();
    if text.trim().is_empty() || target_width == 0 {
        return None;
    }
    let font = load_font(watermark.font_family.as_deref())?;
    let face = Face::new(&font)?;
    let lines: Vec<&str> = text.lines().collect();
    // Measure at 100 px, then scale the font to the requested width.
    let widest = line_widths(&face, face.scale(100.0), &lines)
        .into_iter()
        .fold(0.0f32, f32::max);
    if widest <= 0.0 {
        return None;
    }
    let size = (100.0 * target_width as f32 / widest).clamp(4.0, 4000.0);
    let scale = face.scale(size);
    let ascent = face.ascent * scale;
    let line_height = (face.ascent - face.descent) * scale + face.line_gap * scale;
    let shadow_offset = if watermark.shadow {
        (size * 0.04).ceil().max(1.0)
    } else {
        0.0
    };
    let margin = shadow_offset * 3.0;
    let width = (target_width as f32 + 2.0 * margin).ceil() as u32 + 2;
    let height = (line_height * lines.len() as f32 + 2.0 * margin).ceil() as u32 + 2;

    // Coverage of the glyphs, then coloured (and shadowed) into RGBA.
    let mut coverage = image::GrayImage::new(width, height);
    for (row, line) in lines.iter().enumerate() {
        let baseline = margin + ascent + row as f32 * line_height;
        let mut x = margin;
        let mut previous = None;
        for c in line.chars() {
            let id = face.glyph_id(c);
            if let Some(prev) = previous {
                x += face.kern(prev, id) * scale;
            }
            draw_glyph(&face, id, scale, point(x, baseline), |px, py, c| {
                if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < height {
                    let pixel = coverage.get_pixel_mut(px as u32, py as u32);
                    pixel[0] = pixel[0].max((c.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
            });
            x += face.advance(id) * scale;
            previous = Some(id);
        }
    }
    let [r, g, b] = parse_color(&watermark.color);
    let mut out = RgbaImage::new(width, height);
    if watermark.shadow {
        let shadow = image::imageops::blur(&coverage, shadow_offset);
        let offset = shadow_offset as u32;
        for (x, y, pixel) in shadow.enumerate_pixels() {
            let (sx, sy) = (x + offset, y + offset);
            if sx < width && sy < height {
                out.put_pixel(sx, sy, Rgba([0, 0, 0, (pixel[0] as f32 * 0.6) as u8]));
            }
        }
    }
    for (x, y, pixel) in coverage.enumerate_pixels() {
        let a = pixel[0] as f32 / 255.0;
        if a <= 0.0 {
            continue;
        }
        // Text over its shadow ("over" compositing).
        let under = *out.get_pixel(x, y);
        let ua = under[3] as f32 / 255.0 * (1.0 - a);
        let alpha = a + ua;
        let mix =
            |top: u8, bottom: u8| ((top as f32 * a + bottom as f32 * ua) / alpha).round() as u8;
        out.put_pixel(
            x,
            y,
            Rgba([
                mix(r, under[0]),
                mix(g, under[1]),
                mix(b, under[2]),
                (alpha * 255.0).round() as u8,
            ]),
        );
    }
    Some(out)
}

/// One drawing operation of a glyph outline, in points of a y-up page
/// (`Typeface::outline`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathOp {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

/// An installed font for running text (proof sheet captions and headers),
/// sized in points per em, whose faces allow embedding in documents.
/// Characters the font lacks come from another installed face that has
/// them (`for_text`), so names in any script print.
pub struct Typeface {
    /// The chosen face first, then the fallbacks.
    faces: Vec<Loaded>,
}

struct Loaded {
    font: FontData,
    /// The same face for shaping (GSUB/GPOS: ligatures, joining scripts,
    /// marks), sharing its data.
    shaper: harfrust::Font,
    units_per_em: f32,
}

impl Loaded {
    fn new(font: FontData) -> Option<Self> {
        Face::new(&font)?;
        if !embedding_allowed(&font) {
            return None;
        }
        let units_per_em = FontRef::from_index(&font.data, font.index)
            .ok()?
            .head()
            .ok()?
            .units_per_em() as f32;
        let data: std::sync::Arc<dyn AsRef<[u8]> + Send + Sync> = font.data.clone();
        let shaper = harfrust::Font::new(harfrust::font::Blob::from(data), font.index)?;
        (units_per_em > 0.0).then_some(Self {
            font,
            shaper,
            units_per_em,
        })
    }
}

/// Whether a face's OS/2 `fsType` lets its glyphs go into a document: not
/// "restricted license embedding" (bits 0–3 = 2) and not bitmap-only.
fn embedding_allowed(font: &FontData) -> bool {
    let Ok(font) = FontRef::from_index(&font.data, font.index) else {
        return false;
    };
    match font.os2() {
        Ok(os2) => {
            let fs_type = os2.fs_type();
            fs_type & 0x000F != 0x0002 && fs_type & 0x0200 == 0
        }
        // No OS/2 table: no restriction stated.
        Err(_) => true,
    }
}

/// A glyph placed on a line: which face, which glyph, its pen position
/// and its offset above the baseline (marks).
struct Placed {
    face: usize,
    id: GlyphId,
    x: f32,
    y: f32,
}

impl Typeface {
    /// `family`, else the system's sans-serif font, as the watermark finds
    /// them; a face whose licence forbids embedding is passed over for the
    /// next. None when no installed face can be used.
    pub fn load(family: Option<&str>) -> Option<Self> {
        let main = load_font(family).and_then(Loaded::new).or_else(|| {
            let db = database();
            [
                "Helvetica",
                "Arial",
                "Liberation Sans",
                "DejaVu Sans",
                "Noto Sans",
            ]
            .iter()
            .filter_map(|name| {
                db.query(&fontdb::Query {
                    families: &[fontdb::Family::Name(name)],
                    ..Default::default()
                })
            })
            .filter_map(|id| {
                db.with_face_data(id, |data, index| FontData {
                    data: std::sync::Arc::new(data.to_vec()),
                    index,
                })
            })
            .find_map(Loaded::new)
        })?;
        Some(Self { faces: vec![main] })
    }

    /// `load`, with fallback faces for the characters of `texts` the font
    /// lacks (the first installed face, by name, that has each).
    pub fn for_text<'a>(
        family: Option<&str>,
        texts: impl IntoIterator<Item = &'a str>,
    ) -> Option<Self> {
        let mut typeface = Self::load(family)?;
        let mut missing: Vec<char> = Vec::new();
        for c in texts.into_iter().flat_map(str::chars) {
            if !c.is_control()
                && !c.is_whitespace()
                && !missing.contains(&c)
                && typeface.face_for(c).is_none()
            {
                missing.push(c);
            }
        }
        if missing.is_empty() {
            return Some(typeface);
        }
        let db = database();
        let mut candidates: Vec<&fontdb::FaceInfo> = db
            .faces()
            .filter(|face| {
                face.families
                    .first()
                    .is_some_and(|(name, _)| !name.starts_with('.'))
            })
            .filter(|face| face.style == fontdb::Style::Normal)
            .collect();
        candidates.sort_by_key(|face| (face.weight.0.abs_diff(400), face.families[0].0.clone()));
        for face in candidates {
            if missing.iter().all(|c| typeface.face_for(*c).is_some()) {
                break;
            }
            let Some(font) = db.with_face_data(face.id, |data, index| FontData {
                data: std::sync::Arc::new(data.to_vec()),
                index,
            }) else {
                continue;
            };
            let useful = FontRef::from_index(&font.data, font.index)
                .map(|f| {
                    let charmap = f.charmap();
                    missing
                        .iter()
                        .any(|c| typeface.face_for(*c).is_none() && charmap.map(*c).is_some())
                })
                .unwrap_or(false);
            if useful && let Some(loaded) = Loaded::new(font) {
                typeface.faces.push(loaded);
            }
        }
        Some(typeface)
    }

    /// The first face that has `c`.
    fn face_for(&self, c: char) -> Option<usize> {
        self.faces.iter().position(|loaded| {
            FontRef::from_index(&loaded.font.data, loaded.font.index)
                .is_ok_and(|f| f.charmap().map(c).is_some())
        })
    }

    fn faces(&self) -> Vec<Face<'_>> {
        self.faces
            .iter()
            .map(|loaded| Face::new(&loaded.font).expect("checked when loaded"))
            .collect()
    }

    /// The glyphs of `text` at `size` from pen position 0, in visual order,
    /// and the line's width: the line is ordered by the Unicode bidi
    /// algorithm, each run cut where the face that has its characters
    /// changes and shaped with that face (HarfBuzz rules: kerning,
    /// ligatures, Arabic joining, Indic reordering, mark positions).
    fn place(&self, faces: &[Face], text: &str, size: f32) -> (Vec<Placed>, f32) {
        let mut placed = Vec::with_capacity(text.len());
        let mut x = 0.0;
        let bidi = unicode_bidi::BidiInfo::new(text, None);
        for paragraph in &bidi.paragraphs {
            let (levels, runs) = bidi.visual_runs(paragraph, paragraph.range.clone());
            for run in runs {
                let rtl = levels[run.start].is_rtl();
                // The run's stretches of one face, in logical order.
                let mut stretches: Vec<(usize, std::ops::Range<usize>)> = Vec::new();
                for (offset, c) in text[run.clone()].char_indices() {
                    let at = run.start + offset;
                    let face = if c.is_whitespace() || c.is_control() {
                        stretches.last().map_or(0, |(face, _)| *face)
                    } else {
                        faces
                            .iter()
                            .position(|face| face.charmap.map(c).is_some())
                            .unwrap_or(0)
                    };
                    match stretches.last_mut() {
                        Some((last, range)) if *last == face => range.end = at + c.len_utf8(),
                        _ => stretches.push((face, at..at + c.len_utf8())),
                    }
                }
                if rtl {
                    stretches.reverse();
                }
                for (face, range) in stretches {
                    let scale = size / self.faces[face].units_per_em;
                    let mut buffer = harfrust::Buffer::new();
                    buffer.push_str(&text[range]);
                    buffer.set_direction(if rtl {
                        harfrust::Direction::RightToLeft
                    } else {
                        harfrust::Direction::LeftToRight
                    });
                    buffer.guess_segment_properties();
                    let shaper = harfrust::ShaperFont::new(&self.faces[face].shaper);
                    if harfrust::shape(&shaper, &mut buffer, harfrust::ShapeOptions::new()).is_err()
                    {
                        continue;
                    }
                    for (info, position) in
                        buffer.glyph_infos().iter().zip(buffer.glyph_positions())
                    {
                        placed.push(Placed {
                            face,
                            id: GlyphId::new(info.glyph_id),
                            x: x + position.x_offset as f32 * scale,
                            y: position.y_offset as f32 * scale,
                        });
                        x += position.x_advance as f32 * scale;
                    }
                }
            }
        }
        (placed, x)
    }

    /// Ascender and descender (negative) of `size` points per em.
    pub fn extent(&self, size: f32) -> (f32, f32) {
        let face = &self.faces()[0];
        let scale = size / self.faces[0].units_per_em;
        (face.ascent * scale, face.descent * scale)
    }

    /// The width of `text` at `size`, with kerning.
    pub fn width(&self, text: &str, size: f32) -> f32 {
        self.place(&self.faces(), text, size).1
    }

    /// The advance of `c` alone at `size` (a document's width table).
    pub fn advance(&self, c: char, size: f32) -> f32 {
        self.width(c.encode_utf8(&mut [0; 4]), size)
    }

    /// `text`, cut in the middle with an ellipsis until it is at most
    /// `max_width` wide at `size` (file names keep their number at the end).
    pub fn fit(&self, text: &str, size: f32, max_width: f32) -> String {
        let faces = self.faces();
        if self.place(&faces, text, size).1 <= max_width {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        for keep in (0..chars.len()).rev() {
            let head = keep.div_ceil(2);
            let tail = keep - head;
            let candidate: String = chars[..head]
                .iter()
                .chain(std::iter::once(&'…'))
                .chain(&chars[chars.len() - tail..])
                .collect();
            if self.place(&faces, &candidate, size).1 <= max_width {
                return candidate;
            }
        }
        String::new()
    }

    /// The outlines of `text` at `size`, its baseline starting at `(x, y)`
    /// of a y-up page, each glyph's contours closed.
    pub fn outline(&self, text: &str, size: f32, x: f32, y: f32) -> Vec<PathOp> {
        let faces = self.faces();
        let mut ops = Vec::new();
        for glyph in self.place(&faces, text, size).0 {
            let Some((curves, _, _)) = faces[glyph.face].outline(glyph.id) else {
                continue;
            };
            let scale = size / self.faces[glyph.face].units_per_em;
            let (pen_x, pen_y) = (x + glyph.x, y + glyph.y);
            let at = |p: Point| (pen_x + p.x * scale, pen_y + p.y * scale);
            let mut last: Option<Point> = None;
            for curve in &curves {
                let start = match *curve {
                    Curve::Line(a, _) | Curve::Quad(a, _, _) | Curve::Cubic(a, _, _, _) => a,
                };
                if last != Some(start) {
                    if last.is_some() {
                        ops.push(PathOp::Close);
                    }
                    let (sx, sy) = at(start);
                    ops.push(PathOp::Move(sx, sy));
                }
                let end = match *curve {
                    Curve::Line(_, b) => {
                        let (bx, by) = at(b);
                        ops.push(PathOp::Line(bx, by));
                        b
                    }
                    Curve::Quad(a, q, b) => {
                        // The quadratic as a cubic of the same shape.
                        let c1 =
                            point(a.x + (q.x - a.x) * 2.0 / 3.0, a.y + (q.y - a.y) * 2.0 / 3.0);
                        let c2 =
                            point(b.x + (q.x - b.x) * 2.0 / 3.0, b.y + (q.y - b.y) * 2.0 / 3.0);
                        let ((x1, y1), (x2, y2), (x3, y3)) = (at(c1), at(c2), at(b));
                        ops.push(PathOp::Cubic(x1, y1, x2, y2, x3, y3));
                        b
                    }
                    Curve::Cubic(_, c1, c2, b) => {
                        let ((x1, y1), (x2, y2), (x3, y3)) = (at(c1), at(c2), at(b));
                        ops.push(PathOp::Cubic(x1, y1, x2, y2, x3, y3));
                        b
                    }
                };
                last = Some(end);
            }
            if last.is_some() {
                ops.push(PathOp::Close);
            }
        }
        ops
    }

    /// Rasterises `text` at `size` pixels per em, its baseline starting at
    /// `(x, y)` of a y-down image, calling `put(x, y, coverage)`.
    pub fn rasterize(
        &self,
        text: &str,
        size: f32,
        x: f32,
        y: f32,
        mut put: impl FnMut(i64, i64, f32),
    ) {
        let faces = self.faces();
        for glyph in self.place(&faces, text, size).0 {
            let scale = size / self.faces[glyph.face].units_per_em;
            draw_glyph(
                &faces[glyph.face],
                glyph.id,
                scale,
                point(x + glyph.x, y - glyph.y),
                &mut put,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_text_is_shaped_and_ordered_right_to_left() {
        let text = "\u{0633}\u{0644}\u{0627}\u{0645}"; // Arabic "salam"
        let Some(typeface) = Typeface::for_text(None, [text]) else {
            return; // No fonts on this machine.
        };
        let faces = typeface.faces();
        let Some(face) = text.chars().next().and_then(|c| typeface.face_for(c)) else {
            return; // No installed face has Arabic.
        };
        let (glyphs, width) = typeface.place(&faces, text, 100.0);
        assert!(width > 0.0);
        let nominal: Vec<GlyphId> = text.chars().map(|c| faces[face].glyph_id(c)).collect();
        let shaped: Vec<GlyphId> = glyphs.iter().map(|g| g.id).collect();
        // Joining forms (and the lam-alef ligature) replace the nominal
        // glyphs, and the first letter is drawn rightmost.
        assert_ne!(shaped, nominal);
        assert!(!shaped.contains(&nominal[0]) || glyphs.last().unwrap().x > glyphs[0].x);
        // Latin is drawn left to right, glyph by glyph.
        let (latin, _) = typeface.place(&faces, "AB", 100.0);
        assert!(latin[1].x > latin[0].x);
    }

    #[test]
    fn renders_text_at_the_requested_width_in_its_colour() {
        let watermark = TextWatermark {
            text: "© Rawtonic".into(),
            font_family: None,
            color: "#ff0000".into(),
            shadow: false,
        };
        let Some(image) = render(&watermark, 400) else {
            // No fonts on this machine (minimal CI images): nothing to draw.
            assert!(font_families().is_empty());
            return;
        };
        let inked: Vec<u32> = image
            .enumerate_pixels()
            .filter(|(_, _, p)| p[3] > 128)
            .map(|(x, _, _)| x)
            .collect();
        let (left, right) = (*inked.iter().min().unwrap(), *inked.iter().max().unwrap());
        assert!(
            (right - left) as i64 > 360 && (right - left) <= 402,
            "{left}..{right}"
        );
        assert!(
            image
                .pixels()
                .filter(|p| p[3] > 128)
                .all(|p| p[0] > 200 && p[1] < 60)
        );
        let shadowed = render(
            &TextWatermark {
                shadow: true,
                ..watermark
            },
            400,
        )
        .unwrap();
        assert!(shadowed.pixels().any(|p| p[3] > 0 && p[0] < 60));
        assert!(
            render(
                &TextWatermark {
                    text: "  ".into(),
                    ..watermark_none()
                },
                400
            )
            .is_none()
        );
    }

    fn watermark_none() -> TextWatermark {
        TextWatermark {
            text: String::new(),
            font_family: None,
            color: default_color(),
            shadow: false,
        }
    }
}

#[cfg(test)]
mod tools {
    /// Tool: `$TW_IMAGE` with a text watermark (`$TW_TEXT`, `$TW_FONT`) at
    /// the bottom right, written to `$TW_OUT`.
    #[test]
    #[ignore = "tool: needs TW_IMAGE, TW_TEXT, TW_OUT"]
    fn write_text_watermark() {
        let mut image = image::open(std::env::var("TW_IMAGE").unwrap()).unwrap();
        let settings: crate::export_processing::WatermarkSettings =
            serde_json::from_value(serde_json::json!({
                "anchor": "bottomRight", "scale": 30.0, "spacing": 4.0, "opacity": 85.0,
                "text": {"text": std::env::var("TW_TEXT").unwrap(),
                    "fontFamily": std::env::var("TW_FONT").ok(), "color": "#ffffff", "shadow": true}
            }))
            .unwrap();
        crate::export_processing::apply_watermark(&mut image, &settings).unwrap();
        image.save(std::env::var("TW_OUT").unwrap()).unwrap();
    }
}
