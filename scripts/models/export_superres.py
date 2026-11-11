#!/usr/bin/env python3
"""Export the Super Resolution model (2x enlargement at export) as ONNX and
as a Core ML program for macOS's fast path.

Checkpoint: Philip Hofmann's 2xBHI_small_compact_pretrain (a Compact network,
SRVGGNetCompact: 64 features, 16 convolutions, PReLU, pixel shuffle; trained
with L1 and MS-SSIM losses only), released under CC BY 4.0 at
github.com/Phhofm/models/releases/tag/2xBHI_small_compact_pretrain. The
network is rebuilt here from its weights (no third-party loader).

Changes made by this export (CC BY 4.0 asks to indicate them): the four
right-angle rotations of each tile are enlarged and averaged (self-ensemble)
inside the graph, and the result is clamped to 0..1. The weights are
unchanged.

- input  image float32 [1, 3, 512, 512]: an RGB tile of the rendered
  export in its output encoding (sRGB-like transfer), 0..1;
- output enlarged float32 [1, 3, 1024, 1024], 0..1.

Writes OUT_DIR/superres_compact_x2.onnx (fp32), the Core ML package
OUT_DIR/superres_compact_x2.mlpackage (ML program, fp16, macOS 13) and the
package's three files as flat pack assets superres_coreml_Manifest.json,
superres_coreml_model.mlmodel, superres_coreml_weight.bin (the app puts the
package back together, as for the sky model).

Usage: export_superres.py OUT_DIR [--check IMAGE_DIR] [--cpu-only]
  --cpu-only writes superres_compact_x2_cpu.onnx, the same weights and
  rotation average with each rotation run separately (CPU batch size one);
  existing ONNX/Core ML exports are unchanged.
  --check enlarges 512 px tiles of every image in IMAGE_DIR with PyTorch
  (fp32), the ONNX file (ONNX Runtime, CPU) and the Core ML program and
  prints their largest differences and times.
Needs: the pack environment (build_model_pack.py SUPERRES_EXPORT: torch 2.7.0,
coremltools 9.0, onnx 1.23.2; macOS for the Core ML part), in which both
files repeat byte for byte.
Notes: benchmark-results/srx-super-resolution-2026-11-08/REPORT.md.
"""
import argparse
import hashlib
import shutil
import sys
import urllib.request
from pathlib import Path

import numpy as np
import torch
import torch.nn.functional as F
from torch import nn

sys.path.insert(0, str(Path(__file__).resolve().parent))
from coreml_package import MEMBERS, fix_manifest, sort_spec  # noqa: E402

URL = (
    "https://github.com/Phhofm/models/releases/download/"
    "2xBHI_small_compact_pretrain/2xBHI_small_compact_pretrain.pth"
)
SHA256 = "17135b91d28e6e11583c96ec6d0d8e05f89a1c7dc0906234ff16a43d633f9ddc"
TILE = 512
SCALE = 2
ONNX_FILE = "superres_compact_x2.onnx"
CPU_ONNX_FILE = "superres_compact_x2_cpu.onnx"
PACKAGE = "superres_compact_x2.mlpackage"
ASSETS = {f"superres_coreml_{name}": member for name, member in MEMBERS.items()}
CONVERSION_DATE = "2026-11-09"


class Compact(nn.Module):
    """SRVGGNetCompact (Real-ESRGAN's compact network, BSD-3-Clause design)."""

    def __init__(self, feats: int = 64, convs: int = 16, scale: int = SCALE):
        super().__init__()
        self.scale = scale
        body: list[nn.Module] = [nn.Conv2d(3, feats, 3, 1, 1), nn.PReLU(feats)]
        for _ in range(convs):
            body += [nn.Conv2d(feats, feats, 3, 1, 1), nn.PReLU(feats)]
        body.append(nn.Conv2d(feats, 3 * scale * scale, 3, 1, 1))
        self.body = nn.Sequential(*body)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        out = F.pixel_shuffle(self.body(x), self.scale)
        return out + F.interpolate(x, scale_factor=self.scale, mode="nearest")


def rot90(x: torch.Tensor, k: int) -> torch.Tensor:
    """torch.rot90(x, k, (2, 3)) from flips and a transpose (ONNX has no
    rot90)."""
    k %= 4
    if k == 1:
        return x.flip(3).transpose(2, 3)
    if k == 2:
        return x.flip(2).flip(3)
    if k == 3:
        return x.transpose(2, 3).flip(3)
    return x


class Ensemble(nn.Module):
    """The four right-angle rotations of a square tile, enlarged together and
    rotated back, averaged and clamped to 0..1."""

    def __init__(self, net: nn.Module):
        super().__init__()
        self.net = net

    def forward(self, image: torch.Tensor) -> torch.Tensor:
        batch = torch.cat([rot90(image, k) for k in range(4)], 0)
        out = self.net(batch)
        back = [rot90(out[k : k + 1], -k) for k in range(4)]
        return ((back[0] + back[1] + back[2] + back[3]) * 0.25).clamp(0.0, 1.0)


