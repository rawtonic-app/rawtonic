# ADR 0011 — GPU execution providers for the AI models on Windows and Linux
Status: accepted, 2026-10-10. Speed on real Windows/Linux GPUs is **not measured** yet (see Open).

## Starting point
Every ONNX model (BiRefNet Subject/Background, SAM 2.1, sky, depth, SigLIP 2, MI-GAN, AI Denoise's MatchFilter, the face models) ran on ONNX Runtime's CPU provider on Windows and Linux. macOS has Core ML fast paths for BiRefNet and MatchFilter. Cold Subject on the ONNX path took 6.7–10.4 s in the app (M5 Pro, CPU) against Lightroom's 0.7–2.2 s (docs/MODELS.md). The runtime was Microsoft's ONNX Runtime 1.22.0 release, loaded dynamically (`ort` 2.0.0-rc.10, `load-dynamic`).

## Options (licences checked 2026-10-10)
| Provider | Hardware | Binaries | Licence of what we would ship | Verdict |
|---|---|---|---|---|
| **WebGPU** plugin EP 0.4.0 | any GPU with Direct3D 12 (Windows) or Vulkan (Linux); Metal on macOS | Microsoft, PyPI `onnxruntime-ep-webgpu` / NuGet `Microsoft.ML.OnnxRuntime.EP.WebGpu`: win x64/arm64, linux x64/aarch64, macOS; 11–16 MB | MIT (Microsoft), contains Google's Dawn (BSD-3-Clause), abseil etc. — the ONNX Runtime 1.30.0 third-party notices. The Windows package also carries `dxcompiler.dll`/`dxil.dll` (DirectX Shader Compiler 1.8.2502, Microsoft Software License Terms): **not shipped** | **Chosen** for Windows and Linux |
| DirectML EP | any DX12 GPU, Windows only | `Microsoft.ML.OnnxRuntime.DirectML` (MIT) + `DirectML.dll` from `Microsoft.AI.DirectML` 1.15.4 | `DirectML.dll`: Microsoft Software License Terms ("solely for use on Windows and Xbox") — proprietary; DirectML is in "sustained engineering" (no new features) | Rejected: a proprietary DLL in the app and a frozen API, while WebGPU covers the same GPUs |
| CUDA plugin EP 0.2.0 (matched to 1.30.0) | NVIDIA | Microsoft GitHub release `plugin-ep-cuda/v0.2.0`: CUDA 13 linux-x64 300 MB, win-x64 190 MB | Plugin MIT. It loads the user's CUDA 13 runtime, cuBLAS and cuDNN 9 (NVIDIA licences); Rawtonic distributes none of them | **Optional**: in the pack, installed only by `install_models.sh --cuda`, used first when present |
| TensorRT | NVIDIA | NVIDIA | proprietary runtime, minutes of engine building per model and GPU | Not now |
| ROCm / MIGraphX | AMD | no Microsoft desktop builds (AMD's own) | MIT EP; ROCm stack AMD's | Not now: AMD GPUs run WebGPU |
| OpenVINO EP | Intel CPU/GPU/NPU | Intel's builds (pip `onnxruntime-openvino`), ~100 MB+ runtime | Apache-2.0 | Not now: Intel GPUs run WebGPU |
| CoreML EP | Apple | in the macOS ORT build | MIT | Measured before and rejected (load times, docs/MODELS.md); macOS keeps its own Core ML conversions |

Plugin EPs need ONNX Runtime **1.24.4 or later**. The WebGPU plugin 0.4.0 is built from the 1.30.0 revision; the CUDA plugin 0.2.0 names 1.30.0 as its matched version (contributed-operator schemas must match). So Windows and Linux move to **ONNX Runtime 1.30.0**; macOS stays on 1.22.0 (Core ML is its fast path, and 1.30 has no Intel macOS build). `ort` rc.10 asks the runtime for API version 22, which 1.30 still provides; the plugin functions (`RegisterExecutionProviderLibrary`, `GetEpDevices`, `SessionOptionsAppendExecutionProvider_V2`) are part of that API, so no `ort` upgrade is needed.

Licence of loading. Rawtonic ships only MIT/BSD-licensed binaries (ONNX Runtime, the WebGPU plugin with Dawn). On Windows Dawn compiles shaders with the system's `d3dcompiler_47.dll` when DXC is absent, and on Linux it loads the system's Vulkan loader and driver: system components. The optional CUDA plugin loads NVIDIA libraries the user installed; Rawtonic neither bundles nor redistributes them. (No lawyer was consulted; this follows the project's legal guardrails.)

## Decision
- New crate `crates/onnx` (`rawtonic-onnx`, Tauri-free): every model loads through `Model::load(path, ModelKind)`. Providers are tried in order **CUDA (if its plugin is installed) → WebGPU → CPU**. A plugin library is registered once per process from the folders in `Settings::plugin_dirs` (the app's `resources/` beside ONNX Runtime, then the model folder; `RAWTONIC_ORT_PLUGIN_DIR`).
- **Check before use.** A GPU can return wrong numbers without an error: on Mesa's lavapipe the SAM 2.1 encoder ran on WebGPU and returned an embedding with cosine −0.04 to the CPU's. So the first time a model loads on a device, it runs once on the GPU and once on the CPU with the same deterministic input (a synthetic picture, prepared as the app prepares a photo) and is used on the GPU only if masks agree (IoU ≥ 0.99) and other outputs are within 1 % of the output's range (MI-GAN: mean ≤ 2 levels). Verdicts are kept per model file, provider, device and plugin library in `<app data>/ai-gpu-checks.tsv`, so this costs one CPU run per model and device, once.
- **Fallback.** A provider without a library or device, a failed session creation, a failed check, a failed GPU run or non-finite output (`ensure_finite`) moves the model to the CPU; a failed run is repeated there at once. A run stopped on purpose (Subject's terminate flag) is not a failure.
- **Per-model exceptions** (`ModelKind::gpu_exception`): YuNet and Face Mesh (3–5 ms on the CPU, 5–9 ms on WebGPU on Metal: dispatch and upload cost more than they save) and the SAM 2.1 decoder (runs twice per click in tens of milliseconds; kept on the CPU for the click's latency).
- **Preference** "Use the GPU for AI" (Settings → Processing, Windows and Linux; on by default; takes effect after a relaunch). `RAWTONIC_AI_PROVIDER=cpu|webgpu|cuda` overrides it for tests and benchmarks.
- **Logging**: `AI <model>: loaded on <provider> (<device>) in … ms`, `first run on … in … ms`, the check's verdict, every fallback.
- AI Denoise's cache key includes the GPU provider (WebGPU and CUDA results differ from the CPU's in the last bits); CPU and Core ML keys are unchanged.
- Shipping: `src-tauri/build.rs` (`RAWTONIC_BUNDLE_ONNX=1`) bundles ONNX Runtime 1.30.0 and the WebGPU plugin on Windows and Linux, pinned by SHA-256, and copies ONNX Runtime's licence and notices into `resources/licenses/`. Nothing new is downloaded at run time (PRIVACY.md). `build_model_pack.py` adds the files; `install_models.sh --runtime` and `--cuda` install them.
- Tools: `rawtonic ai-bench [--provider webgpu|cuda] [--image photo.jpg] [--only birefnet,…]` times every model on the CPU and the GPU and compares outputs; `cargo test -p rawtonic-onnx --test providers -- --ignored` does the same as tests; `scripts/models/compare_providers.py` in Python; `.github/workflows/gpu-ai.yml` runs them by hand on GitHub's runners.

## Evidence (2026-10-10)
Inputs: renders of CC0 raw.pixls.us files (licence checked per file by checksum against raw.pixls.us' repository list), never users' photos.

**ONNX Runtime 1.30.0 vs 1.22.0 on the CPU** (macOS arm64 builds, all eleven models, two photos): masks identical, largest difference ≤ 2e-5 of the output's range except MI-GAN (≤ 2 levels of 255) and YuNet (2e-3).

**WebGPU on a real GPU: Metal** (M5 Pro, macOS; the same plugin code as on Windows/Linux but Dawn's Metal backend, so op coverage and numerics, not Windows/Linux speed): all eleven models ran with every node on WebGPU except MI-GAN (152 of 561 nodes on the CPU) and SigLIP 2 (130 of 710); mask IoU 1.0 (BiRefNet, sky, SAM 2.1 decoder), largest difference ≤ 1.2e-5 of the range, MI-GAN ≤ 1 level. Times in the Rust path (`rawtonic ai-bench`, other agents' builds loading the machine, load average 18–30; median of 2–3 runs after the first):

| Model | CPU | WebGPU (Metal) |
|---|---|---|
| BiRefNet 1024² | 3.2–4.7 s | 1.3–2.5 s (first run 1.4–1.7 s) |
| SAM 2.1 encoder | 0.40 s | 0.13 s |
| Sky | 0.35–0.39 s | 0.14 s |
| Depth | 0.13–0.15 s | 0.06 s |
| MatchFilter, one 320 px tile | 1.0–1.2 s | 0.58 s |
| SigLIP 2 vision | 24 ms | 21 ms |
| MI-GAN 512² | 0.11–0.15 s | 0.10 s |
| selfie parse | 17 ms | 8 ms |
| YuNet / Face Mesh / SAM decoder | 3–4 / 2 / 12–20 ms | 9 / 5 / 22 ms |

These are not the Windows/Linux numbers the bar asks for; Core ML stays macOS's path (BiRefNet 0.54 s).

**Linux, WebGPU on lavapipe** (Mesa 25 llvmpipe, software Vulkan; Docker arm64 on this Mac and GitHub's ubuntu-24.04 x86-64; ONNX Runtime lists GPUs from `/sys/class/drm`, which a container has not, so a stand-in DRM entry was mounted — Dawn picks the Vulkan adapter itself): see the table below. lavapipe is a CPU emulation, much slower than the CPU provider; this checks the provider path and numerics only.

| Model | arm64 (Docker), largest difference / range | x86-64 (GitHub) | Where the app's check puts it |
|---|---|---|---|
| BiRefNet | run fails: Conv kernel `absl … raw_hash_map::at` | same | CPU |
| SAM 2.1 encoder | `image_embed` wrong (max difference 4395, cosine −0.04), high-res features right | same | CPU |
| SAM 2.1 decoder | 1.4e-6, mask IoU 1.0 | within limits | (CPU by exception) |
| Sky | 1.0e-5, mask IoU 1.0 | within limits | WebGPU |
| Depth | 3.3e-6 | within limits | WebGPU |
| SigLIP 2 vision | 5.4e-7 | not in CI (export not public) | WebGPU |
| MatchFilter | 1.8e-6 | within limits | WebGPU |
| MI-GAN | 1 level, mean 0.002 | within limits | WebGPU |
| YuNet, Face Mesh, selfie parse | ≤ 4e-3 / 4.5e-5 / 1.5e-6 | YuNet within limits; Face Mesh and selfie parse not in CI | WebGPU (YuNet, Face Mesh: CPU by exception) |

In CI the models were rebuilt from public sources: SAM 2.1's export had the pack's SHA-256; BiRefNet's and the sky model's exports (CPU-only PyTorch on Linux) differ in bytes from the pack's, same checkpoints and scripts. The workflow's Python steps failed on a relative plugin path in that run (fixed afterwards, not run again); the Rust steps ran.

The Rust test `models_run_on_the_gpu_only_where_it_matches_the_cpu` loaded every model as the app does: BiRefNet and the SAM 2.1 encoder on the CPU, the rest on WebGPU. The Python harness on Docker gave the same agreement. Time on lavapipe is 2-250× the CPU's (it is a CPU emulation): it says nothing about GPUs.

**Windows** (GitHub windows-latest, no GPU): ONNX Runtime 1.30.0 and the WebGPU plugin load (without the DirectX Shader Compiler); device discovery lists no GPU on the runner (WARP is not a hardware device), so the plugin offers none and every model runs on the CPU, as designed. **WebGPU on Direct3D 12 is not verified on any machine**, including whether Dawn's FXC fallback without DXC covers every kernel.

## Open
- Speed and correctness on real GPUs under Windows (D3D12) and Linux (Vulkan): NVIDIA, AMD, Intel. Run `rawtonic ai-bench --provider webgpu --image photo.jpg` (and `--provider cuda` with the CUDA plugin) and the app's click-to-mask timing; record against Lightroom's 0.7–2.2 s. Until then the parity gap for cold Subject on Windows/Linux stays open.
- BiRefNet failed on lavapipe in a Conv kernel (`absl … raw_hash_map::at`); on real Vulkan drivers unknown. The check and fallback keep the CPU path.
- Shipping DXC would need its licence cleared; measure first whether FXC limits speed (no f16, no subgroups).
- GPU memory: BiRefNet takes about 1 GB on the GPU in fp32; small GPUs may fail to load it (falls back to the CPU).
- macOS could use WebGPU (Metal) for SAM 2.1, sky, depth and the others that have no Core ML path (2–3× in the table) once macOS moves to ONNX Runtime ≥ 1.24.4 (drops Intel Macs' ORT build) — not part of this change.
- The bundled build (`RAWTONIC_BUNDLE_ONNX=1`) for Windows/Linux needs the new pack files uploaded (`build_model_pack.py` lists them).

## Runtime maintenance, 2026-10-13

`ort` rc.12 / `ndarray` 0.17 now retain API 22 explicitly. CPU sessions register
CPU explicitly (the binding's automatic NPU policy is incompatible with the
old macOS runtime); device handles retain their environment and registry entries
are weak. macOS bundles Microsoft Runtime 1.23.2 on both architectures after
1.22 reproduced an exit-time mutex abort even with the newer binding. Twelve
installed ONNX models passed two CPU load/inference/unload rounds and process
shutdown on Apple Silicon. Windows/Linux retain Runtime 1.30 and their pinned
plugins; provider hardware regression there remains unverified in this run.
See `docs/testing/relight.md`.

## AI Denoise on Linux with NVIDIA, 2026-10-18

First measurement on a real GPU under Linux (Ryzen 5 3600X, RTX 3080 10 GB, NVIDIA 615, KDE Wayland; ONNX Runtime 1.30.0 and the WebGPU plugin 0.4.0 from the model pack), MatchFilter on four public raw.pixls.us Bayer files, timed by `crates/denoise/examples/denoise_bench.rs` (denoise step only, no cache):

| File | CPU provider | WebGPU (Vulkan) | WebGPU vs CPU |
|---|---|---|---|
| Nikon D7200, 24.2 MP | 434 s, peak 6.7 GB | 35.4 s, peak 3.5 GB | max 1.2e-4, mean 1.6e-8 |
| Canon EOS R10, 24.2 MP | 437 s, 6.8 GB | 35.5 s, 3.5 GB | max 4.9e-4, mean 2.6e-8 |
| Panasonic S5, 24.0 MP | 432 s, 6.8 GB | 35.6 s, 3.5 GB | max 2.4e-4, mean 3.3e-8 |
| Panasonic S1R II, 44.2 MP | 762 s, 7.1 GB | 60.6 s, 3.6 GB | max 4.9e-4, mean 4.8e-8 |

Differences are in normalised levels (1.0 = white) of the f16 result the app caches: at most one f16 step, 0.05–0.07 % of samples differ. The GPU check accepted WebGPU; it used about 4.3 GB of GPU memory and was ~85 % busy. The release app (headless export, AI Denoise 100, D7200) chose WebGPU by itself: 65 s the first time on the device (the check took 6.6 s, then remembered), 39 s for a new photo, 3.7 s from the cache. On the CPU, 12 instead of 6 intra-op threads was 4 % slower and two tiles at once (2 × 3 threads) 6 % faster at twice the memory (12.7 GB), so neither was adopted; a second WebGPU session would not leave 2 GB of GPU memory free on this card. The CUDA plugin was not measured: it needs CUDA 13 and cuDNN 9, which the machine does not have.
