#!/usr/bin/env bash
# Prepares the tether helper for an app bundle: builds libgphoto2
# (build_libgphoto2.sh), builds `rawtonic-tether` with camera support
# against it, and stages helper, libraries and camera/port drivers in
# target/tether-bundle together with a Tauri config overlay that adds them
# to the bundle:
#
#   macOS: Contents/MacOS/rawtonic-tether, Contents/Frameworks/*.dylib,
#          Contents/Resources/gphoto2/{camlibs,iolibs}/ (CAMLIBS/IOLIBS are
#          set by the app when it starts the helper); the port driver is
#          the ImageCaptureCore one (icc), no libusb
#   Linux: usr/bin/rawtonic-tether, <resources>/gphoto2/{camlibs,iolibs}/
#          with libusb's port driver (usb1); the libraries are staged in
#          target/tether-bundle/lib for linuxdeploy, which copies them into
#          the AppImage's usr/lib (packaging/linux/build-appimage.sh runs
#          this inside its container and puts lib/ on the library path).
#
# Every library reference is rewritten to the bundle and re-signed ad hoc;
# the script fails if anything still points outside the bundle.
#
# Usage: scripts/tether/bundle_helper.sh   then
#        tauri build --config target/tether-bundle/tauri.tether.conf.json …
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$REPO/target}
PREFIX=$TARGET/gphoto2
OUT=$TARGET/tether-bundle
"$REPO/scripts/tether/build_libgphoto2.sh" "$PREFIX"

TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
GPHOTO2_LIB_DIR=$PREFIX/lib GPHOTO2_INCLUDE_DIR=$PREFIX/include \
  cargo build --release --locked -p rawtonic-tether --features gphoto2 --manifest-path "$REPO/Cargo.toml"

rm -rf "$OUT"
mkdir -p "$OUT/binaries" "$OUT/Frameworks" "$OUT/gphoto2/camlibs" "$OUT/gphoto2/iolibs"
HELPER=$OUT/binaries/rawtonic-tether-$TRIPLE
cp "$TARGET/release/rawtonic-tether" "$HELPER"
cp "$PREFIX"/lib/libgphoto2/*/ptp2.so "$OUT/gphoto2/camlibs/"

case "$(uname -s)" in
  Darwin)
    cp "$PREFIX"/lib/libgphoto2_port/*/icc.so "$OUT/gphoto2/iolibs/"
    LIBS=(libgphoto2.6.dylib libgphoto2_port.12.dylib libltdl.7.dylib)
    for lib in "${LIBS[@]}"; do
      cp "$PREFIX/lib/$lib" "$OUT/Frameworks/$lib"
      chmod u+w "$OUT/Frameworks/$lib"
      install_name_tool -id "@executable_path/../Frameworks/$lib" "$OUT/Frameworks/$lib" 2>/dev/null
    done
    FILES=("$HELPER" "$OUT"/Frameworks/*.dylib "$OUT"/gphoto2/camlibs/*.so "$OUT"/gphoto2/iolibs/*.so)
    for file in "${FILES[@]}"; do
      chmod u+w "$file"
      for lib in "${LIBS[@]}"; do
        install_name_tool -change "$PREFIX/lib/$lib" "@executable_path/../Frameworks/$lib" "$file" 2>/dev/null
      done
      # libgphoto2 records its build-time camlib/iolib folders; the app
      # passes the bundled ones (CAMLIBS/IOLIBS), so no rpath is needed.
      codesign --force --sign - "$file" >/dev/null 2>&1
    done
    if otool -L "${FILES[@]}" | grep -v ':$' | grep -v -e '@executable_path/../Frameworks/' -e '^\s*/usr/lib/' -e '^\s*/System/'; then
      echo "bundle_helper: a library reference points outside the bundle" >&2
      exit 1
    fi
    FRAMEWORKS=$(printf '"%s",' "$OUT"/Frameworks/*.dylib)
    cat >"$OUT/tauri.tether.conf.json" <<EOF
{
  "bundle": {
    "externalBin": ["$OUT/binaries/rawtonic-tether"],
    "resources": {
      "resources/": "resources/",
      "lensfun_db/": "lensfun_db/",
      "$OUT/gphoto2/": "gphoto2/"
    },
    "macOS": { "frameworks": [${FRAMEWORKS%,}] }
  }
}
EOF
    ;;
  Linux)
    cp "$PREFIX"/lib/libgphoto2_port/*/usb1.so "$OUT/gphoto2/iolibs/"
    mkdir -p "$OUT/lib"
    for lib in libgphoto2.so.6 libgphoto2_port.so.12 libltdl.so.7 libusb-1.0.so.0; do
      cp -L "$PREFIX/lib/$lib" "$OUT/lib/$lib"
    done
    FILES=("$HELPER" "$OUT"/lib/*.so* "$OUT"/gphoto2/camlibs/*.so "$OUT"/gphoto2/iolibs/*.so)
    for file in "${FILES[@]}"; do
      chmod u+w "$file"
      # libtool records the build folder; linuxdeploy sets the AppImage's.
      patchelf --remove-rpath "$file"
    done
    if LD_LIBRARY_PATH=$OUT/lib ldd "${FILES[@]}" | grep -v ':$' |
      grep -E "not found|libgphoto2|libusb|libltdl" | grep -v "=> $OUT/lib/"; then
      echo "bundle_helper: a library reference points outside the bundle" >&2
      exit 1
    fi
    cat >"$OUT/tauri.tether.conf.json" <<EOF
{
  "bundle": {
    "externalBin": ["$OUT/binaries/rawtonic-tether"],
    "resources": {
      "resources/": "resources/",
      "lensfun_db/": "lensfun_db/",
      "$OUT/gphoto2/": "gphoto2/"
    }
  }
}
EOF
    ;;
  *)
    echo "bundle_helper: unsupported system $(uname -s)" >&2
    exit 1
    ;;
esac
echo "bundle_helper: staged in $OUT"
