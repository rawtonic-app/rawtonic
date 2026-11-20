# SRF16: fp16 Super Resolution graph on GPU providers (2026-11-20)

`superres_compact_x2_fp16.onnx` (pack-2026-11-20, SHA-256 `4ee4e846…289d`, `scripts/models/convert_superres_fp16.py`) against the fp32 graph `superres_compact_x2.onnx` on the same provider. One machine: Linux (CachyOS, kernel 7.3), RTX 3080 10 GB, driver 620.70.06, CUDA 13.5 / cuDNN 9.28.1, Rawtonic's own ONNX Runtime 1.30.0 core, CUDA plugin 0.2.0, WebGPU plugin 0.4.0 (all from the pack). Windows was not measured.

## Method

- Photos (public, CC0, raw.pixls.us): Nikon Z7 `3-Nikon-Z7-RAW-14bit-lossless-compressed-L.NEF` (45 MP; blue sky, wood grain, foliage; SHA-256 `f0e756db…8114`) and the 24 MP NEF of `scripts/bench/fetch_fixtures.sh` (`bayer-nef`).
- Each run: `rawtonic export PHOTO --output OUT.tif --tiff-bit-depth 16 --super-resolution --models DIR` (default recipe), its own process, `RAWTONIC_AI_PROVIDER=cuda|webgpu`. DIR holds the fp32 graph and the CPU conversion; the fp16 folder holds the fp16 graph too (the fp32 folder is what an older pack gives). The time is the CLI's `Super Resolution: … enlarged on … in N s` line (the model step, not the export's wall time; the GPU check of the file ran in an earlier process).
- Timing: three interleaved fp16/fp32 runs per provider, the host's quiet-measurement lock held, normal scheduling priority, machine load about 3.
- Pixels: `cmp16.py` (this folder) over the whole 16-bit output: PSNR, maximum, RMS, share of values above thresholds, and the same after rounding to 8 bits. Crops 192 px at 200 % (nearest): the three most detailed blocks, the flattest block of the upper third (Z7: sky) and the block with the largest difference, each as fp32 | fp16 | |difference| × 64.

## Results

Model step, 45 MP Z7 → 182 MP (16512 × 11008):

| Provider | fp16 (s) | fp32 (s) | fp16 / fp32 |
|---|---|---|---|
| CUDA | 15.38 / 15.45 / 15.46 | 22.30 / 22.13 / 22.17 | 0.69 (1.44× faster) |
| WebGPU | 29.07 / 29.05 / 28.78 | 40.37 / 40.54 / 40.35 | 0.72 (1.39× faster) |

24 MP NEF → 96 MP, one run each without the quiet lock, indicative only: CUDA 8.64 s vs 12.51 s; WebGPU (under heavy load from other work) 21.9 s vs 29.4 s.

16-bit exports, fp16 against fp32 on the same provider:

| Photo | Provider | PSNR | max (of 65535) | RMS | > 64 | 8-bit max | 8-bit values differing |
|---|---|---|---|---|---|---|---|
| Z7 | CUDA | 75.27 dB | 157 | 11.3 | 0.0003 % | 1 | 3.04 % |
| Z7 | WebGPU | 72.91 dB | 417 | 14.8 | 0.18 % (> 257: 0.0001 %) | 2 | 4.05 % |
| NEF | CUDA | 75.20 dB | 69 | 11.4 | 0 | 1 | 3.06 % |
| NEF | WebGPU | 74.20 dB | 251 | 12.8 | 0.003 % | 1 | 3.66 % |
| Z7, fp32 CUDA vs fp32 WebGPU (for comparison) | – | 83.95 dB | 271 | 4.2 | 0.009 % | 1 | 0.91 % |

Crops: no visible difference in any of the five crops per photo and provider; the × 64 difference panels are black to faint noise, the sky shows no banding.

Selection checks: with no forced provider the CLI chose CUDA with the fp16 graph, output byte-identical to the forced CUDA fp16 run; the fp32-only folder runs `ONNX Runtime (CUDA)`. The ignored test `fp16_gpu_graph_matches_fp32_graph` (`crates/superres/tests/real_model.rs`) gives a per-tile maximum of 0.00116 (CUDA) and 0.00328 (WebGPU) and asserts that `Model::onnx` picks the fp16 graph on a GPU.

## Not covered

Windows (CUDA plugin, WebGPU on Direct3D 12) and other GPUs; exports through the app's GUI (the app uses the same `Model::onnx`); more photos than these two.
