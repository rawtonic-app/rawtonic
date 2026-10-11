// SPDX-License-Identifier: AGPL-3.0-or-later
//! MatchFilter on ONNX Runtime (any platform): a GPU provider when one
//! works (rawtonic-onnx), else the CPU, about 15 times slower than Core ML
//! on Apple Silicon.

use std::path::Path;

use anyhow::Result;
use ndarray::Array4;
use ort::value::Tensor;
use rawtonic_onnx::{Model, ModelKind, Provider};

use crate::{Backend, TILE};

pub struct Onnx {
    model: Model,
}

impl Onnx {
    pub fn load(model: &Path) -> Result<Self> {
        Ok(Self {
            model: Model::load(model, ModelKind::Denoise)?,
        })
    }

    pub fn provider(&self) -> Provider {
        self.model.provider()
    }
}

impl Backend for Onnx {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>> {
        let array = Array4::from_shape_vec((1, 8, TILE, TILE), input.to_vec())?;
        self.model.run(|session| {
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

    fn cache_tag(&self) -> &'static str {
        match self.provider() {
            Provider::Cpu => "",
            Provider::WebGpu => "webgpu",
            Provider::Cuda => "cuda",
        }
    }
}
