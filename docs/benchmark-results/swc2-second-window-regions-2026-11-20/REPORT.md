# SWC2: lossless 100 % frames on Linux with the second window open (2026-11-20)

On Linux both windows draw the photograph as images in their webviews. Since VPR1 (`../vpr1-linux-region-frames-2026-11-19/REPORT.md`) the main window's settled frame at 100 % and closer is a lossless PNG of the visible region, but only while the second window was closed. With it open, the whole photograph went out as a JPEG (quality 99, 4:4:4), which the second window showed too, and the second window's own 100 % region was a JPEG of the same quality with a quarter of its size as margin on each side. A measurement before this change (SWC1, same host and test set) found 45 of 80 fine-chroma cells out of the bar in the main window and 45–46 in the second window, against 7 with the second window closed, and about 0.8 s from a change to the main window's decoded frame.

This change keeps the main window's lossless region frame while the second window is open, and the second window develops its own frames losslessly (PNG): its 100 % region (no margin, as the main window's) and its fit frame when the main window sends none. Zoomed in, the second window develops the region first and the whole frame of the same edit after it. Display path only; exports untouched; macOS and Windows unchanged.

Host: Linux PC, Ryzen 5 3600X, RTX 3080 (Vulkan), KDE Plasma Wayland. Main window maximised on a 1920 × 1200 screen at scale 1 (viewer region 1596 × 1008); second window 1700 × 1000 on a 3840 × 2160 screen at scale 1.45 (its page draws 3400 × 1944 device pixels at 100 %). QA build of this branch with two measurement-only additions that were never committed: every frame the second window's page is served is also written to a scratch folder and logged with its place (the page has no QA channel), and an environment switch that restores the old transport in the same binary, for A/B runs in one session (old: whole JPEG to the main window while the second window is open; JPEG q99 regions with the quarter margin; a whole JPEG at 1:1 stands for the region; whole frame before region). The RAW files are private test fixtures and are not included.

Metric and test set as in VPR1 and `../lnxj1-linux-preview-transport-2026-11-17/REPORT.md`: four high-ISO RAWs, Sharpening, Luminance and Grain 0, Color noise reduction 0, 25, 50 and 100: 16 recipes × 5 chroma bands = 80 cells (60 for norm); out of bar below 0.85 or above 1.15. Reference: the CLI's 16-bit export of the recipe the app held (checked identical for all 16), cut at the same pixels, no resize (`analyze.py`). For a second-window region its central 2400 × 1500 pixels are analysed. "8-bit floor" is the export at those pixels rounded to 8 bits.

## 1. Fine chroma at 100 %

| case | frame | abs (80 cells) | norm (60 cells) |
|---|---|---|---|
| second window closed, main | region PNG 1596 × 1008 | 1.00–1.26, **7/80** | 0.99–1.03, 0/60 |
| 8-bit floor of that region | — | 1.00–1.26, 7/80 | 1.00–1.03, 0/60 |
| **second window open (at 100 %), main at 100 %** | region PNG 1596 × 1008 (before: whole JPEG, 45/80) | 1.00–1.26, **7/80** | 0.99–1.03, 0/60 |
| same, the second window's own region | PNG 3400 × 1944 (before: the main's whole JPEG, 45/80) | 0.83–1.30, **7/80** | 0.83–1.08, 3/60 |
| 8-bit floor of that region | — | 1.00–1.31, 6/80 | 1.00–1.04, 0/60 |
| second window at 100 %, main at fit: its own region | PNG 3400 × 1944 (before: JPEG q99 5100 × 2916, 46/80) | 0.83–1.30, **7/80** | 0.83–1.08, 3/60 |

The main window's frame is at the floor with the second window open: the same 7 cells are out as for the export rounded to 8 bits. The second window's region has the floor's 4 cells of the Z6 file's finest band, plus 3 cells of one D5500 file's coarsest band with colour noise reduction on (0.83–0.84). There the region has less colour noise than the export. That is not the transport: the PNG holds the developed pixels exactly. It is the second window's own development of the region.

**Same pixels in both windows?** (`windows_match.py`: the main window's region against the second window's region where both cover the photograph, 1596 × 1008 px, all at least 256 px from the second region's edges.) With Color 0 they differ by at most 1 code (58–60 % of pixels by 1). With colour noise reduction on, the mean difference is 0.38–0.97 codes, the p99 2–9 and the maximum 15–210 codes. The second window develops the region from a crop of the photograph (multi_view.rs `develop_region`), and its colour noise filter runs on that crop (the log shows the chroma pyramid built for the region's upload). The main window and the export run it on the whole photograph. So the two windows show the same pixels within 1 code without colour noise reduction, but not with it. This is a development difference that was already there, which the JPEG used to hide. It is not fixed here.

