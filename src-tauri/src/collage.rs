//! Collage rendering. The dialog lays the photos out on previews; saving
//! renders every photo with its recipe through the export pipeline at the
//! size its cell needs, places it on the collage canvas (rounded corners
//! antialiased) and writes a JPEG or 16-bit TIFF with its colour profile,
//! never over an existing file.
use crate::app_state::AppState;
use crate::file_management::parse_virtual_path;
use crate::formats::is_raw_file;
use crate::gpu_processing::get_or_init_gpu_context;
use rawtonic_export::export_processing::{
    ExportSettings, RenderEnv, encode_with_metadata, parse_hex_color, prepare_export_source,
    process_image_for_export,
};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{Emitter, Manager};

/// One photo of the collage: its cell and where the photo is drawn, in
/// output pixels (the dialog's layout scaled to the output size).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollageCell {
    path: String,
    adjustments: Value,
    /// x, y, width, height of the cell (the clip).
    cell: [f64; 4],
    /// x, y, width, height of the whole photo; it may extend past the cell.
    draw: [f64; 4],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollageSpec {
    width: u32,
    height: u32,
    background: String,
    radius: f64,
    /// jpeg or tiff (16-bit).
    format: String,
    cells: Vec<CollageCell>,
}

/// Coverage of a pixel centre by a rounded rectangle (1 inside, a 1-pixel
/// ramp at the edge).
fn coverage(px: f64, py: f64, rect: &[f64; 4], radius: f64) -> f32 {
    let [x, y, w, h] = *rect;
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let qx = (px - cx).abs() - (w / 2.0 - r);
    let qy = (py - cy).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let inside = qx.max(qy).min(0.0);
    let distance = outside + inside - r;
    (0.5 - distance).clamp(0.0, 1.0) as f32
}

fn output_path(first: &Path, extension: &str, attempt: usize) -> Result<PathBuf, String> {
    let parent = first
        .parent()
        .ok_or_else(|| "The photo has no folder".to_string())?;
    let stem = first
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Collage");
    Ok(if attempt == 0 {
        parent.join(format!("{stem}-Collage.{extension}"))
    } else {
        parent.join(format!("{stem}-Collage-{}.{extension}", attempt + 1))
    })
}

#[tauri::command]
pub(crate) async fn save_collage(
    spec: CollageSpec,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    // RAWs develop as in a single-image export, AI Denoise included.
    let raw_recipes = spec
        .cells
        .iter()
        .filter(|cell| is_raw_file(parse_virtual_path(&cell.path).0))
        .map(|cell| &cell.adjustments);
    crate::export_processing::prepare_ai_denoise(&app_handle, raw_recipes).await;
    tokio::task::spawn_blocking(move || render_and_write(&spec, &app_handle))
        .await
        .map_err(|e| format!("Collage failed: {e}"))?
}

