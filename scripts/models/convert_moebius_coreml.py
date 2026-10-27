#!/usr/bin/env python3
"""Convert Moebius (hustvl, Apache-2.0) and the SDXL VAE (fp16-fix, MIT) into
the Core ML programs Remove runs on macOS for large holes.

Moebius is a 0.23 B distilled latent-diffusion inpainter; the checkpoint is
`ft_places2` of huggingface.co/hustvl/Moebius (revision and SHA-256 pinned),
the network code is the authors' `model_lib` (github.com/hustvl/Moebius,
pinned commit). Three ML programs, fp16, macOS 13 like the app:
- moebius_unet: latent_in float32 [2, 9, 64, 64] (the noisy latent, the
  hole at 64 x 64, the latent of the photo with the hole blanked; batch:
  unconditional then conditional), t float32 [1] (the DDIM timestep),
  embeddings float32 [2, 10, 3072] -> noise float32 [2, 4, 64, 64].
  The 20 learned embeddings that condition the network (10 unconditional,
  10 conditional for classifier-free guidance) are an input, not baked in:
  as constants coremltools folds the cross-λ layers' key and value path,
  and Core ML's GPU then computes those layers wrongly (14 % error after the
  first down block, objects painted back into the hole; every op of it is
  exact on its own; notes: benchmark-results/moebius-coreml-2026-10-27). The
  embeddings ship beside the program as moebius_embeddings.bin (float32
  little-endian, [2, 10, 3072]).
- sdxl_vae_encoder: image float32 [2, 3, 512, 512] in -1..1 -> latent
  float32 [2, 4, 64, 64] (the posterior's mean x the scaling factor 0.13025);
  the photo and the photo with the hole blanked in one call.
- sdxl_vae_decoder: latent float32 [1, 4, 64, 64] -> image float32
  [1, 3, 512, 512] in -1..1.
The VAE is madebyollin/sdxl-vae-fp16-fix (MIT; the SDXL VAE fine-tuned to
run in fp16), whose fp16 cast the Moebius authors ship as PixelHacker's VAE.

Writes OUT_DIR/<name>.mlpackage and, for the model pack, every package's
three files as flat assets (<prefix>Manifest.json, <prefix>model.mlmodel,
<prefix>weight.bin; the app puts each package back together, as for
BiRefNet) plus moebius_embeddings.bin; the bytes repeat in every conversion
(coreml_package.py).

Usage (macOS, uv):
  uv run --python 3.12 --with torch==2.7.0 --with coremltools==9.0 \\
    --with diffusers==0.38.0 --with numpy==2.2.6 --with einops==0.8.1 \\
    --with timm==1.0.30 --with pyyaml==6.0.3 --with safetensors==0.8.0 --with pillow==12.0.0 \\
    python scripts/models/convert_moebius_coreml.py OUT_DIR [--cache DIR] [--check IMAGE MASK ...]
  --onnx also writes the three networks as ONNX (opset 17, fp32, same inputs
  and outputs; moebius_unet.onnx, sdxl_vae_encoder.onnx, sdxl_vae_decoder.onnx)
  and checks one run of each on ONNX Runtime's CPU against PyTorch (needs
  onnx and onnxruntime): the route for Windows and Linux on the WebGPU or
  CUDA provider (docs/MODELS.md, "Moebius on other platforms"); the app does
  not use them yet.
  --check runs the app's removal (moebius_pipeline below, the same steps as
  src-tauri/src/moebius.rs) on each image and mask (any size; scaled to
  512 x 512) with the Core ML programs on the GPU and with the PyTorch fp32
  networks, and prints how far the fills differ in 8-bit codes.
"""
import argparse
import hashlib
import io
import shutil
import sys
import tarfile
import time
import types
import urllib.request
from pathlib import Path

import numpy as np
import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from coreml_package import MEMBERS, fix_manifest, sort_spec  # noqa: E402

