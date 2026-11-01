//! One source file exported without the desktop app. It follows the per-photo
//! path of the app's export queue (`export_images_impl`) for a recipe given
//! as JSON or none. It reads no editor sidecars; kept metadata and the
//! capture date come from the source file, catalog metadata from the caller.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::adjustment_utils::hydrate_adjustments_from_cache;
use crate::exif_processing::{capture_date_with, read_exif_data_from_bytes};
use crate::export_processing::{
    ExportOutput, ExportSettings, RenderEnv, prepare_export_source, render_and_save,
    set_capture_file_times,
};
use crate::gpu_processing::{GpuContext, GpuImageCache, GpuProcessorSlot};
use crate::output::write_new_output;
use rawtonic_engine::ai_selections::{SelectionDetector, detect_missing};
use rawtonic_engine::file_io::read_file_mapped;
use rawtonic_engine::formats::is_raw_file;
use rawtonic_engine::image_loader::{
    BaseImageOptions, ai_denoise_settings, load_base_image_measured,
};
use rawtonic_rawdecode::mosaic_denoise::{MosaicDenoise, MosaicDenoiser};

/// One export: `output` must not exist; its extension, else `format`,
/// chooses the encoder.
pub struct FileExport<'a> {
    pub source: &'a Path,
    pub output: &'a Path,
    pub format: &'a str,
    /// The editor's adjustments JSON; `None` exports the photo unedited.
    pub recipe: Option<Value>,
    pub settings: ExportSettings,
    pub base: BaseImageOptions,
    /// The session's metadata for the source (creator, copyright, keywords…),
    /// written with `settings.keep_metadata`; rights and creator contacts are
    /// retained when other metadata is stripped. Creator contacts require
    /// `settings.keep_creator_contact` in that mode.
    pub catalog: Option<rawtonic_metadata::Metadata>,
    /// The capture time the library corrected for the source (Shift Capture
    /// Time), which its EXIF and file times take over the file's.
    pub capture: Option<crate::exif_processing::CaptureFields>,
}

