#!/usr/bin/env python3
"""Build Rawtonic's self-hosted AI model pack from the original upstream files.

Every file comes from its authors' own release (or is exported from their
checkpoint by the scripts/models/export_*.py scripts) and is checked against the
SHA-256 recorded here, so the pack can be rebuilt and audited. The output
directory holds the model files, the ONNX Runtime libraries the app's opt-in
build bundles, the license texts and `models.json` (file, size, SHA-256,
source, license), all at the top level: they are uploaded as the assets of a
release of github.com/rawtonic-app/models, and the app downloads
`<release URL>/<file>` and verifies the hash (docs/MODELS.md).

Usage:
  SAM2_BUILD_CUDA=0 uv run --python 3.12 --with torch==2.9.1 --with torchvision==0.24.1 \
      --with segment-anything==1.0 \
      --with "SAM-2 @ git+https://github.com/facebookresearch/sam2@2b90b9f5ceec907a1c18123530e92e794ad901a4" \
      --with transformers==4.57.3 --with sentencepiece \
      --with timm==1.0.30 --with kornia==0.8.3 --with einops==0.8.2 \
      --with onnx==1.23.2 --with onnxruntime==1.22.0 \
      python scripts/models/build_model_pack.py OUT_DIR [--cache CACHE_DIR]
The face models are converted in an environment of their own (TensorFlow,
tf2onnx), which `uv` sets up (export_faces.py).
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import urllib.request
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LICENSES = REPO / "licenses" / "models"

# Upstream files that go into the pack unchanged. `sha256` is the published
# file's hash; the pack's copy is byte-identical.
DIRECT = [
    {
        "file": "depth_anything_v2_vits.onnx",
        "url": "https://github.com/fabio-sim/Depth-Anything-ONNX/releases/download/v2.0.0/depth_anything_v2_vits.onnx",
        "sha256": "d2b11a11c1d4a12b47608fa65a17ee9a4c605b55ee1730c8e3b526304f2562be",
        "used_by": "Depth mask",
        "source": "Depth Anything V2 Small (github.com/DepthAnything/Depth-Anything-V2), ONNX export by fabio-sim",
        "license": "Apache-2.0 (Small only; Base/Large/Giant are CC BY-NC 4.0)",
        "license_files": ["Depth-Anything-V2-Apache-2.0.txt", "Depth-Anything-ONNX-Apache-2.0.txt"],
    },
    {
        "file": "migan_pipeline_v2.onnx",
        "url": "https://huggingface.co/andraniksargsyan/migan/resolve/406830d0fa60666da0071c342ad2fbc8f30c5c64/migan_pipeline_v2.onnx",
        "sha256": "6f1f3530a1a2324b19752018ce756088b07973cda8d7d890034ace5c8a48c40b",
        "used_by": "Object removal (inpainting)",
        "source": "MI-GAN 512 Places2 (Picsart AI Research, github.com/Picsart-AI-Research/MI-GAN), ONNX pipeline by its first author (huggingface.co/andraniksargsyan/migan, revision 406830d), unchanged",
        "license": "MIT (weights, LICENSE-WEIGHTS)",
        "license_files": ["MI-GAN-weights-MIT.txt"],
    },
]

# YuNet face detector (Shiqi Yu et al., OpenCV Zoo, MIT), unchanged; the
# Git LFS object of opencv/opencv_zoo at commit f12e127.
DIRECT.append({
    "file": "face_detection_yunet_2023mar.onnx",
    "url": "https://media.githubusercontent.com/media/opencv/opencv_zoo/f12e12798e8314f7c074a6656816c048dcc95b7a/models/face_detection_yunet/face_detection_yunet_2023mar.onnx",
    "sha256": "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
    "used_by": "People masks (face detection)",
    "source": "YuNet 2023mar (Wu, Peng and Yu; github.com/opencv/opencv_zoo, models/face_detection_yunet at f12e127), unchanged",
    "license": "MIT",
    "license_files": ["YuNet-MIT.txt"],
})

# DAViD Base surface normals of people (Microsoft, ICCV 2025), the rights
# holder's own ONNX file, unchanged. Weights and runtime code MIT (README
# "Model License" of github.com/microsoft/DAViD at 20a3eb6).
DIRECT.append({
    "file": "relight_people_normals_david_base.onnx",
    "url": "https://facesyntheticspubwedata.z6.web.core.windows.net/iccv-2025/models/normal-model-vitb16_384.onnx",
    "sha256": "3d37ecc812b6b50a09c514f3c8c8ad95da5cd6ae9b68c425bc52f1c9cd78c344",
    "used_by": "Relight surface normals of people",
    "source": "DAViD normal-model-vitb16_384 (Saleh, Hewitt et al., Microsoft; github.com/microsoft/DAViD at 20a3eb6), unchanged",
    "license": "MIT",
    "license_files": ["DAViD-MIT.txt"],
})

MATCHFILTER = "https://huggingface.co/sheldonxxxx/RapidRAW-Nonlocal-Denoise/resolve/af6bf36ca55591d103e465200efc4f13b7112a7b/"
MATCHFILTER_ENTRY = {
    "source": "Nonlocal MatchFilter (Sanchez-Beeckman and Buades, github.com/MIA-UIB/nonlocal-matchfilter, checkpoint rawnoise_25_15_9nbr), Core ML and ONNX conversions by sheldonxxxx/RapidRAW-Nonlocal-Denoise (revision af6bf36), unchanged",
    "license": "MIT",
    "license_files": ["nonlocal-matchfilter-MIT.txt", "nonlocal-matchfilter-NOTICE.txt"],
}
# The Core ML package is a folder; its three files go in as separate assets
# and the app puts them back together (nlmf.mlpackage).
for name, path, sha in (
    ("nlmf_coreml_Manifest.json", "coreml/packed.mlpackage/Manifest.json",
     "434aa032d34804d4de386d2e6aeee6a0cb1b37a2e244c5819e6163238141b9d3"),
    ("nlmf_coreml_model.mlmodel", "coreml/packed.mlpackage/Data/com.apple.CoreML/model.mlmodel",
     "92f31612c9f526395f7c6cad010d157e94663e77394c16fff755126fb176aa9b"),
    ("nlmf_coreml_weight.bin", "coreml/packed.mlpackage/Data/com.apple.CoreML/weights/weight.bin",
     "a5bf7b0b9d7efc24b3141f09b8c6fc7cadd61f46eea874a41326de8216a500a0"),
    ("nlmf.onnx", "onnx/model.onnx", "ba788a97f247058ea03d14bbf2f289adc8f4666039f27d0d530e6f67d23dd73c"),
):
    DIRECT.append({"file": name, "url": MATCHFILTER + path, "sha256": sha,
                   "used_by": "AI Denoise (raw Bayer mosaic)"} | MATCHFILTER_ENTRY)

SAM_CHECKPOINT = {
    "url": "https://dl.fbaipublicfiles.com/segment_anything/sam_vit_b_01ec64.pth",
    "sha256": "ec2df62732614e57411cdcf32a23ffdf28910380d03139ee0f4fcbe91eb8c912",
}
RELIGHT_SHA256 = "070a4ff34ed406d057044daf9b84fd6287e5a7cc66134060acf2ccfe5d5dea23"
RELIGHT_ENTRY = {
    "used_by": "Relight relative scene geometry and camera",
    "source": "DA3 Base (ByteDance-Seed/Depth-Anything-3 source 3d835ec1a5802d64a8b8b15f817a1ab54809bfe4; depth-anything/DA3-BASE revision f4a6c9b3c95e41c82048423d3493a81ec3fa810e), exported by scripts/models/export_relight.py",
    "license": "Apache-2.0",
    "license_files": ["Depth-Anything-3-Apache-2.0.txt"],
    "checkpoint_sha256": "e01067dc1659613083d9145a9a2547ccdbe6ccbbf83c4fe7b3e8a4e2bdae78b5",
}
RELIGHT_EXPORT = [
    "uv", "run", "--python", "3.12", "--with", "torch==2.14.1",
    "--with", "numpy==2.5.4", "--with", "onnx==1.23.2",
    "--with", "safetensors==0.8.0", "--with", "omegaconf==2.4.0",
    "--with", "einops==0.8.2", "--with", "addict==2.4.0",
    "python", str(Path(__file__).resolve().parent / "export_relight.py"),
]

SIGLIP_ENTRY = {
    "source": "SigLIP 2 B/16 (google/siglip2-base-patch16-224, revision 75de2d5), exported by scripts/models/export_siglip2.py",
    "license": "Apache-2.0",
    "license_files": ["siglip2-Apache-2.0.txt"],
}
SIGLIP_USE = {
    "siglip2_vision.onnx": "Automatic tags (image encoder)",
    "siglip2_tags.bin": "Automatic tags (text embeddings of the built-in tag list)",
    "siglip2_text.onnx": "Automatic tags (text encoder, only for the user's own tags)",
    "siglip2_tokenizer.json": "Automatic tags (tokenizer, only for the user's own tags)",
}

SKY_ENTRY = {
    "used_by": "Sky mask",
    "source": "UperNet + ConvNeXt-T on ADE20K (Meta, facebookresearch/ConvNeXt; transformers conversion openmmlab/upernet-convnext-tiny, revision 876ffc5), exported by scripts/models/export_sky.py",
    "license": "MIT",
    "license_files": ["ConvNeXt-MIT.txt"],
}
SKY_SHA256 = "e5dc2a0bee8b4cdfbd866d8f811b4390d04ba621644d99de25478338a5b15bbf"
# The same checkpoint as a Core ML program (macOS's fast path), converted by
# convert_sky_coreml.py in the BiRefNet Core ML environment (PYTHONHASHSEED=0;
# the converter numbers generated ops by position, so the bytes repeat).
SKY_COREML_ENTRY = SKY_ENTRY | {
    "used_by": "Sky mask (Core ML, macOS)",
    "source": SKY_ENTRY["source"].replace("export_sky.py", "convert_sky_coreml.py (Core ML, fp32)"),
}
SKY_COREML_SHA256 = {
    "skyseg_coreml_Manifest.json": "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
    "skyseg_coreml_model.mlmodel": "514793240a846abdee20271682172fc7da03c4642e5d9fff37f3edd18502aec5",
    "skyseg_coreml_weight.bin": "33bf57800ebd7ccd2005133e997dd064c8b59d0e7c8cac99d8a69728642206f3",
}

SUBJECT_ENTRY = {
    "used_by": "Subject and background masks",
    "source": "BiRefNet general checkpoint (Peng Zheng et al., github.com/ZhengPeng7/BiRefNet; huggingface.co/ZhengPeng7/BiRefNet, revision e2bf8e4), exported by scripts/models/export_birefnet.py",
    "license": "MIT",
    "license_files": ["BiRefNet-MIT.txt"],
}
SUBJECT_SHA256 = "1019aa4d150a10830d95d05b0e0e08ed2703c39d3e168dea3333bb0272c16cbf"
# The same checkpoint as a Core ML program (macOS's fast path), converted by
# convert_birefnet_coreml.py in an environment of its own (coremltools 9.0
# is tested with torch up to 2.7). fp16 since 2026-10-11: equal to fp32
# against subject references, a third of the time and of the memory a run
# holds (docs/MODELS.md). The fp32 files (birefnet_coreml_*, `--precision
# fp32`: Manifest 935b1ef0…, model 7cce117a…, weight 24a474c2…) stay in
# pack-2026-10-08 for earlier builds. The package's three files go in as
# separate assets; the app puts them back together.
SUBJECT_COREML_ENTRY = SUBJECT_ENTRY | {
    "used_by": "Subject and background masks (Core ML, macOS)",
    "source": SUBJECT_ENTRY["source"].replace("export_birefnet.py", "convert_birefnet_coreml.py (Core ML, fp16)"),
}
SUBJECT_COREML_SHA256 = {
    "birefnet_fp16_coreml_Manifest.json": "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
    "birefnet_fp16_coreml_model.mlmodel": "5ca548c32b1427581c1011b65b60027686b925dfbb218db80b3084cfef27c544",
    "birefnet_fp16_coreml_weight.bin": "3e890026789c6dbe487caff5d28dde5df6baea9e4e117fc8071b07d88b1259ae",
}
SUBJECT_COREML_EXPORT = [
    "uv", "run", "--python", "3.12", "--with", "torch==2.7.0", "--with", "torchvision==0.22.0",
    "--with", "transformers==4.57.3", "--with", "timm==1.0.30", "--with", "kornia==0.8.3",
    "--with", "einops==0.8.2", "--with", "onnx==1.23.2", "--with", "coremltools==9.0", "--with", "numpy==2.5.3",
    "python", str(Path(__file__).resolve().parent / "convert_birefnet_coreml.py"),
]

# AI Denoise version 2 on macOS: the same MatchFilter checkpoint converted by
# convert_nlmf_coreml.py from the authors' release (mixed precision, 640 px
# tiles; reproducible, same SHA-256 in two conversions). Prepared
# 2026-10-18; not yet uploaded to a pack release (the app falls back to
# nlmf_coreml_* without it).
MATCHFILTER_MIXED_ENTRY = {
    "used_by": "AI Denoise version 2 (raw Bayer mosaic, Core ML, macOS)",
    "source": "Nonlocal MatchFilter (Sanchez-Beeckman and Buades, github.com/MIA-UIB/nonlocal-matchfilter, release v1.0.0 checkpoint rawnoise_25_15_9nbr), converted by convert_nlmf_coreml.py (Core ML, convolutions fp16, matching fp32, 640 px tiles) with RapidRAW's vendored network and static packed sampler (sheldonxxxx/RapidRAW denoise/, commit 2375e17)",
    "license": "MIT",
    "license_files": ["nonlocal-matchfilter-MIT.txt", "nonlocal-matchfilter-NOTICE.txt"],
}
MATCHFILTER_MIXED_SHA256 = {
    "nlmf_mixed640_coreml_Manifest.json": "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
    "nlmf_mixed640_coreml_model.mlmodel": "a4da084674a43eb69f33e94e73c9454a993867ee5d5e38ed4afaec8dcda8d4f6",
    "nlmf_mixed640_coreml_weight.bin": "e50e91a84be8ca50f58566020eae4dc0bcd165e77c2c999bab418a4c492e3fe9",
}
MATCHFILTER_MIXED_EXPORT = [
    "uv", "run", "--python", "3.12", "--with", "torch==2.7.0", "--with", "coremltools==9.0",
    "--with", "numpy==2.2.6", "--with", "einops==0.8.1", "--with", "omegaconf==2.3.0",
    "python", str(Path(__file__).resolve().parent / "convert_nlmf_coreml.py"),
]

SAM_ENTRY = {
    "used_by": "Subject mask (point/box prompts; builds before 2026-10-09)",
    "source": "Segment Anything ViT-B checkpoint sam_vit_b_01ec64.pth (Meta), exported by scripts/models/export_sam.py",
    "license": "Apache-2.0",
    "license_files": ["segment-anything-Apache-2.0.txt"],
}

# SAM 2.1 Hiera-T (Meta): the checkpoint of facebookresearch/sam2's README
# ("The SAM 2 model checkpoints ... are licensed under Apache 2.0").
SAM2_CHECKPOINT = {
    "url": "https://dl.fbaipublicfiles.com/segment_anything_2/092824/sam2.1_hiera_tiny.pt",
    "sha256": "7402e0d864fa82708a20fbd15bc84245c2f26dff0eb43a4b5b93452deb34be69",
}
SAM2_ENTRY = {
    "used_by": "Click and box masks (point/box prompts)",
    "source": "SAM 2.1 Hiera-T checkpoint sam2.1_hiera_tiny.pt (Meta, github.com/facebookresearch/sam2 at 2b90b9f), exported by scripts/models/export_sam2.py",
    "license": "Apache-2.0",
    "license_files": ["sam2-Apache-2.0.txt"],
}
SAM2_SHA256 = {
    "sam2.1_tiny_encoder.onnx": "b6249b5dfb882d3eaf447d57273340005cd6efd9645c0004f86ab197a7170226",
    "sam2.1_tiny_decoder.onnx": "998c1cc053a2aad703fd2519fe890a1f80115b238a0da6d7b5fba70b210131d7",
}

# MediaPipe Face Mesh V2 and multiclass selfie segmentation (Google,
# Apache-2.0 by their model cards), converted by export_faces.py, which
# needs TensorFlow and tf2onnx in an environment of its own.
FACES_ENTRY = {
    "source": "MediaPipe (Google; storage.googleapis.com/mediapipe-models, version 1), converted by scripts/models/export_faces.py",
    "license": "Apache-2.0",
    "license_files": ["mediapipe-Apache-2.0.txt"],
}
FACES_USE = {
    "face_mesh_v2.onnx": "People masks (478 face landmarks: eyes, brows, lips, teeth)",
    "selfie_multiclass_256.onnx": "People masks (hair, face and body skin, clothes)",
}
FACES_SHA256 = {
    "face_mesh_v2.onnx": "5e82081f46411caa2b8a6734bd977c0120293f0debe29cf76cbc0c5703d48be3",
    "selfie_multiclass_256.onnx": "c949f945f549e3f865a19b67753694b928e061e40217ac38c0ad39daa8c4b194",
}
FACES_EXPORT = [
    "uv", "run", "--python", "3.11", "--with", "tensorflow==2.15.1", "--with", "tf2onnx==1.16.1",
    "--with", "numpy<2", "--with", "onnx==1.16.2", "--with", "onnxruntime==1.19.2",
    "python", str(Path(__file__).resolve().parent / "export_faces.py"),
]

# Microsoft's ONNX Runtime 1.22.0 release archives; the library inside each is
# the file src-tauri/build.rs pins (same hash).
ORT_BASE = "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/"
ORT = [
    ("onnxruntime-osx-arm64-1.22.0.tgz", "onnxruntime-osx-arm64-1.22.0/lib/libonnxruntime.1.22.0.dylib",
     "libonnxruntime-macos-aarch64.dylib", "2b885992d3d6fa4130d39ec84a80d7504ff52750027c547bb22c86165f19406a"),
    ("onnxruntime-osx-x86_64-1.22.0.tgz", "onnxruntime-osx-x86_64-1.22.0/lib/libonnxruntime.1.22.0.dylib",
     "libonnxruntime-macos-x86_64.dylib", "283e595e61cf65df7a6b1d59a1616cbd35c8b6399dd90d799d99b71a3ff83160"),
    ("onnxruntime-linux-x64-1.22.0.tgz", "onnxruntime-linux-x64-1.22.0/lib/libonnxruntime.so.1.22.0",
     "libonnxruntime-linux-x86_64.so", "3da6146e14e7b8aaec625dde11d6114c7457c87a5f93d744897da8781e35c673"),
    ("onnxruntime-linux-aarch64-1.22.0.tgz", "onnxruntime-linux-aarch64-1.22.0/lib/libonnxruntime.so.1.22.0",
     "libonnxruntime-linux-aarch64.so", "0afd69a0ae38c5099fd0e8604dda398ac43dee67cd9c6394b5142b19e82528de"),
    ("onnxruntime-win-x64-1.22.0.zip", "onnxruntime-win-x64-1.22.0/lib/onnxruntime.dll",
     "onnxruntime-windows-x86_64.dll", "579b636403983254346a5c1d80bd28f1519cd1e284cd204f8d4ff41f8d711559"),
    ("onnxruntime-win-arm64-1.22.0.zip", "onnxruntime-win-arm64-1.22.0/lib/onnxruntime.dll",
     "onnxruntime-windows-aarch64.dll", "79281671a386ed1baab9dbdbb09fe55f99577011472e9526cf9d0b468bb6bcc7"),
]
# macOS shutdown fixes; versioned filenames retain compatibility with old builds.
ORT123_BASE = "https://github.com/microsoft/onnxruntime/releases/download/v1.23.2/"
ORT123 = [
    ("onnxruntime-osx-arm64-1.23.2.tgz", "onnxruntime-osx-arm64-1.23.2/lib/libonnxruntime.1.23.2.dylib",
     "libonnxruntime-1.23.2-macos-aarch64.dylib", "d306d2bc768540766c7ed8a1e0ff05d2870c77a934ebeee4a7bafa1b732ef299"),
    ("onnxruntime-osx-x86_64-1.23.2.tgz", "onnxruntime-osx-x86_64-1.23.2/lib/libonnxruntime.1.23.2.dylib",
     "libonnxruntime-1.23.2-macos-x86_64.dylib", "8c9c78de65ea3786f987c0d980e9c1b13a3a5fbc6b3e2965ba05b450e6e4c054"),
]
# Windows and Linux: ONNX Runtime 1.30.0 (plugin execution providers need
# 1.24.4 or later) and Microsoft's WebGPU plugin 0.4.0, built from the same
# revision (docs/decisions/0011-gpu-ai.md). New file names, so builds that
# pin the 1.22.0 files keep working. The plugin comes from Microsoft's PyPI
# wheels (onnxruntime-ep-webgpu; the GitHub release has no binaries); the
# Windows wheel's DirectX Shader Compiler (dxcompiler.dll, dxil.dll:
# Microsoft Software License Terms) is not taken. (archive URL, archive
# SHA-256, member, pack name, member SHA-256)
ORT130_BASE = "https://github.com/microsoft/onnxruntime/releases/download/v1.30.0/"
WEBGPU_WHEELS = "https://files.pythonhosted.org/packages/"
ORT130 = [
    (ORT130_BASE + "onnxruntime-linux-x64-1.30.0.tgz", "a5ed5a3cac51fbb2e90da632ae43d19212faaa20e76484e62bcb7c23ddb3b3fd",
     "onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so.1.30.0",
     "libonnxruntime-1.30.0-linux-x86_64.so", "245a6f8c38127551057a1cd1ffd59f0a186a227ade4f3492dea2494eb565542e"),
    (ORT130_BASE + "onnxruntime-linux-aarch64-1.30.0.tgz", "e16a27a8ed330bbc698df7330b0cf56e722f354e3bcc92118682c74ef3c3e3da",
     "onnxruntime-linux-aarch64-1.30.0/lib/libonnxruntime.so.1.30.0",
     "libonnxruntime-1.30.0-linux-aarch64.so", "64e903a43a041240fd6bcffe0ac6d4fea47ef87bf24b9d097801bd00a9612a4b"),
    (ORT130_BASE + "onnxruntime-win-x64-1.30.0.zip", "c6ba983baf5681af108599675d2a89c2d145512d02de28aed0bff177cd0ba949",
     "onnxruntime-win-x64-1.30.0/lib/onnxruntime.dll",
     "onnxruntime-1.30.0-windows-x86_64.dll", "7e39e2bdbba836d98071ef28620735ba36a47c554cf794585269aecc50fab0da"),
    (ORT130_BASE + "onnxruntime-win-arm64-1.30.0.zip", "e53db8a50b23ae35be901cc93428baf997dc8d420333b097b2eae53d3ea9f2d3",
     "onnxruntime-win-arm64-1.30.0/lib/onnxruntime.dll",
     "onnxruntime-1.30.0-windows-aarch64.dll", "46b7ec24a6ee2932bb315edb39d26d6b2d51eef21f02c8e1ceafe801fe991a89"),
]
WEBGPU = [
    (WEBGPU_WHEELS + "c1/96/2a18a45079250afcd825687aef2895de266d5abc1cff9f52d2dede7598f2/onnxruntime_ep_webgpu-0.4.0-py3-none-manylinux_2_28_x86_64.whl",
     "5d8c961eda91a88961cad1fb7621f389f006b18497d6ea9ecf96f729f066cbfb",
     "onnxruntime_ep_webgpu/libonnxruntime_providers_webgpu.so",
     "libonnxruntime_providers_webgpu-0.4.0-linux-x86_64.so", "9f76e3dc4459421958ad086a9da03bf7e5de9289e9a3a56e28bbb459773dec81"),
    (WEBGPU_WHEELS + "4e/ca/00c70322c19913c81a6bb2239aca83781b97a818b55c2ec3d25cec72dc4c/onnxruntime_ep_webgpu-0.4.0-py3-none-manylinux_2_28_aarch64.whl",
     "17f660db53b1a509c63e721ac6784a2daa1c4e91a968523ee9986505ee5b0a55",
     "onnxruntime_ep_webgpu/libonnxruntime_providers_webgpu.so",
     "libonnxruntime_providers_webgpu-0.4.0-linux-aarch64.so", "720e1288e7e465f924db44b88a98a9d1c543eba3fbb2ff801cf2545f5e7f0279"),
    (WEBGPU_WHEELS + "d7/a4/c98a9e9433b3eeb576977b26c5c1cd0364f15ba9d198bb16101e7563ab06/onnxruntime_ep_webgpu-0.4.0-py3-none-win_amd64.whl",
     "7db646669d1a2390551da675115ea125e68cb3d4d2c3a2d6e463372bf8d6ea87",
     "onnxruntime_ep_webgpu/onnxruntime_providers_webgpu.dll",
     "onnxruntime_providers_webgpu-0.4.0-windows-x86_64.dll", "85d7b2065b1968b846cd00d5d87d5286d3c44e8f6c557cd382d96300e524b6cb"),
    (WEBGPU_WHEELS + "cd/35/e1b1b12e596354d05af43cea09cd9fbd8a24b6df3853c527307b5cfb8d4b/onnxruntime_ep_webgpu-0.4.0-py3-none-win_arm64.whl",
     "a9586f6744370776059edd794c11bca2f563258a5bd2707864aefff31d3af39f",
     "onnxruntime_ep_webgpu/onnxruntime_providers_webgpu.dll",
     "onnxruntime_providers_webgpu-0.4.0-windows-aarch64.dll", "e2a72894617c0519cae3080534ca1fc957c4ab607a993c05f326d83b322a581d"),
]
# NVIDIA GPUs, optional (install_models.sh --cuda; never bundled): Microsoft's
# CUDA plugin 0.2.0 (MIT, matched to 1.30.0) for CUDA 13. It needs the
# user's own CUDA 13 runtime, cuBLAS and cuDNN 9 (NVIDIA's licenses), which
# Rawtonic does not distribute.
CUDA_BASE = "https://github.com/microsoft/onnxruntime/releases/download/plugin-ep-cuda%2Fv0.2.0/"
CUDA = [
    (CUDA_BASE + "cuda_ep_cuda13_0.2.0_linux-x64.tar.gz", "5c4ede4442427bcfb6e5cdc3298b27ca32ec8174688c06d01a47b5632d66ba19",
     "libonnxruntime_providers_cuda.so",
     "libonnxruntime_providers_cuda-0.2.0-cuda13-linux-x86_64.so", "279d14b03d7ed7856d273326d4cde90489c51d41da2c88dc4eb2dda1ea946422"),
    (CUDA_BASE + "cuda_ep_cuda13_0.2.0_win-x64.zip", "e204c362a151e157cc56e047c76a3de54dd12c9a55d6ba3b1068686f4b47a489",
     "onnxruntime_providers_cuda.dll",
     "onnxruntime_providers_cuda-0.2.0-cuda13-windows-x86_64.dll", "ecbd0f4a75c172a63b93bf93cb4484b24aa7d488c99b957f55b86a77f195225f"),
]
ORT130_ENTRY = {
    "used_by": "Runtime for all models on Windows and Linux (bundled by the opt-in build, RAWTONIC_BUNDLE_ONNX=1)",
    "source": "ONNX Runtime 1.30.0 release (github.com/microsoft/onnxruntime)",
    "license": "MIT",
    "license_files": ["onnxruntime-MIT.txt", "onnxruntime-1.30.0-ThirdPartyNotices.txt"],
}
WEBGPU_ENTRY = {
    "used_by": "GPU for the AI models on Windows (Direct3D 12) and Linux (Vulkan), bundled beside the runtime",
    "source": "ONNX Runtime WebGPU plugin execution provider 0.4.0 (Microsoft; PyPI onnxruntime-ep-webgpu, built from ONNX Runtime 1.30.0), library unchanged",
    "license": "MIT (includes Dawn, BSD-3-Clause; notices as ONNX Runtime 1.30.0)",
    "license_files": ["onnxruntime-MIT.txt", "onnxruntime-1.30.0-ThirdPartyNotices.txt"],
}
CUDA_ENTRY = {
    "used_by": "NVIDIA GPUs for the AI models (optional; install_models.sh --cuda), with the user's CUDA 13 and cuDNN 9",
    "source": "ONNX Runtime CUDA plugin execution provider 0.2.0 (Microsoft, github.com/microsoft/onnxruntime release plugin-ep-cuda/v0.2.0), unchanged",
    "license": "MIT (CUDA and cuDNN are not included)",
    "license_files": ["onnxruntime-MIT.txt", "onnxruntime-1.30.0-ThirdPartyNotices.txt"],
}

ORT_ENTRY = {
    "used_by": "Runtime for all models (bundled by the opt-in build, RAWTONIC_BUNDLE_ONNX=1)",
    "source": "ONNX Runtime 1.22.0 release (github.com/microsoft/onnxruntime)",
    "license": "MIT",
    "license_files": ["onnxruntime-MIT.txt", "onnxruntime-ThirdPartyNotices.txt"],
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def fetch(url: str, expected: str, cache: Path, name: str | None = None) -> Path:
    target = cache / (name or url.rsplit("/", 1)[1])
    if not target.exists() or sha256(target) != expected:
        partial = target.with_suffix(target.suffix + ".part")
        with urllib.request.urlopen(url) as response, partial.open("wb") as out:
            shutil.copyfileobj(response, out)
        partial.replace(target)
    actual = sha256(target)
    if actual != expected:
        sys.exit(f"{url}: SHA-256 {actual}, expected {expected}")
    return target


def place(source: Path, out: Path, name: str, expected: str | None = None) -> dict:
    target = out / name
    if target.exists():
        sys.exit(f"{target} exists; build into an empty directory")
    shutil.copyfile(source, target)
    digest = sha256(target)
    if expected and digest != expected:
        sys.exit(f"{name}: SHA-256 {digest}, expected {expected}")
    return {"file": name, "size": target.stat().st_size, "sha256": digest}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out", type=Path)
    parser.add_argument("--cache", type=Path, default=Path("model-pack-cache"))
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    args.cache.mkdir(parents=True, exist_ok=True)
    entries = []

    for model in DIRECT:
        source = fetch(model["url"], model["sha256"], args.cache, model["file"])
        meta = {k: v for k, v in model.items() if k not in ("file", "sha256")}
        entries.append(place(source, args.out, model["file"], model["sha256"]) | meta)

    relight_dir = args.cache / "relight"
    subprocess.run(RELIGHT_EXPORT + [str(relight_dir)], check=True)
    relight_name = "relight_geometry_da3_base.onnx"
    entries.append(place(relight_dir / relight_name, args.out, relight_name, RELIGHT_SHA256) | RELIGHT_ENTRY)

    from export_sam import export  # needs torch; imported only when building

    checkpoint = fetch(SAM_CHECKPOINT["url"], SAM_CHECKPOINT["sha256"], args.cache)
    for path in export(checkpoint, args.cache / "sam"):
        entries.append(place(path, args.out, path.name) | SAM_ENTRY | {"checkpoint_sha256": SAM_CHECKPOINT["sha256"]})

    from export_sam2 import export as export_sam2

    checkpoint = fetch(SAM2_CHECKPOINT["url"], SAM2_CHECKPOINT["sha256"], args.cache)
    for path in export_sam2(checkpoint, "tiny", args.cache / "sam2"):
        entries.append(place(path, args.out, path.name, SAM2_SHA256[path.name]) | SAM2_ENTRY
                       | {"checkpoint_sha256": SAM2_CHECKPOINT["sha256"]})

    from export_siglip2 import export as export_siglip2

    for path in export_siglip2(args.cache / "siglip2"):
        entries.append(place(path, args.out, path.name) | {"used_by": SIGLIP_USE[path.name]} | SIGLIP_ENTRY)

    from export_sky import export as export_sky

    sky = export_sky(args.cache / "sky")
    entries.append(place(sky, args.out, sky.name, SKY_SHA256) | SKY_ENTRY)
    sky_coreml = args.cache / "sky-coreml"
    subprocess.run(SUBJECT_COREML_EXPORT[:-1] + [str(Path(__file__).resolve().parent / "convert_sky_coreml.py"),
                                                 str(sky_coreml)],
                   check=True, env=os.environ | {"PYTHONHASHSEED": "0"})
    for name, expected in SKY_COREML_SHA256.items():
        entries.append(place(sky_coreml / name, args.out, name, expected) | SKY_COREML_ENTRY)

    from export_birefnet import export as export_birefnet

    subject = export_birefnet(args.cache / "subject")
    entries.append(place(subject, args.out, subject.name, SUBJECT_SHA256) | SUBJECT_ENTRY)
    subject_coreml = args.cache / "subject-coreml"
    subprocess.run(SUBJECT_COREML_EXPORT + [str(subject_coreml), "--precision", "fp16"], check=True)
    for name, expected in SUBJECT_COREML_SHA256.items():
        entries.append(place(subject_coreml / name, args.out, name, expected) | SUBJECT_COREML_ENTRY)

    nlmf_mixed = args.cache / "nlmf-mixed"
    subprocess.run(MATCHFILTER_MIXED_EXPORT + [str(nlmf_mixed)], check=True)
    for name, expected in MATCHFILTER_MIXED_SHA256.items():
        entries.append(place(nlmf_mixed / name, args.out, name, expected) | MATCHFILTER_MIXED_ENTRY)

    faces = args.cache / "faces-out"
    subprocess.run(FACES_EXPORT + [str(faces), "--cache", str(args.cache / "faces")], check=True)
    for name, expected in FACES_SHA256.items():
        entries.append(place(faces / name, args.out, name, expected) | {"used_by": FACES_USE[name]} | FACES_ENTRY)

    for archive_name, member, name, expected in ORT + ORT123:
        base = ORT123_BASE if "1.23.2" in archive_name else ORT_BASE
        meta = ORT_ENTRY if base == ORT_BASE else ORT_ENTRY | {
            "source": "ONNX Runtime 1.23.2 release (github.com/microsoft/onnxruntime)",
            "license_files": ["onnxruntime-MIT.txt", "onnxruntime-1.23.2-ThirdPartyNotices.txt"],
        }
        archive = args.cache / archive_name
        if not archive.exists():
            fetch_url = base + archive_name
            with urllib.request.urlopen(fetch_url) as response, archive.open("wb") as out:
                shutil.copyfileobj(response, out)
        extracted = args.cache / name
        if archive_name.endswith(".zip"):
            with zipfile.ZipFile(archive) as bundle:
                extracted.write_bytes(bundle.read(member))
        else:
            with tarfile.open(archive) as bundle:
                extracted.write_bytes(bundle.extractfile(member).read())
        entries.append(place(extracted, args.out, name, expected) | {"url": base + archive_name} | meta)

    for files, meta in ((ORT130, ORT130_ENTRY), (WEBGPU, WEBGPU_ENTRY), (CUDA, CUDA_ENTRY)):
        for url, archive_sha, member, name, expected in files:
            archive = fetch(url, archive_sha, args.cache)
            extracted = args.cache / name
            if archive.name.endswith((".zip", ".whl")):
                with zipfile.ZipFile(archive) as bundle:
                    extracted.write_bytes(bundle.read(member))
            else:
                with tarfile.open(archive) as bundle:
                    extracted.write_bytes(bundle.extractfile(member).read())
            entries.append(place(extracted, args.out, name, expected) | {"url": url} | meta)

    licenses = args.out / "licenses"
    licenses.mkdir(exist_ok=True)
    for name in sorted({f for entry in entries for f in entry["license_files"]}):
        shutil.copyfile(LICENSES / name, licenses / name)
    (args.out / "models.json").write_text(json.dumps({"version": 1, "files": entries}, indent=2) + "\n")
    for entry in entries:
        print(f"{entry['sha256']}  {entry['size']:>11}  {entry['file']}")


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    main()
