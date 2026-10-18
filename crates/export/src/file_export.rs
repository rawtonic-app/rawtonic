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
use crate::gpu_processing::{GpuContext, GpuImageCache, GpuProcessorState};
use crate::output::write_new_output;
use rawtonic_engine::file_io::read_file_mapped;
use rawtonic_engine::formats::is_raw_file;
use rawtonic_engine::image_loader::{BaseImageOptions, ai_denoise_settings, load_base_image};
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
    /// written with `settings.keep_metadata`.
    pub catalog: Option<rawtonic_metadata::Metadata>,
}

/// A GPU context with the processor and source cache that consecutive
/// exports reuse, as the app keeps them in its state.
pub struct FileExporter {
    context: GpuContext,
    processor: Mutex<Option<GpuProcessorState>>,
    image_cache: Mutex<Option<GpuImageCache>>,
    /// For recipes with AI Denoise; without it such exports fail.
    /// AI Denoise's denoisers by `aiDenoiseVersion` (index 0: version 1).
    mosaic_denoisers: [Option<Arc<dyn MosaicDenoiser>>; 2],
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
            processor: Mutex::new(None),
            image_cache: Mutex::new(None),
            mosaic_denoisers: [None, None],
        })
    }

    /// The denoiser for recipes with AI Denoise (`aiDenoise` > 0) of
    /// `version` (`aiDenoiseVersion`, `ai_denoise_settings`).
    pub fn with_mosaic_denoiser(mut self, version: u8, denoiser: Arc<dyn MosaicDenoiser>) -> Self {
        if let Some(slot) = self
            .mosaic_denoisers
            .get_mut(version.saturating_sub(1) as usize)
        {
            *slot = Some(denoiser);
        }
        self
    }

    pub fn export(&self, job: &FileExport) -> Result<(), String> {
        let source_path_str = job.source.to_string_lossy().to_string();

        let mut js_adjustments = job.recipe.clone().unwrap_or(Value::Null);
        hydrate_adjustments_from_cache(&mut HashMap::new(), &mut js_adjustments);
        rawtonic_engine::embedded_lens::apply_default(&mut js_adjustments, job.source);
        let is_raw = is_raw_file(&source_path_str);

        // The recipe's colour noise filter decides the decode-time chroma
        // clean-up (skipped for the multi-scale filter).
        let mut base = job.base.clone().for_recipe(&js_adjustments, is_raw);
        if let Some(ai) = ai_denoise_settings(&js_adjustments).filter(|_| is_raw) {
            let denoiser = self.mosaic_denoisers[ai.version as usize - 1]
                .clone()
                .ok_or_else(|| {
                    format!(
                        "{} uses AI Denoise, but the denoising model is not available",
                        job.source.display()
                    )
                })?;
            base.mosaic_denoise =
                Some(MosaicDenoise::new(denoiser, ai.blend).with_noise_scale(ai.noise_scale));
        }
        let pristine_source = match read_file_mapped(job.source) {
            Ok(mmap) => load_base_image(&mmap, &source_path_str, false, &base, None)
                .map_err(|e| format!("Failed to load from mmap: {}", e))?,
            Err(_) => {
                let bytes = fs::read(&source_path_str).map_err(|e| e.to_string())?;
                load_base_image(&bytes, &source_path_str, false, &base, None)
                    .map_err(|e| format!("Failed to load from bytes: {}", e))?
            }
        };
        let source =
            prepare_export_source(&source_path_str, Arc::new(pristine_source), &js_adjustments)?;

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
        let exif_sidecar = || {
            let bytes = fs::read(job.source).ok()?;
            let map = read_exif_data_from_bytes(&source_path_str, &bytes);
            (!map.is_empty()).then_some(map)
        };
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