/// The capture time an app correction left in the source's `.rrdata` EXIF
/// map when it differs from the file's own: what an export of a photograph
/// outside any session folder carries (a catalog keeps its photographs
/// anywhere, so the CLI cannot find the catalog from the photograph).
pub fn sidecar_capture(source: &Path) -> Option<crate::exif_processing::CaptureFields> {
    use crate::exif_processing::parse_creation_datetime;
    let sidecar = crate::exif_processing::get_primary_sidecar_path(source);
    let shown = crate::sidecars::read_sidecar_file(&sidecar).0.exif?;
    let bytes = fs::read(source).ok()?;
    let own = read_exif_data_from_bytes(&source.to_string_lossy(), &bytes);
    let field = |map: &crate::export_processing::ExifMap, key: &str| {
        map.get(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let date = |map: &crate::export_processing::ExifMap| {
        field(map, "DateTimeOriginal").and_then(|v| parse_creation_datetime(&v))
    };
    let corrected = date(&shown)?;
    let same = date(&own) == Some(corrected)
        && ["SubSecTimeOriginal", "OffsetTimeOriginal"]
            .iter()
            .all(|key| field(&shown, key) == field(&own, key));
    (!same).then(|| {
        [
            (
                "DateTimeOriginal",
                Some(corrected.format("%Y-%m-%d %H:%M:%S").to_string()),
            ),
            ("SubSecTimeOriginal", field(&shown, "SubSecTimeOriginal")),
            ("OffsetTimeOriginal", field(&shown, "OffsetTimeOriginal")),
        ]
    })
}

/// The source's EXIF map as the app's export reads it: extracted from the
/// file, with a corrected capture time marked on it. `None` without EXIF.
fn source_exif(
    source: &Path,
    capture: Option<&crate::exif_processing::CaptureFields>,
) -> Option<crate::export_processing::ExifMap> {
    let bytes = fs::read(source).ok()?;
    let mut map = read_exif_data_from_bytes(&source.to_string_lossy(), &bytes);
    if map.is_empty() {
        return None;
    }
    if let Some(fields) = capture {
        crate::exif_processing::mark_corrected_capture(&mut map, fields);
    }
    Some(map)
}

/// A GPU context with the processor and source cache that consecutive
/// exports reuse, as the app keeps them in its state.
pub struct FileExporter {
    context: GpuContext,
    processor: GpuProcessorSlot,
    image_cache: Mutex<Option<GpuImageCache>>,
    /// For recipes with AI Denoise; without it such exports fail.
    /// AI Denoise's denoisers by model (`AiDenoiseSettings::model`).
    mosaic_denoisers: [Option<Arc<dyn MosaicDenoiser>>; 2],
    /// Computes the Sky and Subject selections a recipe holds without their
    /// bitmap (a never-opened import); without it such exports fail.
    selections: Option<Arc<dyn SelectionDetector>>,
}

impl FileExporter {
    /// `lensfun_db`: the Lensfun XML folder the app bundles (`lensfun_db`).
    /// Camera lens profiles and diffraction correction look lenses up there,
    /// so exports without it would differ from the app's.
    pub fn new(lensfun_db: &Path) -> Result<Self, String> {
        if !lensfun_db.is_dir() {
            return Err(format!(
                "Lensfun database folder not found: {}",
                lensfun_db.display()
            ));
        }
        rawtonic_engine::lens_correction::set_global_lens_db(Arc::new(
            rawtonic_engine::lens_correction::load_lensfun_dir(lensfun_db),
        ));
        Ok(Self {
            context: GpuContext::headless()?,
            processor: GpuProcessorSlot::background(None),
            image_cache: Mutex::new(None),
            mosaic_denoisers: [None, None],
            selections: None,
        })
    }

    /// The denoiser for recipes with AI Denoise (`aiDenoise` > 0) whose
    /// version runs `model` (`AiDenoiseSettings::model`).
    pub fn with_mosaic_denoiser(mut self, model: usize, denoiser: Arc<dyn MosaicDenoiser>) -> Self {
        if let Some(slot) = self.mosaic_denoisers.get_mut(model) {
            *slot = Some(denoiser);
        }
        self
    }

    /// The networks for Sky and Subject selections still to compute.
    pub fn with_selection_detector(mut self, detector: Arc<dyn SelectionDetector>) -> Self {
        self.selections = Some(detector);
        self
    }

    pub fn export(&self, job: &FileExport) -> Result<(), String> {
        let source_path_str = job.source.to_string_lossy().to_string();

        let mut js_adjustments = job.recipe.clone().unwrap_or(Value::Null);
        hydrate_adjustments_from_cache(&mut HashMap::new(), &mut js_adjustments);
        crate::exif_processing::prepare_recipe_lens(&mut js_adjustments, job.source)?;
        let is_raw = is_raw_file(&source_path_str);

        // The recipe's colour noise filter decides the decode-time chroma
        // clean-up (skipped for the multi-scale filter).
        let mut base = job
            .base
            .clone()
            .for_recipe(&js_adjustments, &source_path_str);
        if let Some(ai) = ai_denoise_settings(&js_adjustments).filter(|_| is_raw) {
            let denoiser = self.mosaic_denoisers[ai.model()].clone().ok_or_else(|| {
                format!(
                    "{} uses AI Denoise, but the denoising model is not available",
                    job.source.display()
                )
            })?;
            base.mosaic_denoise =
                Some(MosaicDenoise::new(denoiser, ai.blend).with_noise_scale(ai.noise_scale));
        }
        let (pristine_source, measures) = match read_file_mapped(job.source) {
            Ok(mmap) => load_base_image_measured(&mmap, &source_path_str, false, &base, None)
                .map_err(|e| format!("Failed to load from mmap: {}", e))?,
            Err(_) => {
                let bytes = fs::read(&source_path_str).map_err(|e| e.to_string())?;
                load_base_image_measured(&bytes, &source_path_str, false, &base, None)
                    .map_err(|e| format!("Failed to load from bytes: {}", e))?
            }
        };
        let pristine_source = Arc::new(pristine_source);
        // As the editor computes them when the photo opens (nothing is
        // written back: the CLI never changes a recipe).
        detect_missing(
            &mut js_adjustments,
            &pristine_source,
            is_raw,
            self.selections.as_deref(),
            &mut || {},
        )
        .map_err(|e| format!("{}: not exported, {e}", job.source.display()))?;
        let source =
            prepare_export_source(&source_path_str, pristine_source, measures, &js_adjustments)?;

        // The app's default preferences: no forced tone mapper.
        let tonemapper_override = |_is_raw: bool| None;
        let load_lut = |path: &str| {
            rawtonic_engine::lut::parse_lut_file(path)
                .ok()
                .map(Arc::new)
        };
        let env = RenderEnv {
            context: &self.context,
            processor: &self.processor,
            image_cache: &self.image_cache,
            tonemapper_override: &tonemapper_override,
            load_lut: &load_lut,
        };
        // The app reads the EXIF map its loader cached for the source; it is
        // extracted the same way here.
        let exif_sidecar = || source_exif(job.source, job.capture.as_ref());
        let catalog = || job.catalog.clone();
        let write = |path: &Path, _extension: &str, bytes: Vec<u8>| {
            write_new_output(path, bytes)
                .map_err(|e| format!("Failed to write file to '{}': {}", path.display(), e))
        };
        render_and_save(
            &source_path_str,
            source,
            &js_adjustments,
            &job.settings,
            &env,
            is_raw,
            &ExportOutput {
                path: job.output,
                format: job.format,
                exif_sidecar: &exif_sidecar,
                catalog: &catalog,
                write: &write,
            },
            &|| Ok(()),
        )?;
        if job.settings.preserve_timestamps {
            set_capture_file_times(job.output, capture_date_with(job.source, &exif_sidecar));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use little_exif::{exif_tag::ExifTag, metadata::Metadata};

    #[test]
    fn the_cli_export_reads_a_corrected_capture_time_like_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("camera.jpg");
        image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 4))
            .save(&source)
            .unwrap();
        let mut exif = Metadata::new();
        exif.set_tag(ExifTag::Make("Maker".into()));
        exif.set_tag(ExifTag::DateTimeOriginal("2026:10:30 23:30:00".into()));
        exif.set_tag(ExifTag::SubSecTimeOriginal("05".into()));
        exif.write_to_file(&source).unwrap();
        let plain = source_exif(&source, None).unwrap();
        assert!(!plain.contains_key(crate::exif_processing::CAPTURE_CORRECTED));
        let fields = [
            ("DateTimeOriginal", Some("2026-10-31 00:30:00".to_string())),
            ("SubSecTimeOriginal", Some("05".to_string())),
            ("OffsetTimeOriginal", None),
        ];
        let corrected = source_exif(&source, Some(&fields)).unwrap();
        assert_eq!(corrected["DateTimeOriginal"], "2026-10-31 00:30:00");
        assert_eq!(corrected["Make"], "Maker");
        assert!(corrected.contains_key(crate::exif_processing::CAPTURE_CORRECTED));
        assert!(!corrected.contains_key("OffsetTimeOriginal"));
        // Outside a session: the correction the app left beside the photo.
        assert_eq!(sidecar_capture(&source), None, "no sidecar");
        let sidecar = crate::exif_processing::get_primary_sidecar_path(&source);
        let state = |date: &str| {
            serde_json::json!({"version": 1, "rating": 0, "adjustments": null,
                "exif": {"Make": "Maker", "DateTimeOriginal": date, "SubSecTimeOriginal": "05"}})
            .to_string()
        };
        fs::write(&sidecar, state("2026-10-30 23:30:00")).unwrap();
        assert_eq!(sidecar_capture(&source), None, "the file's own time");
        fs::write(&sidecar, state("2026-10-31 00:30:00")).unwrap();
        assert_eq!(
            sidecar_capture(&source).unwrap()[0].1.as_deref(),
            Some("2026-10-31 00:30:00")
        );
        // Its file times follow the corrected date.
        let date = crate::exif_processing::capture_date_with(&source, &|| Some(corrected.clone()));
        use chrono::{Datelike, Timelike};
        let local = date.with_timezone(&chrono::Local);
        assert_eq!((local.day(), local.hour(), local.minute()), (31, 0, 30));
    }
}
