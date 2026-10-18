#!/usr/bin/env python3
"""Convert Nonlocal MatchFilter (MIT) into the mixed-precision Core ML
program AI Denoise version 3 runs on macOS.

The authors' raw-noise checkpoint (MIA-UIB/nonlocal-matchfilter release
v1.0.0, rawnoise_25_15_9nbr.ckpt, SHA-256 pinned) in the pure-PyTorch form
RapidRAW vendors (sheldonxxxx/RapidRAW denoise/, pinned commit: the authors'
network under MIT plus RapidRAW's static packed sampler, AGPL-3.0), the same
route as the pack's nlmf_coreml_* program, with two changes:
- 640 px tiles instead of 320 (input raw_with_noise float32 [1, 8, 640, 640]:
  R, G, B, G packed planes and their noise standard deviations; output
  denoised_raw float32 [1, 4, 640, 640]). With the app's 40 px halo a tile
  keeps 560² of 640² samples instead of 240² of 320².
- Mixed precision: convolutions in float16, the feature matching and
  sampling (`KEEP`) in float32. All-float16 loses 2 % of the finest detail's
  energy on real high-ISO tiles; this keeps it (0.01 noise σ RMS from the
  float32 program) at 0.84 µs per sample on an M5 Pro GPU instead of 2.2
  (notes: benchmark-results/ai-denoise-mac-2026-10-18, task DN).

Writes OUT_DIR/nlmf_mixed640.mlpackage and, for the model pack, its three
files as flat assets (nlmf_mixed640_coreml_Manifest.json, …_model.mlmodel,
…_weight.bin); the bytes repeat in every conversion (coreml_package.py).

Usage (macOS, uv):
  uv run --python 3.12 --with torch==2.7.0 --with coremltools==9.0 \\
    --with numpy==2.2.6 --with einops==0.8.1 --with omegaconf==2.3.0 \\
    python scripts/models/convert_nlmf_coreml.py OUT_DIR [--tile 640] [--check]
  --check runs the package (GPU) and the float32 PyTorch network on a
  synthetic noisy tile and prints their difference in noise σ.
"""
import argparse
import hashlib
import importlib
import shutil
import sys
import tempfile
import urllib.request
from pathlib import Path

import numpy as np
import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from coreml_package import MEMBERS, fix_manifest, sort_spec  # noqa: E402

CHECKPOINT = {
    "url": "https://github.com/MIA-UIB/nonlocal-matchfilter/releases/download/v1.0.0/rawnoise_25_15_9nbr.ckpt",
    "sha256": "b31bc8e21022389b0b209a9d1e6649485ed4dacc552b7ea04612df5f7515fcf2",
}
RAPIDRAW = "https://raw.githubusercontent.com/sheldonxxxx/RapidRAW/2375e17c6077e2a6bb3352f913f0d6dc894e222f/denoise/rapidraw_denoise/"
SOURCES = [f"vendor/nonlocalmf/{n}.py" for n in ("__init__", "conv", "ffn", "network", "padding", "sampling")] + [
    "static_packed_sampler.py"
]
# Operations kept in float32 (the rest computes in float16).
KEEP = {"resample", "linear", "sigmoid", "tanh", "stack", "sub", "mul", "add", "reshape", "transpose", "slice_by_index"}
PACKAGE = "nlmf_mixed{tile}.mlpackage"
PREFIX = "nlmf_mixed{tile}_coreml_"
CONVERSION_DATE = "2026-10-18"
# The checkpoint's pickled configuration classes (omegaconf), allowed when loading.
ALLOWED_GLOBALS = {
    "omegaconf.listconfig.ListConfig", "omegaconf.nodes.AnyNode", "omegaconf.base.ContainerMetadata",
    "omegaconf.base.Metadata", "builtins.list", "builtins.dict", "builtins.int", "collections.defaultdict", "typing.Any",
}


def fetch(url: str, dest: Path) -> Path:
    dest.parent.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(url) as response:
        dest.write_bytes(response.read())
    return dest


