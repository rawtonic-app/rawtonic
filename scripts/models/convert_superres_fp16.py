#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Convert the Super Resolution ONNX graph to fp16 compute for GPU providers.

Source: superres_compact_x2.onnx as export_superres.py writes it (Philip
Hofmann's 2xBHI_small_compact_pretrain, CC BY 4.0, four-rotation average in
the graph), checked against its pinned SHA-256. The result,
superres_compact_x2_fp16.onnx, stores the weights as fp16 and computes in
fp16 between a cast after the fp32 input and a cast before the fp32 output,
so the app feeds and reads it like the fp32 graph. Changes made (CC BY 4.0
asks to indicate them): the weights are rounded to fp16 (the converter
raises magnitudes below 1e-7 to 1e-7; that touches one weight, 4.5e-8) and
the network computes in fp16. Same weights and average otherwise.

On an RTX 3080 the 24 -> 96 MP enlargement took 8.2 s with CUDA instead of
11.9-12.2 s and 15.9 s with WebGPU instead of 22.4 s, PSNR 75 dB against the
fp32 graph at 16 bit, at most one 8-bit code (notes:
benchmark-results/cuda1-2026-11-20/REPORT.md, srf16-2026-11-20/REPORT.md).
ONNX Runtime's CPU provider has no fp16 convolution on x86 and casts back
to fp32, so the app uses this graph only on a GPU provider.

Usage: convert_superres_fp16.py SOURCE.onnx OUT_DIR
Needs: onnx 1.23.2, onnxconverter-common 1.16.0 (build_model_pack.py
SUPERRES_FP16_EXPORT), in which the output repeats byte for byte.
"""
import hashlib
import sys
from pathlib import Path

import onnx
from onnxconverter_common import float16

SOURCE_SHA256 = "088990646244e01a74c12460dda5a86547d967299d70b4e58afd4904abecd0d8"
FP16_FILE = "superres_compact_x2_fp16.onnx"


def convert(source: Path, out: Path) -> Path:
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    if digest != SOURCE_SHA256:
        sys.exit(f"{source}: SHA-256 {digest}, expected {SOURCE_SHA256}")
    model = onnx.load(str(source))
    # keep_io_types: the graph's input and output stay float32.
    converted = float16.convert_float_to_float16(model, keep_io_types=True)
    onnx.checker.check_model(converted)
    out.mkdir(parents=True, exist_ok=True)
    path = out / FP16_FILE
    part = path.with_suffix(".part")
    part.write_bytes(converted.SerializeToString(deterministic=True))
    part.replace(path)
    return path


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    path = convert(Path(sys.argv[1]), Path(sys.argv[2]))
    print(path, hashlib.sha256(path.read_bytes()).hexdigest())


if __name__ == "__main__":
    main()