class SequentialEnsemble(Ensemble):
    """The same four rotations, with one tile per convolution on the CPU."""

    def forward(self, image: torch.Tensor) -> torch.Tensor:
        back = [rot90(self.net(rot90(image, k)), -k) for k in range(4)]
        return ((back[0] + back[1] + back[2] + back[3]) * 0.25).clamp(0.0, 1.0)


def checkpoint(cache: Path) -> Path:
    path = cache / "2xBHI_small_compact_pretrain.pth"
    if not path.exists():
        cache.mkdir(parents=True, exist_ok=True)
        part = path.with_suffix(".part")
        urllib.request.urlretrieve(URL, part)
        part.rename(path)
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if digest != SHA256:
        sys.exit(f"{path}: SHA-256 {digest}, expected {SHA256}")
    return path


def load(path: Path) -> Ensemble:
    state = torch.load(path, map_location="cpu", weights_only=True)
    state = state.get("params_ema", state.get("params", state))
    net = Compact()
    net.load_state_dict(state, strict=True)
    return Ensemble(net).eval()


def export_onnx(model: Ensemble, out: Path, cpu: bool = False) -> Path:
    path = out / (CPU_ONNX_FILE if cpu else ONNX_FILE)
    probe = torch.zeros(1, 3, TILE, TILE)
    torch.onnx.export(
        model,
        probe,
        str(path),
        input_names=["image"],
        output_names=["enlarged"],
        opset_version=17,
        do_constant_folding=True,
        dynamo=False,
    )
    return path


def convert_coreml(model: Ensemble, out: Path) -> Path:
    import coremltools as ct

    probe = torch.from_numpy(np.random.default_rng(0).random((1, 3, TILE, TILE), dtype=np.float32))
    with torch.no_grad():
        traced = torch.jit.trace(model, probe, check_trace=False)
    mlmodel = ct.convert(
        traced,
        inputs=[ct.TensorType(name="image", shape=(1, 3, TILE, TILE), dtype=np.float32)],
        outputs=[ct.TensorType(name="enlarged", dtype=np.float32)],
        convert_to="mlprogram",
        compute_precision=ct.precision.FLOAT16,
        minimum_deployment_target=ct.target.macOS13,
    )
    mlmodel.short_description = (
        "2xBHI_small_compact_pretrain (Philip Hofmann, CC BY 4.0), 4-rotation ensemble: 2x enlargement"
    )
    mlmodel.license = "CC-BY-4.0"
    package = out / PACKAGE
    if package.exists():
        shutil.rmtree(package)
    mlmodel.save(str(package))
    fix_manifest(package / "Manifest.json")
    sort_spec(package / MEMBERS["model.mlmodel"], CONVERSION_DATE)
    for asset, member in ASSETS.items():
        shutil.copyfile(package / member, out / asset)
    return package


def tiles(images: Path):
    from PIL import Image

    for path in sorted(p for p in images.iterdir() if p.suffix.lower() in (".jpg", ".jpeg", ".png", ".tif", ".tiff")):
        image = np.asarray(Image.open(path).convert("RGB"), dtype=np.float32) / 255.0
        h, w = image.shape[:2]
        if h < TILE or w < TILE:
            continue
        y, x = (h - TILE) // 2, (w - TILE) // 2
        yield path.name, image[y : y + TILE, x : x + TILE].transpose(2, 0, 1)[None].copy()


def check(model: Ensemble, onnx_path: Path, package: Path | None, images: Path) -> None:
    import time

    import onnxruntime as ort

    session = ort.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    coreml = None
    if package is not None:
        import coremltools as ct

        coreml = ct.models.MLModel(str(package), compute_units=ct.ComputeUnit.CPU_AND_GPU)
    for name, x in tiles(images):
        with torch.no_grad():
            ref = model(torch.from_numpy(x)).numpy()
        start = time.perf_counter()
        got = session.run(None, {"image": x})[0]
        onnx_time = time.perf_counter() - start
        line = f"{name}: ONNX max |diff| {np.abs(got - ref).max():.6f} ({onnx_time:.3f} s)"
        if coreml is not None:
            start = time.perf_counter()
            ml = coreml.predict({"image": x})["enlarged"]
            ml_time = time.perf_counter() - start
            line += (
                f", Core ML max |diff| {np.abs(ml - ref).max():.4f} mean {np.abs(ml - ref).mean():.5f}"
                f" ({ml_time:.3f} s)"
            )
        print(line)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", type=Path)
    parser.add_argument("--check", type=Path)
    parser.add_argument("--no-coreml", action="store_true", help="only the ONNX file (not on macOS)")
    parser.add_argument("--cpu-only", action="store_true", help="export only the sequential CPU ONNX graph")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    model = load(checkpoint(args.out / "checkpoint"))
    if args.cpu_only:
        model = SequentialEnsemble(model.net).eval()
    onnx_path = export_onnx(model, args.out, cpu=args.cpu_only)
    print(onnx_path, hashlib.sha256(onnx_path.read_bytes()).hexdigest())
    package = None
    if not args.no_coreml and not args.cpu_only:
        package = convert_coreml(model, args.out)
        for asset in ASSETS:
            print(args.out / asset, hashlib.sha256((args.out / asset).read_bytes()).hexdigest())
    if args.check:
        check(model, onnx_path, package, args.check)


if __name__ == "__main__":
    main()
