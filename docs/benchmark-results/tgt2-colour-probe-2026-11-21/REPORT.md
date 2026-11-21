# TGT2: renderer probe for the colour editor's target solve (2026-11-21)

Follow-up of `docs/benchmark-results/tgt1-target-colour-2026-11-21`. TGT1 solved a range's shifts from the 8-bit colour the display shows at the pick. Three gaps were left open:

1. A range picked again through a strong change of its own could not be inverted reliably from the 8-bit sample (Saturation −90).
2. Layer ranges in feathered masks were only predicted at full strength.
3. Saturated product colours beyond sRGB missed by ΔE00 4–13, because the picker reads them clipped.

## What changed

- **Probe** (shader `color_range_probe_pixel`, `MIXER_PROBES` display variants only; Tauri `sample_color_range_input`). It renders the picker's 11 × 11 preview block from the cached preview base, as the colour mixer's probe does. For each pixel it stores the colour entering the photo's colour editor or a mask layer's. For a layer, that is the colour after the photo's ranges and the layers below, plus the layer's mask influence. The colour is linear ProPhoto clipped to the working gamut (the colour editor's own `gamut_unit`), encoded as sqrt over 16 bits in two 8-bit renders: high bytes with alpha 0 as the probe mark, then low bytes with the influence in alpha. Export pipelines do not compile the probe, so they store Levels' input there and the frontend falls back to the display sample. The probe is its own block before the colour ranges. The colour-range code after it is unchanged text (see "Exports" for why).
- **Pick** (`pickColorRange`): the range takes the mean of the probe's colours, with display values averaged as the pickers average. It does not undo the ranges from the shown colour, and nothing is uncertain. The point, the colour it set and the sample are kept per range as view state (`colorRangeInputs`, never in the recipe).
- **Solve** (`solveColorTarget(…, input)`): the model is every probe pixel through the ranges, each layer's pixel mixed by its influence as `main` mixes display values. Results are averaged as the pickers average, in the working gamut, with no sRGB clip. The closed form at the mean colour and influence is followed by a whole-step local search on that full model. The coarse grid runs on a one-pixel stand-in. A typed or picked target on a range picked in the photo first samples that point again, because earlier tools or the mask may have changed. The edit is applied once the sample is back.
- **Readout**: it uses the same model and samples the pick again 250 ms after any edit settles, so earlier tools and mask edits are followed. A target beyond sRGB says that an sRGB export maps the colour into sRGB; that mapping is not part of the prediction. A target beyond the working gamut (ProPhoto) says so (new). A layer whose mask no longer covers the pick keeps the range's shifts and says so (new). A pick stops counting when the photo's framing changes (crop, orientation, warp), because its normalised point then lies on other pixels. The full-strength wording and the "edge of sRGB" warning remain only for ranges without a valid probe sample, for example ranges moved on the wheel.
- **Search**: when the closed form plus the local search miss the target, a coarse grid over all three sliders follows, evaluated on up to nine of the probe pixels, then the local search on the full model. A slider counts as run out only when the target stays out of reach (review round 1: a patch across a red/green edge on a feathered layer was reported pinned at Hue −100 while reachable).
- **Changed semantics:** TGT1 counted targets at the sRGB edge as reached when the model's per-channel clip put them there (#FF9600 from a pink, #C8006A, #00ECFA; review rounds 1–3). The renderer maps colours beyond sRGB into the output instead of clipping each channel, so the solve now works in the working gamut and shows those targets' real distance. The tests were rewritten accordingly.

## Method

The `measure.ts` folder layout is as in TGT1, with the same 12 picks in 9 public raw.pixls.us RAWs and the same 5 targets per pick, built from the pick's own colour. The probe is stood in for by CLI exports:
- **Colour:** a 16-bit ProPhoto export, with a range that changes nothing, of the block of max(11, width/200) px at the pick, averaged into 11 × 11 cells. ProPhoto output keeps the working gamut. The range that changes nothing makes the export clip what the colour editor receives as the probe does. An export without ranges hue-maps colours beyond the working gamut at the output instead: the blue robot's pixels lie beyond ProPhoto, and a first run without that range missed it by ΔE00 14–22.
- **Influence:** the change a layer range makes, divided by the change the same range makes on the whole photo. The layer is a linear gradient through the pick, with its 50 % line at the pick or half a range above it.
- **Measured:** the same cells of the solved recipe's ProPhoto export ("working", the colour the photo holds) and the block mean of its sRGB export ("sRGB", for targets inside sRGB). ΔE00 is in CIELAB D50.

Each run writes only into `runs/<time>/`. A second run reproduced every number. After review round 1 changed the search, a third run (`results.json`) changed only limit flags (reached targets are no longer marked as at a limit) and two out-of-reach results (31.7 → 24.4, 2.12 → 2.08).

## Results (60 global targets, 20 layer targets)

| Group | n | ΔE00 median / max | ≤ 2 |
|---|---|---|---|
| Prediction vs export, all global targets | 60 | 0.00 / 0.46 | 60 |
| Product, target inside sRGB, in reach: sRGB export | 11 | 0.18 / 1.54 | 11 |
| Product, source and target inside sRGB (TGT1 grouping), working / sRGB | 7 | 0.14 / 0.58; 0.22 / 1.54 | 7 |
| Product, source beyond sRGB, in reach (working) | 15 | 0.15 / 3.56 | 13 (both misses: target beyond the working gamut, flagged) |
| Target beyond sRGB, inside the working gamut, in reach (working) | 11 | 0.12 / 0.17 | 11 |
| Sky, in reach | 17 | 0.10 / 2.21 | 16 (miss: target beyond the working gamut, flagged) |
| Skin, in reach | 3 | 0.14 / 0.15 | 3 |
| Out of reach (a slider at its limit; flagged) | 17 | 12.7 / 24.4 | 0 |
| Feathered layer (influence 0.50–0.75), solved at the influence, in reach | 17 | 0.10 / 3.70 | 16 (miss: target beyond the working gamut, flagged); prediction vs export ≤ 0.18 |
| All 20 layer targets: at the influence / at full strength (TGT1) | 20 | – | 17 / 6 |

- Bar "in-sRGB products ≤ ΔE00 2": met. All 11 land within 2 in the sRGB export. TGT1 met it for 8 of 9, worst 2.42; the textured red can is now 0.58 working / 1.54 sRGB.
- Bar "beyond-sRGB targets reach the gamut-mapped nearest within 2 or are flagged": met. Every target inside the working gamut is reached within 0.17 in the working colour. An sRGB export shows that colour through the renderer's output mapping, which is the same for every colour; that mapping was not measured separately here. Every in-reach miss has its target beyond the working gamut, and the readout flags it and predicts it within 0.01.
- Bar "re-picks through strong changes exact": met by construction and checked. The probe is before the ranges. The GPU test asserts byte-equal probe pixels with and without a Saturation −90 / hue +80 range. In the QA build, a re-pick of the blue robot through Saturation −90 left the range's hue, chroma and lightness bit-identical and set no uncertainty flag.
- Bar "feathered-mask layers correct at the actual weight": met. 16 of 17 in-reach targets are within 2, median 0.10, and the miss is flagged. Of all 20 layer targets, 17 land within 2 when solved at the influence and 6 when solved at full strength.

## Exports

- On the Mac mini (Metal), the preservation matrix of the notes repo has 280 CLI exports (10 RAWs × 28 recipes; the 326-export matrix includes raster sources on the laptop). Base `09402edf` against this branch: 280/280 byte-identical.
- None of those recipes has colour ranges, so a second set compared 40 more exports (10 RAWs × {legacy recipe, legacy + global ranges, global ranges, ranges + a layer with ranges in a linear gradient}): 40/40 byte-identical.
- The first version of the shader change broke byte identity for the 10 `raw.legacy` exports (general shader). That version had put the probe into the colour-range loop's condition, and Metal compiled it differently. Moving the probe into its own block made all exports identical.

## QA build (macOS, `cc.rawtonic.qa.tgt2`, scratch session)

- A pick on the blue robot stored 121 probe pixels at influence 1. The range came out at Oklab hue 256.5°, chroma 0.288 and L 0.460.
- Saturation −90 followed by "pick the range colour again" at the same point left the range's colour identical, with `colorPicksThroughRanges` empty.
- Typing #0057B8 gave shifts 2 / −41 / 8 and the readout "Ziel erreicht (ΔE00 0.1)". That recipe, exported by the CLI and sampled as above, is at ΔE00 0.67 from #0057B8 in sRGB. TGT1 reached 10.98 for this pick.
- The QA bundle was built before one later frontend change: a typed target now applies to the ranges current when the sample returns. That change is covered by the unit test.

## Not covered

- Tools after the colour editor (curves, colour grading, B&W, vignette) are still not in the prediction.
- The output mapping of colours beyond sRGB is not modelled. The working colour is reached, and each output maps it.
- For a layer, pixels beyond the working gamut before the colour editor enter the mix unclipped, while the probe clips them.
- The probe's preview resolution differs from an export's.
- Vulkan/Windows: not run. The probe uses only constructs the mixer probe already uses there.
- The probe's latency in the app was not timed: two small renders of the 11 × 11 block per pick, off the UI thread.
- No side-by-side comparison with Capture One's typed target.

## Files

- `measure.ts`: the measurement (Node 24 type stripping; `RAWTONIC_REPO`, `TGT2_SCRATCH`, `RAWTONIC_CLI`).
- `results.json`: every target, with shifts, limits, predicted and measured ΔE00, the sRGB export's ΔE00 and prediction vs measured.