def load_network(work: Path):
    """The authors' network with its weights, sampling through RapidRAW's
    static packed sampler (traceable, no custom operators)."""
    root = work / "rapidraw_denoise"
    for name in SOURCES:
        fetch(RAPIDRAW + name, root / name)
    (root / "__init__.py").touch()
    (root / "vendor" / "__init__.py").touch()
    ckpt = fetch(CHECKPOINT["url"], work / "rawnoise_25_15_9nbr.ckpt")
    if hashlib.sha256(ckpt.read_bytes()).hexdigest() != CHECKPOINT["sha256"]:
        raise SystemExit("checkpoint SHA-256 differs from the pinned one")
    names = set(torch.serialization.get_unsafe_globals_in_checkpoint(str(ckpt)))
    if not names <= ALLOWED_GLOBALS:
        raise SystemExit(f"unexpected pickled globals: {names - ALLOWED_GLOBALS}")
    objects = [getattr(importlib.import_module(n.rsplit(".", 1)[0]), n.rsplit(".", 1)[1]) for n in names]
    with torch.serialization.safe_globals(objects):
        state = torch.load(ckpt, map_location="cpu", weights_only=True)["state_dict"]
    state = {k.removeprefix("network.").removeprefix("model."): v for k, v in state.items()}
    sys.path.insert(0, str(work))
    from rapidraw_denoise.static_packed_sampler import packed_deform_neighbourhood
    from rapidraw_denoise.vendor.nonlocalmf import network

    model = network.SimpleBlockMatchingUNet(input_channels=8, output_channels=4, n_features=32)
    model.load_state_dict(state, strict=True)
    network.deform_neighbourhood = packed_deform_neighbourhood
    return model.eval().float()


def convert(out: Path, model, tile: int) -> Path:
    import coremltools as ct

    out.mkdir(parents=True, exist_ok=True)
    probe = torch.from_numpy(np.random.default_rng(0).random((1, 8, tile, tile), dtype=np.float32) * 0.1)
    with torch.no_grad():
        traced = torch.jit.trace(model, probe, strict=False, check_trace=False)
        drift = float((traced(probe) - model(probe)).abs().max())
    if drift > 1e-5:
        raise SystemExit(f"trace differs from the network by {drift}")
    mlmodel = ct.convert(
        traced,
        inputs=[ct.TensorType(name="raw_with_noise", shape=(1, 8, tile, tile), dtype=np.float32)],
        outputs=[ct.TensorType(name="denoised_raw", dtype=np.float32)],
        convert_to="mlprogram",
        compute_precision=ct.transform.FP16ComputePrecision(op_selector=lambda op: op.op_type not in KEEP),
        minimum_deployment_target=ct.target.macOS15,
    )
    mlmodel.short_description = (
        "Nonlocal MatchFilter rawnoise_25_15_9nbr (Sánchez-Beeckman and Buades, MIT): Bayer mosaic denoising"
    )
    mlmodel.license = "MIT"
    package = out / PACKAGE.format(tile=tile)
    if package.exists():
        shutil.rmtree(package)
    mlmodel.save(str(package))
    fix_manifest(package / "Manifest.json")
    sort_spec(package / MEMBERS["model.mlmodel"], CONVERSION_DATE)
    for name, member in MEMBERS.items():
        shutil.copyfile(package / member, out / (PREFIX.format(tile=tile) + name))
    return package


def check(package: Path, model, tile: int) -> None:
    """A smooth scene with Poisson-Gaussian noise as the app packs it."""
    import coremltools as ct

    rng = np.random.default_rng(1)
    y, x = np.mgrid[0:tile, 0:tile] / tile
    clean = np.stack([0.05 + 0.3 * (0.5 + 0.5 * np.sin(9 * x + c) * np.cos(7 * y - c)) for c in range(4)])
    shot, read = 1.2e-3, 4e-6
    std = np.sqrt(shot * clean + read)
    noisy = clean + std * rng.standard_normal(clean.shape)
    inp = np.concatenate([noisy, std])[None].astype(np.float32)
    with torch.no_grad():
        ref = model(torch.from_numpy(inp)).numpy()[0]
    got = ct.models.MLModel(str(package), compute_units=ct.ComputeUnit.CPU_AND_GPU).predict({"raw_with_noise": inp})
    diff = (got["denoised_raw"][0] - ref) / std
    core = diff[:, 40:-40, 40:-40]
    print(f"package vs float32 network: RMS {np.sqrt(np.mean(core ** 2)):.4f} σ, max {np.abs(core).max():.3f} σ")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", type=Path)
    parser.add_argument("--tile", type=int, default=640)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as work:
        model = load_network(Path(work))
        package = convert(args.out, model, args.tile)
        print(package)
        if args.check:
            check(package, model, args.tile)


if __name__ == "__main__":
    main()
