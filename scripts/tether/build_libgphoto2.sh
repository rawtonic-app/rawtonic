#!/usr/bin/env bash
# Builds libgphoto2 for the tether helper from pinned, checksummed sources:
# libltdl (GNU libtool) and libgphoto2 with only what tethering needs — the
# PTP camera driver (ptp2: current Sony, Fujifilm, Canon, Nikon … bodies)
# and one port driver. No network port driver (ptpip), no serial or disk
# drivers, no translations, libexif, libjpeg, libxml or gd. All are
# LGPL-2.1-or-later and linked dynamically (THIRD_PARTY_NOTICES).
#
# Port driver: Linux uses libgphoto2's USB driver on libusb (also built
# here). macOS uses our ImageCaptureCore driver (crates/tether/macos/
# icc_port.m): macOS's camera service owns PTP cameras there, and
# ImageCaptureCore passes PTP through it instead of claiming the device —
# no libusb on macOS.
#
# macOS: built for the app's minimum macOS (Homebrew's bottles need the
# build machine's macOS). Linux: run inside the AppImage container.
#
# Usage: scripts/tether/build_libgphoto2.sh [PREFIX]   (default target/gphoto2)
# Sources are cached in target/gphoto2-src. The build is skipped when PREFIX
# already holds this exact build (its stamp file).
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
PREFIX=${1:-$REPO/target/gphoto2}
SRC=$REPO/target/gphoto2-src

# Same versions and checksums as Homebrew's formulae (cross-checked 2026-10-18).
LIBUSB=libusb-1.0.30
LIBUSB_URL=https://github.com/libusb/libusb/releases/download/v1.0.30/libusb-1.0.30.tar.bz2
LIBUSB_SHA=fea36f34f9156400209595e300840767ab1a385ede1dc7ee893015aea9c6dbaf
LIBTOOL=libtool-2.6.2
LIBTOOL_URL=https://ftpmirror.gnu.org/libtool/libtool-2.6.2.tar.xz
LIBTOOL_SHA=2ef1067c16c97db930fd740cc9bc3d3ba9a583804ae5ac42cc3e8719e49e191e
GPHOTO=libgphoto2-2.5.34
GPHOTO_URL=https://downloads.sourceforge.net/project/gphoto/libgphoto/2.5.34/libgphoto2-2.5.34.tar.bz2
GPHOTO_SHA=5df9e774eb4ab6087a193afff84dcafe9f666e3443fe29e73e6779f659ff7f28

ICC_PORT=$REPO/crates/tether/macos/icc_port.m
case "$(uname -s)" in
  Darwin)
    export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-13.0}
    STAMP="$LIBTOOL $GPHOTO ptp2 icc-$(shasum -a 256 "$ICC_PORT" | cut -c1-16) macos$MACOSX_DEPLOYMENT_TARGET"
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
    ;;
  *)
    STAMP="$LIBUSB $LIBTOOL $GPHOTO ptp2 usb1"
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
    ;;
esac

if [ -f "$PREFIX/.rawtonic-build" ] && [ "$(cat "$PREFIX/.rawtonic-build")" = "$STAMP" ]; then
  echo "libgphoto2: $PREFIX is up to date"
  exit 0
fi

# A different earlier build here: start clean (no stale drivers or libraries).
if [ -f "$PREFIX/.rawtonic-build" ]; then rm -rf "$PREFIX"; fi
mkdir -p "$SRC"
fetch() { # url sha file
  local file=$SRC/$3
  if [ ! -f "$file" ] || [ "$(sha256 "$file")" != "$2" ]; then
    curl -fsSL -o "$file.download" "$1"
    mv "$file.download" "$file"
  fi
  if [ "$(sha256 "$file")" != "$2" ]; then
    echo "libgphoto2: checksum mismatch for $3" >&2
    exit 1
  fi
}
[ "$(uname -s)" = Darwin ] || fetch "$LIBUSB_URL" "$LIBUSB_SHA" "$LIBUSB.tar.bz2"
fetch "$LIBTOOL_URL" "$LIBTOOL_SHA" "$LIBTOOL.tar.xz"
fetch "$GPHOTO_URL" "$GPHOTO_SHA" "$GPHOTO.tar.bz2"

