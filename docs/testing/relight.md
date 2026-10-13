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
macOS and Windows/Linux GPU-provider hardware. Image pattern import exists
(see the workflow revision below); direct canvas controls and fifteen built-in
pattern choices are covered below. No overall superiority or production-parity claim. Detailed
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

## Transitional depth edges and guide visibility, 2026-10-13

- A saved portrait recipe reproduced dark, dashed shading along a defocused
  furniture edge. The inferred 518×346 depth map contained short intermediate
  steps that passed the slope fit's range cutoff and invented a steep bevel.
  Local curvature now fades unreliable surface normals there, retaining the
  original height for pattern projection. The dark line disappeared in a
  native GPU render of the same photograph, recipe and persisted map.
- This is a confidence heuristic, not a more detailed depth reconstruction.
  The map separates broad subject/background forms but simplifies hair,
  clothing silhouettes and defocused boundaries. Pattern projection can still
  expose those errors. Quality gates above remain open.
- Analytic tests cover intermediate steps on both axes and diagonals, exact
  planes, image borders and smooth curvature. GPU tests require disconnected
  flat surfaces with transitional edges to match flat-map sunlight pixel for
  pixel, alongside the existing plane/Lambert-response check.
- A native **Show light guides** checkbox replaces the ambiguous editing
  button. It hides source handles and pattern guides without changing any
  lighting values; a UI regression checks both directions and recipe identity.
  The ready message explicitly describes estimated depth and points to surface
  strength for unnatural edges. Existing depth-lit thumbnails are invalidated.

## Refined portrait surfaces and light transport

A new analysis stores 468 image-specific XYZ facial landmarks plus skin confidence
per detected face (up to eight), alongside scene depth. MediaPipe's crop-relative
z is retained in photo units. The renderer aligns the local relief to scene depth,
builds smooth vertex normals with the Apache-2.0 canonical topology, and blends
away at the face boundary/occlusions. This is not a canonical face replacement.
Scene directions are regularized within depth surfaces; fitting still happens
before upsampling. The resulting texture is cached across light edits.

Version-2 analysis uses directional diffuse response, bounded local facial
visibility, finite point-source distance falloff and independent coloured light
addition. A common highlight scale preserves the added light's RGB direction.
The spot's **Light distance** is relative to image scale, not measured metres.
Legacy analyses retain their previous light model until **Analyze again**.
New lights start with original illumination 60 and surface strength 100 only
when those values have not previously been set.

The first global height-ray test produced implausible silhouette shadows on a
real defocused background. It was rejected: visibility tracing is currently
restricted to confident facial interiors. Object surfaces receive directional
lighting, depth-dependent spot falloff and gobo projection; general object-to-
background cast shadows are not claimed. Existing lighting/reflections remain
in the input; albedo, roughness, transparent materials and off-screen geometry
are not recovered. Relative monocular depth does not determine metric distance.
The broader portrait/product realism acceptance gate remains open.

Targeted gates: canonical test surface retains nose relief and opposed cheek
normals; actual native GPU tests exchange the lit cheek with the light direction,
exercise nose visibility, near/far and foreground/background falloff, pure red
illumination, light ordering, and preview/general equivalence. Local portrait
inference found one face in 119 ms (models already loaded; not end-to-end).
Final native macOS QA on copied fixtures: reanalysis returned 468 landmarks
for one portrait face in 947 ms (including model initialization and QA polling);
source dragging reused the persisted geometry, Undo restored it, and the guide
checkbox changed no lighting values. A sphere produced no false face and distance
edits reused its depth. All four ignored Relight GPU tests pass, including
above-nose visibility and legacy/refined full-frame versus tiled U8 (≤1 code)
and U16 (exact). All project gates pass (872 frontend tests).

On the same 1280×854 portrait, 30 warm GPU/render/readback samples measured one
light p50/p95 2.11/2.17 ms, four lights 2.15/3.71 ms. A 36-step native source drag
at 16.7 ms intervals produced 34 interactive replies with IPC p50/p95 7/9 ms.
Neither measurement is pointer-to-photon; first load and analysis are excluded
from the warm edit timings. Broader timing/quality limits above still apply.