MOEBIUS_COMMIT = "b88d462bacb9af6e7128a3b4cc4a07418bedfd61"
MOEBIUS_SOURCE = f"https://codeload.github.com/hustvl/Moebius/tar.gz/{MOEBIUS_COMMIT}"
CHECKPOINT = {
    "url": "https://huggingface.co/hustvl/Moebius/resolve/cd01f47fb648219d3fa605806c5ce00b713faa5e/ft_places2/diffusion_pytorch_model.bin",
    "sha256": "6525afb888e55f9b5c74fa0a5d19ca0762d720d6c716fb0f8422fbeb6868a09a",
}
VAE_BASE = "https://huggingface.co/madebyollin/sdxl-vae-fp16-fix/resolve/207b116dae70ace3637169f1ddd2434b91b3a8cd/"
VAE_FILES = {
    "config.json": "f18b16fa4381c90ab44a3d7abd2e90afd05a2c31b8ca69998bc93c6453aeb7b7",
    "diffusion_pytorch_model.safetensors": "1b909373b28f2137098b0fd9dbc6f97f8410854f31f84ddc9fa04b077b0ace2c",
}
SCALING = 0.13025
EMBEDDINGS = "moebius_embeddings.bin"
PACKAGES = {
    "moebius_unet": "moebius_unet_fp16_coreml_",
    "sdxl_vae_encoder": "sdxl_vae_encoder_fp16_coreml_",
    "sdxl_vae_decoder": "sdxl_vae_decoder_fp16_coreml_",
}
CONVERSION_DATE = "2026-10-27"


def fetch(url: str, dest: Path, sha256: str | None = None) -> Path:
    if not dest.exists():
        dest.parent.mkdir(parents=True, exist_ok=True)
        part = dest.with_suffix(dest.suffix + ".part")
        with urllib.request.urlopen(url) as response, open(part, "wb") as out:
            shutil.copyfileobj(response, out, 1 << 20)
        part.rename(dest)
    if sha256:
        digest = hashlib.sha256()
        with open(dest, "rb") as f:
            for block in iter(lambda: f.read(1 << 20), b""):
                digest.update(block)
        if digest.hexdigest() != sha256:
            raise SystemExit(f"{dest.name}: SHA-256 {digest.hexdigest()}, expected {sha256}")
    return dest


def moebius_source(cache: Path) -> Path:
    """The authors' model_lib and model config at the pinned commit."""
    root = cache / f"Moebius-{MOEBIUS_COMMIT}"
    if not (root / "model_lib").is_dir():
        with urllib.request.urlopen(MOEBIUS_SOURCE) as response:
            data = response.read()
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
            wanted = [m for m in tar.getmembers()
                      if m.name.split("/", 2)[1:2] in (["model_lib"], ["config"], ["LICENSE"]) and (m.isfile() or m.isdir())]
            tar.extractall(cache, members=wanted, filter="data")
    return root


class Removal(torch.nn.Module):
    """The checkpoint's RemovalModel (removal/v1_2/removal_model.py): the
    learned embeddings and the UNet."""

    def __init__(self, diff_model, num_embeddings: int):
        super().__init__()
        self.embedding_layer = torch.nn.Embedding(num_embeddings, diff_model.config.encoder_hid_dim)
        self.diff_model = diff_model


