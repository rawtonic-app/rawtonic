# Auto straighten: measurement and remaining gaps

Implementation: `crates/engine/src/auto_straighten.rs` (estimator, crop
reframing), `src-tauri/src/auto_straighten.rs` (one click: crop panel Auto,
Shift+R, the editor's context menu). Batch Auto straighten for a selection is
not part of this version (user decision 2026-10-19: one click first; the batch
needs its own design for photos open in the editor). Measured 2026-10-18 on the
Mac mini M4 Pro. Inputs are public photographs (Wikimedia Commons Quality
Images, raw.pixls.us CC0 frames); they and the scripts are not checked in.

## Method

- **Set A, synthetic (62):** 31 level photographs (horizon 9, parallel
  verticals 10, interior 12), each rotated by two seeded angles in
  ±[0.5°, 8°] and cropped inside so no border shows. Truth = the applied
  rotation (residual tilt of the source checked ≤ 0.1°). Split by source:
  even = tuning, odd = held-out (30).
- **Set B, real tilted frames (21):** uncropped camera frames; truth from a
  true horizon or from the vertical vanishing point seen from the frame
  centre (≥ 5 lines, ≥ 0.4 diagonal of support), constructed by hand and
  checked visually. All held-out.
- **Set C, no structure (16):** landscapes without horizon, portraits,
  macro, foliage. Harm = an answer beyond 1°.
- Failure = no answer, |error| > 1°, or the wrong direction.
- **Thresholds** (support, line length, the horizon/vertical consistency
  rule) were set in four rounds against failures on set A's tuning half.
  The held-out half and set B were scored at every round as well, so they
  are not strictly blind; the numbers below are on them.
- **References**, the same pixels: Lightroom Classic's crop tool **Auto**
  (scratch catalog, one click per photo, angle read from the catalog) and
  Camera Raw's Upright **Level** (linear DNG of the same pixels opened through
  Photoshop). Capture One's Auto Adjust rotation was not measured.

## Results (held-out A + B, n = 51)

| | median | p90 | failures | harm (C) |
|---|---|---|---|---|
| Rawtonic | 0.06° | 0.46° | 4 (7.8 %) | 0 / 16 |
| Lightroom crop Auto | 0.18° | 0.42° | 4 (7.8 %) | 5 / 16 |
| Camera Raw Upright Level | 0.18° | 0.67° | 3 (5.9 %) | 2 / 16 |
| Upright's level (length-weighted median, the inherited baseline) | — | — | A 6.7 %, B 52 % | 5 / 16 |

Per set: A held-out Rawtonic 0.06 / 0.31 / 6.7 %, Lightroom 0.15 / 0.39 /
3.3 %, Camera Raw 0.14 / 0.51 / 0 %; B Rawtonic 0.06 / 0.59 / 9.5 %,
Lightroom 0.26 / 0.75 / 14.3 %, Camera Raw 0.32 / 1.05 / 14.3 %. A
RapidRAW-v1.6.6-style estimator (studied, not ported) failed 10 % on A
held-out and 33 % on B.

Speed (Rawtonic only): click → recipe 276–291 ms on a 45 MP photograph
(the editor's QA channel: IPC + analysis + reframing, not pointer-to-photon);
crop validity and reframing about 105 ms at 45 MP (release build,
`frame_validity_time_45mp`, `time_*` ignored tools).

## Against the bar — not met yet

Better median and harm than both references; p90 0.04° above Lightroom's and
one failure more than Camera Raw Level (within the noise of 51 photographs,
still not ≤). Not measured: Capture One, crop preservation side by side,
one-click screen recording against the references; no batch. Set B's truths are hand constructions and its horizon class has
one photograph.
