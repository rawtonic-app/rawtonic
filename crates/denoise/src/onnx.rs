// SPDX-License-Identifier: AGPL-3.0-or-later
//! MatchFilter, the Fast and the X-Trans network on ONNX Runtime (any platform): a GPU
//! provider when one works (rawtonic-onnx), else the CPU, about 15 times
//! slower than Core ML on Apple Silicon for MatchFilter.

use std::path::Path;

use anyhow::Result;
use ndarray::Array4;
use ort::value::Tensor;
use rawtonic_onnx::{Model, ModelKind, Provider};

use crate::{
    Backend, FAST_HALO, FAST_MODEL_ID, FAST_TILE, HALO, MODEL_ID, TILE, XTRANS_HALO,
    XTRANS_MODEL_ID, XTRANS_TILE,
};

pub struct Onnx {
    model: Model,
    tile: usize,
    halo: usize,
    model_id: &'static str,
    /// Input planes, and the input's and output's names.
    planes: usize,
    names: (&'static str, &'static str),
}

/// MatchFilter's and the Fast network's input and output.
const PACKED: (&str, &str) = ("raw_with_noise", "denoised_raw");

impl Onnx {
    /// MatchFilter (`ONNX_MODEL`).
    pub fn load(model: &Path) -> Result<Self> {
        Ok(Self {
            model: Model::load(model, ModelKind::Denoise)?,
            tile: TILE,
            halo: HALO,
            model_id: MODEL_ID,
            planes: 8,
            names: PACKED,
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
            planes: 8,
            names: PACKED,
        })
    }

    /// The X-Trans network (`XTRANS_ONNX_MODEL`): the samples and their
    /// noise in, the denoised samples out.
    pub fn load_xtrans(model: &Path) -> Result<Self> {
        Ok(Self {
            model: Model::load(model, ModelKind::Denoise)?,
            tile: XTRANS_TILE,
            halo: XTRANS_HALO,
            model_id: XTRANS_MODEL_ID,
            planes: 2,
            names: ("mosaic_with_noise", "denoised_mosaic"),
        })
    }

    pub fn provider(&self) -> Provider {
        self.model.provider()
    }
}

impl Backend for Onnx {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>> {
        let array = Array4::from_shape_vec((1, self.planes, self.tile, self.tile), input.to_vec())?;
        let (input_name, output_name) = self.names;
        // Watched: a GPU tile that hangs moves the model to the CPU instead
        // of stopping AI Denoise (gap G76).
        self.model.run_watched(move |session| {
            let outputs =
                session.run(ort::inputs![input_name => Tensor::from_array(array.clone())?])?;
            let out: Vec<f32> = outputs[output_name]
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
