// SPDX-License-Identifier: AGPL-3.0-or-later
//! MatchFilter and the Fast network on ONNX Runtime (any platform): a GPU
//! provider when one works (rawtonic-onnx), else the CPU, about 15 times
//! slower than Core ML on Apple Silicon for MatchFilter.

use std::path::Path;

use anyhow::Result;
use ndarray::Array4;
use ort::value::Tensor;
use rawtonic_onnx::{Model, ModelKind, Provider};

use crate::{Backend, FAST_HALO, FAST_MODEL_ID, FAST_TILE, HALO, MODEL_ID, TILE};

pub struct Onnx {
    model: Model,
    tile: usize,
    halo: usize,
    model_id: &'static str,
}

impl Onnx {
    /// MatchFilter (`ONNX_MODEL`).
    pub fn load(model: &Path) -> Result<Self> {
        Ok(Self {
            model: Model::load(model, ModelKind::Denoise)?,
            tile: TILE,
            halo: HALO,
            model_id: MODEL_ID,
        })
    }

    /// The Fast network (`FAST_ONNX_MODEL`): the same input and output
    /// names, on its own tiles.
    pub fn load_fast(model: &Path) -> Result<Self> {
        Ok(Self {
            model: Model::load(model, ModelKind::Denoise)?,
            tile: FAST_TILE,
            halo: FAST_HALO,
            model_id: FAST_MODEL_ID,
        })
    }

    pub fn provider(&self) -> Provider {
        self.model.provider()
    }
}

impl Backend for Onnx {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>> {
        let array = Array4::from_shape_vec((1, 8, self.tile, self.tile), input.to_vec())?;
        // Watched: a GPU tile that hangs moves the model to the CPU instead
        // of stopping AI Denoise (gap G76).
        self.model.run_watched(move |session| {
            let outputs = session
                .run(ort::inputs!["raw_with_noise" => Tensor::from_array(array.clone())?])?;
            let out: Vec<f32> = outputs["denoised_raw"]
                .try_extract_array::<f32>()?
                .iter()
                .copied()
                .collect();
            rawtonic_onnx::ensure_finite(&out)?;
            Ok(out)
        })
    }

    fn cache_tag(&self) -> String {
        match self.provider() {
            Provider::Cpu => "",
            Provider::WebGpu => "webgpu",
            Provider::Cuda => "cuda",
        }
        .to_string()
    }

    fn tile(&self) -> usize {
        self.tile
    }

    fn halo(&self) -> usize {
        self.halo
    }

    fn model_id(&self) -> &'static str {
        self.model_id
    }
}
