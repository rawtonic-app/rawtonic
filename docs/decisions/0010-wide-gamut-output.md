# ADR 0010 — One wide-gamut rendering for every output
Status: accepted, 2026-10-09 (revised the same day: a first version kept a separate sRGB pipeline).

## Starting point
Exports were sRGB only, and the sRGB gamut was enforced long before the encoder. The decode clipped linear sRGB at zero (rawler's colour step, Rawtonic's own demosaic path and the normalisation), and so did the RAW preprocessing's colour filter. The shader clipped again in about thirty places: at the tone map's entry, after its saturation and look tables, in the colour tools (saturation, vibrance, colour mixer, local hue, defringe, dehaze, colour noise reduction) and in every display-referred tool (curves, levels, grading, colour editor, vignette). The look tables were fitted against Camera Raw's sRGB renders, which are clipped exactly where colour is most saturated.

## Decision
The working values stay linear sRGB primaries, so every calibrated tool keeps its arithmetic, but their gamut is ProPhoto. Colours beyond sRGB are negative channels; display values encode them with the sRGB curve mirrored below zero. Every output is this one rendering, clipped only at the end (`encode_output`): sRGB, Display P3, Adobe RGB (1998) and ProPhoto RGB exports, and the display. Camera Raw does the same with its ProPhoto rendering.

- **Decode.** Negative channels are kept and limited to the ProPhoto gamut (`rawdecode::gamut`, rawler `RawDevelop::keep_negative`, the preprocessing's colour filter).
- **Shader.** Each former clip is a gamut helper (`gamut_floor`, `gamut_unit`, `display_to_linear`, `linear_to_display`, `display_unit`). The Camera Raw look's tone curve works in ProPhoto, as Camera Raw's does. Per-channel tools (point curves, levels, region curve, LUTs, per-channel Whites/Contrast, the Classic look's curve) act on the colour clipped to the sRGB cube; the part beyond it is carried along at the tool's luminance ratio (`carry_beyond_srgb`, `carry_scene`). AgX keeps the ProPhoto gamut instead of lifting negative channels with grey.
- **Look.** The look residual's chroma and hue (`LOOK_RESIDUAL_CHROMA`/`TURN`) were refitted against Camera Raw's 16-bit ProPhoto renders of 281 in-scope camera models, with the same bins and on top of the old table.
- **Files.** JPEG, PNG, TIFF and WebP carry the space's version 2 matrix/TRC profile, built in `icc_profile.rs` from the published primaries, whites and curves. JPEG XL declares the space in its header: lossless keeps the rendered values; lossy hands the encoder linear values in the space's primaries, and because the encoder ignores custom white points when it derives XYB, ProPhoto's D50 values are adapted first. AVIF gets an ICC `colr` property and, with metadata, an XMP item (`avif_container.rs`); the encoder wrote neither, so until now AVIF exports also lacked the IPTC marking of AI-generated areas.
- **Display.** On macOS the Metal layer is tagged Display P3 and display frames are encoded in P3 (`GpuContext::display_space`), so a P3 panel shows colours beyond sRGB. Core Animation converts for other monitors, as WebKit does for the interface. Elsewhere the display stays sRGB.
- **Soft proofing.** A view state (Proof in the histogram bar, Shift+S; never stored in recipes): the display shows the colour a chosen output space keeps, optionally with Lightroom's destination gamut warning in red.

Rejected:
- A ProPhoto (or Rec. 2020) working space for all tools: it would change every calibrated tool's response inside sRGB, and the per-channel tools would need refitting.
- Keeping a separate sRGB pipeline, the first version of this ADR: there the sRGB export clipped before the look tables, and on held-out photos it was further from Camera Raw than clipping the wide rendering at the end. It was also a second rendering path to maintain.

## Evidence (2026-10-09)
Camera Raw references: Photoshop 2026 / Camera Raw default renders (Adobe Color), 16-bit ProPhoto, 1200 px, of one photo per in-scope model (raw.pixls.us, 285 models, 281 rendered by both). Metric: Oklab ΔE × 100, median per photo after ECC alignment at 400 px, mean over photos; "beyond" = pixels Camera Raw renders outside sRGB (117 photos).

| | before | after |
|---|---|---|
| sRGB export vs Camera Raw clipped to sRGB | 0.937 | 0.908 |
| … beyond sRGB | 1.600 | 1.602 |
| ProPhoto export vs Camera Raw ProPhoto | 0.958 | 0.924 |
| … beyond sRGB | 2.616 | 2.067 |

- sRGB: 119 photos better by more than 0.02, 23 worse; the table fitted on even model ids and tested on the odd ones gave 0.931 → 0.909.
- Before the refit our saturated colours carried about 8 % more chroma than Camera Raw's, inside sRGB as well; clipping hid it in sRGB exports.
- Timing: exports are unchanged within noise (five full-size 16-bit TIFFs 4.67 s → 4.77 s). Preview frames of the general pipeline take 3–8 % more GPU time (clock-dependent probes); specialised display variants are unchanged within the probe's 1.56 ms steps, except colour-range and defringe frames (one step).
- Profiles convert like moxcms's Display P3, Adobe RGB and ProPhoto profiles within one 8-bit code. JPEG XL files decode to their values (lossless exactly; lossy checked with libjxl's djxl, mean error 0.005–0.016). A P3 AVIF decodes with libavif to the P3 TIFF's values (mean 1 code, lossy); libavif reports its ICC, XMP and EXIF, and ImageIO its profile.
- Found on the way: Vibrance turned some saturated reds and oranges grey (a NaN from `pow` of a negative base), 0.1 % of an EOS R5 frame; fixed.

## Follow-up (2026-10-09, same corpus)
- The look residual gained a highlight lightness bin (Oklab L 0.93) and its lightness was refitted. The lightness is taken net of each photo's exposure, applied from chroma 0.07 on (pale colours moved large areas of single photos), at half strength and 0.85 in the highlight bin. Held-out tests chose these strengths: full strength improved the mean further but doubled the worst per-photo regression; refitting the per-make tables did not generalise.
- Result (all 281 photos): sRGB 0.901 → 0.887, beyond sRGB 2.085 → 1.953, saturated yellow highlights 2.207 → 1.880, saturated blues 2.410 → 2.193; yellow highlights still slightly lighter than Camera Raw (Oklab L −0.0083 → −0.0052), pixels beyond P3 0.221 % → 0.185 % (Camera Raw 0.117 %); blue hue within 0.6°. Per photo 78 better, 20 worse (largest D3400 +0.23).
- Embedded ICC profiles of JPEG, PNG, TIFF and WebP sources are honoured (`input_profile.rs`): matrix/TRC profiles exactly and without clipping, others through moxcms into sRGB. An Adobe RGB / ProPhoto / P3 TIFF exported by Rawtonic reads back to the direct sRGB export within ΔE 0.00 median (before: 1.67 / 5.26 / 0.40).

## Open
- The remaining highlight lightness and per-camera spread (20 photos further from Camera Raw than before the refit).
- A perceptual rendering intent (gamut compression) as an option to clipping at the output gamut.
- Monitor profiles other than the system's (the display relies on macOS colour management) and HDR/EDR output.

## Update (2026-10-11)

The look residual tables named above, the per-hue table before them and the per-make tables were replaced by Rawtonic's own colour rendition (smooth functions of hue with 22 parameters) and its own tone curve, so the base look contains no table fitted point by point to another editor's rendering. The wide-gamut pipeline itself is unchanged.
