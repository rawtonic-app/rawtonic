#!/usr/bin/env python3
"""Convert the eye and teeth model (EasyPortrait FPN + ResNet-50) into a Core
ML program for macOS's fast path.

The same checkpoint and graph as export_easyportrait.py (normalisation,
mirror pass, softmax and class merging inside), converted with coremltools
from the PyTorch weights, not from the ONNX file:
- input  image float32 [1, 3, 1024, 1024], sRGB 8-bit values 0..255;
- output parts float32 [1, 2, 1024, 1024], eye and teeth probabilities.
ML program, macOS 13 or later, fp16 compute by default (`--precision fp32`
for comparisons).

Writes OUT_DIR/easyportrait_fpn.mlpackage and, for the model pack, its
three files as flat assets (the app puts the package back together, as for
the sky model): easyportrait_coreml_Manifest.json,
easyportrait_coreml_model.mlmodel, easyportrait_coreml_weight.bin.

Usage: convert_easyportrait_coreml.py CHECKPOINT OUT_DIR [--precision fp16|fp32]
Needs: coremltools 9.0, torch 2.7 (build_model_pack.py's EASYPORTRAIT_EXPORT); macOS.
"""
import argparse
import shutil
import sys
from pathlib import Path

import numpy as np
import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from coreml_package import fix_manifest, sort_spec  # noqa: E402
from export_easyportrait import SIZE, load  # noqa: E402

PACKAGE = "easyportrait_fpn.mlpackage"
ASSETS = {
    "easyportrait_coreml_Manifest.json": "Manifest.json",
    "easyportrait_coreml_model.mlmodel": "Data/com.apple.CoreML/model.mlmodel",
    "easyportrait_coreml_weight.bin": "Data/com.apple.CoreML/weights/weight.bin",
}
CONVERSION_DATE = "2026-11-21"


def convert(checkpoint: Path, out: Path, precision: str) -> Path:
    import coremltools as ct

    out.mkdir(parents=True, exist_ok=True)
    model = load(checkpoint)
    probe = torch.from_numpy(np.random.default_rng(0).uniform(0, 255, (1, 3, SIZE, SIZE)).astype(np.float32))
    with torch.no_grad():
        traced = torch.jit.trace(model, probe, check_trace=False)
    mlmodel = ct.convert(
        traced,
        inputs=[ct.TensorType(name="image", shape=(1, 3, SIZE, SIZE), dtype=np.float32)],
        outputs=[ct.TensorType(name="parts", dtype=np.float32)],
        convert_to="mlprogram",
        compute_precision=ct.precision.FLOAT32 if precision == "fp32" else ct.precision.FLOAT16,
        minimum_deployment_target=ct.target.macOS13,
    )
    mlmodel.short_description = "EasyPortrait FPN + ResNet-50 (SberDevices, CC BY-SA 4.0): eye and teeth probabilities"
    mlmodel.license = "CC-BY-SA-4.0"
    package = out / PACKAGE
    if package.exists():
        shutil.rmtree(package)
    mlmodel.save(str(package))
    fix_manifest(package / "Manifest.json")
    sort_spec(package / ASSETS["easyportrait_coreml_model.mlmodel"], CONVERSION_DATE)
    for asset, member in ASSETS.items():
        shutil.copyfile(package / member, out / asset)
    return package


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("checkpoint", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--precision", choices=("fp16", "fp32"), default="fp16")
    args = parser.parse_args()
    print(convert(args.checkpoint, args.out, args.precision))


if __name__ == "__main__":
    main()
