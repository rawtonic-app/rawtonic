#!/bin/sh
# Build ONNX Runtime 1.30.0 without Microsoft's usage telemetry for the model
# pack (PRIVACY.md). Microsoft's release libraries since 1.30 contain a 1DS
# uploader on macOS and Linux (and register their Windows ETW provider in
# Windows' telemetry group); `build.py` turns both on by default, and
# `--no_telemetry` builds neither. Everything else follows Microsoft's release
# build of the same platform, so the CPU kernels are the same source and flags
# (outputs bit-identical on every pack model, macOS arm64:
# notes: benchmark-results/ortt2-2026-11-18/REPORT.md).
#
# Usage: scripts/models/build_onnxruntime.sh SOURCE_ARCHIVE BUILD_DIR
#   SOURCE_ARCHIVE  GitHub's source archive of commit SOURCE_COMMIT (below),
#                   the commit Microsoft's 1.30.0 libraries name in their
#                   build info; any fork that has the commit serves it, e.g.
#                   https://github.com/<fork>/onnxruntime/archive/<commit>.tar.gz
#   BUILD_DIR       a new, empty directory (sources are unpacked into it)
#
# Needs Python 3.10+, CMake and Ninja or Make; it downloads the third-party
# sources pinned in the archive's cmake/deps.txt. Prints the library's
# SHA-256 and checks it holds no uploader. macOS arm64 only so far; Linux
# and Windows (with the WebGPU and CUDA plugins) are open in gap G65.
set -eu

SOURCE_COMMIT=f2c39fe2f838cf35ce7da92824f5a5e3ee6e88a7
# GitHub's archive of that commit, fetched 2026-11-18.
SOURCE_SHA256=1b4ec55fa26ee8fe622ea256894dfe9381bdfa99d41489994a1961720f556c48

[ $# -eq 2 ] || { sed -n '2,21p' "$0"; exit 2; }
archive=$1
build=$2
python=${PYTHON:-python3}

if command -v shasum >/dev/null 2>&1; then
  sum() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  sum() { sha256sum "$1" | cut -d' ' -f1; }
fi
got=$(sum "$archive")
[ "$got" = "$SOURCE_SHA256" ] || {
  echo "build_onnxruntime: $archive has SHA-256 $got, expected $SOURCE_SHA256" >&2
  exit 1
}
if [ -e "$build" ] && [ -n "$(ls -A "$build")" ]; then
  echo "build_onnxruntime: $build is not empty" >&2
  exit 1
fi
mkdir -p "$build"
tar -xzf "$archive" -C "$build"
src=$build/onnxruntime-$SOURCE_COMMIT
[ "$(cat "$src/VERSION_NUMBER")" = 1.30.0 ] || { echo "build_onnxruntime: not 1.30.0" >&2; exit 1; }

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)
    # Microsoft's macOS packaging job (mac-cpu-packing-jobs.yml), less what
    # the app does not load: Java, Node.js, the built-in WebGPU provider
    # (macOS runs the models on the CPU; Core ML natively) and vcpkg
    # (deps.txt pins the same versions). The SDK's newer availability
    # warnings are not errors; CMake 4 needs the policy floor for psimd.
    MACOSX_DEPLOYMENT_TARGET=14.0 "$python" "$src/tools/ci_build/build.py" \
      --update --build --build_dir "$build/out" --skip_submodule_sync --parallel \
      --use_binskim_compliant_compile_flags --build_shared_lib --config Release \
      --use_coreml --no_telemetry --skip_tests --compile_no_warning_as_error \
      --cmake_extra_defines CMAKE_OSX_ARCHITECTURES=arm64 CMAKE_POLICY_VERSION_MINIMUM=3.5
    library=$build/out/Release/libonnxruntime.1.30.0.dylib
    name=libonnxruntime-1.30.0-notelemetry-macos-aarch64.dylib
    ;;
  *)
    echo "build_onnxruntime: $(uname -s)-$(uname -m) is not scripted yet (gap G65)" >&2
    exit 1
    ;;
esac

cp "$library" "$build/$name"
# No 1DS SDK, collector address or HTTP client in the library.
if LC_ALL=C grep -a -q -E 'OneCollector|events\.data\.microsoft|MATSDK|libcurl|NSURLSession' "$build/$name"; then
  echo "build_onnxruntime: $name still contains the telemetry uploader" >&2
  exit 1
fi
echo "$(sum "$build/$name")  $build/$name"
