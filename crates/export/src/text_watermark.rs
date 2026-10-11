//! Text watermarks: a line or lines of text in one of the computer's
//! installed fonts, with a colour and an optional soft shadow (Lightroom's
//! and Capture One's text watermarks), rendered at the size they are placed
//! at so they stay sharp.

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
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

fn load_font(family: Option<&str>) -> Option<FontVec> {
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
    db.with_face_data(id, |data, index| {
        FontVec::try_from_vec_and_index(data.to_vec(), index).ok()
    })?
}

fn parse_color(hex: &str) -> [u8; 3] {
    crate::export_processing::parse_hex_color(hex).unwrap_or([255, 255, 255])
}

/// Width of each line at `font`'s scale, with kerning.
fn line_widths<F: Font>(font: &ab_glyph::PxScaleFont<&F>, lines: &[&str]) -> Vec<f32> {
    lines
        .iter()
        .map(|line| {
            let mut width = 0.0;
            let mut previous = None;
            for c in line.chars() {
                let id = font.glyph_id(c);
                if let Some(prev) = previous {
                    width += font.kern(prev, id);
                }
                width += font.h_advance(id);
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
    let lines: Vec<&str> = text.lines().collect();
    // Measure at 100 px, then scale the font to the requested width.
    let probe = font.as_scaled(PxScale::from(100.0));
    let widest = line_widths(&probe, &lines)
        .into_iter()
        .fold(0.0f32, f32::max);
    if widest <= 0.0 {
        return None;
    }
    let size = (100.0 * target_width as f32 / widest).clamp(4.0, 4000.0);
    let scaled = font.as_scaled(PxScale::from(size));
    let line_height = scaled.height() + scaled.line_gap();
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
        let baseline = margin + scaled.ascent() + row as f32 * line_height;
        let mut x = margin;
        let mut previous = None;
        for c in line.chars() {
            let id = scaled.glyph_id(c);
            if let Some(prev) = previous {
                x += scaled.kern(prev, id);
            }
            let glyph = id.with_scale_and_position(size, ab_glyph::point(x, baseline));
            x += scaled.h_advance(id);
            previous = Some(id);
            let Some(outline) = font.outline_glyph(glyph) else {
                continue;
            };
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, c| {
                let (px, py) = (
                    bounds.min.x as i64 + gx as i64,
                    bounds.min.y as i64 + gy as i64,
                );
                if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < height {
                    let pixel = coverage.get_pixel_mut(px as u32, py as u32);
                    pixel[0] = pixel[0].max((c.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
            });
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