## 2. Settled-frame latency at 100 %

Twelve Exposure changes 2 s apart at 100 % on two RAWs per run (n = 25–26 per run), runs alternated closed / open / open / closed; then the old transport (same binary, switch on) open / open. From the change's request to the frame decoded: main window = request → reply + reply → loaded; second window = request → its page's first shown frame after it.

| run | main p50 / p95 | second window p50 / p95 | main frame |
|---|---|---|---|
| closed 1 | **88** / 435 ms | — | region PNG 3.5 MB, encode 17 ms |
| open 1 | **82** / 428 ms | 874 / 1076 ms | region PNG 3.5 MB, encode 20 ms |
| open 2 | **87** / 4314 ms | 858 / 1391 ms | region PNG, encode 19 ms |
| closed 2 | **74** / 446 ms | — | region PNG, encode 17 ms |
| old open 1 | 928 / 1200 ms | 685 / 937 ms | whole JPEG 22.2 MB, encode 74 ms |
| old open 2 | 848 / 1062 ms | 698 / 918 ms | whole JPEG 22.2 MB, encode 74 ms |

Open 2's p95 comes from one stall of about 12 s. In it the main window's render and the second window's region waited together, and a 1596 × 1008 PNG encode took 976 ms instead of about 18 ms. The cause was not found: the host is shared, and only batch drivers pause while a measurement runs.

The main window now meets VPR1's bar (≤ 107 ms) with the second window open, about 10 times faster than before. The second window is slower than before when both windows are at 100 % (median +175 ms): before, it showed the main window's whole JPEG; now it develops its own region. A typical cycle from the log: the second window waits 120 ms for the edit to settle (`SETTLE`). It then waits about 330–440 ms for its turn on the GPU (median wait 439 ms over the run, behind the main window's analytics pass and background jobs). It renders for about 60 ms (upload 15, colour noise pyramid 16, run 24), encodes the PNG in about 54 ms, and its page decodes and shows the 6.6 MP frame about 140 ms after that.

Second window at 100 % with the main window at fit (two runs each, about 32 main frames per run): the second window's own region is shown 670 / 659 ms after the request (median; p95 3878 / 3713 ms, first regions after opening a photo). With the old transport it was 833 / 812 ms (p95 2395 / 2917). Its develop takes 257 / 250 ms (median), against 248 / 286 ms before.

## 3. Drags with the second window open

Main window at 100 %, second window open at 100 %. Color and Exposure, 60 inputs at 60 Hz per drag, 8 drags per slider and RAW per round, each 0.6 s after the previous release (VPR1 method). Eight rounds alternated new / old / old / new …, each after a fresh launch. Metric: input → preview reply, p95 per drag.

| rounds | new: drags, mean / median p95 | old: drags, mean / median p95 | paired cell deltas new − old |
|---|---|---|---|
| 2–4 (1-min load 4–14) | 96, **20.01** / 18.0 ms | 96, **20.15** / 19.0 ms | median −0.75, mean −0.92 ms (12 cells) |
| 3–4 (load 4–9) | 64, 20.19 / 18.0 ms | 64, 19.83 / 19.0 ms | median −0.50, mean −0.62 ms (8 cells) |

Round 1 is left out. Its first RAW after launch gave drags with no preview reply in both versions (0–19 frames per drag), and the host load was 10–14. Including it, new 54.07 / 18.5 ms over 128 drags and old 23.77 / 19.0 ms over 125, both dominated by that cell. Drags are unchanged within ±0.5 ms on the mean of rounds 2–4 (−0.14 ms).

## Not covered

- The second window at fit while the main window is at 100 %. It now develops its own lossless fit frame (in the runs above that took 50–125 ms from its cached base) instead of showing the main window's whole JPEG. Its latency was not measured.
- What the compositor shows on the 1.45-scale screen: the page draws its 100 % region at 2 device pixels per logical pixel, and the compositor scales that.
- Pans in the second window. Without a margin, every pan past its region's edge develops a new region (cut from the kept base) once the pan rests.
- 2560 × 1440 and 4K main windows; macOS and Windows (unchanged).
