#!/usr/bin/env python3
"""Export UperNet + ConvNeXt-T (ADE20K, MIT) as Rawtonic's sky mask model.

Weights: `openmmlab/upernet-convnext-tiny` on Hugging Face (model card
`license: mit`), the transformers conversion of Meta's ADE20K UperNet
checkpoint from facebookresearch/ConvNeXt (MIT), via mmsegmentation
(Apache-2.0). ADE20K class 2 is "sky".

Writes OUT_DIR/skyseg_upernet_convnext_t.onnx (opset 17, fixed size):
- input  pixel_values float32 [1, 3, 512, 512]: sRGB 8-bit / 255, ImageNet
  normalised ((x - [0.485, 0.456, 0.406]) / [0.229, 0.224, 0.225]); the photo
  letterboxed into 512 x 512 (fit, centred, padding 0 after normalisation,
  i.e. the mean colour), as `run_sky_seg_model` does;
- output sky float32 [1, 1, 512, 512]: the sky probability in 0..1, the sky
  channel of the softmax over the 150 classes (its logit margin computed at
  the decoder's 1/4 resolution, bilinearly upsampled to the input size in the
  graph, then a sigmoid). It is a probability:
  multiply by 255, do not min-max normalise (that turns a sky-less photo's
  0.01 into a full mask).
The auxiliary FCN head (training only) is left out. ONNX has no adaptive
average pooling to 3 x 3 or 6 x 6 of the 16 x 16 last stage, so the pyramid
pooling bins become constant averaging matrices (identical results).
The weights are stored as fp16, each followed by a Cast to fp32, so the file
is half the size (119 MB) while ONNX Runtime folds the casts at load and
computes in fp32 as fast as the fp32 file; against fp32 storage the masks
keep an IoU of 0.993 or more. Full fp16 compute (slower on CPU) and dynamic
uint8 quantisation (IoU down to 0.70) were measured and rejected
(docs/MODELS.md).

Usage: export_sky.py OUT_DIR
Needs: torch, transformers, onnx, onnxruntime (see
scripts/models/build_model_pack.py for pinned versions).
"""
import sys
from pathlib import Path

import numpy as np
import onnx
import torch
import torch.nn.functional as F
from transformers import UperNetForSemanticSegmentation

MODEL = "openmmlab/upernet-convnext-tiny"
REVISION = "876ffc56b819a829448f7e81e9f8606deef6fb65"
SKY = 2
SIZE = 512
FILE = "skyseg_upernet_convnext_t.onnx"


def bins(size: int, out: int) -> torch.Tensor:
    """AdaptiveAvgPool's bins along one axis as an [out, size] averaging matrix."""
    matrix = torch.zeros(out, size)
    for i in range(out):
        start, end = (i * size) // out, -((-(i + 1) * size) // out)
        matrix[i, start:end] = 1.0 / (end - start)
    return matrix


class FixedAdaptivePool(torch.nn.Module):
    def __init__(self, out: int, size: int):
        super().__init__()
        self.register_buffer("rows", bins(size, out), persistent=False)

    def forward(self, x):
        return self.rows @ x @ self.rows.T


def store_fp16(path: Path) -> None:
    """Store the large fp32 initializers as fp16 behind Cast-to-fp32 nodes."""
    model = onnx.load(str(path))
    graph = model.graph
    initializers, casts = [], []
    for init in graph.initializer:
        array = onnx.numpy_helper.to_array(init)
        if array.dtype == np.float32 and array.size >= 1024:
            half = onnx.numpy_helper.from_array(array.astype(np.float16), init.name + "_fp16")
            initializers.append(half)
            casts.append(onnx.helper.make_node("Cast", [half.name], [init.name], to=onnx.TensorProto.FLOAT))
        else:
            initializers.append(init)
    del graph.initializer[:]
    graph.initializer.extend(initializers)
    nodes = list(graph.node)
    del graph.node[:]
    graph.node.extend(casts + nodes)
    onnx.checker.check_model(model)
    onnx.save(model, str(path))


class Sky(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.backbone = model.backbone
        self.decode_head = model.decode_head
        last = SIZE // 32
        for block in self.decode_head.psp_modules.blocks:
            pool = block.layers[0]
            assert isinstance(pool, torch.nn.AdaptiveAvgPool2d), pool
            block.layers[0] = FixedAdaptivePool(pool.output_size, last)
            block.add_module("0", block.layers[0])

    def forward(self, pixel_values):
        features = self.backbone.forward_with_filtered_kwargs(pixel_values).feature_maps
        logits = self.decode_head(features)
        # softmax's sky channel = sigmoid(sky logit - logsumexp(other logits));
        # upsampling that margin instead of 150 logits keeps the edges as
        # crisp as the full model's (logits upsampled, then softmax).
        others = torch.cat([logits[:, :SKY], logits[:, SKY + 1 :]], dim=1)
        margin = logits[:, SKY : SKY + 1] - torch.logsumexp(others, dim=1, keepdim=True)
        margin = F.interpolate(margin, size=pixel_values.shape[2:], mode="bilinear", align_corners=False)
        return torch.sigmoid(margin)


def export(out: Path) -> Path:
    out.mkdir(parents=True, exist_ok=True)
    model = UperNetForSemanticSegmentation.from_pretrained(MODEL, revision=REVISION, use_safetensors=True).eval()
    assert model.config.id2label[SKY] == "sky", model.config.id2label[SKY]
    path = out / FILE
    probe = torch.from_numpy(np.random.default_rng(0).standard_normal((1, 3, SIZE, SIZE), dtype=np.float32))
    with torch.no_grad():
        # The unmodified model (adaptive pooling, logits upsampled before the
        # softmax) as the reference for the exported graph.
        reference = torch.softmax(model(pixel_values=probe).logits, dim=1)[:, SKY : SKY + 1]
        sky = Sky(model).eval()
        print(f"max |export graph - original model| on noise: {float((sky(probe) - reference).abs().max()):.5f}")
        torch.onnx.export(sky, (torch.zeros(1, 3, SIZE, SIZE),), str(path),
                          input_names=["pixel_values"], output_names=["sky"],
                          opset_version=17, do_constant_folding=True, dynamo=False)
    store_fp16(path)
    return path


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    print(export(Path(sys.argv[1])))
