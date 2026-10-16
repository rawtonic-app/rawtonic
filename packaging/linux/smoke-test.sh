#!/bin/bash
# Headless exports with a release AppImage inside a fresh distribution
# container (Ubuntu, Debian, Fedora): installs what a desktop would provide
# (GTK 3, Mesa's software Vulkan, Xvfb), exports every photograph in /in to
# JPEG and checks each export and that the inputs are unchanged.
#   docker run --rm -v DIR_WITH_APPIMAGE:/app:ro -v PHOTOS:/in:ro -v THIS_DIR:/test:ro \
#     ubuntu:24.04 bash /test/smoke-test.sh
# /app holds exactly one *.AppImage; /in the photographs and SHA256SUMS.
set -uo pipefail
if command -v apt-get >/dev/null; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq >/dev/null
  apt-get install -y -qq libgtk-3-0 mesa-vulkan-drivers libvulkan1 libegl1 libgl1 \
    ca-certificates xvfb xauth >/dev/null 2>&1
else
  dnf install -y -q gtk3 mesa-vulkan-drivers vulkan-loader mesa-libEGL mesa-libGL \
    libgpg-error xorg-x11-server-Xvfb xorg-x11-xauth >/dev/null 2>&1
fi
app=$(ls /app/*.AppImage)
work=$(mktemp -d)
export HOME=$work XDG_CONFIG_HOME=$work/config XDG_DATA_HOME=$work/data XDG_CACHE_HOME=$work/cache
failed=0
for photo in /in/*; do
  name=$(basename "$photo")
  [ "$name" = SHA256SUMS ] && continue
  out=$work/$name.jpg
  xvfb-run -a "$app" --appimage-extract-and-run export "$photo" --output "$out" --format jpeg \
    > "$work/$name.log" 2>&1
  code=$?
  size=$(stat -c %s "$out" 2>/dev/null || echo 0)
  # A JPEG starts with FF D8.
  magic=$(head -c 2 "$out" 2>/dev/null | od -An -tx1 | tr -d ' ')
  if [ "$code" -eq 0 ] && [ "$size" -gt 100000 ] && [ "$magic" = ffd8 ]; then
    echo "ok   $name -> $size bytes"
  else
    echo "FAIL $name: exit $code, $size bytes"; tail -5 "$work/$name.log"; failed=1
  fi
done
(cd /in && sha256sum -c --quiet SHA256SUMS) || { echo "FAIL inputs changed"; failed=1; }
exit $failed
