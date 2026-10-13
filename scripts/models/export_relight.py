#!/usr/bin/env python3
"""Export DA3 Base's relative depth and camera for cached Relight analysis.

Apache-2.0 code/weights from ByteDance-Seed/Depth-Anything-3 and depth-anything.
Use the pinned environment in build_model_pack.py. One image, no square padding:
float32 ImageNet-normalized sRGB [1,1,3,H,W], multiples of 14, longest side 518.
Outputs depth/confidence [1,1,H,W], intrinsics [1,1,3,3]. IR9/opset20.

The accompanying patch preserves dynamic positional/UV interpolation. Its
output was compared with unmodified upstream Torch across ten aspect sizes,
including the exact positional-embedding size, on ONNX Runtime 1.23.2.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.request

SOURCE = "3d835ec1a5802d64a8b8b15f817a1ab54809bfe4"
REVISION = "f4a6c9b3c95e41c82048423d3493a81ec3fa810e"
WEIGHTS_SHA = "e01067dc1659613083d9145a9a2547ccdbe6ccbbf83c4fe7b3e8a4e2bdae78b5"
SHA256 = "070a4ff34ed406d057044daf9b84fd6287e5a7cc66134060acf2ccfe5d5dea23"
CONFIG_SHA = "5e34115ebc17bd2d8d43033c5f72e9446ac8833fd61d3fa160b7e67e0bb5b7b5"
FILENAME = "relight_geometry_da3_base.onnx"

def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()

def fetch(root, name, sha):
    path = root / name
    if not path.exists():
        url = f"https://huggingface.co/depth-anything/DA3-BASE/resolve/{REVISION}/{name}"
        part = path.with_suffix(".partial")
        urllib.request.urlretrieve(url, part)
        if digest(part) != sha:
            raise ValueError(f"{name}: upstream hash mismatch")
        part.replace(path)
    if digest(path) != sha:
        raise ValueError(f"{name}: cached hash mismatch")
    return path

def export(root):
    root = Path(root).resolve()
    root.mkdir(parents=True, exist_ok=True)
    output = root / FILENAME
    if output.exists():
        if digest(output) != SHA256:
            raise ValueError("Cached Relight export differs from the qualified artifact")
        return output
    source = root / "source"
    if not source.exists():
        subprocess.run(["git", "clone", "--filter=blob:none", "--no-checkout",
                        "https://github.com/ByteDance-Seed/Depth-Anything-3.git", str(source)], check=True)
        subprocess.run(["git", "-C", str(source), "checkout", "--detach", SOURCE], check=True)
    head = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    if head != SOURCE:
        raise ValueError("Relight source revision mismatch")
    patch = Path(__file__).with_name("relight_da3_dynamic.patch").resolve()
    reverse = subprocess.run(["git", "-C", str(source), "apply", "--reverse", "--check", str(patch)], capture_output=True)
    if reverse.returncode:
        subprocess.run(["git", "-C", str(source), "apply", str(patch)], check=True)
    config = fetch(root, "config.json", CONFIG_SHA)
    weights_path = fetch(root, "model.safetensors", WEIGHTS_SHA)
    sys.path.insert(0, str(source / "src"))
    import torch
    from safetensors.torch import load_file
    from omegaconf import OmegaConf
    from depth_anything_3.cfg import create_object
    torch.set_num_threads(4)
    model = create_object(OmegaConf.create(json.loads(config.read_text())["config"]))
    weights = {k.removeprefix("model."): v for k, v in load_file(weights_path).items()}
    # Upstream shares LayerNorm across four auxiliary levels; safetensors
    # stores aliases once. Restore these same-object parameters before load.
    for i in [1, 2, 3]:
        for kind in ["weight", "bias"]:
            weights[f"head.scratch.output_conv2_aux.{i}.2.{kind}"] = weights[f"head.scratch.output_conv2_aux.0.2.{kind}"]
    model.load_state_dict(weights, strict=True)
    model.eval()
    class Export(torch.nn.Module):
        def __init__(self, m):
            super().__init__()
            self.m = m
        def forward(self, x):
            out = self.m(x)
            return out["depth"], out["depth_conf"], out["intrinsics"]
    part = root / "relight.partial.onnx"
    torch.onnx.export(Export(model), (torch.randn(1, 1, 3, 392, 518),), str(part),
                      input_names=["image"], output_names=["depth", "confidence", "intrinsics"],
                      opset_version=20, dynamo=False,
                      dynamic_axes={"image": {3: "height", 4: "width"},
                                    "depth": {2: "height", 3: "width"},
                                    "confidence": {2: "height", 3: "width"}})
    if digest(part) != SHA256:
        raise ValueError("Rebuilt Relight export differs: requalify it before changing the pack hash")
    os.replace(part, output)
    return output

if __name__ == "__main__":
    print(export(Path(sys.argv[1])))