def load_moebius(cache: Path) -> tuple[torch.nn.Module, torch.Tensor]:
    """The UNet and the embeddings in the pipeline's CFG order: the
    unconditional ids 10..19, then the conditional ids 0..9."""
    import yaml

    root = moebius_source(cache)
    # flash-linear-attention is used only by the PixelHacker teacher (unet_gla).
    gla = types.ModuleType("model_lib.nets.unet_gla")
    gla.UNet2DGLAConditionModel = None
    sys.modules["model_lib.nets.unet_gla"] = gla
    sys.path.insert(0, str(root))
    import model_lib

    config = yaml.safe_load((root / "config/model_cfg/moebius.yaml").read_text())
    cfg = dict(config["model"])
    model_type = cfg.pop("model_type")
    cfg["sample_size"] = config["data"]["image_size"] // config["vae"]["downsample_ratio"]
    cfg["num_embeddings"] = 20
    diff = getattr(model_lib, model_type)(**cfg)
    model = Removal(diff, 20)
    ckpt = fetch(CHECKPOINT["url"], cache / "moebius_ft_places2.bin", CHECKPOINT["sha256"])
    model.load_state_dict(torch.load(ckpt, map_location="cpu", weights_only=True), strict=True)
    model = model.eval().float()
    ids = torch.tensor([list(range(10, 20)), list(range(10))])
    with torch.no_grad():
        embeddings = model.embedding_layer(ids)
    return model.diff_model, embeddings


def load_vae(cache: Path):
    from diffusers.models import AutoencoderKL

    folder = cache / "sdxl-vae-fp16-fix"
    for name, sha in VAE_FILES.items():
        fetch(VAE_BASE + name, folder / name, sha)
    vae = AutoencoderKL.from_pretrained(folder).eval().float()
    if abs(vae.config.scaling_factor - SCALING) > 1e-9:
        raise SystemExit(f"VAE scaling factor {vae.config.scaling_factor}, expected {SCALING}")
    return vae


class Unet(torch.nn.Module):
    def __init__(self, diff):
        super().__init__()
        self.diff = diff

    def forward(self, latent_in, t, embeddings):
        return self.diff(latent_in, timestep=t, encoder_hidden_states=embeddings).sample


class Encoder(torch.nn.Module):
    def __init__(self, vae):
        super().__init__()
        self.vae = vae

    def forward(self, image):
        return self.vae.encode(image).latent_dist.mean * SCALING


class Decoder(torch.nn.Module):
    def __init__(self, vae):
        super().__init__()
        self.vae = vae

    def forward(self, latent):
        return self.vae.decode(latent / SCALING).sample


def networks(cache: Path) -> tuple[dict, torch.Tensor]:
    diff, embeddings = load_moebius(cache)
    vae = load_vae(cache)
    rng = np.random.default_rng(0)
    probe = lambda *shape: torch.from_numpy(rng.standard_normal(shape, dtype=np.float32))  # noqa: E731
    nets = {
        "moebius_unet": (Unet(diff).eval(), [("latent_in", probe(2, 9, 64, 64)), ("t", torch.tensor([500.0])),
                                             ("embeddings", embeddings)], "noise"),
        "sdxl_vae_encoder": (Encoder(vae).eval(), [("image", probe(2, 3, 512, 512).clamp(-1, 1))], "latent"),
        "sdxl_vae_decoder": (Decoder(vae).eval(), [("latent", probe(1, 4, 64, 64))], "image"),
    }
    return nets, embeddings


def convert(out: Path, cache: Path) -> dict[str, Path]:
    import coremltools as ct

    out.mkdir(parents=True, exist_ok=True)
    nets, embeddings = networks(cache)
    (out / EMBEDDINGS).write_bytes(embeddings.numpy().astype("<f4").tobytes())
    packages = {}
    for name, (net, inputs, output) in nets.items():
        args = tuple(x for _, x in inputs)
        with torch.no_grad():
            traced = torch.jit.trace(net, args, check_trace=False)
            drift = float((traced(*args) - net(*args)).abs().max())
        if drift > 1e-4:
            raise SystemExit(f"{name}: trace differs from the network by {drift}")
        mlmodel = ct.convert(
            traced,
            inputs=[ct.TensorType(name=n, shape=tuple(x.shape), dtype=np.float32) for n, x in inputs],
            outputs=[ct.TensorType(name=output, dtype=np.float32)],
            convert_to="mlprogram",
            compute_precision=ct.precision.FLOAT16,
            minimum_deployment_target=ct.target.macOS13,
        )
        if name == "moebius_unet":
            mlmodel.short_description = "Moebius ft_places2 (hustvl, Apache-2.0): object-removal latent diffusion UNet"
            mlmodel.license = "Apache-2.0"
        else:
            mlmodel.short_description = "SDXL VAE fp16-fix (madebyollin, MIT; Stability AI's SDXL VAE)"
            mlmodel.license = "MIT"
        package = out / f"{name}.mlpackage"
        if package.exists():
            shutil.rmtree(package)
        mlmodel.save(str(package))
        fix_manifest(package / "Manifest.json")
        sort_spec(package / MEMBERS["model.mlmodel"], CONVERSION_DATE)
        for asset, member in MEMBERS.items():
            shutil.copyfile(package / member, out / (PACKAGES[name] + asset))
        packages[name] = package
        print(f"{name}: {package}", flush=True)
    return packages


