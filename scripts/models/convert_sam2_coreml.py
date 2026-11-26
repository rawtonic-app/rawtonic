#!/usr/bin/env python3
"""Convert SAM 2.1 Hiera-T's image encoder (Meta, Apache-2.0) into a Core ML
program for macOS's fast path.

The same checkpoint and graph as export_sam2.py's encoder (the ONNX model
every platform runs), converted with coremltools from the PyTorch weights,
not from the ONNX file:
- input  image float32 [1, 3, 1024, 1024]: the photo stretched to
  1024 x 1024, sRGB 8-bit values 0..255 (the ONNX model takes the same
  values as uint8, which Core ML has no input type for); ImageNet
  normalisation runs in the graph;
- outputs image_embed [1, 256, 64, 64], high_res_feats_0 [1, 32, 256, 256],
  high_res_feats_1 [1, 64, 128, 128], as the ONNX model's.
ML program, macOS 13 or later, fp32 compute. Two changes against the
PyTorch model, both to match the ONNX model the other platforms run:
- every weight tensor of 1024 values or more is rounded to fp16 (the ONNX
  file stores them so and computes in fp32);
- Hiera's position embedding, which the model interpolates (bicubic) to the
  1024 x 1024 input's 256 x 256 tokens on every run, is computed once and
  kept as a constant (coremltools has no bicubic upsampling; the ONNX export
  folds it into a constant too).
Against the ONNX model on ONNX Runtime's CPU, through the app's decoder and
edge refinement on 32 RAW photos (4 clicks and 2 boxes each), compared at
the masks' 50 % threshold, 186 of 192 agree on every pixel and the lowest
IoU is 0.99995 (docs/MODELS.md); fp16
compute (`--precision fp16`, for comparisons) is faster again but its masks
differ more (lowest IoU 0.997 on 3 photos).

Writes OUT_DIR/sam2.1_tiny_encoder.mlpackage (fp16:
sam2.1_tiny_encoder_fp16.mlpackage) and, for the model pack, its three files
as flat assets (the app puts the package back together, as for
the sky model's skyseg_coreml_*): sam2_tiny_encoder_coreml_Manifest.json,
sam2_tiny_encoder_coreml_model.mlmodel, sam2_tiny_encoder_coreml_weight.bin.
The bytes repeat in every conversion (manifest identifiers, protobuf map
order and coremltools' conversion date are fixed; checked twice).

Usage: convert_sam2_coreml.py CHECKPOINT.pt OUT_DIR (without an earlier conversion of that precision) [--check IMAGE_DIR --onnx-dir DIR] [--precision fp32|fp16]
  --check runs the package and the ONNX encoder (ONNX Runtime, CPU) on every
  image in IMAGE_DIR, then the ONNX decoder on both encodings with clicks
  and boxes, and prints the masks' agreement and the times.
Needs: the export_sam2.py packages (torch 2.7, SAM-2 at 2b90b9f) plus
coremltools==9.0 and onnxruntime (macOS); see build_model_pack.py.
"""
import argparse
import shutil
import sys
import tempfile
from pathlib import Path

import numpy as np
import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from coreml_package import MEMBERS, fix_manifest, sort_spec  # noqa: E402
from export_sam2 import CONFIGS, SIZE, Encoder  # noqa: E402

PACKAGE = {"fp32": "sam2.1_tiny_encoder.mlpackage", "fp16": "sam2.1_tiny_encoder_fp16.mlpackage"}
PREFIX = {"fp32": "sam2_tiny_encoder_coreml_", "fp16": "sam2_tiny_encoder_fp16_coreml_"}
OUTPUTS = ("image_embed", "high_res_feats_0", "high_res_feats_1")
# coremltools writes the day of conversion into the model's metadata; the
# pack's program carries this day, so a conversion on any day gives the
# same bytes.
CONVERSION_DATE = "2026-11-26"


def assets(precision: str) -> dict[str, str]:
    return {PREFIX[precision] + name: member for name, member in MEMBERS.items()}


def load_encoder(checkpoint: Path) -> Encoder:
    from sam2.build_sam import build_sam2

    torch.manual_seed(0)
    model = build_sam2(CONFIGS["tiny"], str(checkpoint), device="cpu", mode="eval")
    with torch.no_grad():
        # export_sam2.store_fp16: tensors of 1024 values or more as fp16.
        for tensor in model.state_dict().values():
            if tensor.dtype == torch.float32 and tensor.numel() >= 1024:
                tensor.copy_(tensor.half().float())
        trunk = model.image_encoder.trunk
        tokens = SIZE // 4  # Hiera's patch embedding: stride 4
        trunk.register_buffer("fixed_pos_embed", trunk._get_pos_embed((tokens, tokens)).clone(), persistent=False)
    trunk._get_pos_embed = lambda hw: trunk.fixed_pos_embed
    return Encoder(model).eval()


