//! Proof sheets from the Library selection (`rawtonic_export::proof_sheet`).
//! A sheet is an export job: its photos are rendered by the export queue at
//! the cell size into a folder of the app's cache (pinned, cancellable,
//! resumable, retried like any export), and when every photo is rendered
//! the queue lays out the pages and writes the PDF and/or JPEGs into the
//! chosen folder, never over a file (`compose`). Originals are only read.
use crate::export_processing::{ExportSettings, naming_capture, pin_photos};
use crate::export_queue::{ExportRequest, SubmittedFrom};
use rawtonic_export::export_processing::WatermarkSettings;
use rawtonic_export::output_sharpening::OutputSharpening;
use rawtonic_export::proof_sheet::{
    CellImage, Layout, PageValues, ProofSheet, caption, cell_settings, file_stem, page_text,
    write_files, write_sheet,
};
use rawtonic_export::queue::{Item, ItemState};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tauri::Manager;

/// What a queued sheet is laid out with once its photos are rendered,
/// stored with its export job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofJob {
    pub sheet: ProofSheet,
    /// The folder the sheet is written into.
    pub destination: String,
    /// The file name without extension.
    pub stem: String,
    pub values: PageValues,
    /// Each photo's caption, read when the sheet was queued.
    pub captions: Vec<String>,
    /// Some photo has generated areas: the sheet carries the AI marking.
    pub generated: bool,
}

/// What the Proof Sheet dialog asks for.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofSheetRequest {
    pub paths: Vec<String>,
    pub sheet: ProofSheet,
    pub destination: String,
    /// The file name; `{job}` and `{date}` are filled.
    pub name: String,
    pub job_name: String,
    #[serde(default)]
    pub watermark: Option<WatermarkSettings>,
    #[serde(default)]
    pub output_sharpening: Option<OutputSharpening>,
    #[serde(default)]
    pub current_edit_path: Option<String>,
    #[serde(default)]
    pub current_edit_adjustments: Option<Value>,
    pub library: SubmittedFrom,
}

/// The sheet's first page for the dialog's preview, laid out exactly as it
/// will be written.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofSheetPreview {
    pub layout: Layout,
    /// Page 1's crop marks as `[x0, y0, x1, y1]` in millimetres; none when off.
    pub crop_marks: Vec<[f64; 4]>,
    /// The captions of the first page's photos, in their order.
    pub captions: Vec<String>,
    pub header: Option<String>,
    pub footer: Option<String>,
    pub file_name: String,
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

impl ProofSheetRequest {
    fn values(&self) -> PageValues {
        PageValues {
            job: self.job_name.trim().to_string(),
            date: today(),
            count: self.paths.len(),
        }
    }

    /// Captions of `paths` (photos `first..` of the sheet) and whether
    /// any has generated areas, read as their export reads them.
    fn captions(&self, first: usize, paths: &[String]) -> Result<(Vec<String>, bool), String> {
        let photos = pin_photos(
            paths,
            self.current_edit_path.as_deref(),
            self.current_edit_adjustments.as_ref(),
            self.library.directory(),
        )?;
        let naming = ExportSettings {
            job_name: Some(self.job_name.clone()),
            ..cell_settings(&self.sheet.layout(1)?, None, None)
        };
        let template = self
            .sheet
            .caption
            .as_deref()
            .filter(|c| !c.trim().is_empty());
        let total = self.paths.len();
        let mut generated = false;
        let captions = paths
            .iter()
            .zip(&photos)
            .enumerate()
            .map(|(offset, (path, pinned))| {
                generated |=
                    rawtonic_engine::image_loader::has_generated_areas(&pinned.adjustments);
                let Some(template) = template else {
                    return String::new();
                };
                let (source, _) = crate::file_management::parse_virtual_path(path);
                let (sequence, last) = naming.filename_counter(first + offset, total);
                let capture = rawtonic_naming::needs_capture(template)
                    .then(|| naming_capture(&source, pinned.metadata.as_ref()));
                let context = naming.naming_context(
                    &source,
                    sequence,
                    last,
                    &pinned.naming_date(&source),
                    "jpg",
                    capture.as_ref(),
                );
                caption(template, &context)
            })
            .collect();
        Ok((captions, generated))
    }

