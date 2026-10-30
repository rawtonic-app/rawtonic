//! Moebius on ONNX Runtime (Windows, Linux): the three networks of
//! `scripts/models/convert_moebius_coreml.py --onnx` (fp16 weights, fp32
//! compute) on a GPU provider (`rawtonic_onnx`: CUDA when installed, else
//! WebGPU). Only on a GPU: on the CPU a removal takes about a minute, so
//! without a usable GPU provider MI-GAN fills (the caller's fallback), and
//! nothing is downloaded.

use std::path::Path;

use anyhow::{Result, anyhow, ensure};
use ort::value::Tensor;
use rawtonic_onnx::{Model, ModelKind, Provider};

use super::{EMBEDDINGS_FILE, LATENT, Networks, SIZE, read_embeddings};

/// The networks: model kind, pack file, SHA-256.
pub const FILES: [(ModelKind, &str, &str); 3] = [
    (
        ModelKind::MoebiusUnet,
        "moebius_unet_fp16w.onnx",
        "a93b4880344c252a30bf39fcb2f29e6d5a68e8ea3c5a00e1aadeb4d63f18851d",
    ),
    (
        ModelKind::MoebiusEncoder,
        "sdxl_vae_encoder_fp16w.onnx",
        "866b0df3037855cb3464dd7aab9a0b8c8f7326df1e93c856adc7dc8517866961",
    ),
    (
        ModelKind::MoebiusDecoder,
        "sdxl_vae_decoder_fp16w.onnx",
        "307bf01ebd3105b99f0742eae74ecbaaaf5aa6be84d1ddd17318473e83130150",
    ),
];

/// The GPU providers Moebius may run on, best first: the user's preference
/// and `RAWTONIC_AI_PROVIDER` as for every model, without the CPU, and only
/// those whose plugin offers a device.
pub fn gpu_providers() -> Vec<Provider> {
    let settings = rawtonic_onnx::settings();
    rawtonic_onnx::candidates(
        ModelKind::MoebiusUnet,
        &settings,
        rawtonic_onnx::forced_provider(),
    )
    .into_iter()
    .filter(|&p| p != Provider::Cpu && rawtonic_onnx::device(p, &settings.plugin_dirs).is_ok())
    .collect()
}

/// Moebius's three networks, each on a GPU provider.
pub struct Moebius {
    unet: Model,
    encoder: Model,
    decoder: Model,
    embeddings: Vec<f32>,
}

impl Moebius {
    /// Loads the networks from `models_dir` (files the caller checked) on
    /// the first of `providers` that takes them and whose output matched
    /// the CPU's there (`rawtonic_onnx`'s check, once per device).
    pub fn load(models_dir: &Path, providers: &[Provider]) -> Result<Self> {
        ensure!(!providers.is_empty(), "no GPU provider for Moebius");
        let dirs = rawtonic_onnx::settings().plugin_dirs;
        let load = |(kind, file, _): (ModelKind, &str, &str)| {
            Model::load_on(&models_dir.join(file), kind, providers, &dirs)
        };
        Ok(Self {
            unet: load(FILES[0])?,
            encoder: load(FILES[1])?,
            decoder: load(FILES[2])?,
            embeddings: read_embeddings(&models_dir.join(EMBEDDINGS_FILE.0))?,
        })
    }

    /// The provider the UNet runs on, for the log.
    pub fn provider(&self) -> Provider {
        self.unet.provider()
    }
}

/// Runs `model` on `inputs` and returns its one output, of `shape`. Never
/// moves to the CPU: a failure or a non-finite value is an error, and
/// MI-GAN fills instead.
fn run(model: &Model, inputs: Vec<(&str, Tensor<f32>)>, shape: &[usize]) -> Result<Vec<f32>> {
    model.run_without_fallback(|session| {
        let outputs = session.run(inputs)?;
        let (got, values) = outputs[0].try_extract_tensor::<f32>()?;
        ensure!(
            got.iter().map(|&d| d as usize).eq(shape.iter().copied()),
            "{} output shape {got:?}",
            model.kind().name()
        );
        rawtonic_onnx::ensure_finite(values)?;
        Ok(values.to_vec())
    })
}

fn tensor(shape: &[usize], values: &[f32]) -> Result<Tensor<f32>> {
    Ok(Tensor::from_array((shape.to_vec(), values.to_vec()))?)
}

impl Networks for Moebius {
    fn encode(&self, images: &[f32], n: usize) -> Result<Vec<f32>> {
        ensure!(n == 2, "the encoder takes two photos");
        run(
            &self.encoder,
            vec![("image", tensor(&[2, 3, SIZE, SIZE], images)?)],
            &[2, 4, LATENT, LATENT],
        )
    }

    fn noise(&self, latent_in: &[f32], t: f32) -> Result<Vec<f32>> {
        run(
            &self.unet,
            vec![
                ("latent_in", tensor(&[2, 9, LATENT, LATENT], latent_in)?),
                ("t", tensor(&[1], &[t])?),
                ("embeddings", tensor(&[2, 10, 3072], &self.embeddings)?),
            ],
            &[2, 4, LATENT, LATENT],
        )
    }

    fn decode(&self, latent: &[f32]) -> Result<Vec<f32>> {
        run(
            &self.decoder,
            vec![("latent", tensor(&[1, 4, LATENT, LATENT], latent)?)],
            &[1, 3, SIZE, SIZE],
        )
    }
}

/// Downloads what is missing of `FILES` and the embeddings into
/// `models_dir` (each checked by its SHA-256) and loads Moebius on a GPU.
/// An error, before any download, when no GPU provider is usable.
pub async fn load(
    app_handle: &tauri::AppHandle,
    models_dir: &Path,
    preparing: &(dyn Fn() + Send + Sync),
) -> Result<Moebius> {
    crate::ai_processing::ensure_local_runtime()?;
    let providers = tokio::task::spawn_blocking(gpu_providers)
        .await
        .map_err(|e| anyhow!("Moebius: {e}"))?;
    ensure!(
        !providers.is_empty(),
        "no GPU provider (Moebius runs only on a GPU)"
    );
    let (file, sha) = EMBEDDINGS_FILE;
    crate::ai_processing::download_verified(
        app_handle,
        models_dir,
        file,
        sha,
        "Object removal model",
    )
    .await?;
    for (_, file, sha) in FILES {
        crate::ai_processing::download_verified(
            app_handle,
            models_dir,
            file,
            sha,
            "Object removal model",
        )
        .await?;
    }
    preparing();
    let dir = models_dir.to_path_buf();
    let model = tokio::task::spawn_blocking(move || Moebius::load(&dir, &providers))
        .await
        .map_err(|e| anyhow!("Moebius load: {e}"))??;
    log::info!("AI fill: Moebius on {}", model.provider());
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_files_are_the_models_they_load_as() {
        for (kind, file, sha) in FILES {
            assert_eq!(ModelKind::from_file_name(file), Some(kind), "{file}");
            assert_eq!(sha.len(), 64);
        }
    }

    #[test]
    fn without_a_gpu_provider_moebius_does_not_load() {
        let error = Moebius::load(Path::new("/nonexistent"), &[]).err().unwrap();
        assert!(error.to_string().contains("no GPU provider"), "{error}");
    }
}
