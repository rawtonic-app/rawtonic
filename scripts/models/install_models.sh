#!/bin/sh
# Install Rawtonic's model pack (github.com/rawtonic-app/models) ahead with
# `gh`, for example for QA builds. The app itself downloads each model the
# first time a tool needs it.
#
# Usage: scripts/models/install_models.sh [--runtime] [--cuda] [APP_IDENTIFIER]
#   APP_IDENTIFIER  cc.rawtonic.editor (default) or cc.rawtonic.qa
#   --runtime       also put this platform's ONNX Runtime library (on Windows
#                   and Linux with its WebGPU plugin) into src-tauri/resources/
#                   for the opt-in RAWTONIC_BUNDLE_ONNX=1 build
#   --cuda          also install NVIDIA's CUDA plugin (Windows and Linux x86-64,
#                   about 190-300 MB) into the model folder; it needs your own
#                   CUDA 13 and cuDNN 9 (docs/decisions/0011-gpu-ai.md)
#
# Each file is checked against models.json before it replaces an installed one.
# On macOS the Core ML packages are put together too, so the `rawtonic` CLI
# can use the folder (--models).
set -eu

REPO=rawtonic-app/models
TAG=pack-2026-10-27
RUNTIME=0
CUDA=0
while [ $# -gt 0 ]; do
  case "$1" in
    --runtime) RUNTIME=1; shift ;;
    --cuda) CUDA=1; shift ;;
    *) break ;;
  esac
done
APP_ID=${1:-cc.rawtonic.editor}

case "$(uname -s)" in
  Darwin) MODELS="$HOME/Library/Application Support/$APP_ID/models" ;;
  Linux) MODELS="${XDG_DATA_HOME:-$HOME/.local/share}/$APP_ID/models" ;;
  MINGW*|MSYS*|CYGWIN*) MODELS="$APPDATA/$APP_ID/models" ;;
  *) echo "unsupported system: $(uname -s)" >&2; exit 1 ;;
esac
# Runtime files: "pack name:name in resources" (Windows and Linux: ONNX
# Runtime 1.30.0 and the WebGPU plugin; macOS: 1.23.2).
CUDA_LIB=
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) LIBS="libonnxruntime-1.23.2-macos-aarch64.dylib:libonnxruntime.dylib" ;;
  Darwin-x86_64) LIBS="libonnxruntime-1.23.2-macos-x86_64.dylib:libonnxruntime.dylib" ;;
  Linux-x86_64)
    LIBS="libonnxruntime-1.30.0-linux-x86_64.so:libonnxruntime.so libonnxruntime_providers_webgpu-0.4.0-linux-x86_64.so:libonnxruntime_providers_webgpu.so"
    CUDA_LIB="libonnxruntime_providers_cuda-0.2.0-cuda13-linux-x86_64.so:libonnxruntime_providers_cuda.so" ;;
  Linux-aarch64)
    LIBS="libonnxruntime-1.30.0-linux-aarch64.so:libonnxruntime.so libonnxruntime_providers_webgpu-0.4.0-linux-aarch64.so:libonnxruntime_providers_webgpu.so" ;;
  *-x86_64)
    LIBS="onnxruntime-1.30.0-windows-x86_64.dll:onnxruntime.dll onnxruntime_providers_webgpu-0.4.0-windows-x86_64.dll:onnxruntime_providers_webgpu.dll"
    CUDA_LIB="onnxruntime_providers_cuda-0.2.0-cuda13-windows-x86_64.dll:onnxruntime_providers_cuda.dll" ;;
  *-aarch64|*-arm64)
    LIBS="onnxruntime-1.30.0-windows-aarch64.dll:onnxruntime.dll onnxruntime_providers_webgpu-0.4.0-windows-aarch64.dll:onnxruntime_providers_webgpu.dll" ;;
esac
if [ "$CUDA" = 1 ] && [ -z "$CUDA_LIB" ]; then
  echo "no CUDA plugin for $(uname -s)-$(uname -m)" >&2; exit 1
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
gh release download "$TAG" --repo "$REPO" --dir "$WORK" --pattern '*.onnx' --pattern 'siglip2_tags.bin' \
  --pattern 'siglip2_tokenizer.json' --pattern 'nlmf_coreml_*' --pattern 'nlmf_mixed640_macos13_coreml_*' --pattern 'birefnet_fp16_coreml_*' --pattern 'skyseg_coreml_*' --pattern 'moebius_unet_fp16_coreml_*' --pattern 'sdxl_vae_*_fp16_coreml_*' --pattern moebius_embeddings.bin --pattern models.json