    fn file_name(&self, pages: usize) -> String {
        file_stem(&page_text(&self.name, &self.values(), 1, pages))
    }
}

#[tauri::command]
pub async fn proof_sheet_preview(request: ProofSheetRequest) -> Result<ProofSheetPreview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let layout = request.sheet.layout(request.paths.len())?;
        let first_page = request.paths.len().min(request.sheet.photos_per_page());
        let (captions, _) = request.captions(0, &request.paths[..first_page])?;
        let values = request.values();
        let pages = layout.pages.len();
        let text = |t: &Option<String>| {
            t.as_deref()
                .filter(|t| !t.trim().is_empty())
                .map(|t| page_text(t, &values, 1, pages))
        };
        Ok(ProofSheetPreview {
            captions,
            header: text(&request.sheet.header),
            footer: text(&request.sheet.footer),
            file_name: request.file_name(pages),
            crop_marks: rawtonic_export::proof_sheet::crop_marks(&request.sheet, &layout, 0),
            layout,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The folder in the app's cache that holds the sheets' rendered photos
/// until their pages are written.
fn staging_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("proof-sheets"))
}

/// Queues a proof sheet of `request.paths`; returns its export job.
#[tauri::command]
pub async fn queue_proof_sheet(
    request: ProofSheetRequest,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    if request.paths.is_empty() {
        return Err("Choose photos for the proof sheet.".into());
    }
    let destination = PathBuf::from(&request.destination);
    if !destination.is_dir() {
        return Err(format!(
            "The folder {} is not available.",
            destination.display()
        ));
    }
    crate::export_processing::ensure_writable_folder(&destination)?;
    let layout = request.sheet.layout(request.paths.len())?;
    let staging = staging_root(&app_handle)?.join(uuid::Uuid::new_v4().to_string());
    let (request, staging) = tauri::async_runtime::spawn_blocking(move || {
        let mut captions = Vec::with_capacity(request.paths.len());
        let mut generated = false;
        for (chunk, paths) in request.paths.chunks(64).enumerate() {
            let (mut some, any) = request.captions(chunk * 64, paths)?;
            captions.append(&mut some);
            generated |= any;
        }
        std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        Ok::<_, String>(((request, captions, generated), staging))
    })
    .await
    .map_err(|e| e.to_string())??;
    let (request, captions, generated) = request;
    let stem = request.file_name(layout.pages.len());
    let job = ProofJob {
        sheet: request.sheet.clone(),
        destination: request.destination.clone(),
        stem: stem.clone(),
        values: request.values(),
        captions,
        generated,
    };
    let export = ExportRequest {
        paths: request.paths,
        display_label: Some(format!("{stem} (proof sheet)")),
        output_folder_or_file: staging.to_string_lossy().into_owned(),
        is_explicit_file_path: false,
        base_origin_folders: Vec::new(),
        export_settings: cell_settings(&layout, request.watermark, request.output_sharpening),
        output_format: "jpg".into(),
        current_edit_path: request.current_edit_path,
        current_edit_adjustments: request.current_edit_adjustments,
        library: request.library,
        pinned: None,
        edit_with: None,
        edit_target: None,
        proof_sheet: Some(job),
    };
    let queued = crate::export_queue::enqueue(&app_handle, export).await;
    if queued.is_err() {
        remove_staging(&app_handle, &staging);
    }
    queued
}