def export_onnx(out: Path, cache: Path) -> None:
    import onnxruntime as ort

    nets, _ = networks(cache)
    for name, (net, inputs, output) in nets.items():
        path = out / f"{name}.onnx"
        args = tuple(x for _, x in inputs)
        with torch.no_grad():
            ref = net(*args).numpy()
            torch.onnx.export(net, args, str(path), input_names=[n for n, _ in inputs], output_names=[output],
                              opset_version=17, dynamo=False)
        session = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
        got = session.run(None, {n: x.numpy() for n, x in inputs})[0]
        rel = float(np.linalg.norm(got - ref) / np.linalg.norm(ref))
        print(f"{path.name}: ONNX Runtime CPU vs PyTorch, relative error {rel:.1e}", flush=True)
        if rel > 1e-4:
            raise SystemExit(f"{path.name} differs from PyTorch")


def schedule() -> np.ndarray:
    """DDPM's scaled-linear betas (0.00085 .. 0.012, 1000 steps), cumulated."""
    betas = np.linspace(0.00085**0.5, 0.012**0.5, 1000, dtype=np.float64) ** 2
    return np.cumprod(1.0 - betas)


def start_noise(seed: int = 0) -> np.ndarray:
    """The app's starting noise (src-tauri/src/moebius.rs `start_noise`):
    standard normals from SplitMix64 and Box-Muller, 4 x 64 x 64 of them,
    then one per channel times the authors' offset 0.0357."""
    count = 4 * 64 * 64 + 4
    state = (np.uint64(seed) + np.arange(1, 2 * count + 1, dtype=np.uint64) * np.uint64(0x9E3779B97F4A7C15))
    with np.errstate(over="ignore"):
        z = state
        z = (z ^ (z >> np.uint64(30))) * np.uint64(0xBF58476D1CE4E5B9)
        z = (z ^ (z >> np.uint64(27))) * np.uint64(0x94D049BB133111EB)
        z = z ^ (z >> np.uint64(31))
    u = ((z >> np.uint64(11)).astype(np.float64) + 0.5) / float(1 << 53)
    normal = (np.sqrt(-2.0 * np.log(u[0::2])) * np.cos(2 * np.pi * u[1::2])).astype(np.float32)
    noise = normal[: 4 * 64 * 64].reshape(1, 4, 64, 64)
    return (noise + np.float32(0.0357) * normal[4 * 64 * 64 :].reshape(1, 4, 1, 1)).astype(np.float32)


