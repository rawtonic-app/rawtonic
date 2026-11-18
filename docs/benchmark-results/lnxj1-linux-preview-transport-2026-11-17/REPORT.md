# LNXJ1: the Linux preview transport and fine colour noise (2026-11-17)

On Linux the Develop view is an image in the webview, while macOS and Windows present natively. The settled fit frame used to be a 4:4:4 JPEG at quality 94. This report measures what that transport does to a noise-reduced photo's fine colour noise, which encoding meets the ±15 % bar against the export, and what it costs. Host: Linux PC, Ryzen 5 3600X, RTX 3080 (Vulkan), KDE Wayland; QA build of the app; CLI exports for reference. The RAW files are private test fixtures and are not included.

## Metric (`chroma_metric.py`)

The metric is the colour noise in a frame's fine detail. For each frame, take its chroma (Cb and Cr of the sRGB-encoded values, BT.709), apply a Hann window, and add up the power spectrum in five radial bands: 0.1–0.2, 0.2–0.3, 0.3–0.4, 0.4–0.5 and 0.5–0.75 cycles/px. The reference is the CLI's 16-bit TIFF export of the same recipe, reduced to the frame's size with Lanczos in linear light (ImageMagick). The ratios are:

- **abs**: frame / reference.
- **norm**: (frame at recipe r / frame at Color 0) / (reference at r / reference at Color 0).

The test set is four high-ISO RAWs: a Sony A7C at ISO 12800, two Nikon D5500 files (one at ISO 6400) and a Nikon Z6. Each was rendered with Sharpening 0 and Luminance 0, at Color noise reduction 0, 25, 50 and 100. That gives 16 recipes × 5 bands = 80 cells for abs and 60 for norm. A cell is out when its ratio is below 0.85 or above 1.15.

## 1. The transport alone (`transport.py`)

Each option encodes the 8-bit reference and decodes it again; the table compares that against the float reference.

Fit (1536 × 1024):

| option | abs range | cells out |
|---|---|---|
| JPEG q94 4:2:0 | 0.01–0.71 | 80/80 |
| **JPEG q94 4:4:4 (before)** | 0.45–1.27 | 33/80 |
| JPEG q96 | 0.60–1.45 | 25/80 |
| JPEG q97 | 0.82–1.59 | 39/80 |
| JPEG q98 | 1.01–1.77 | 45/80 |
| JPEG q99 | 1.00–2.07 | 39/80 |
| JPEG q100 | 1.00–1.95 | 27/80 |
| **PNG (lossless 8-bit)** | **1.00–1.15** | **0/80** |

No JPEG quality meets the bar. Up to q96 the JPEG removes fine colour noise. From q97 on it adds noise: the 8-bit YCbCr and DCT rounding puts in noise that the noise-reduced export does not have. Only lossless passes.

At 100 % (2048 × 1536 centre crops, `transport.py EXPDIR 1to1`): q94 0.69–3.14 (28/80), q99 1.00–5.17 (45/80), and lossless 8-bit 1.00–1.76 (10/80). At full resolution the noise-reduced export's fine chroma is below 8-bit quantisation, so 8 bits itself misses the bar there. That is true for any 8-bit display path, native ones included.

## 2. Cost (Rust encoder bench, host under load; app log for 6000 × 4000)

| frame | JPEG q94, 6 strips | PNG (fast, Up filter, RGB), 1 thread | PNG bytes |
|---|---|---|---|
| 1536 × 1024 | 1.6 ms, 0.5 MB | 8–11 ms | 2.4–2.8 MB |
| 2560 × 1440 | 4 ms, 1.6 MB | 22–27 ms | 6.9–7.7 MB |
| 3840 × 2160 | 8–9 ms, 2.4–3.6 MB | 50–59 ms | 12.6–17.7 MB |
| 6000 × 4000 (100 %: the whole photo) | q99: 46–55 ms, 26 MB | — | ~62 MB; decode about twice the JPEG's |

A strip-parallel PNG encoder (deflate per strip, joined with sync flushes) was no faster than the single-thread one here, so it was not built.

## 3. Choice

- **Settled reduced frames are lossless PNG**, including a split's before and multi-view cells.
- A split's before follows the after it is compared with (`encode_settled_like`), so both sides of a comparison are always encoded the same way.
- Drag frames keep their JPEG.
- The 100 % frame stays a q99 JPEG. A lossless 24 MP whole-photo frame costs about 62 MB and twice the decode, and at 100 % the 8-bit floor misses the bar anyway.

## 4. In the app (`inapp.py`)

The same QA binary was run twice, once with a temporary switch back to the old q94 JPEG; the switch was removed before the commit. For each recipe the settled fit frame (1536 × 1024) was read after the colour-noise build finished, through the QA control's `editor.preview_image`. It was compared with the CLI export of the same recipe.

| transport | abs (80 cells) | norm (60 cells) |
|---|---|---|
| JPEG q94 (before) | 0.49–1.36, 39 out | 0.49–1.14, 21 out |
| **PNG** | **0.85–1.16, 2 out** | **0.92–1.15, 0 out** |

The two cells left are at Color 0 (D5500 ISO 6400, 0.2–0.3 cycles: 1.16; the other D5500, top band: 0.85). These are the same values the native pipeline gives without any transport, i.e. the 8-bit render floor.

### Timing

**Settled frame.** Twelve Exposure changes 2 s apart on each of two photos, 1536 × 1024 view. "Request → reply" is the frontend's `preview_final_roundtrip`. "Reply → decoded" is the frontend's `browser_preview_load`, the decoded image being available; it does not include display scanout.

| | PNG | JPEG q94 |
|---|---|---|
| encode (app log), p50 | 13.0 ms | 3.1 ms |
| frame bytes, p50 | 2.6 MB | 0.52 MB |
| request → reply, p50 | 55–57 ms | 41 ms |
| reply → decoded, p50 | 26 ms | 15 ms |

So the decoded frame is ready about 25 ms later.

**Drags.** Four rounds in alternating order (PNG, JPEG, JPEG, PNG). Each round dragged the A7C ISO 12800 and the D5500 ISO 6400 file, on Color and on Exposure: 8 drags of 60 inputs at 60 Hz each. Drags started 0.6 s apart, so each one began right after the previous release's settled frame. The value per drag is its preview_input p95 (input → preview reply). Comparing the median of each cell (round × photo × slider), PNG − JPEG ran from −2.0 to +3.5 ms, with a **median of +0.25 ms and a mean of +0.28 ms** (16 cells, 128 drags per mode). Frames per drag were unchanged.

## Open

- The 100 % view on Linux: lossless would need viewport-sized region frames, and the 8-bit floor misses the bar there on every platform.
- Settled frames above 1.5 MP were costed by the bench only, not measured in the app.
