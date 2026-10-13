# Relight verification and remaining quality gates

Implementation: four analytic projected lights in the native scene-linear GPU
pipeline, plus optional Depth Anything V2 Small surface normals. No external
pattern assets, no photo upload, no new model weights. The photo's original
lighting remains part of the input. This is a depth-assisted photographic
lighting effect, not inverse rendering, material recovery or ray-traced cast
shadows. Transparent and reflective surfaces and fine hair remain difficult
for monocular depth estimation.

## Verified locally, 2026-10-12

- Engine: absent/disabled recipes are neutral; parameter and light-count bounds.
- Frontend: four-light limit, visibility/removal, persisted depth on reload,
  no inference on color edits, errors/retry, geometry invalidation and rejection
  of an analysis completing after the photo changes. Existing Develop,
  copy/paste and history-label tests also pass (57 tests together).
- Native GPU: six distinct patterns, zero/hidden identity, inversion, additive
  light order independence, depth response and cache reuse. General versus
  optimized render: maximum 1/255 difference for the 8-bit preview, exact
  agreement for 16-bit export, including ROI. A separate test crosses the
  2048-pixel tile boundary: max 1/255 at 8-bit, exact at 16-bit.
- Relight controls inspected in a browser component harness at the normal
  sidebar width. This is UI verification, not a browser photo-render substitute.
- Real portrait fixture: inference retained more than 256 distinct depth
  values in a 518×370 map. Preprocessing + inference approximately 223–245 ms
  after model loading on this machine. Analysis output and rendered before /
  projected / surface images were inspected locally; private fixtures are not
  checked in.
- Serial warm GPU benchmark, 1280×914, 30 samples per case: neutral p50 2.00 ms,
  p95 3.56 ms; one depth light 2.06 / 2.12 ms; four depth lights 2.11 / 2.16 ms.
  Includes recipe parsing, rendering and CPU readback; excludes decode, model
  inference, IPC, display presentation and pointer-to-photon latency. This is a
  local workload measurement, not evidence of superiority to another editor.

## Follow-up verification, 2026-10-13

- Fixed native analysis results being discarded: Rust sorts geometry object
  keys, while JavaScript preserves insertion order. Compare nested geometry
  by value, including `guidedPerspective`; regression coverage uses reordered
  keys. Native portrait analysis now reaches ready, survives light changes,
  and restores its saved depth after switching to another photograph.
- Shadow patterns now project onto the inferred height field. Their coordinates
  vary with surface height and light elevation, using the same cached texture
  sample as the normals. Full-frame/ROI/tiled GPU checks still pass.
- ONNX bindings migrated to `ort` rc.12 with explicit API 22, `ndarray` 0.17,
  explicit CPU registration and environment-retaining GPU device handles with
  weak registry entries. macOS Runtime 1.22 still aborted on the face suite;
  Microsoft's 1.23.2 runtime fixes that remaining shutdown failure. Both macOS
  architectures are pinned by SHA-256, use new versioned model-pack filenames,
  and ship the corresponding license notices. Old pack files remain available.
  The opt-in bundle build downloaded and verified the new Apple Silicon file.
- Real AI regression: all **12 installed ONNX models**, two load/infer/unload
  rounds each, pass including process exit. The three face models also pass
  on seven portraits including process exit. Intel runtime hash verified;
  Intel execution and Windows/Linux GPU providers were not tested here.
- Native editor: isolated QA identity and copied fixtures, actual wgpu image
  presentation. Cold depth button-to-ready about 1.10 s including model load
  and QA polling; no model download was needed. A 60-value slider sequence
  at 16.7 ms intervals produced 59 interactive previews plus the final preview.
  IPC preview roundtrip p50 **5 ms**, p95 **11 ms**. These are synthetic DOM
  inputs and IPC completion, **not pointer-to-photon latency**. Native app
  shutdown after depth inference completed with exit status 0.
- Latest serial 1280×914 GPU/readback benchmark (30 samples): neutral
  p50/p95 4.96/5.12 ms, one depth light 4.66/5.18 ms, four 5.00/5.36 ms.
  Machine load/power state differs from the preceding run; do not infer a
  regression or speed-up between dates from these numbers.
- All 13 locales now have translated Relight labels and help. Automated
  translation-key/plural checks pass; native-speaker review remains useful.
- `scripts/check.sh`: all checks passed (frontend, full Rust workspace,
  formatting, lint, clippy, layering, dependency/license checks). All three
  ignored Relight GPU tests pass with their required fixtures.

## Reference use test and remaining quality gates

ImageFlow was inspected on 2026-10-13 using an independently drawn 800×600
shaded sphere, also opened and depth-lit in Rawtonic. No private photographs
were sent to the reference site and no implementation/assets were copied.
Both tools deform projected patterns over estimated surface depth. ImageFlow
has direct canvas light handles, a larger pattern library and file/layer
pattern import; Rawtonic currently has six analytic patterns and sidebar
position/direction controls. Rawtonic's native portrait and sphere results
were visually checked; the sphere exposes some depth-edge ringing. Its
original illumination is retained, so neither this test nor the shader is
proof of inverse-rendering accuracy.

The basic reference use test is complete, but controlled matched-light
portrait/product quality evaluation and matched end-to-end timing against
ImageFlow and professional editors remain open. No quality/performance
superiority claim or production-parity sign-off is justified by the current
evidence. The detailed remaining acceptance gate is notes: PARITY_GAPS G70.

## Reproduce

```sh
cargo test -p rawtonic-engine relight --lib
cargo test -p rawtonic-gpu relight_pixels -- --ignored --nocapture
cargo test -p rawtonic-gpu relight_tiled -- --ignored --nocapture
npx vitest run src/components/studio/StudioRelight.test.tsx src/components/studio/StudioDevelop.test.tsx src/components/studio/historyLabels.test.ts src/utils/adjustments.test.ts
```

For local real-photo analysis, set `RELIGHT_PHOTO`, `RELIGHT_MODEL`,
`RELIGHT_OUT` (a separate artifact directory), and `ORT_DYLIB_PATH`, then run
`cargo test -p rawtonic-app --lib relight_depth_preserves -- --ignored --nocapture`.
It writes `depth16.png`. Using the same photo and output directory, run
`cargo test -p rawtonic-gpu relight_real_photo_and_timing -- --ignored --nocapture --test-threads=1`.
Do not commit the fixture, its path or the generated photo artifacts.