Sources: [MediaPipe face coordinates](https://github.com/google-ai-edge/mediapipe/blob/master/docs/solutions/face_mesh.md#multi_face_landmarks),
[point-source distance falloff](https://www.pbr-book.org/4ed/Light_Sources/Point_Lights).

## Dense facial shadow reference

The original 16 geometrically growing ray steps could skip narrow nose/eye
relief. Simply increasing the count still left gaps, while uniformly short
rays missed blockers on large faces. Rays now start finely, grow slowly with a
two-texel/image-space cap, and stop at 256 samples, outside confident facial
interiors, or full occlusion. Shadow-disabled and unlit surfaces skip tracing.
Blended face/scene boundary texels are excluded as blockers: the blended height
otherwise invents a raised rim. This remains a height-field shadow approximation,
not recovery of hidden geometry or illumination already present in a photograph.

A separate CPU reference marches at quarter-texel intervals. The native GPU test
compares final sRGB output across 36 cases: three face scales, two depth-map
resolutions, side/diagonal/overhead light, 5/20/75-degree elevations and hard/soft
shadows. Acceptance is mean error <0.5, p99 ≤2 and maximum ≤20 code values on
confident facial pixels. Observed p99 is 1 in all cases; maximum is 15. The old
marcher failed six of the initial nine cases (p99 up to 19). This is a numerical
visibility check on known geometry, not an assessment of inferred face accuracy.

On the same 1280×854 copied portrait (30 warm native GPU/render/readback samples),
one light measured p50/p95 2.13/2.18 ms and four 2.14/2.20 ms. A deliberately large
analytic face covering much of the frame with four lights measured 3.63/5.18 ms.
The latter measures cost only, not inferred geometry or a photo-quality result.
These measurements exclude input/display latency and initial analysis. Full/ROI
and tiled rendering still agree within U8 ≤1 and U16 exactly.

## Workflow and transport revision, 2026-10-13

- Previews send the depth map, facial landmarks and imported patterns once
  per name (`cacheKey`, pattern `id`); later frames send only the name and the
  renderer no longer hashes the bitmap per frame. Exports and thumbnails still
  carry the full data. Not yet measured as IPC payload/latency in the native app.
- Analysis continues with scene depth alone when the face models are missing
  or fail; the panel shows the backend's error text.
- One set of light defaults in engine and editor (parity test reads
  `relight.rs`); pattern ids are parity-tested the same way.
- Panel: Scene section (original light, depth) separate from the selected
  light; stable light numbers; solo preview (view state, not recipe); reset
  keeps placement; Kelvin swatches and slider beside the picker; five lighting
  presets; Advanced shows only controls that affect the current light.
- Canvas: Alt-drag (or Alt+arrows) on a sun's source sets elevation; the other
  lights' pattern frames stay faintly visible.
- Lights can be limited to a mask layer (GPU multiplies the footprint by that
  layer's bitmap). Custom patterns: an imported image, stored in the recipe.
- Native GPU test `relight_masks_and_imported_patterns`: no pixel changes
  outside the mask, a hidden layer is identity, the imported pattern differs
  from unpatterned and inverted light and renders deterministically. Existing
  parity tests still pass (preview/export max difference 0 at 8 and 16 bit).
  Native-app QA of the new panel and pattern import is still open.

## Reproduce

```sh
cargo test -p rawtonic-engine relight --lib
cargo test -p rawtonic-gpu relight_pixels -- --ignored --nocapture
cargo test -p rawtonic-gpu relight_tiled -- --ignored --nocapture
cargo test -p rawtonic-gpu relight_refined -- --ignored --nocapture
cargo test -p rawtonic-gpu relight_masks -- --ignored --nocapture
npx vitest run src/utils/relight.test.ts src/components/panel/editor/RelightOverlay.test.tsx src/components/studio/StudioRelight.test.tsx src/components/studio/StudioDevelop.test.tsx src/components/studio/historyLabels.test.ts src/utils/adjustments.test.ts
```

For local real-photo analysis, set `RELIGHT_PHOTO`, `RELIGHT_MODEL`,
`RELIGHT_OUT` (a separate artifact directory), and `ORT_DYLIB_PATH`, then run
`cargo test -p rawtonic-app --lib relight_depth_preserves -- --ignored --nocapture`.
It writes `depth16.png`. Using the same photo and output directory, run
`cargo test -p rawtonic-gpu relight_real_photo_and_timing -- --ignored --nocapture --test-threads=1`.
Optionally set `RELIGHT_RECIPE` to a local adjustments JSON file to reproduce
its light settings with that map.
Do not commit the fixture, its path or the generated photo artifacts.

## Facial reflection response, 2026-10-13

- A cached facial normal now drives a modest GGX dielectric reflection. The
  lobe moves with the emitter and reflects its colour independently of the
  photographed diffuse colour. Face confidence fades the contribution; no
  material is guessed for products or background pixels. The same gobo,
  visibility and point-source falloff modulate diffuse and reflected light.
- “Face reflections” under Advanced controls the amount. Analysis version 3
  defaults to 25/100; existing version 2 recipes retain zero unless explicitly
  adjusted. No additional inference is performed during interaction.
- The implementation uses separable Smith masking and Schlick Fresnel with
  IOR 1.4 (F0 = 1/36). These are our equations based on the
  [PBRT dielectric model](https://www.pbr-book.org/4ed/Reflection_Models/Dielectric_BSDF)
  and the skin IOR discussed in
  [GPU Gems 3](https://developer.nvidia.com/gpugems/gpugems3/part-iii-rendering/chapter-14-advanced-techniques-realistic-real-time-skin).
  Roughness is a fixed conservative approximation broadened for soft lights;
  the camera is orthographic as in the existing height-field geometry.
- The photograph still contains its original illumination and highlights.
  This is not intrinsic-image recovery, measured skin material or full
  subsurface transport. Accurate geometry on textured products, separation
  from background, and calibrated portrait/product comparisons remain open.

Verification: six ignored Relight GPU tests passed, including a black dielectric
fixture isolating reflections (left/right highlight centroid moves 21 pixels
on a 128-pixel frame), pure-red reflection, gobo occlusion, unchanged background,
zero-amount compatibility and general/optimized agreement. Reflection-enabled
full/tiled/ROI export remains within U8 max 1 / U16 exact. Full project checks
pass, including 877 frontend tests, 13 locales and the Rust workspace.
Warm 1280×854 GPU/render/readback, 30 samples: one light p50/p95 2.13/2.20 ms,
four lights 2.12/2.19 ms; deliberately large facial coverage with four lights
3.64/5.18 ms. Includes recipe parsing; excludes presentation and inference and
is not pointer-to-photon latency.

Native QA verifies persisted old default zero, editing without geometry changes,
new analysis version 3/default 25, and exact recipe restoration after history
commit. A pre-existing immediate-undo race within the 500 ms history debounce
was observed; this verification does not claim that race is resolved.

## Immediate edit history, 2026-10-13

The observed 500 ms history race is now resolved. Pending edits belong to the
editor store and are settled before Undo/Redo/history selection, canceled on
reset/photo changes, and reflected immediately in toolbar/context-menu states.
A focused range slider admits Undo/Redo; text fields retain native text undo.
Eight store regression tests plus two keyboard cases cover pending first edits,
coalescing, branching, reset/photo isolation, direct commits and the 50-entry
history cap. Full project checks pass (887 frontend tests).
Native QA: first slider edit, pending=true, Undo enabled; Cmd+Z completed after
36 ms, before the 500 ms commit. Waiting past the debounce does not resurrect
it, Cmd+Shift+Z restores it, and a final Undo restores the exact saved recipe.
This is control-channel test timing, not rendering or pointer-to-photon latency.
