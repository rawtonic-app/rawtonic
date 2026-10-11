#!/usr/bin/env python3
"""Export BiRefNet (MIT) as Rawtonic's subject mask model.

Weights: `ZhengPeng7/BiRefNet` on Hugging Face (model card `license: mit`,
Peng Zheng et al., "Bilateral Reference for High-Resolution Dichotomous Image
Segmentation", the general-use checkpoint with a Swin-L backbone), loaded with
its authors' modelling code (`trust_remote_code`, pinned revision).

Writes OUT_DIR/birefnet_subject.onnx (opset 17, fixed size):
- input  pixel_values float32 [1, 3, 1024, 1024]: sRGB 8-bit / 255, ImageNet
  normalised ((x - [0.485, 0.456, 0.406]) / [0.229, 0.224, 0.225]); the photo
  resized (stretched) to 1024 x 1024, as the authors' inference code does;
- output subject float32 [1, 1, 1024, 1024]: the subject probability in 0..1
  (sigmoid of the last decoder output). Multiply by 255; do not min-max
  normalise (a photo without a subject must stay empty).
The decoder's deformable convolutions (torchvision.ops.deform_conv2d) have no
ONNX Runtime kernel, so they are exported as an equivalent of GridSample
(bilinear, zero padding) per kernel tap and a matrix product
(`deform_conv2d_portable`, max difference to torchvision 1.5e-5 on random
input, 5e-10 on the model's output). The weights are stored as fp16 behind
Cast-to-fp32 nodes (half the size, computed in fp32), as export_sky.py does.

Usage: export_birefnet.py OUT_DIR
Needs: torch, torchvision, transformers, timm, kornia, einops, onnx,
onnxruntime (see scripts/models/build_model_pack.py for pinned versions).
"""
import sys
from pathlib import Path

import numpy as np
import onnx
import torch
import torch.nn.functional as F
from transformers import AutoModelForImageSegmentation

MODEL = "ZhengPeng7/BiRefNet"
REVISION = "e2bf8e4460fc8fa32bba5ea4d94b3233d367b0e4"
SIZE = 1024
FILE = "birefnet_subject.onnx"


def _pair(v):
    return (v, v) if isinstance(v, int) else tuple(v)


def deform_conv2d_portable(input, offset, weight, bias=None, stride=(1, 1), padding=(0, 0),
                           dilation=(1, 1), mask=None):
    """torchvision.ops.deform_conv2d (one offset group, one weight group) from
    GridSample and a matrix product, so ONNX Runtime can run it."""
    sh, sw = _pair(stride)
    ph, pw = _pair(padding)
    dh, dw = _pair(dilation)
    n, c, h, w = input.shape
    o, _, kh, kw = weight.shape
    ho = (h + 2 * ph - dh * (kh - 1) - 1) // sh + 1
    wo = (w + 2 * pw - dw * (kw - 1) - 1) // sw + 1
    ys = (torch.arange(ho, dtype=input.dtype) * sh - ph).view(1, ho, 1)
    xs = (torch.arange(wo, dtype=input.dtype) * sw - pw).view(1, 1, wo)
    cols = []
    for i in range(kh):
        for j in range(kw):
            k = i * kw + j
            py = ys + i * dh + offset[:, 2 * k]
            px = xs + j * dw + offset[:, 2 * k + 1]
            grid = torch.stack((2.0 * px / max(w - 1, 1) - 1.0, 2.0 * py / max(h - 1, 1) - 1.0), dim=-1)
            sample = F.grid_sample(input, grid, mode="bilinear", padding_mode="zeros", align_corners=True)
            if mask is not None:
                sample = sample * mask[:, k : k + 1]
            cols.append(sample)
    cols = torch.stack(cols, dim=2).reshape(n, c * kh * kw, ho * wo)
    out = torch.matmul(weight.reshape(o, c * kh * kw), cols).reshape(n, o, ho, wo)
    if bias is not None:
        out = out + bias.view(1, o, 1, 1)
    return out


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


class Subject(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, pixel_values):
        return torch.sigmoid(self.model(pixel_values)[-1])


def export(out: Path) -> Path:
    out.mkdir(parents=True, exist_ok=True)
    model = AutoModelForImageSegmentation.from_pretrained(MODEL, revision=REVISION, trust_remote_code=True).eval()
    module = sys.modules[type(model).__module__]
    probe = torch.from_numpy(np.random.default_rng(0).random((1, 3, SIZE, SIZE), dtype=np.float32))
    subject = Subject(model).eval()
    with torch.no_grad():
        reference = subject(probe)
        module.deform_conv2d = deform_conv2d_portable
        portable = subject(probe)
        print(f"max |portable deform conv - torchvision| on the model output: {float((portable - reference).abs().max()):.2e}")
        path = out / FILE
        torch.onnx.export(subject, (probe,), str(path), input_names=["pixel_values"], output_names=["subject"],
                          opset_version=17, do_constant_folding=True, dynamo=False)
    store_fp16(path)
    return path


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    print(export(Path(sys.argv[1])))
