#!/bin/bash
# Builds packaging/linux/arch/PKGBUILD in a fresh Arch Linux container, installs
# it, checks the declared dependencies, exports one RAW headless, starts the GUI
# and removes the package again. Run from the repository root (needs Docker):
#   packaging/linux/arch/test-in-container.sh PHOTOS_DIR OUT_DIR [CACHE_DIR]
# PHOTOS_DIR: RAW files, mounted read-only (the first one is exported);
# OUT_DIR: receives the logs, the exported JPEG and a GUI screenshot;
# CACHE_DIR (optional): holds the downloaded AppImage so it is fetched once.
set -euo pipefail
photos=$(realpath "$1"); out=$(realpath -m "$2"); cache=$(realpath -m "${3:-$out/cache}")
mkdir -p "$out" "$cache"
# A fresh folder per run: nothing an earlier run left is replaced.
out=$(mktemp -d "$out/run-$(date +%Y%m%d-%H%M%S)-XXXX")
here=$(cd "$(dirname "$0")" && pwd)
exec docker run --rm --network host -e RELATIVE_EXPORT \
  -v "$here":/pkg:ro -v "$photos":/in:ro -v "$out":/out -v "$cache":/cache \
  archlinux:latest bash /pkg/test-inside.sh