/// Lays out the sheet of a job whose photos are all rendered (`items`, the
/// rendered photos in their outputs) and writes its files. Blocking.
pub(crate) fn compose(items: &[Item], job: &ProofJob) -> Result<Vec<PathBuf>, String> {
    let mut items: Vec<&Item> = items.iter().collect();
    items.sort_by_key(|item| item.index);
    let cells = items
        .iter()
        .map(|item| {
            let output = item
                .output
                .as_ref()
                .filter(|_| item.state == ItemState::Done)
                .ok_or_else(|| format!("{} was not rendered", item.path))?;
            let jpeg = std::fs::read(output)
                .map_err(|e| format!("{}: the rendered photo is gone ({e})", item.path))?;
            Ok(Some(CellImage { jpeg }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let sheet = write_sheet(
        &job.sheet,
        &cells,
        &job.captions,
        &job.values,
        job.generated,
    )?;
    write_files(Path::new(&job.destination), &job.stem, &sheet)
}

/// Removes a sheet's folder of rendered photos; only ever a folder directly
/// in the app's proof sheet cache.
pub(crate) fn remove_staging(app: &tauri::AppHandle, staging: &Path) {
    let Ok(root) = staging_root(app) else {
        return;
    };
    if staging.parent() == Some(root.as_path())
        && let Err(e) = std::fs::remove_dir_all(staging)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("Proof sheet: {} not removed: {e}", staging.display());
    }
}

/// Whether `path` is a sheet's rendered photo in the app's cache.
pub(crate) fn is_staged(app: &tauri::AppHandle, path: &Path) -> bool {
    staging_root(app).is_ok_and(|root| path.starts_with(root))
}

/// At start-up: removes the rendered photos of sheets no queued job needs
/// any more (`keep`: the staging folders of the jobs still in the queue).
pub(crate) fn prune_staging(app: &tauri::AppHandle, keep: &[PathBuf]) {
    let Ok(root) = staging_root(app) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !keep.contains(&path) {
            remove_staging(app, &path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn item(index: usize, output: Option<PathBuf>, state: ItemState) -> Item {
        Item {
            index,
            path: format!("/photos/{index}.NEF"),
            state,
            output,
            error: None,
            attempts: 1,
            started_ms: None,
            finished_ms: None,
        }
    }

    #[test]
    fn a_sheet_is_composed_in_photo_order_and_never_with_a_photo_missing() {
        if rawtonic_export::text_watermark::Typeface::load(None).is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let cell = |name: &str, shade: u8| {
            let path = dir.path().join(name);
            image::RgbImage::from_pixel(60, 40, image::Rgb([shade, shade, shade]))
                .save(&path)
                .unwrap();
            path
        };
        let job = ProofJob {
            sheet: serde_json::from_value(serde_json::json!({
                "pageWidthMm": 210.0, "pageHeightMm": 297.0,
                "marginsMm": [10.0, 10.0, 10.0, 10.0], "columns": 2, "rows": 2,
                "gapMm": 3.0, "caption": "{orig}", "ppi": 72,
            }))
            .unwrap(),
            destination: dir.path().to_string_lossy().into_owned(),
            stem: "Sheet".into(),
            values: PageValues::default(),
            captions: vec!["a".into(), "b".into()],
            generated: false,
        };
        // Items come back from the queue in any order.
        let items = [
            item(1, Some(cell("cell-2.jpg", 20)), ItemState::Done),
            item(0, Some(cell("cell-1.jpg", 200)), ItemState::Done),
        ];
        let written = compose(&items, &job).unwrap();
        assert_eq!(written, [dir.path().join("Sheet.pdf")]);
        let pdf = std::fs::read(&written[0]).unwrap();
        let first_image = pdf.windows(4).position(|w| w == b"/Im0").unwrap();
        let second_image = pdf.windows(4).position(|w| w == b"/Im1").unwrap();
        assert!(first_image < second_image);

        let failed = [
            item(0, Some(dir.path().join("cell-1.jpg")), ItemState::Done),
            item(1, None, ItemState::Failed),
        ];
        assert!(compose(&failed, &job).unwrap_err().contains("not rendered"));
        assert!(!dir.path().join("Sheet-2.pdf").exists());
    }
}
