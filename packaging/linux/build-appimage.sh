#!/bin/bash
# Builds the release AppImage (with ONNX Runtime) in the Ubuntu 22.04 container.
# Run from the repository root on an x86-64 Linux host with Docker:
#   packaging/linux/build-appimage.sh [CACHE_DIR]
# The AppImage lands in CACHE_DIR/target/release/bundle/appimage/ (default
# CACHE_DIR: ../rawtonic-linux-cache). Extra `tauri build` arguments can be
# passed in TAURI_ARGS (split on spaces), e.g. `--features qa-control`.
set -euo pipefail
if [ "${1:-}" != "--inside" ]; then
  cache=$(realpath -m "${1:-../rawtonic-linux-cache}")
  mkdir -p "$cache"
  docker build -t rawtonic-linux-build --build-arg UID="$(id -u)" packaging/linux
  # linuxdeploy runs extracted (APPIMAGE_EXTRACT_AND_RUN), FUSE only if present.
  fuse=()
  [ -e /dev/fuse ] && fuse=(--device /dev/fuse --cap-add SYS_ADMIN)
  exec docker run --rm -v "$PWD":/src -v "$cache":/cache \
    -e TAURI_ARGS="${TAURI_ARGS:-}" -e VITE_RAWTONIC_QA="${VITE_RAWTONIC_QA:-}" \
    "${fuse[@]}" rawtonic-linux-build bash packaging/linux/build-appimage.sh --inside
fi
export CARGO_HOME=/cache/cargo CARGO_TARGET_DIR=/cache/target npm_config_cache=/cache/npm
export PATH=$CARGO_HOME/bin:/home/builder/.cargo/bin:$PATH RUSTUP_HOME=/home/builder/.rustup
# linuxdeploy is itself an AppImage; containers have no FUSE mount.
export APPIMAGE_EXTRACT_AND_RUN=1 CI=true RAWTONIC_BUNDLE_ONNX=1
npm ci
# shellcheck disable=SC2086
npm run tauri -- build --bundles appimage $TAURI_ARGS -- --locked
ls -la /cache/target/release/bundle/appimage/
