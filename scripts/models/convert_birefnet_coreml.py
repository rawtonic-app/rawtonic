#!/usr/bin/env python3
"""Convert BiRefNet (MIT) into a Core ML program for macOS's fast path.

The same checkpoint, revision and deformable-convolution decomposition as
export_birefnet.py (the ONNX model every platform runs), converted with
coremltools from the PyTorch weights, not from the ONNX file:
- input  pixel_values float32 [1, 3, 1024, 1024], as the ONNX model
  (sRGB 8-bit / 255, ImageNet normalised, the photo stretched to 1024 x 1024);
- output subject float32 [1, 1, 1024, 1024], the subject probability in 0..1.
ML program, macOS 13 or later (GridSample needs it), fp16 weights and
compute (the app's model since 2026-10-11): against SAM 2.1 Large subject
references on 36 public raw.pixls.us photos (people, hair, fur, feathers,
fine plants) it scores as fp32 does (IoU 0.9169 vs 0.9170, boundary F
0.690 vs 0.691; the difference is a fortieth or less of the references' own
uncertainty, no edge visibly worse at 100 %), at a third of the time
(0.17-0.19 s vs 0.51-0.54 s per run on an M5 Pro) and with about 1.6 GB
instead of 4.4 GB held by a run (docs/MODELS.md). `--precision fp32` makes
the earlier fp32 program (pack files birefnet_coreml_*), e.g. for comparisons.

Writes OUT_DIR/birefnet_subject.mlpackage and, for the model pack, its three
files as flat assets (the app puts the package back together, as for AI
Denoise's nlmf_coreml_*): birefnet_fp16_coreml_Manifest.json,
birefnet_fp16_coreml_model.mlmodel, birefnet_fp16_coreml_weight.bin (fp32:
birefnet_coreml_*). The bytes repeat in every conversion (manifest
identifiers, protobuf map order and coremltools' conversion date are fixed).

Usage: convert_birefnet_coreml.py OUT_DIR [--check IMAGE_DIR] [--precision fp16|fp32]
  --check runs the package and the fp32 PyTorch model on every image in
  IMAGE_DIR (as the app prepares them) and prints the masks' agreement.
Needs: the export_birefnet.py packages plus coremltools==9.0 (macOS).
"""
import argparse
import json
import shutil
import sys
import uuid
from pathlib import Path

import numpy as np
import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from export_birefnet import MODEL, REVISION, SIZE, Subject, deform_conv2d_portable  # noqa: E402

PACKAGE = "birefnet_subject.mlpackage"
MEMBERS = {
    "Manifest.json": "Manifest.json",
    "model.mlmodel": "Data/com.apple.CoreML/model.mlmodel",
    "weight.bin": "Data/com.apple.CoreML/weights/weight.bin",
}
# The pack's names of the package's files, per precision.
PREFIX = {"fp16": "birefnet_fp16_coreml_", "fp32": "birefnet_coreml_"}
# coremltools writes the day of conversion into the model's metadata; the
# pack's programs carry this day, so a conversion on any day gives the same
# bytes.
CONVERSION_DATE = "2026-10-10"


def assets(precision: str) -> dict[str, str]:
    return {PREFIX[precision] + name: member for name, member in MEMBERS.items()}


def load_subject() -> Subject:
    from transformers import AutoModelForImageSegmentation

    model = AutoModelForImageSegmentation.from_pretrained(MODEL, revision=REVISION, trust_remote_code=True).eval()
    sys.modules[type(model).__module__].deform_conv2d = deform_conv2d_portable
    return Subject(model).eval()


def patch_coremltools() -> None:
    """coremltools 9.0 casts a folded one-element shape tensor with
    `int(array)`, which NumPy 2.5 refuses for arrays of one dimension (Swin's
    padding arithmetic hits it); take the element instead."""
    from coremltools.converters.mil import Builder as mb
    from coremltools.converters.mil.frontend.torch import ops

    original = ops._cast

    def cast(context, node, dtype, dtype_name):
        x = ops._get_inputs(context, node, expected=1)[0]
        if x.can_be_folded_to_const() and np.ndim(x.val) > 0 and np.size(x.val) == 1:
            context.add(mb.const(val=dtype(np.asarray(x.val).item()), name=node.name), node.name)
            return
        original(context, node, dtype, dtype_name)

    ops._cast = cast


