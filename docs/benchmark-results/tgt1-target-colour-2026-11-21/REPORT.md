# TGT1: target colour for colour editor ranges (2026-11-21)

How close the colour editor's target solve (`src/utils/colorTarget.ts`) brings a picked colour to its target in a real export. One machine (Mac mini, Metal), the debug CLI of this branch.

## Method

- Photos (public, CC0, raw.pixls.us; `https://raw.pixls.us/data/<path>`): c0108 `Canon/EOS Kiss M/IMG_2695.CR3` (portrait, red sash), c0398 `NIKON CORPORATION/NIKON Z fc/Nikon_Z_fc_14bit_uncompressed.NEF` (red watering can, terracotta pot), c1182 `Sony/DSLR-A100/_DSC0258.ARW` (orange and blue toy robots), c0402 `Nikon/1 J1/_DSC8140.NEF` (teal beads), skies c0428 `Nikon/D2Hs/DSC_5858.NEF`, c0519 `Nikon/D800/D800-14b-lossless-comp.NEF`, c0320 `Fujifilm/XQ2/DSCF4186.RAF`, c0326 `Leica/C-Lux/_1001076.RWL`, c1015 `Panasonic/DMC-LF1/image-20170106_111000-1.rw2`.
- Nine photos. Each exported once with the default recipe: `rawtonic export PHOTO --output BASE.tif --adjustments empty.json` (`empty.json` holds `{}`; 16-bit sRGB TIFF).
- 12 picks (positions in `measure.ts`): 1 skin, 6 product colours, 5 skies. Source = the mean of a block of max(11, width/200) px at the pick (the app's picker averages 11 × 11 preview pixels), in sRGB.
- A colour range at the source with the editor's default shape; 5 targets per pick, fixed before measuring: CIELAB D50 LCh offsets from the source (h+12° C×1.15; h−20° C×0.8 L−6; h+5° C×1.3 L+5; h+80° = beyond the ±50° hue reach on purpose) and one same-family sRGB hex colour.
- `solveColorTarget` (the app's code) → recipe `{"colorRanges":[range with the solved shifts]}` → export → the same block sampled → ΔE00 (CIELAB D50) to the target. "Loop" (an experiment, not in the app): the target corrected by the measured error and solved again, once.
- `measure.ts` (this folder; Node 24 runs it with `RAWTONIC_REPO`, `TGT1_SCRATCH`, optional `RAWTONIC_CLI`; each run writes only into a new `runs/<time>/` folder). Every target in `results.json`: `shifts`, `predictedDE`, `measuredDE`, `got` and `renderMs` belong to the first solve; `loop` lists the measured ΔE00 of the first solve and of two closed-loop passes.

## Results

| Group                                                                                  | Targets | Measured ΔE00 median / max | ≤ 2 | After one loop step: max / ≤ 2 |
| -------------------------------------------------------------------------------------- | ------- | -------------------------- | --- | ------------------------------ |
| Sky, in reach                                                                          | 14      | 0.09 / 0.18                | 14  | 0.26 / 14                      |
| Skin, in reach                                                                         | 3       | 0.15 / 0.15                | 3   | 0.16 / 3                       |
| Product colours, source and target inside sRGB, in reach                               | 9       | 1.45 / 2.42                | 8   | 1.19 / 9                       |
| Product colours whose sample is clipped (a channel at 0 or 255: toy robots, teal bead) | 5       | 9.5 / 12.7                 | 0   | 23.0 / 0                       |
| Target outside sRGB (the readout flags it)                                             | 11      | 3.0 / 16.4                 | 3   | 22.4 / 4                       |
| Out of reach: a slider at its limit (the readout says which)                           | 18      | 12.7 / 27.0                | 1   | 25.3 / 1                       |

- Sky, skin and in-gamut product colours: 25 of 26 in-reach targets within ΔE00 2. The worst (2.42) is the textured, noisy red can, where the mean of the shifted pixels differs from the shifted mean. The readout's prediction agreed with the export within 0.08 on skies and 0.02 on skin.
- Clipped picks: the picker reads the 8-bit display frame, so a colour beyond sRGB is sampled at the edge of sRGB and the range centre is not the pixels' own colour (the blue robot is Lab 17.7, 86.5, −135.7 in a ProPhoto export). The real pixels move differently and are clipped again at output. The readout warns ("at the edge of sRGB: check the result in the photo") but cannot predict these. Fixing it needs an unclipped sample (open).
- Reach: ±50° hue never limited a same-family target. The lightness reach (×0.6–1.4) limited only a skin tone moved to a much lighter one (ΔE00 26 apart), and saturation and lightness limited a dark teal bead moved to a bright teal (ΔE00 30). The in-reach misses come from clipped samples (5) and from a textured, noisy patch (the red can, 2.42), not from reach.
- Closed loop helps textured in-gamut colours (2.42 → 1.19) but oscillates on clipped samples and runs away at a slider limit; not built.

Readout limits the app states instead of measuring: a mask layer range is shown as its full-strength prediction (the frontend has no mask weight at the pick), and a range picked again through a strong change of its own (the sample is undone through the ranges, `colorBeforeRanges`; where that multiplies an 8-bit sample error by more than 1.5, `inverseGain`, e.g. Saturation −50 or less) is marked uncertain; near grey several colours end the same, so a strong desaturation can still lead the inverse to a different colour below that gain (review round 3: an sRGB 126/122/119 patch at Saturation −90). Both need the colour entering the colour editor and the mask weight at the pick, sampled by the renderer (open, like the unclipped sample above).

Covers: global colour ranges with the default shape, one sRGB export per target, the CLI render (the app's export path). Not covered: mask layer ranges in a render (the readout says it predicts full layer strength), tools after the colour editor (curves, color grading, B&W are not in the prediction), Display P3 previews, the time from typing to a settled preview, and Capture One's typed target on the same picks: the side-by-side comparison with Capture One (the editor with typed targets) is open.
