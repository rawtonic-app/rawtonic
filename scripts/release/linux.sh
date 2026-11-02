#!/bin/bash
# The Linux half of scripts/release.sh, run on the Linux build host through
# `ssh HOST bash -s -- STEP ARGS < scripts/release/linux.sh`. Paths are
# relative to the remote home or absolute; "-" stands for an empty argument
# (ssh drops empty words).
#   unpack D                     D/src.tar → D/src (every file touched: git
#                                archive's old mtimes would hide changes from
#                                the shared cargo cache)
#   build D CACHE SEED LOCK VER  the AppImage in Docker, under flock LOCK, with
#                                CACHE (first copied from SEED when missing)
#   smoke D PHOTOS VER           relay address, update information, host exports
#                                vs D/prev/prev.AppImage, distribution containers
set -uo pipefail
cd ~ || exit 1
step=$1; shift
dash() { [ "$1" = - ] && echo "" || echo "$1"; }

case $step in
  unpack)
    D=$1
    rm -rf "${D:?}/src" && mkdir -p "$D/src" "$D/out" && tar -x -C "$D/src" -f "$D/src.tar" || exit 1
    find "$D/src" -exec touch {} + && rm -f "$D/src.tar"
    echo "source in $D/src: $(find "$D/src" -type f | wc -l) files"
    ;;
  build)
    D=$1 CACHE=$2 SEED=$(dash "$3") LOCK=$(dash "$4") V=$5
    if [ ! -d "$CACHE" ] && [ -n "$SEED" ]; then
      echo "seeding $CACHE from $SEED"; cp -a --reflink=auto "$SEED" "$CACHE" || exit 1
    fi
    CACHE=$(realpath -m "$CACHE")
    [ -z "$LOCK" ] || { LOCK=$(realpath -m "$LOCK"); mkdir -p "$(dirname "$LOCK")"; }
    cd "$D/src" || exit 1
    echo "waiting for the build lock $(date +%T)"
    if [ -n "$LOCK" ]; then
      flock "$LOCK" chrt --other 0 packaging/linux/build-appimage.sh "$CACHE"
    else
      chrt --other 0 packaging/linux/build-appimage.sh "$CACHE"
    fi || { echo "build failed"; exit 1; }
    echo "build done $(date +%T)"
    cd ~ || exit 1
    cp "$CACHE/target/release/bundle/appimage/Rawtonic_${V}_amd64.AppImage" \
      "$CACHE/target/release/bundle/appimage/Rawtonic_${V}_amd64.AppImage.zsync" "$D/out/" || exit 1
    (cd "$D/out" && sha256sum Rawtonic_"${V}"_amd64.AppImage*)
    ;;
  smoke)
    D=$1 P=$2 V=$3 T=$1/smoke bad=0
    rm -rf "${T:?}"; mkdir -p "$T/app" "$T/host" "$T/prev"
    cp "$D/out/Rawtonic_${V}_amd64.AppImage" "$T/app/"; chmod +x "$T/app/"*.AppImage
    app=$T/app/Rawtonic_${V}_amd64.AppImage
    (cd "$T" && "$app" --appimage-extract usr/bin/rawtonic-app >/dev/null 2>&1)
    n=$(grep -c -a relay.rawtonic.cc "$T/squashfs-root/usr/bin/rawtonic-app"); echo "relay strings: $n"; [ "$n" -ge 1 ] || bad=1
    u=$(grep -a -o 'gh-releases-zsync[^ ]*zsync' "$app" | head -1); echo "update information: $u"; [ -n "$u" ] || bad=1
    export XDG_CONFIG_HOME=$T/cfg XDG_DATA_HOME=$T/data XDG_CACHE_HOME=$T/cache
    for f in "$P"/*; do b=$(basename "$f"); [ "$b" = SHA256SUMS ] && continue
      s=$(date +%s)
      xvfb-run -a "$app" export "$f" --output "$T/host/$b.jpg" --format jpeg > "$T/host/$b.log" 2>&1 < /dev/null; c=$?
      XDG_CONFIG_HOME=$T/pcfg XDG_DATA_HOME=$T/pdata XDG_CACHE_HOME=$T/pcache \
        xvfb-run -a "$D/prev/prev.AppImage" export "$f" --output "$T/prev/$b.jpg" --format jpeg > "$T/prev/$b.log" 2>&1 < /dev/null
      cmp -s "$T/host/$b.jpg" "$T/prev/$b.jpg" && same=identical-to-prev || same=DIFF-vs-prev
      echo "host $b exit $c $(( $(date +%s) - s ))s $(stat -c %s "$T/host/$b.jpg" 2>/dev/null) $same"
      [ $c = 0 ] || bad=1
    done
    (cd "$P" && sha256sum -c SHA256SUMS --quiet) && echo "host photographs unchanged" || bad=1
    for img in ubuntu:22.04 ubuntu:24.04 debian:12 fedora:42; do
      timeout 3600 docker run --rm -v "$(realpath "$T/app")":/app:ro -v "$(realpath "$P")":/in:ro \
        -v "$(realpath "$D/src/packaging/linux")":/test:ro "$img" bash /test/smoke-test.sh > "$T/$img.log" 2>&1 < /dev/null
      c=$?; echo "container $img exit $c: $(grep -c '^ok ' "$T/$img.log") ok, $(grep -c '^FAIL' "$T/$img.log") failed"
      [ $c = 0 ] || bad=1
    done
    rm -rf "${T:?}/squashfs-root"
    if [ $bad = 0 ]; then echo "linux smoke: ok"; else echo "linux smoke: FAILED"; exit 1; fi
    ;;
  *) echo "unknown step $step" >&2; exit 2 ;;
esac
