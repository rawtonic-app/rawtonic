#!/usr/bin/env bash
# Measures Fujifilm live view through libgphoto2 without a camera (Linux).
# Builds the pinned libgphoto2 with its virtual USB camera (vusb) plus the
# test patch test-patches/libgphoto2-2.5.34-vusb-fuji-liveview.patch — a
# Fujifilm body in USB tether mode serving live view as object 0x80000001,
# InvalidObjectHandle until its next frame is ready, interrupt reads that
# wait their whole timeout like a body that sends no event — as pinned and
# with the drafted patches (patches/libgphoto2-2.5.34-fuji-*.patch, one by
# one and together), and runs `rawtonic-tether liveview` against it through
# each. vusb and the test patch are never bundled.
#
# Usage: scripts/tether/test_fuji_liveview_vusb.sh [SECONDS [RUN...]]
#   RUN = VARIANT[:EXPERIMENT]: VARIANT one of pinned, event-queue,
#   frame-wait, both; EXPERIMENT a RAWTONIC_TETHER_LV_EXPERIMENT value.
# Env: VCAMERA_FUJI_FRAME_MS (the virtual camera's frame interval, 33).
# Prints one line per run; reports and traces stay in target/vusb-fuji/.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
SPAN=${1:-4}
shift || true
RUNS=("$@")
if [ ${#RUNS[@]} -eq 0 ]; then
  RUNS=(pinned pinned:poll-ms=1000 pinned:poll-ms=1000,tolerate=50 event-queue frame-wait both)
fi
OUT=$REPO/target/vusb-fuji
SRC=$REPO/target/gphoto2-src
eval "$(grep -E '^GPHOTO(_URL|_SHA)?=' "$REPO/scripts/tether/build_libgphoto2.sh")"
TEST_PATCH=$REPO/scripts/tether/test-patches/libgphoto2-2.5.34-vusb-fuji-liveview.patch
PATCHES=$REPO/scripts/tether/patches
JOBS=$(nproc)

mkdir -p "$SRC" "$OUT"
if [ ! -f "$SRC/$GPHOTO.tar.bz2" ] || [ "$(sha256sum "$SRC/$GPHOTO.tar.bz2" | cut -d' ' -f1)" != "$GPHOTO_SHA" ]; then
  curl -fsSL -o "$SRC/$GPHOTO.tar.bz2.download" "$GPHOTO_URL"
  mv "$SRC/$GPHOTO.tar.bz2.download" "$SRC/$GPHOTO.tar.bz2"
fi
[ "$(sha256sum "$SRC/$GPHOTO.tar.bz2" | cut -d' ' -f1)" = "$GPHOTO_SHA" ] || {
  echo "vusb test: checksum mismatch for $GPHOTO" >&2; exit 1; }

patches_of() {
  case "$1" in
    pinned) ;;
    event-queue) echo "$PATCHES/libgphoto2-2.5.34-fuji-event-queue.patch" ;;
    frame-wait) echo "$PATCHES/libgphoto2-2.5.34-fuji-frame-wait.patch" ;;
    both) echo "$PATCHES/libgphoto2-2.5.34-fuji-event-queue.patch $PATCHES/libgphoto2-2.5.34-fuji-frame-wait.patch" ;;
    *) echo "vusb test: unknown variant $1" >&2; return 1 ;;
  esac
}

build() { # variant
  local variant=$1 prefix=$OUT/$1 stamp
  local patches
  read -r -a patches <<< "$(patches_of "$variant")"
  stamp=$(cat "$TEST_PATCH" ${patches[@]+"${patches[@]}"} | sha256sum | cut -c1-16)
  if [ -f "$prefix/.stamp" ] && [ "$(cat "$prefix/.stamp")" = "$stamp" ]; then return; fi
  rm -rf "$prefix" "$OUT/build-$variant"
  mkdir -p "$OUT/build-$variant"
  tar -xjf "$SRC/$GPHOTO.tar.bz2" -C "$OUT/build-$variant"
  (cd "$OUT/build-$variant/$GPHOTO" &&
    for p in "$TEST_PATCH" ${patches[@]+"${patches[@]}"}; do patch -s -p1 < "$p"; done &&
    ./configure --prefix="$prefix" --disable-static --disable-nls --with-camlibs=ptp2 \
      --without-libexif --without-jpeg --with-libxml-2.0=no --with-libcurl=no --with-gdlib=no \
      --with-libtiff-4=no --without-libusb --with-libusb-1.0=no --enable-vusb \
      --disable-serial --disable-disk --disable-ptpip >/dev/null 2>&1 &&
    make -j"$JOBS" >/dev/null 2>&1 && make install >/dev/null 2>&1)
  echo "$stamp" > "$prefix/.stamp"
}

for run in "${RUNS[@]}"; do
  build "${run%%:*}"
done

# One helper for all: the variants differ only in the drivers libgphoto2
# loads at run time (CAMLIBS, IOLIBS) and its libraries.
cargo build --locked -q -p rawtonic-tether --features gphoto2 --manifest-path "$REPO/Cargo.toml"
HELPER=$REPO/target/debug/rawtonic-tether
EMPTY=$OUT/empty-card
mkdir -p "$EMPTY"

for run in "${RUNS[@]}"; do
  variant=${run%%:*}
  experiment=
  [ "$run" = "$variant" ] || experiment=${run#*:}
  prefix=$OUT/$variant
  camlibs=$(PKG_CONFIG_LIBDIR=$prefix/lib/pkgconfig pkg-config --variable=driverdir libgphoto2)
  iolibs=$(PKG_CONFIG_LIBDIR=$prefix/lib/pkgconfig pkg-config --variable=driverdir libgphoto2_port)
  out=$OUT/run-$variant-${experiment//[^a-z0-9-]/_}-$(date +%Y%m%dT%H%M%S)
  LD_LIBRARY_PATH=$prefix/lib CAMLIBS=$camlibs IOLIBS=$iolibs VCAMERA_TYPE=fuji VCAMERADIR=$EMPTY \
    RAWTONIC_TETHER_TRACE=$out.trace RAWTONIC_TETHER_LV_EXPERIMENT=$experiment \
    "$HELPER" liveview --seconds "$SPAN" --shots 0 --out "$out" > "$out.json" 2> "$out.log" || true
  python3 -I - "$run" "$out.json" <<'EOF'
import json, sys
run, path = sys.argv[1], sys.argv[2]
try:
    report = json.load(open(path))
except ValueError:
    print(f"{run}: no report ({path})"); sys.exit(0)
phase = (report.get("phases") or [{}])[0]
stops = [s["error"]["message"][:60] for s in phase.get("stopped", []) if "error" in s]
print(f"{run}: {report.get('model')}: {phase.get('frames')} frames, {phase.get('fps')} fps, "
      f"first {phase.get('firstFrameMs')} ms, median gap {phase.get('medianGapMs')} ms, "
      f"p95 {phase.get('p95GapMs')} ms, max {phase.get('maxGapMs')} ms, "
      f"stopped {len(stops)}x {stops[:1]}  ({path.rsplit('/', 1)[-1]})")
EOF
done