BUILD=$(mktemp -d "${TMPDIR:-/tmp}/rawtonic-gphoto2.XXXXXX")
trap 'rm -rf "$BUILD"' EXIT
JOBS=$( (sysctl -n hw.ncpu 2>/dev/null || nproc) | head -1)
mkdir -p "$PREFIX"
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"

USB=(--with-libusb-1.0=no)
if [ "$(uname -s)" != Darwin ]; then
  tar -xjf "$SRC/$LIBUSB.tar.bz2" -C "$BUILD"
  (cd "$BUILD/$LIBUSB" && ./configure --prefix="$PREFIX" --disable-static --disable-udev \
    --disable-examples-build --disable-tests-build >/dev/null && make -j"$JOBS" >/dev/null && make install >/dev/null)
  USB=()
fi

tar -xJf "$SRC/$LIBTOOL.tar.xz" -C "$BUILD"
(cd "$BUILD/$LIBTOOL" && ./configure --prefix="$PREFIX" --disable-static --enable-ltdl-install >/dev/null &&
  make -j"$JOBS" >/dev/null && make install >/dev/null)

tar -xjf "$SRC/$GPHOTO.tar.bz2" -C "$BUILD"
(cd "$BUILD/$GPHOTO" && ./configure --prefix="$PREFIX" --disable-static --disable-nls \
  --with-camlibs=ptp2 --without-libexif --without-jpeg --with-libxml-2.0=no --with-libcurl=no \
  --with-gdlib=no --with-libtiff-4=no --without-libusb "${USB[@]}" --disable-serial --disable-disk --disable-ptpip \
  LTDLINCL="-I$PREFIX/include" LIBLTDL="-L$PREFIX/lib -lltdl" >/dev/null &&
  make -j"$JOBS" >/dev/null && make install >/dev/null)

if [ "$(uname -s)" = Darwin ]; then
  # The ImageCaptureCore port driver, built against this libgphoto2 (its
  # port-driver header is not installed; it comes from the source tree).
  IOLIBS_DIR=$(PKG_CONFIG_LIBDIR="$PREFIX/lib/pkgconfig" pkg-config --variable=driverdir libgphoto2_port)
  mkdir -p "$IOLIBS_DIR"
  clang -fobjc-arc -O2 -Wall -Wextra -Wno-unused-parameter -Werror -bundle \
    -mmacosx-version-min="$MACOSX_DEPLOYMENT_TARGET" \
    -I"$PREFIX/include" -I"$BUILD/$GPHOTO/libgphoto2_port" -o "$IOLIBS_DIR/icc.so" "$ICC_PORT" \
    -L"$PREFIX/lib" -lgphoto2_port -framework Foundation -framework ImageCaptureCore
  # A weak import from the system libraries is a function newer than the
  # minimum macOS: it would be NULL there.
  WEAK=$(for f in "$PREFIX"/lib/*.dylib "$PREFIX"/lib/libgphoto2/*/*.so "$PREFIX"/lib/libgphoto2_port/*/*.so; do
    [ -L "$f" ] || nm -m "$f" | grep 'weak external' | grep -E 'from lib(System|objc)|from [A-Z]' | sed "s|^|$f: |"
  done) || true
  if [ -n "$WEAK" ]; then
    echo "libgphoto2: functions newer than macOS $MACOSX_DEPLOYMENT_TARGET are weak-linked:" >&2
    echo "$WEAK" >&2
    exit 1
  fi
fi
echo "$STAMP" >"$PREFIX/.rawtonic-build"
echo "libgphoto2: built into $PREFIX"
