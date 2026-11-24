#!/bin/bash
# Builds the Flatpak in an Arch Linux container (Flatpak and flatpak-builder from
# pacman; privileged, for the sandbox's user namespaces), installs it, exports
# one RAW headless through the sandbox and starts the GUI under Xvfb.
# Run from the repository root (needs Docker and network for Flathub):
#   packaging/linux/flatpak/test-in-container.sh PHOTOS_DIR OUT_DIR [CACHE_DIR]
# CACHE_DIR (optional) keeps the Flatpak runtimes between runs.
set -euo pipefail
photos=$(realpath "$1"); out=$(realpath -m "$2"); cache=$(realpath -m "${3:-$out/cache}")
mkdir -p "$out" "$cache"
# A fresh folder per run: nothing an earlier run left is replaced.
out=$(mktemp -d "$out/run-$(date +%Y%m%d-%H%M%S)-XXXX")
here=$(cd "$(dirname "$0")/.." && pwd)
exec docker run --rm --privileged --network host \
  -v "$here":/pkg:ro -v "$photos":/in:ro -v "$out":/out -v "$cache":/cache \
  archlinux:latest bash /pkg/flatpak/test-inside.sh