def moebius_pipeline(image, hole, unet, encode, decode, steps=10, guidance=2.0, strength=0.99, noise=None):
    """The authors' removal pipeline as the app runs it: DDIM (eta 0) over
    `steps` leading timesteps starting at `strength`, classifier-free
    guidance, noise with the authors' offset 0.0357; the VAE posterior's
    mean instead of a sample. image float [512, 512, 3] in 0..1, hole bool
    [512, 512], noise [1, 4, 64, 64] (default: `start_noise`, the app's);
    returns uint8 [512, 512, 3]."""
    acp = schedule()
    x = (image.astype(np.float32) * 2 - 1).transpose(2, 0, 1)[None]
    mask = hole.astype(np.float32)[None, None]
    latents = encode(np.concatenate([x, x * (1 - mask)]))
    lat, masked = latents[:1], latents[1:]
    mask64 = mask[:, :, ::8, ::8]
    ratio = 1000 // steps
    ts = (np.arange(steps) * ratio)[::-1][steps - min(int(steps * strength), steps):]
    if noise is None:
        noise = start_noise()
    z = (np.sqrt(acp[ts[0]]) * lat + np.sqrt(1 - acp[ts[0]]) * noise).astype(np.float32)
    cond = np.concatenate([mask64, masked], 1)
    for t in ts:
        single = np.concatenate([z, cond], 1)
        eps = unet(np.concatenate([single, single]), np.array([t], np.float32))
        eps = eps[:1] + guidance * (eps[1:] - eps[:1])
        a, prev = acp[t], (acp[t - ratio] if t >= ratio else 1.0)
        x0 = (z - np.sqrt(1 - a) * eps) / np.sqrt(a)
        z = (np.sqrt(prev) * x0 + np.sqrt(1 - prev) * eps).astype(np.float32)
    out = decode(z)[0].transpose(1, 2, 0)
    return (np.clip((out + 1) / 2, 0, 1) * 255).round().astype(np.uint8)


def check(packages: dict[str, Path], cache: Path, pairs: list[tuple[Path, Path]]) -> None:
    import coremltools as ct
    from PIL import Image

    nets, embeddings = networks(cache)
    e = embeddings.numpy()

    def torch_fn(name):
        net = nets[name][0]
        return lambda *a: net(*map(torch.from_numpy, a)).detach().numpy()

    ref_unet, ref_enc, ref_dec = torch_fn("moebius_unet"), torch_fn("sdxl_vae_encoder"), torch_fn("sdxl_vae_decoder")
    models = {n: ct.models.MLModel(str(p), compute_units=ct.ComputeUnit.CPU_AND_GPU) for n, p in packages.items()}
    cm_unet = lambda x, t: models["moebius_unet"].predict({"latent_in": x, "t": t, "embeddings": e})["noise"]  # noqa: E731
    cm_enc = lambda i: models["sdxl_vae_encoder"].predict({"image": i})["latent"]  # noqa: E731
    cm_dec = lambda z: models["sdxl_vae_decoder"].predict({"latent": z})["image"]  # noqa: E731
    for image_path, mask_path in pairs:
        image = np.asarray(Image.open(image_path).convert("RGB").resize((512, 512), Image.Resampling.LANCZOS)) / 255.0
        hole = np.asarray(Image.open(mask_path).convert("L").resize((512, 512), Image.Resampling.BOX)) > 0
        with torch.no_grad():
            ref = moebius_pipeline(image, hole, lambda x, t: ref_unet(x, t, e), ref_enc, ref_dec)
        start = time.perf_counter()
        got = moebius_pipeline(image, hole, cm_unet, cm_enc, cm_dec)
        secs = time.perf_counter() - start
        d = np.abs(got.astype(int) - ref.astype(int))[hole]
        print(f"{image_path.name}: hole {hole.mean() * 100:.0f} %, |diff| in the hole mean {d.mean():.2f}, "
              f"p99 {np.percentile(d, 99):.0f} codes; Core ML {secs:.2f} s")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", type=Path)
    parser.add_argument("--cache", type=Path, default=Path("model-pack-cache") / "moebius")
    parser.add_argument("--check", type=Path, nargs="+", metavar="IMAGE MASK")
    parser.add_argument("--onnx", action="store_true")
    args = parser.parse_args()
    if args.check and len(args.check) % 2:
        parser.error("--check takes pairs of IMAGE MASK")
    packages = convert(args.out, args.cache)
    if args.onnx:
        export_onnx(args.out, args.cache)
    if args.check:
        check(packages, args.cache, list(zip(args.check[::2], args.check[1::2])))


if __name__ == "__main__":
    main()
