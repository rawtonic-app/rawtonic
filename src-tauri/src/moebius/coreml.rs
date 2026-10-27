//! Moebius on Core ML (macOS): the three programs of
//! scripts/models/convert_moebius_coreml.py on the GPU.

use std::path::Path;

use anyhow::{Result, anyhow, ensure};
use rawtonic_denoise::coreml::{ComputeUnits, CoreMlModel};

use super::{EMBEDDINGS_FILE, LATENT, Networks, SIZE, read_embeddings};

/// A Core ML program of the model pack: its package folder and its files
/// (pack file, place in the package, SHA-256).
pub type Package = (
    &'static str,
    [(&'static str, &'static str, &'static str); 3],
);

/// The three Core ML programs.
pub const PACKAGES: [Package; 3] = [
    (
        "moebius_unet_fp16.mlpackage",
        [
            (
                "moebius_unet_fp16_coreml_Manifest.json",
                "Manifest.json",
                "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
            ),
            (
                "moebius_unet_fp16_coreml_model.mlmodel",
                "Data/com.apple.CoreML/model.mlmodel",
                "cfa6c064e439ceb7904fb1f396986a5b067219acb2ff2dca52f3bd5ce55640a7",
            ),
            (
                "moebius_unet_fp16_coreml_weight.bin",
                "Data/com.apple.CoreML/weights/weight.bin",
                "5a70ccf4037c43560f2a81dacd494a9c8fe3937715becf14d4cf7e4d0d28d859",
            ),
        ],
    ),
    (
        "sdxl_vae_encoder_fp16.mlpackage",
        [
            (
                "sdxl_vae_encoder_fp16_coreml_Manifest.json",
                "Manifest.json",
                "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
            ),
            (
                "sdxl_vae_encoder_fp16_coreml_model.mlmodel",
                "Data/com.apple.CoreML/model.mlmodel",
                "73532ac0c8eec0fe13fca9d6fb2ed6bdc2a54cecf7d1956070e1a31eef60c5cb",
            ),
            (
                "sdxl_vae_encoder_fp16_coreml_weight.bin",
                "Data/com.apple.CoreML/weights/weight.bin",
                "1ab2c204f5b39ec9ad32226638e53be2ad169f1f6b6b438547350128a5125ca5",
            ),
        ],
    ),
    (
        "sdxl_vae_decoder_fp16.mlpackage",
        [
            (
                "sdxl_vae_decoder_fp16_coreml_Manifest.json",
                "Manifest.json",
                "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
            ),
            (
                "sdxl_vae_decoder_fp16_coreml_model.mlmodel",
                "Data/com.apple.CoreML/model.mlmodel",
                "17e00d408ff23eab25fbd2d1c081223c87e6b3fde4588d7c94308339dda5aca2",
            ),
            (
                "sdxl_vae_decoder_fp16_coreml_weight.bin",
                "Data/com.apple.CoreML/weights/weight.bin",
                "a08df0be795fc242c02d8af66b1f4e0daf8a0b7951a41d55bf88707125763650",
            ),
        ],
    ),
];
/// Moebius on Core ML (CPU and GPU: with the Neural Engine as well the
/// UNet's results were wrong, 110 % error; on the Neural Engine alone it
/// took 3 s per step).
pub struct Moebius {
    unet: CoreMlModel,
    encoder: CoreMlModel,
    decoder: CoreMlModel,
    embeddings: Vec<f32>,
}

impl Moebius {
    /// Loads the programs assembled in `models_dir` (their `PACKAGES`
    /// folders) and the embeddings file, whose SHA-256 the caller checked.
    pub fn load(models_dir: &Path) -> Result<Self> {
        let load =
            |package: &str| CoreMlModel::load(&models_dir.join(package), ComputeUnits::CpuAndGpu);
        let embeddings = read_embeddings(&models_dir.join(EMBEDDINGS_FILE.0))?;
        Ok(Self {
            unet: load(PACKAGES[0].0)?,
            encoder: load(PACKAGES[1].0)?,
            decoder: load(PACKAGES[2].0)?,
            embeddings,
        })
    }
}

impl Networks for Moebius {
    fn encode(&self, images: &[f32], n: usize) -> Result<Vec<f32>> {
        ensure!(n == 2, "the encoder takes two photos");
        let (shape, out) = self
            .encoder
            .predict("image", &[2, 3, SIZE, SIZE], images, "latent")?;
        ensure!(shape == [2, 4, LATENT, LATENT], "latent shape {shape:?}");
        Ok(out)
    }

    fn noise(&self, latent_in: &[f32], t: f32) -> Result<Vec<f32>> {
        let (shape, out) = self.unet.predict_inputs(
            &[
                ("latent_in", &[2, 9, LATENT, LATENT], latent_in),
                ("t", &[1], &[t]),
                ("embeddings", &[2, 10, 3072], &self.embeddings),
            ],
            "noise",
        )?;
        ensure!(shape == [2, 4, LATENT, LATENT], "noise shape {shape:?}");
        Ok(out)
    }

    fn decode(&self, latent: &[f32]) -> Result<Vec<f32>> {
        let (shape, out) =
            self.decoder
                .predict("latent", &[1, 4, LATENT, LATENT], latent, "image")?;
        ensure!(shape == [1, 3, SIZE, SIZE], "image shape {shape:?}");
        Ok(out)
    }
}

/// Puts the files of `PACKAGES` and the embeddings into `models_dir`
/// (downloading what is missing with `download`) and loads Moebius.
/// `Ok(None)` when files are missing and `download` is false.
pub async fn load(
    app_handle: &tauri::AppHandle,
    models_dir: &Path,
    download: bool,
) -> Result<Option<Moebius>> {
    let (file, sha) = EMBEDDINGS_FILE;
    if !crate::ai_processing::verify_sha256(&models_dir.join(file), sha)? {
        if !download {
            return Ok(None);
        }
        crate::ai_processing::download_verified(
            app_handle,
            models_dir,
            file,
            sha,
            "Object removal model",
        )
        .await?;
    }
    for (package, files) in &PACKAGES {
        if !crate::ai_processing::assemble_package(
            app_handle,
            models_dir,
            &models_dir.join(package),
            files,
            download,
            "Object removal model",
        )
        .await?
        {
            return Ok(None);
        }
    }
    let dir = models_dir.to_path_buf();
    tokio::task::spawn_blocking(move || Moebius::load(&dir))
        .await
        .map_err(|e| anyhow!("Moebius load: {e}"))?
        .map(Some)
}
