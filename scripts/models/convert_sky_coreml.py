#!/usr/bin/env python3
"""Convert the sky model (UperNet + ConvNeXt-T, ADE20K, MIT) into a Core ML
program for macOS's fast path.

The same checkpoint, revision and graph as export_sky.py (the ONNX model
every platform runs; fixed pyramid pooling, the sky channel's softmax
margin upsampled, then a sigmoid), converted with coremltools from the
PyTorch weights, not from the ONNX file:
- input  pixel_values float32 [1, 3, 512, 512], as the ONNX model (the photo
  fitted into 512 x 512 and centred, sRGB 8-bit / 255, ImageNet normalised);
- output sky float32 [1, 1, 512, 512], the sky probability in 0..1.
ML program, macOS 13 or later; fp32 by default (`--precision fp16` for
comparisons).

Writes OUT_DIR/skyseg_upernet.mlpackage and, for the model pack, its three
files as flat assets (the app puts the package back together, as for
BiRefNet's birefnet_coreml_*): skyseg_coreml_Manifest.json,
skyseg_coreml_model.mlmodel, skyseg_coreml_weight.bin.

Usage: convert_sky_coreml.py OUT_DIR [--check IMAGE_DIR] [--precision fp32|fp16]
  --check runs the package and the ONNX export's PyTorch graph on every image
  in IMAGE_DIR (prepared as the app does) and prints the masks' agreement.
Needs: the convert_birefnet_coreml.py environment (coremltools 9.0, torch 2.7,
transformers; macOS).
"""
import argparse
import shutil
import sys
from pathlib import Path

import numpy as np
import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from convert_birefnet_coreml import compute_precision, fix_manifest, patch_coremltools, sort_spec  # noqa: E402
from export_sky import MODEL, REVISION, SIZE, Sky  # noqa: E402

PACKAGE = "skyseg_upernet.mlpackage"
ASSETS = {
    "skyseg_coreml_Manifest.json": "Manifest.json",
    "skyseg_coreml_model.mlmodel": "Data/com.apple.CoreML/model.mlmodel",
    "skyseg_coreml_weight.bin": "Data/com.apple.CoreML/weights/weight.bin",
}


def load_sky() -> Sky:
    from transformers import UperNetForSemanticSegmentation

    model = UperNetForSemanticSegmentation.from_pretrained(MODEL, revision=REVISION, use_safetensors=True).eval()
    sky = Sky(model).eval()
    # The pooling matrices' transposes as constants: coremltools numbers the
    # transpose ops of `rows.T` in an order that changes between runs, which
    # made the converted file differ in bytes (same values).
    for block in sky.decode_head.psp_modules.blocks:
        pool = block.layers[0]
        pool.register_buffer("cols", pool.rows.T.contiguous(), persistent=False)
        pool.forward = (lambda p: lambda x: p.rows @ x @ p.cols)(pool)
    return sky


def canonical_names(path: Path) -> None:
    """coremltools numbers some generated transpose ops in an order that
    changes between runs (same graph); number them by where they appear in
    the program instead, so two conversions give the same bytes."""
    import re

    from coremltools.proto import Model_pb2
    from google.protobuf import text_format

    spec = Model_pb2.Model()
    spec.ParseFromString(path.read_bytes())
    text = text_format.MessageToString(spec)
    order: dict[str, str] = {}
    for match in re.finditer(r"\btranspose_\d+(?!\d)", text):
        order.setdefault(match.group(0), f"transpose_{len(order)}")
    # one pass: every old name is replaced at once, so new names never collide
    text = re.sub(r"\btranspose_\d+(?!\d)", lambda m: order[m.group(0)], text)
    canonical = Model_pb2.Model()
    text_format.Parse(text, canonical)
    path.write_bytes(canonical.SerializeToString(deterministic=True))


def convert(out: Path, sky: Sky, precision: str) -> Path:
    import coremltools as ct

    patch_coremltools()
    out.mkdir(parents=True, exist_ok=True)
    probe = torch.from_numpy(np.random.default_rng(0).standard_normal((1, 3, SIZE, SIZE), dtype=np.float32))
    with torch.no_grad():
        traced = torch.jit.trace(sky, probe, check_trace=False)
    mlmodel = ct.convert(
        traced,
        inputs=[ct.TensorType(name="pixel_values", shape=(1, 3, SIZE, SIZE), dtype=np.float32)],
        outputs=[ct.TensorType(name="sky", dtype=np.float32)],
        convert_to="mlprogram",
        compute_precision=compute_precision(ct, precision),
        minimum_deployment_target=ct.target.macOS13,
    )
    mlmodel.short_description = "UperNet + ConvNeXt-T on ADE20K (Meta, MIT): sky probability"
    mlmodel.license = "MIT"
    package = out / PACKAGE
    if package.exists():
        shutil.rmtree(package)
    mlmodel.save(str(package))
    fix_manifest(package / "Manifest.json")
    sort_spec(package / ASSETS["skyseg_coreml_model.mlmodel"])
    canonical_names(package / ASSETS["skyseg_coreml_model.mlmodel"])
    for asset, member in ASSETS.items():
        shutil.copyfile(package / member, out / asset)
    return package


def prepare(path: Path) -> np.ndarray:
    """The app's input (sky_probability): fitted into 512 px, centred."""
    from PIL import Image

    image = Image.open(path).convert("RGB")
    image.thumbnail((SIZE, SIZE), Image.Resampling.BILINEAR)
    x = np.asarray(image, dtype=np.float32) / 255.0
    x = (x - np.array([0.485, 0.456, 0.406], np.float32)) / np.array([0.229, 0.224, 0.225], np.float32)
    canvas = np.zeros((SIZE, SIZE, 3), np.float32)
    top, left = (SIZE - x.shape[0]) // 2, (SIZE - x.shape[1]) // 2
    canvas[top : top + x.shape[0], left : left + x.shape[1]] = x
    return canvas.transpose(2, 0, 1)[None].copy()


def check(package: Path, sky: Sky, images: Path) -> None:
    import time

    import coremltools as ct

    files = sorted(p for p in images.iterdir() if p.suffix.lower() in (".jpg", ".jpeg", ".png", ".tif", ".tiff"))
    model = ct.models.MLModel(str(package), compute_units=ct.ComputeUnit.CPU_AND_GPU)
    ious, worst, times = [], 0.0, []
    for path in files:
        x = prepare(path)
        with torch.no_grad():
            ref = sky(torch.from_numpy(x)).numpy()[0, 0]
        start = time.perf_counter()
        got = model.predict({"pixel_values": x})["sky"][0, 0]
        times.append(time.perf_counter() - start)
        a, b = ref >= 0.5, got >= 0.5
        union = np.logical_or(a, b).sum()
        iou = 1.0 if union == 0 else np.logical_and(a, b).sum() / union
        ious.append(iou)
        worst = max(worst, float(np.abs(ref - got).max()))
        print(f"{path.name}: IoU {iou:.5f}, max |diff| {np.abs(ref - got).max():.4f}, {times[-1]:.3f} s")
    print(f"{len(ious)} images: IoU mean {np.mean(ious):.5f}, min {np.min(ious):.5f}, "
          f"max |diff| {worst:.4f}, median {np.median(times):.3f} s")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", type=Path)
    parser.add_argument("--check", type=Path)
    parser.add_argument("--precision", choices=("fp32", "fp16"), default="fp32")
    args = parser.parse_args()
    sky = load_sky()
    package = convert(args.out, sky, args.precision)
    print(package)
    if args.check:
        check(package, sky, args.check)


if __name__ == "__main__":
    main()