def convert(out: Path, encoder: Encoder, precision: str) -> Path:
    import coremltools as ct

    out.mkdir(parents=True, exist_ok=True)
    probe = torch.from_numpy(np.random.default_rng(0).integers(0, 256, (1, 3, SIZE, SIZE)).astype(np.float32))
    with torch.no_grad():
        traced = torch.jit.trace(encoder, probe, check_trace=False)
    mlmodel = ct.convert(
        traced,
        inputs=[ct.TensorType(name="image", shape=(1, 3, SIZE, SIZE), dtype=np.float32)],
        outputs=[ct.TensorType(name=name, dtype=np.float32) for name in OUTPUTS],
        convert_to="mlprogram",
        compute_precision=ct.precision.FLOAT32 if precision == "fp32" else ct.precision.FLOAT16,
        minimum_deployment_target=ct.target.macOS13,
    )
    mlmodel.short_description = "SAM 2.1 Hiera-T image encoder (Meta, Apache-2.0): image embeddings"
    mlmodel.license = "Apache-2.0"
    # Never over an earlier conversion (nothing of it is lost); built
    # complete in a staging folder, then moved into place.
    package = out / PACKAGE[precision]
    targets = [package] + [out / asset for asset in assets(precision)]
    if any(t.exists() for t in targets):
        sys.exit(f"{out} already holds a {precision} conversion; convert into a fresh folder")
    staging = Path(tempfile.mkdtemp(prefix=".convert-", dir=out))
    try:
        staged = staging / PACKAGE[precision]
        mlmodel.save(str(staged))
        fix_manifest(staged / "Manifest.json")
        sort_spec(staged / MEMBERS["model.mlmodel"], CONVERSION_DATE)
        for asset, member in assets(precision).items():
            shutil.copyfile(staged / member, staging / asset)
        staged.rename(package)
        for asset in assets(precision):
            (staging / asset).rename(out / asset)
    finally:
        shutil.rmtree(staging)
    return package


def prepare(path: Path) -> np.ndarray:
    """The app's input (generate_image_embeddings): stretched to 1024 px."""
    from PIL import Image

    image = Image.open(path).convert("RGB").resize((SIZE, SIZE), Image.Resampling.BILINEAR)
    return np.asarray(image).transpose(2, 0, 1)[None].copy()


def check(package: Path, onnx_dir: Path, images: Path) -> None:
    import time

    import coremltools as ct
    import onnxruntime as ort

    files = sorted(p for p in images.iterdir() if p.suffix.lower() in (".jpg", ".jpeg", ".png", ".tif", ".tiff"))
    options = ort.SessionOptions()
    cpu = ort.InferenceSession(str(onnx_dir / "sam2.1_tiny_encoder.onnx"), options, providers=["CPUExecutionProvider"])
    decoder = ort.InferenceSession(str(onnx_dir / "sam2.1_tiny_decoder.onnx"), options, providers=["CPUExecutionProvider"])
    model = ct.models.MLModel(str(package), compute_units=ct.ComputeUnit.CPU_AND_GPU)

    def masks(encoding, size):
        prompts = [([[x, y]], [1]) for x, y in ((512, 512), (341, 600), (683, 450), (512, 300))]
        prompts += [([[307, 307], [717, 717]], [2, 3]), ([[100, 200], [900, 800]], [2, 3])]
        out = []
        for coords, labels in prompts:
            feeds = dict(zip(OUTPUTS, encoding))
            feeds.update(point_coords=np.array([coords], np.float32), point_labels=np.array([labels], np.float32),
                         mask_input=np.zeros((1, 1, 256, 256), np.float32), has_mask_input=np.zeros(1, np.float32),
                         orig_im_size=np.array(size, np.float32))
            out.append(decoder.run(["masks"], feeds)[0][0, 0] > 0)
        return out

    ious, times = [], []
    for path in files:
        x = prepare(path)
        reference = cpu.run(list(OUTPUTS), {"image": x})
        model.predict({"image": x.astype(np.float32)})
        start = time.perf_counter()
        got = model.predict({"image": x.astype(np.float32)})
        times.append(time.perf_counter() - start)
        got = [got[name].astype(np.float32) for name in OUTPUTS]
        diff = max(float(np.abs(a - b).max() / np.abs(b).max()) for a, b in zip(got, reference))
        worst = 1.0
        for a, b in zip(masks(got, [SIZE, SIZE]), masks(reference, [SIZE, SIZE])):
            union = np.logical_or(a, b).sum()
            worst = min(worst, 1.0 if union == 0 else float(np.logical_and(a, b).sum() / union))
        ious.append(worst)
        print(f"{path.name}: lowest mask IoU {worst:.5f}, max relative |diff| {diff:.2e}, {times[-1]:.3f} s")
    print(f"{len(ious)} images: lowest mask IoU mean {np.mean(ious):.5f}, min {np.min(ious):.5f}, "
          f"median {np.median(times):.3f} s")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("checkpoint", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--check", type=Path)
    parser.add_argument("--onnx-dir", type=Path)
    parser.add_argument("--precision", choices=("fp32", "fp16"), default="fp32")
    args = parser.parse_args()
    package = convert(args.out, load_encoder(args.checkpoint), args.precision)
    print(package)
    if args.check:
        check(package, args.onnx_dir, args.check)


if __name__ == "__main__":
    main()
