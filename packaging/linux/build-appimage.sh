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
# Update information for AppImageUpdate and Gear Lever, which the user runs
# (the app only links to releases; it never installs updates): the newest GitHub
# release, pre-releases included; appimagetool writes the matching .zsync
# beside the AppImage, to be attached to the release with it.
export LDAI_UPDATE_INFORMATION='gh-releases-zsync|rawtonic-app|rawtonic|latest-all|Rawtonic_*_amd64.AppImage.zsync'
npm ci
# The tether helper with libgphoto2 built from pinned sources (ADR 0012).
scripts/tether/bundle_helper.sh
# A bundle folder or .zsync left by an earlier run would be shipped as it is.
rm -rf "$CARGO_TARGET_DIR/release/bundle/appimage" src-tauri/*.AppImage.zsync
# linuxdeploy copies the helper's libraries from there into usr/lib.
# shellcheck disable=SC2086
LD_LIBRARY_PATH="$CARGO_TARGET_DIR/tether-bundle/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  npm run tauri -- build --bundles appimage --config "$CARGO_TARGET_DIR/tether-bundle/tauri.tether.conf.json" \
  $TAURI_ARGS -- --locked
bundle=/cache/target/release/bundle/appimage
# appimagetool writes the .zsync into its working directory (src-tauri).
# A release AppImage without it could not be updated.
mv src-tauri/*.AppImage.zsync "$bundle"/
ls -la "$bundle"