fn render_and_write(spec: &CollageSpec, app_handle: &tauri::AppHandle) -> Result<String, String> {
    if spec.cells.is_empty() || spec.width == 0 || spec.height == 0 {
        return Err("The collage is empty.".to_string());
    }
    if spec.width.max(spec.height) > 30_000
        || (spec.width as u64 * spec.height as u64) > 400_000_000
    {
        return Err("The collage is too large.".to_string());
    }
    let state = app_handle.state::<AppState>();
    let context = get_or_init_gpu_context(&state, app_handle)?;
    let settings = crate::app_settings::load_settings(app_handle.clone()).unwrap_or_default();
    let tiff = spec.format == "tiff";
    let extension = if tiff { "tiff" } else { "jpg" };
    let export_settings: ExportSettings = serde_json::from_value(serde_json::json!({
        "jpegQuality": 92,
        "tiffBitDepth": 16,
        "keepMetadata": false,
        "stripGps": true,
    }))
    .map_err(|e| e.to_string())?;

    let (w, h) = (spec.width as usize, spec.height as usize);
    let bg = parse_hex_color(&spec.background).unwrap_or([255, 255, 255]);
    let mut canvas: Vec<f32> = (0..w * h)
        .flat_map(|_| bg.map(|c| c as f32 / 255.0))
        .collect();
    let mut generated = false;

    for (index, cell) in spec.cells.iter().enumerate() {
        let _ = app_handle.emit(
            "collage-progress",
            serde_json::json!({ "current": index + 1, "total": spec.cells.len() }),
        );
        let (source_path, _) = parse_virtual_path(&cell.path);
        let source = source_path.to_string_lossy().to_string();
        let is_raw = is_raw_file(&source);
        let mut adjustments = cell.adjustments.clone();
        crate::adjustment_utils::hydrate_adjustments(&state, &mut adjustments);
        // A written image: never with a lens correction that could not be
        // resolved (as the single-photo export).
        rawtonic_export::exif_processing::prepare_recipe_lens(&mut adjustments, &source_path)?;
        generated |= rawtonic_engine::image_loader::has_generated_areas(&adjustments);
        let pristine = crate::export_processing::load_export_base_image(
            &state,
            &source,
            is_raw,
            &adjustments,
            &settings,
        )
        .map_err(|e| format!("Failed to load {}: {e}", source))?;
        let export_source = prepare_export_source(&source, Arc::new(pristine), &adjustments)?;
        let tonemapper_override = |is_raw| {
            crate::image_processing::resolve_tonemapper_override_from_handle(app_handle, is_raw)
        };
        let load_lut = |path: &str| crate::lut_processing::get_or_load_lut(&state, path).ok();
        let env = RenderEnv {
            context: &context,
            processor: &state.gpu_processor,
            image_cache: &state.gpu_image_cache,
            tonemapper_override: &tonemapper_override,
            load_lut: &load_lut,
        };
        let rendered = process_image_for_export(
            &source,
            export_source,
            &adjustments,
            &export_settings,
            &env,
            is_raw,
            "tiff",
        )?
        .into_rgb32f();

        // Resample the photo to its drawn size, then place it.
        let [dx, dy, dw, dh] = cell.draw;
        let (tw, th) = (dw.round().max(1.0) as u32, dh.round().max(1.0) as u32);
        let photo =
            image::imageops::resize(&rendered, tw, th, image::imageops::FilterType::Lanczos3);
        drop(rendered);
        let [cx, cy, cw, ch] = cell.cell;
        let (x0, y0) = (cx.floor().max(0.0) as usize, cy.floor().max(0.0) as usize);
        let x1 = ((cx + cw).ceil() as usize).min(w);
        let y1 = ((cy + ch).ceil() as usize).min(h);
        let src: &[f32] = &photo;
        for y in y0..y1 {
            for x in x0..x1 {
                let a = coverage(x as f64 + 0.5, y as f64 + 0.5, &cell.cell, spec.radius);
                if a <= 0.0 {
                    continue;
                }
                let sx = (x as f64 - dx).floor();
                let sy = (y as f64 - dy).floor();
                if sx < 0.0 || sy < 0.0 || sx >= tw as f64 || sy >= th as f64 {
                    continue;
                }
                let s = &src[((sy as usize) * tw as usize + sx as usize) * 3..][..3];
                let d = &mut canvas[(y * w + x) * 3..][..3];
                for c in 0..3 {
                    d[c] = d[c] * (1.0 - a) + s[c].clamp(0.0, 1.0) * a;
                }
            }
        }
    }

    let image = if tiff {
        let data: Vec<u16> = canvas
            .iter()
            .map(|v| (v * 65535.0).round() as u16)
            .collect();
        image::DynamicImage::ImageRgb16(
            image::ImageBuffer::from_raw(spec.width, spec.height, data)
                .ok_or_else(|| "collage buffer".to_string())?,
        )
    } else {
        let data: Vec<u8> = canvas.iter().map(|v| (v * 255.0).round() as u8).collect();
        image::DynamicImage::ImageRgb8(
            image::ImageBuffer::from_raw(spec.width, spec.height, data)
                .ok_or_else(|| "collage buffer".to_string())?,
        )
    };
    let first = parse_virtual_path(&spec.cells[0].path).0;
    let metadata = rawtonic_export::exif_processing::export_metadata_with(
        &first.to_string_lossy(),
        extension,
        false,
        true,
        spec.width,
        spec.height,
        generated,
        &|| None,
        &|| None,
    );
    let bytes = encode_with_metadata(image, extension, &export_settings, metadata.as_ref())?;
    for attempt in 0..100 {
        let path = output_path(&first, extension, attempt)?;
        match crate::session_bridge::write_new_output(&path, &bytes) {
            Ok(()) => return Ok(path.to_string_lossy().into_owned()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Failed to save the collage: {e}")),
        }
    }
    Err("Too many collages with this name".to_string())
}

#[cfg(test)]
mod tests {
    use super::coverage;

    #[test]
    fn rounded_corners_are_antialiased() {
        let rect = [10.0, 10.0, 100.0, 50.0];
        assert_eq!(coverage(60.0, 35.0, &rect, 8.0), 1.0);
        assert_eq!(coverage(5.0, 35.0, &rect, 8.0), 0.0);
        // The corner itself is cut off by the radius.
        assert_eq!(coverage(10.5, 10.5, &rect, 8.0), 0.0);
        let edge = coverage(10.0, 35.0, &rect, 8.0);
        assert!(edge > 0.4 && edge < 0.6, "{edge}");
        // Without radius the corner pixel is covered.
        assert_eq!(coverage(10.5, 10.5, &rect, 0.0), 1.0);
    }
}
