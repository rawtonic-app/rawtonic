# VPR1: lossless region frames at 100 % on Linux (2026-11-19)

On Linux the Develop view is an image in the webview. At 100 % and closer the settled frame used to be a JPEG (quality 99, 4:4:4) of the whole photograph. This report measures what that did to fine colour noise against the export, what a lossless frame of only the visible region does instead, and what each costs. Host: Linux PC, Ryzen 5 3600X, RTX 3080 (Vulkan), KDE Plasma Wayland, 1920 × 1200 screen; QA build of the app; CLI exports for reference. The RAW files are private test fixtures and are not included.

Metric and test set as in `../lnxj1-linux-preview-transport-2026-11-17/REPORT.md` (`chroma_metric.py` there): chroma power in five bands 0.1–0.75 cycles/px; abs = frame / reference, norm = (frame at r / frame at Color 0) / (reference at r / reference at Color 0); four high-ISO RAWs (Sony A7C ISO 12800, two Nikon D5500 files, Nikon Z6), Sharpening, Luminance and Grain 0, Color noise reduction 0, 25, 50 and 100: 16 recipes × 5 bands = 80 cells (60 for norm). A cell is out when its ratio is below 0.85 or above 1.15. At 100 % there is no resize: the reference is the export's own pixels.

## 1. The transport alone (`region_ceiling.py`)

Centre crops of the 16-bit export at 100 %; each option encodes the crop's 8-bit copy and decodes it.

| crop | option | abs range | cells out |
|---|---|---|---|
| 1920 × 1080 | JPEG q99 4:4:4 (before) | 1.00–5.10 | 45/80 |
| | **PNG (lossless 8-bit)** | **1.00–1.73** | **10/80** |
| 2560 × 1440 | JPEG q99 4:4:4 | 1.00–5.26 | 45/80 |
| | **PNG** | **1.00–1.80** | **10/80** |

**The 8-bit floor.** The lossless frame's out cells are the Z6's two finest bands at every Color setting and one D5500's finest band at the higher settings. There the export's colour noise is smaller than one 8-bit step, so any 8-bit copy of it (a PNG, or the 8-bit surface of a native display) adds rounding noise of that size and the ratio rises above 1.15. No 8-bit transport does better at 100 %. When the frame is reduced (fit view), the reduction averages that rounding noise away, which is why the fit view reaches 0–8/80.

Encode cost (Rust, the app's encoders, indicative on a shared host): a region PNG (fast compression, Up filter) of 1920 × 1080 took 22–32 ms and 2.6–4.5 MB, of 2560 × 1440 26–44 ms and 4.6–7.7 MB; decoding it 15–29 ms. The whole-photograph JPEG took 43 ms and 11.6 MB at 24 MP, 67 ms and 17.9 MB at 45 MP; a whole-photograph PNG would take 186 ms and 28 MB at 24 MP. The region's size depends on the window, not the camera.

## 2. What changed

At 100 % and closer, a settled (not dragged) frame renders only the visible region and sends it lossless; the last whole frame stays under it. A zoom, pan or window change that uncovers more asks for the new region. Before/After (split and side by side) shows the Before as the same region, lossless, over a fit-size whole Before. The histogram, scopes and clipping marks come from a whole-frame render at full resolution after the region frame went out, as before. While the second window is open, the whole photograph is still sent as a JPEG (the second window shows it). Drags are unchanged. No margin is added around the visible region: after a pan comes to rest the new region is on screen about 76 ms after it is asked for, while a 256-pixel margin would make every settled frame about 56 % larger.

## 3. In the app (`inapp100.py`)

Settled 100 % frames taken through the QA control channel after each Color change, against the CLI export of the recipe the app held, cut at the same pixels. "8-bit floor" is that export crop itself rounded to 8 bits. The old whole-photograph frame is cut at the same place.

| frame | abs (80 cells) | norm (60 cells) |
|---|---|---|
| JPEG q99, whole photograph (before) | 1.00–5.13, **45/80** | 0.85–1.29, 5/60 |
| **PNG, visible region (now)** | **1.00–1.73, 11/80** | **0.91–1.03, 0/60** |
| 8-bit floor (export rounded to 8 bits) | 1.00–1.71, 11/80 | 0.91–1.03, 0/60 |

The region frame is at the floor: the same 11 cells are out for an exact 8-bit copy of the export. On screen, the single view at 100 % showed the region frame pixel for pixel (1570 × 980 pixels compared, no difference).

**Settled-frame latency** (12 Exposure changes at 100 % on two RAWs per run, runs alternated; viewer region 1596 × 1008): from the change to the decoded image, median 96 and 102 ms with region frames (PNG 3.5 MB), 853 and 936 ms with the whole-photograph JPEG (22–26 MB, of which ~0.4 s decoding in the webview).

**Drags** at 100 % (Color and Exposure, 60 inputs at 60 Hz, 8 drags per slider and RAW per round, 8 alternating rounds, 128 drags per version; input-to-preview p95 per drag): with another process loading the host (1-minute load 9–13) the old whole-photograph frame delayed the drags that started after it (mean p95 189 ms against 28 ms); on the calmer rounds (load 8–9) both versions had a median p95 of 19.0 ms (means 21.09 and 20.75 ms, paired median difference 0.0 ms).

**The whole-frame analytics pass** (added after review: the histogram, scopes and clipping marks of a settled 100 % frame come from a whole-frame render at full resolution, the same render the whole-photograph frame used, without its encoding and transfer; it gives way to any newer request): about 239 ms per settled frame (median, p95 507 ms, 52 frames), after the region frame went out. Settled-frame latency with and without it, runs alternated: median 96 and 166 ms with, 102 and 154 ms without (host load 11–14). Drags with and without it (same design, 128 drags each) were dominated by another process loading the host (single cells from 18 to 234 ms in both versions; median p95 19.0 ms with, 41.5 ms without), so they show no cost but cannot resolve ±0.5 ms. The pass does part of the work the whole-photograph frame did after every change, and the drags after that frame were measured above.

## Not covered

2560 × 1440 and larger windows and HiDPI rendering were costed by the encoder benchmark only. The second window's own 100 % view was not measured. Exports are unchanged by construction (display path only).
