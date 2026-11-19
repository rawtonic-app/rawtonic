#!/bin/sh
# Build ONNX Runtime 1.30.0 without Microsoft's usage telemetry for the model
# pack (PRIVACY.md). Microsoft's release libraries since 1.30 contain a 1DS
# uploader on macOS and Linux (and register their Windows ETW provider in
# Windows' telemetry group); `build.py` turns both on by default, and
# `--no_telemetry` builds neither. Otherwise it uses the build.py flags of
# Microsoft's release build of the same platform (third-party libraries come
# from cmake/deps.txt, not vcpkg), so the CPU kernels are the same source
# (macOS arm64: CPU outputs bit-identical on the 15 ONNX files of the QA model
# folder, one synthetic input; Moebius, SDXL VAE and the Super Resolution CPU
# graphs not compared yet —
# notes: benchmark-results/ortt2-2026-11-18/REPORT.md).
#
# Usage: [ARCH=aarch64] scripts/models/build_onnxruntime.sh SOURCE_ARCHIVE BUILD_DIR
#   SOURCE_ARCHIVE  GitHub's source archive of commit SOURCE_COMMIT (below),
#                   the commit Microsoft's 1.30.0 libraries name in their
#                   build info; any fork that has the commit serves it, e.g.
#                   https://github.com/<fork>/onnxruntime/archive/<commit>.tar.gz
#   BUILD_DIR       a new, empty directory (sources are unpacked into it)
#   ARCH            Linux only: the target, x86_64 or aarch64 (default: the
#                   host's). Another architecture than the host's runs the
#                   build image under emulation, which needs qemu-user
#                   registered with binfmt_misc (e.g. `docker run --privileged
#                   --rm tonistiigi/binfmt --install arm64`) and is slow.
#
# Needs Python 3.10+, CMake and Ninja or Make; it downloads the third-party
# sources pinned in the archive's cmake/deps.txt. Prints the library's
# SHA-256 and checks it holds no uploader. macOS arm64 and Linux x86_64 and
# aarch64 (Docker) so far; Windows and the WebGPU and CUDA plugins are open
# in gap G65.
set -eu

SOURCE_COMMIT=f2c39fe2f838cf35ce7da92824f5a5e3ee6e88a7
# GitHub's archive of that commit, fetched 2026-11-18.
SOURCE_SHA256=1b4ec55fa26ee8fe622ea256894dfe9381bdfa99d41489994a1961720f556c48

# Linux builds in these images (pinned by digest, pulled 2026-11-18).
LINUX_IMAGE_X86_64=quay.io/pypa/manylinux_2_28_x86_64@sha256:d086f64cddcace250ef0f0073583c51b8298159303290a730c9f59bd91edfd2c
LINUX_IMAGE_AARCH64=quay.io/pypa/manylinux_2_28_aarch64@sha256:34a5e614a4eb26f73f8bf44cbbf2acf8011620453d03ea35a58537bb3cde44de

[ $# -eq 2 ] || { sed -n '2,29p' "$0"; exit 2; }
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
build=$(cd "$build" && pwd)  # absolute: Docker mounts it at the same path
tar -xzf "$archive" -C "$build"
src=$build/onnxruntime-$SOURCE_COMMIT
[ "$(cat "$src/VERSION_NUMBER")" = 1.30.0 ] || { echo "build_onnxruntime: not 1.30.0" >&2; exit 1; }

platform=$(uname -s)-$(uname -m)
[ "$(uname -s)" = Linux ] && platform=Linux-${ARCH:-$(uname -m)}
case "$platform" in
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
  Linux-x86_64 | Linux-aarch64)
    # The build.py flags of Microsoft's Linux packaging job
    # (c-api-linux-cpu.yml, the same for x86_64 and aarch64: `--enable_lto
    # --use_binskim_compliant_compile_flags --build_shared_lib --config
    # Release`), less Java, Node.js and `--use_vcpkg`: the third-party
    # libraries come from cmake/deps.txt (same versions) through ORT's
    # FetchContent path with ORT's own patches (e.g. a cpuinfo sysfs
    # fallback), not from vcpkg's ports. Built in the manylinux_2_28 image
    # (AlmaLinux 8, gcc-toolset-14, as Microsoft's image), so glibc 2.28 is
    # the ceiling of what the library can require, whatever the host has
    # (measured: GLIBC_2.27 on both architectures; Microsoft's: 2.28);
    # needs Docker. The build directory is mounted at the same path, so the
    # embedded paths are the host's.
    arch=${platform#Linux-}
    if [ "$arch" = x86_64 ]; then
      image=$LINUX_IMAGE_X86_64 docker_platform=linux/amd64
    else
      image=$LINUX_IMAGE_AARCH64 docker_platform=linux/arm64
    fi
    docker run --rm --platform "$docker_platform" --user "$(id -u):$(id -g)" -e HOME="$build" \
      --volume "$build:$build" --workdir "$build" "$image" \
      /opt/python/cp312-cp312/bin/python3 "$src/tools/ci_build/build.py" \
      --update --build --build_dir "$build/out" --skip_submodule_sync --parallel \
      --use_binskim_compliant_compile_flags --build_shared_lib --config Release \
      --enable_lto --no_telemetry --skip_tests
    library=$build/out/Release/libonnxruntime.so.1.30.0
    name=libonnxruntime-1.30.0-notelemetry-linux-$arch.so
    ;;
  *)
    echo "build_onnxruntime: $platform is not scripted yet (gap G65)" >&2
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
