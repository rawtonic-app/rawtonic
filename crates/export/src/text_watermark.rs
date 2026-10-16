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
    data: Vec<u8>,
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
        data: data.to_vec(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