if [ "$RUNTIME" = 1 ]; then
  for pair in $LIBS; do
    gh release download "$TAG" --repo "$REPO" --dir "$WORK" --pattern "${pair%%:*}"
  done
fi
if [ "$CUDA" = 1 ]; then
  gh release download "$TAG" --repo "$REPO" --dir "$WORK" --pattern "${CUDA_LIB%%:*}"
fi

# Verify every downloaded file against models.json; one it does not list is
# refused, not installed unchecked.
python3 - "$WORK" <<'PY'
import hashlib, json, os, sys
work = sys.argv[1]
expected = {e["file"]: e["sha256"] for e in json.load(open(os.path.join(work, "models.json")))["files"]}
for name in sorted(os.listdir(work)):
    if name == "models.json":
        continue
    if name not in expected:
        sys.exit(f"{name}: not listed in models.json; not installed")
    digest = hashlib.sha256()
    with open(os.path.join(work, name), "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    if digest.hexdigest() != expected[name]:
        sys.exit(f"{name}: SHA-256 {digest.hexdigest()}, expected {expected[name]}")
PY

# Copy beside the target, then rename over it: an interrupted install leaves
# either the old or the new file.
install_file() {
  cp "$1" "$2/.$3.install"
  mv -f "$2/.$3.install" "$2/$3"
  echo "installed $2/$3"
}

mkdir -p "$MODELS"
for file in "$WORK"/*.onnx "$WORK"/siglip2_tags.bin "$WORK"/siglip2_tokenizer.json "$WORK"/nlmf_coreml_* "$WORK"/nlmf_mixed640_macos13_coreml_* "$WORK"/birefnet_fp16_coreml_* "$WORK"/skyseg_coreml_* "$WORK"/moebius_unet_fp16_coreml_* "$WORK"/sdxl_vae_*_fp16_coreml_* "$WORK"/moebius_embeddings.bin; do
  [ -e "$file" ] || continue
  install_file "$file" "$MODELS" "$(basename "$file")"
done
# macOS: put the Core ML packages back together from their three files, as
# the app does on first use (the `rawtonic` CLI loads them as they are and
# downloads nothing). "file prefix:package".
if [ "$(uname -s)" = Darwin ]; then
  for pair in nlmf_coreml_:nlmf nlmf_mixed640_macos13_coreml_:nlmf_mixed640 birefnet_fp16_coreml_:birefnet_fp16 skyseg_coreml_:skyseg_upernet moebius_unet_fp16_coreml_:moebius_unet_fp16 sdxl_vae_encoder_fp16_coreml_:sdxl_vae_encoder_fp16 sdxl_vae_decoder_fp16_coreml_:sdxl_vae_decoder_fp16; do
    prefix=${pair%%:*}; package="$MODELS/${pair##*:}.mlpackage"
    [ -e "$MODELS/${prefix}weight.bin" ] || continue
    changed=0
    for part in Manifest.json: model.mlmodel:Data/com.apple.CoreML weight.bin:Data/com.apple.CoreML/weights; do
      name=${part%%:*}; sub=${part#*:}; dir="$package${sub:+/$sub}"
      cmp -s "$MODELS/$prefix$name" "$dir/$name" && continue
      mkdir -p "$dir"
      install_file "$MODELS/$prefix$name" "$dir" "$name"
      changed=1
    done
    # A changed package is compiled again on first use.
    [ "$changed" = 0 ] || rm -rf "${package%.mlpackage}.mlmodelc"
  done
fi
if [ "$CUDA" = 1 ]; then
  install_file "$WORK/${CUDA_LIB%%:*}" "$MODELS" "${CUDA_LIB##*:}"
fi
if [ "$RUNTIME" = 1 ]; then
  RESOURCES="$(cd "$(dirname "$0")/../.." && pwd)/src-tauri/resources"
  mkdir -p "$RESOURCES"
  for pair in $LIBS; do
    install_file "$WORK/${pair%%:*}" "$RESOURCES" "${pair##*:}"
  done
fi
