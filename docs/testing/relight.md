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

## Depth-edge correction, 2026-10-13

- Preserve the inference grid: `thumbnail(1024, 1024)` was enlarging smaller
  maps, turning discontinuities into ramps. A 518×370 map now remains that
  size instead of 1024×731: 74.4% fewer depth/normal texels and upload bytes.
- Replace blur/central differences with a spatial/range-weighted local slope
  fit. Neighbors across a normalized depth jump of 0.04 or greater do not
  contribute. Heights remain unfiltered. This removes invented bevels across
  separate surfaces while averaging small depth noise.
- Normal derivatives and projected height now use the same scale (0.25).
  Normals previously exaggerated the height eightfold. Existing depth-lit
  recipes consequently have a gentler surface response. Depth-lit thumbnails
  are invalidated; original photographs and decode caches are unchanged.
- New analytic tests cover rectangular planes, image borders, a single pixel,
  and discontinuities. GPU regression: a hard depth step under unpatterned
  sunlight is pixel-identical to a flat map; a known ramp matches its expected
  Lambert response within 1/255. The step test failed before the grid fix.
- All project checks and all three ignored Relight GPU tests pass. Preview,
  export, ROI and tile agreement remains within the documented tolerances.
- Final serial 1280×914/30-sample GPU + readback p50/p95: neutral 1.98/3.47 ms,
  one depth light 2.07/2.13 ms, four 2.07/2.12 ms. No inference is performed
  on these light edits. A separate native 60-input sequence without capture
  overhead produced 59 interactive previews: IPC p50 5 ms / p95 9 ms.
  Neither measurement is an end-to-end competitor benchmark.

## Reference use test and remaining quality gates

ImageFlow and the native Rawtonic editor were inspected on the same independent
800×600 shaded sphere and the same public-domain NASA portrait, photograph
S99-00858 / GPN-2000-001177 (NASA / Robert Markowitz;
[source and reuse information](https://commons.wikimedia.org/wiki/File:Commander_Eileen_Collins_-_GPN-2000-001177.jpg)).
No private photographs were sent to the reference site. Unpatterned sunlight
and blinds were compared, including skin, hair, the suit and reflective helmet.
The corrected native result no longer has the earlier exaggerated silhouette
response. The original illumination and helmet reflections remain in both
images. Equal percentage settings do not define equal lighting between tools;
this use test is not a calibrated or blind quality ranking.

A recorded sphere test alternated intensity twelve times. ImageFlow's displayed
control and image changed in the same captured frames. Native window captures
showed the image in the same or following capture, but their roughly 31 ms
sampling and synchronous capture overhead delayed the app. The capture methods
are different, so these observations **must not be presented as a latency
ranking**. Matched input-to-present timing remains unverified.

The implementation-caused depth-edge defects above are corrected. The broader
acceptance gate remains open for a calibrated portrait/product assessment,
transparent objects/fine hair, matched end-to-end timing, and execution on Intel
macOS and Windows/Linux GPU-provider hardware. File/layer pattern import remains unimplemented; direct canvas controls and
fifteen built-in pattern choices are covered below. No overall superiority or production-parity claim. Detailed
evidence and remaining acceptance criteria: notes PARITY_GAPS G70.

## Direct canvas controls, 2026-10-13

- The Relight tool exposes light-source handles in the developed photograph.
  Pattern center, rotation and scale are independent; spot lights also have
  a radius handle. Handles stay a fixed screen size at zoom, support keyboard
  arrows (Shift for larger steps), and disappear when the tool closes or a
  conflicting image tool is active. Escape cancels a drag; a completed gesture
  produces one undo step, including when the pointer pauses.
- Fifteen visual choices: none, blinds, window, strip, foliage, grid, palm,
  branches, checkerboard, dots, horizontal lines, diagonal lines, weave, noise
  and crosshatch. All are original analytic GPU patterns with schematic SVG
  swatches. Numeric positioning and secondary controls live under Advanced.
- Adding a light starts local depth analysis if no matching map exists. Edits
  reuse the saved map. The depth input now includes straightening and crop,
  matching the GPU photograph frame; geometry changes invalidate older maps.
  Existing neutral-frame maps and legacy light placement remain compatible.
- Regression tests exercise gesture undo/cancel (including legacy recipes),
  zoom coordinates, photo switching, independent transforms, shared selection,
  automatic analysis/reuse, and stale crop/straightening. Native GPU tests
  verify fifteen distinct outputs, legacy-recipe equivalence, independent spot
  positioning/pattern rotation and stale-depth rejection.

- Final native macOS QA on copied public portrait and synthetic sphere: source,
  pattern move, rotation and scale via native mouse events; focused Cmd-Z;
  independent source placement; saved settings restored after app restart and
  photo switching; fixed 42-pixel source handle at 50% zoom. The viewer's capture
  handler now excludes Relight handles so a completed gesture cannot leave
  panning active. Pattern swatches and controls were inspected in the native UI.
- First-light automatic analysis reached ready in 616 ms in this warm-model QA
  run (including QA polling, not a cold-download or competitor measurement).
  Changing the sphere to a square crop invalidated depth, and reanalysis produced
  a 518×518 map with the matching crop geometry.
- Full project gates pass, including 871 frontend tests; all three ignored
  Relight GPU tests pass. This remains workflow/implementation verification,
  with the broader quality and timing limits stated above.

## Reproduce

```sh
cargo test -p rawtonic-engine relight --lib
cargo test -p rawtonic-gpu relight_pixels -- --ignored --nocapture
cargo test -p rawtonic-gpu relight_tiled -- --ignored --nocapture
npx vitest run src/components/panel/editor/RelightOverlay.test.tsx src/components/studio/StudioRelight.test.tsx src/components/studio/StudioDevelop.test.tsx src/components/studio/historyLabels.test.ts src/utils/adjustments.test.ts
```

For local real-photo analysis, set `RELIGHT_PHOTO`, `RELIGHT_MODEL`,
`RELIGHT_OUT` (a separate artifact directory), and `ORT_DYLIB_PATH`, then run
`cargo test -p rawtonic-app --lib relight_depth_preserves -- --ignored --nocapture`.
It writes `depth16.png`. Using the same photo and output directory, run
`cargo test -p rawtonic-gpu relight_real_photo_and_timing -- --ignored --nocapture --test-threads=1`.
Do not commit the fixture, its path or the generated photo artifacts.