def fix_manifest(path: Path) -> None:
    """coremltools names the package's items by random UUIDs; name them by
    their path instead, so two conversions give the same bytes."""
    manifest = json.loads(path.read_text())
    renamed = {}
    root = manifest["rootModelIdentifier"]
    for key, item in manifest["itemInfoEntries"].items():
        new = str(uuid.uuid5(uuid.NAMESPACE_URL, "rawtonic:" + item["path"])).upper()
        renamed[new] = item
        if key == root:
            manifest["rootModelIdentifier"] = new
    manifest["itemInfoEntries"] = dict(sorted(renamed.items()))
    path.write_text(json.dumps(manifest, indent=4) + "\n")


def sort_spec(path: Path) -> None:
    """The model's protobuf holds maps (metadata, operation arguments) that
    are written in hash order; write them sorted (same bytes every time),
    with the conversion date fixed (`CONVERSION_DATE`)."""
    from coremltools.proto import Model_pb2

    spec = Model_pb2.Model()
    spec.ParseFromString(path.read_bytes())
    metadata = spec.description.metadata.userDefined
    if "com.github.apple.coremltools.conversion_date" in metadata:
        metadata["com.github.apple.coremltools.conversion_date"] = CONVERSION_DATE
    path.write_bytes(spec.SerializeToString(deterministic=True))


PRECISIONS = ("fp16", "fp32")


def compute_precision(ct, precision: str):
    return ct.precision.FLOAT32 if precision == "fp32" else ct.precision.FLOAT16


def convert(out: Path, subject: Subject, precision: str = "fp16") -> Path:
    import coremltools as ct

    patch_coremltools()
    out.mkdir(parents=True, exist_ok=True)
    probe = torch.from_numpy(np.random.default_rng(0).random((1, 3, SIZE, SIZE), dtype=np.float32))
    with torch.no_grad():
        traced = torch.jit.trace(subject, probe, check_trace=False)
    mlmodel = ct.convert(
        traced,
        inputs=[ct.TensorType(name="pixel_values", shape=(1, 3, SIZE, SIZE), dtype=np.float32)],
        outputs=[ct.TensorType(name="subject", dtype=np.float32)],
        convert_to="mlprogram",
        compute_precision=compute_precision(ct, precision),
        minimum_deployment_target=ct.target.macOS13,
    )
    mlmodel.short_description = "BiRefNet (general checkpoint, Swin-L; Peng Zheng et al., MIT): subject probability"
    mlmodel.license = "MIT"
    package = out / PACKAGE
    if package.exists():
        shutil.rmtree(package)
    mlmodel.save(str(package))
    fix_manifest(package / "Manifest.json")
    sort_spec(package / MEMBERS["model.mlmodel"])
    for asset, member in assets(precision).items():
        shutil.copyfile(package / member, out / asset)
    return package


def prepare(path: Path) -> np.ndarray:
    """The app's preprocessing (run_subject_model): stretched to 1024 px with
    a triangle filter, ImageNet normalised."""
    from PIL import Image

    image = Image.open(path).convert("RGB").resize((SIZE, SIZE), Image.Resampling.BILINEAR)
    x = np.asarray(image, dtype=np.float32) / 255.0
    x = (x - np.array([0.485, 0.456, 0.406], np.float32)) / np.array([0.229, 0.224, 0.225], np.float32)
    return x.transpose(2, 0, 1)[None].copy()


def check(package: Path, subject: Subject, images: Path) -> None:
    import coremltools as ct

    files = sorted(p for p in images.iterdir() if p.suffix.lower() in (".jpg", ".jpeg", ".png", ".tif", ".tiff"))
    model = ct.models.MLModel(str(package), compute_units=ct.ComputeUnit.CPU_AND_GPU)
    ious = []
    for path in files:
        x = prepare(path)
        with torch.no_grad():
            ref = subject(torch.from_numpy(x)).numpy()[0, 0]
        got = model.predict({"pixel_values": x})["subject"][0, 0]
        a, b = ref >= 0.5, got >= 0.5
        union = np.logical_or(a, b).sum()
        iou = 1.0 if union == 0 else np.logical_and(a, b).sum() / union
        ious.append(iou)
        print(f"{path.name}: IoU {iou:.5f}, max |diff| {np.abs(ref - got).max():.4f}")
    print(f"{len(ious)} images: IoU mean {np.mean(ious):.5f}, min {np.min(ious):.5f}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", type=Path)
    parser.add_argument("--check", type=Path)
    parser.add_argument("--precision", choices=PRECISIONS, default="fp16")
    args = parser.parse_args()
    subject = load_subject()
    package = convert(args.out, subject, args.precision)
    print(package)
    if args.check:
        check(package, subject, args.check)


if __name__ == "__main__":
    main()
