# Recipe schema
Managed storage uses a versioned envelope, never unversioned loose parameters:
```json
{"schema_version":1,"process_version":"rapidraw-1.6.4","adjustments":{},"layers":[],"snapshot":null}
```
The inherited adjustment schema is defined by `src/utils/adjustments.ts` defaults and UI slider bounds; the adapter preserves fields losslessly. `adjustments` contains the native renderer recipe, including inherited masks/crop/retouch vectors. `layers` is reserved for the future independent composite model and must remain empty until implemented, rather than being silently ignored. Each history step is immutable in SQLite, with a version head pointer; undo/redo changes the head. Metadata is shared between versions.
Known fields: exposure -5..5 EV, contrast/highlights/shadows/whites/blacks -100..100, saturation/vibrance -100..100; exact inherited defaults must be checked before conversion. These are target semantic ranges, not a statement that upstream scale matches. Do not translate without an explicit process migration.
Forward versions are rejected for editing. Unknown adjustment fields are retained. Raster mask paths must be relative to the session and accompanied by hashes before portable package export. Validation and a complete machine-readable schema are outstanding.
`adjustments.sectionVisibility` switches parts of the recipe off without losing their values. Inherited master keys: `basic` (exposure, contrast, brightness, highlights, shadows, whites, blacks), `color` (white balance, saturation, vibrance, hue, colour grading, colour mixer, calibration), `curves`, `details` (sharpening, noise reduction, clarity, structure, dehaze), `effects`. Per-tool keys (2026-10-03, Lightroom's panel eye) narrow these further: `whiteBalance` (temperature, tint), `presence` (clarity, structure, dehaze, vibrance, saturation), `colorMixer` (hsl), `sharpening` (sharpness, sharpnessThreshold), `noiseReduction` (luma and colour noise reduction). A parameter renders only if its master and its tool key are not `false`; a missing key means on.

`adjustments.guidedPerspective` holds the guided perspective: `lines` (up to two `vertical` and two `horizontal` guides, coordinates 0–1 of the uncropped photo), `enabled` (two or more lines), `upright` (2026-10-04: `off`, `auto`, `level`, `vertical`, `full` or `guided`; missing on recipes from before Upright) and `autoCrop`. With `upright` set to anything but `off`, the correction zooms until no empty corners remain (Lightroom's Constrain crop) unless `autoCrop` is `false`; recipes without `upright` never zoom.

`adjustments.baseCurve` (2026-10-04): `"standard"` (missing key), `"highContrast"`, `"extraShadow"`, `"linear"`. RAW files only. Other names render with the standard curve. The curve choices are applied after the base look as a change of display luminance and chroma by display level: High contrast darkens the shadows and brightens the upper midtones around mid grey, Extra shadow lifts the shadows by up to 0.8 EV, Linear response undoes the default tone curve (Rawtonic's own shapes since 2026-10-11). Part of the copy/paste group with Exposure and the tone mapper.

`adjustments.grainStructure` (2026-10-12): `"even"` (missing key, the original grain), `"soft"`, `"clumped"`, `"speckled"`, `"gritty"`. Unknown names render as `"even"`, so recipes without the key export byte-identically to before (the renderer, CLI and headless export never default to anything else). The editor writes the key: grain turned on in a recipe without one gets `"clumped"` (new edits, and grain turned on again after resetting the grain tool, which removes the key); a recipe loaded with grain but without the key gets `"even"` written, so its grain keeps rendering Even when copied, pasted, synced, saved as a preset or turned off and on; presets and pasted settings from before the choice with grain but no key are applied as `"even"`. Lightroom presets, XMP and catalog imports with grain get `"clumped"`. Amount, Size and Roughness work for every structure and each is scaled to Even's strength at Roughness 50 (Gritty 1.4×). Even and Clumped are equally strong in every tone (Clumped: Roughness gathers fine grain into clumps); Soft (rounded grain with irregular clusters), Speckled (bright specks over a fine grain) and Gritty (coarse, hard-edged grain) are strongest around display level 0.45 and fade towards black and white. Capture One styles with grain map `FilmGrainType` onto it (Fine or none → even, Silver Rich/Soft/Cubic → soft, Tabular → speckled, Harsh → gritty). Part of the grain copy/paste group.

RAW detail defaults for recipes without the key (2026-10-04): `sharpness` 40, `colorNoiseReduction` 25, `lumaNoiseReduction` 15; JPEG/TIFF 0.

`adjustments.colorNoiseVersion` (2026-10-13, `crates/engine/src/color_noise.rs`): which colour noise reduction `colorNoiseReduction`, `colorNoiseDetail` and `colorNoiseSmoothness` drive. `2`: the multi-scale filter (chroma pyramid of five scales, each thresholded by the colour noise measured in the photo at that scale and brightness; luminance and colour edges protected; its output changed on 2026-10-14 before any release, with the brightness noise profile and refitted guards, Detail and Smoothness, and again the same day with luminance-following and coherent edge guards, a finest-level edge guard and stronger Color 100, and with edge-aware expansions, the coarsest level smoothed above Color 25 and the base image without the decode-time chroma clean-up). `1`: the earlier cross-bilateral filter in the renderer. Without the key a recipe keeps what it rendered with before: `1` when it sets `colorNoiseReduction`, `2` when a RAW recipe leaves it to the default (never-edited photos, headless exports without a recipe). The editor writes `2` into new edits, keeps a saved recipe's filter when loading it, and copies the key with the noise reduction group. Layers keep their colour amount: with version 2 a layer below the photo's amount blends towards the unfiltered colour, a layer above it gets the photo's filter, and a layer on a photo with Color 0 uses the cross-bilateral filter. With `2` the base image is developed without the decode-time chroma clean-up (preference `rawPreprocessingColorNr`; the multi-scale filter measures and removes the colour noise itself), with `1` as before (`BaseImageOptions::for_color_noise`); decoded-image cache keys and preview proxies record which base they hold, and the editor develops the open photo again when its filter changes. With `2` the luminance noise reduction also follows the photo's luminance noise by brightness (measured with the colour pyramid, also at Color 0) and, below Luminance 50, leaves a more even share of grain (2026-10-14); `1` recipes render their luminance exactly as before.

`adjustments.faceRetouch` (2026-10-09, Capture One's Retouch Faces; `crates/engine/src/face_retouch.rs`): `settings` (all faces: `impact` 0–100, default 100; `blemishes`, `evenSkin`, `darkCircles`, `shine`, `contouring` 0–100; `texture` −100–100; `includeNeck`: Blemishes, Even skin and Texture on the neck too, default off) and `faces`, one per face as the app's `analyze_face_retouch` makes it: `crop` (affine map from the stored weights' pixels to the mask frame: x = a·u + b·v + c, y = d·u + e·v + f), `size` (side of the weights), `eyeDistance` (mask-frame pixels), `skin`, `underEyes` and `neck` (PNG data URLs, 0–255 weights; `skin` leaves out beards; `underEyes` and `neck` come from the landmarks or, without them, from the detector's keypoints where those describe an upright face, else are absent; `neck`: body skin below the chin, absent when none is visible), `blemishes` (`id`, `x`, `y`, `r` in mask-frame pixels, `score` = prominence, `neck` = on the neck, healed only with `includeNeck`; the Blemishes slider takes those above 1.0 · 0.015^(amount^0.25), from 0.045 at 30 to 0.015 at 100, and from about 30 also clears dark and red specks of the fine layer that stand out from the pores; it also evens red blotches and redness above the face's own colour, fitted to Capture One's Blemish), `kept` (blemish ids left alone), `added` (blemish ids healed below the slider's reach), `settings` (the face's own values, overriding the ones for all faces), `enabled`, `box` and `thumbnail` (for the panel). `frame` is the geometry the faces belong to (`face_retouch::frame_key`: orientation, flips, straightening and the geometry keys); with another geometry the faces are not applied until the panel finds them again, and `geometry` is the panel's copy of the same keys. Everything the render needs is stored, so exports need no model. Applied in float on the base image after spot removal, before every adjustment; previews below full resolution apply it to the reduced image with the positions scaled (`face_retouch::scaled_recipe`). Not copied to other photos (per photo, like spots).

Mask geometry (2026-10-10, `crates/engine/src/mask_generation.rs` `PhotoGeometry`): AI mask components (`ai-subject`, `ai-foreground`, `ai-sky`, `ai-people`, `ai-depth`, `quick-eraser`) store `maskDataBase64` in the photo's own frame (lens- and transform-corrected, before orientation, flips, straightening and crop) and are placed by the photo's geometry when drawn, so they stay on the content when the photo is turned, flipped, straightened or cropped later; their `rotation`, `flipHorizontal`, `flipVertical` and `orientationSteps` only record the geometry they were made in. `warp` (masks made since 2026-10-10) holds the lens and transform adjustments (`GEOMETRY_KEYS`) the bitmap was computed in; when they differ from the photo's now, the mask is moved through the source into the current warp. Colour range `samples` and the `targetX`/`targetY` point of `color` and `luminance` components are full-resolution points (and areas) of the photo as shown when they were taken, before the crop; a sample's own `rotation`, `flipHorizontal`, `flipVertical` and `orientationSteps` give that geometry (older samples use the component's).

Drawn mask frames (2026-10-10, `crates/engine/src/mask_generation.rs` `ShapeFrame`): `radial` and `linear` components and every stroke (`lines[]`) of `brush` and `flow` components (and the brush-shaped `clone`, `heal`, `liquify`, `retouch`) keep their coordinates in the photo as shown when they were drawn (full-resolution pixels after orientation, flips and straightening, before the crop). Their optional `frame` records that geometry: `rotation`, `flipHorizontal`, `flipVertical`, `orientationSteps`, and `width` × `height`, the photo's own size (full resolution, before orientation). When the photo's geometry differs from its frame, the shape is drawn where that spot of the photo is shown now (points moved through the photo's own frame; a radial's `rotation` turned with it; a linear's start and end swapped when one side is mirrored relative to the other), so drawn masks stay on the content when the photo is turned, flipped or straightened later. Without `frame` (recipes before 2026-10-10) a shape is drawn as the photo is shown now, as before. Each brush stroke has its own frame, so strokes painted after a turn and those painted before each keep their spot. The editor writes the frame for every new shape and stroke and for a shape whose handles are dragged (converted to the current geometry); when an edit changes the orientation, flips or straightening, shapes without a frame that the edit left alone get the frame they were shown in until then — in the editor (`keepDrawnMasksOnPhoto`, src/utils/maskGeometry.ts) and for pasted, synced and preset settings and auto settings applied to photos in the backend (`keep_drawn_masks_on_photo`; there the masks left alone are those whose parameters the edit did not change). Imported Lightroom masks record the photo's own frame (Lightroom keeps them in the unrotated photo).

Frame sizes (2026-10-11): `width` and `height` may be absent (frames stamped by the backend, which does not decode the photo); such a frame is in the size of the photo it is on. A frame whose size differs from the photo's own size by a pixel or more (a mask copied, synced or saved in a preset from a photo of another size) places the shape proportionally, as Lightroom keeps its masks relative to the photo: every point at the same fraction of the photo's own width and height (pixel index i of n at (i + 0.5) / n), a radial's first radius along its carried axis and its second so that the ellipse's area scales with the photo's, a linear's `range` and `rangeEnd` as distances from the carried central line, a stroke's `brushSize` by the square root of the area ratio. Copying settings, syncing them and saving a preset give the drawn masks they take along the frame of their photo (`frameDrawnMasks`; its size when it is the open photo). Frames recorded with the same size, and shapes without a frame, render as before.

Masks read from the photo's own frame (AI masks, colour and luminance ranges, the luma range, colour samples, Auto Mask strokes) take, on a straightened photo, the photo's pixel nearest the point the straightened photo is resampled at (since 2026-10-11; they rounded that point down before, half a pixel off on average).

## Relight

`relightLights` holds up to four non-generative projected lights. Each has an
`id`, `enabled`, `type` (`sun` or `spot`), sRGB hex `color`, `intensity` (0–100),
`x`/`y` (pattern center, −100…200 percent of the developed frame), `size` (5–200), `rotation`
(−180…180 degrees), `elevation` (5–90 degrees), `scale` (5–300), `softness`,
`diffusion`, `shadows` (all 0–100), `pattern` (`none`, `blinds`, `window`,
`strip`, `foliage`, `grid`, `palm`, `branches`, `checkerboard`, `dots`,
`horizontal`, `diagonal`, `weave`, `noise`, `crosshatch`) and `invert`.
Optional `sourceX`/`sourceY` (−100…200 percent) position the light independently
of the pattern; optional `patternRotation` (−180…180 degrees) rotates only the
pattern. Legacy recipes fall back to `x`/`y` for spot position and `rotation`
for pattern rotation, preserving their rendered output. Missing lights are identity.
`sectionVisibility.relight=false` bypasses all lighting without losing settings.
`relightOriginalLight` (0–100, default 100) scales the existing illumination
when at least one light has nonzero intensity. Up to analysis version 4 it
does not reconstruct albedo or remove the original light direction; version 5
(below) removes the original light's shading and skin shine on the subject.

Optional `relightDepth` contains `{data, geometry}`: a 16-bit grayscale PNG data
URL at inference resolution, and the geometry parameters at analysis time
(engine `relight::geometry`). Analysis uses the displayed GPU input frame, after lens/perspective correction,
orientation, flips, straightening and crop. Geometry records non-neutral crop
and straightening too; older maps remain valid only in their original neutral
frame. A geometry mismatch disables depth until the user analyzes again.
`relightDepthAmount` (0–100, default 60) controls normal strength; zero uses
projected lighting. Surface gradients are computed once using an edge-aware local slope fit on 16-bit
depth. A local curvature heuristic reduces unreliable normals near short
silhouette transitions, without changing height or exact linear planes.
The result is packed in a GPU texture and reused for light edits. Depth is specific to
the photograph and excluded from copy/paste and presets; light settings are
copyable. Analysis version 2 additionally stores `faces`: up to eight arrays of
468 `[x, y, z, confidence]` points (x/y normalized to the displayed frame; z is
camera-facing relief in short-side image units). They refine normals and local
height without rerunning inference on light edits. `version: 2` opts into the
refined light response; legacy maps remain compatible. Each light may store
`distance` (10–300, default 60), the spot source's image-relative distance.
Facial geometry remains photo-specific and is excluded from copy/paste.
No model inference or network request is needed to reopen or export
a recipe containing a depth map. The feature does not synthesize photo pixels
and does not add an AI patch.

Analysis version 3 keeps the same depth and facial geometry and defaults
`relightSkinReflections` (0–100) to 25; older analyses default to zero.
An explicit value overrides either default. This global, copyable setting adds
view-dependent dielectric reflections only within confident facial coverage,
using cached normals, a fixed skin-like roughness and the light's colour.
It does not recover skin/material parameters or remove existing highlights.
Zero, missing faces, disabled depth and stale geometry suppress the reflection.

Analysis version 4 additionally stores `camera` with normalized `fx, fy, cx, cy`,
`inverseMin`, `inverseMax` and `reference` (currently 1). A PNG code `v` decodes
as inverse distance `inverseMin + (inverseMax-inverseMin)*v/65535`. Distance
is relative, normalized by the analysis median, not metres. Version 4 requires
valid finite camera metadata; unknown newer versions are not decoded as old
height maps. The cache key includes the camera. Gradients, perspective positions,
face relief, view direction, gobo projection, spot falloff and local shadows all
use this camera. Facial relief scales with the face's local scene distance.
Surface amount zero flattens this geometry onto its reference plane, retaining
the same light transport. Scene normals are regularized without smoothing across
large depth jumps; only confident facial geometry casts local shadows.

Analysis version 5 (new analyses since 2026-10-15) keeps version 4's fields
and smooths the scene slopes over a wide, depth-aware window when they are
loaded (monocular depth blotches backgrounds and skies). For a subject of at
least 5 % of the frame (BiRefNet matte) it adds `subject`, a 16-bit RGBA PNG
data URL at up to 1024 px per side and twice that wide: the left half holds
the subject normal's x and y (renderer frame: x right, y down, z towards the
camera), the matte and the normal's weight; the right half the share of the
photo that is the original light's skin shine and how far a new light reaches
the background (the inverse square of its distance relative to the
subject's). The subject, as a card at its median depth, casts its shadow on
that background (only the matte occludes). People
(faces, or skin in the whole-photo parse for profiles) take DAViD's normals,
objects the smoothed scene normals. `sourceLight` (nine numbers: ambient,
light, direction x/y/z, four zeros) is the photo's own lighting fitted on the
people's skin or the object's dominant material, scaled so the subject's lit
side has shading 1; a new light is divided by this shading, so it lights the
material (`I·L/S`) and Original light 0 % leaves only the new light, and the
skin shine is reduced with the original light. `subjectDepth` is the subject's
median inverse depth. Outside the matte the scene is lit evenly (no normal
response). Recipes of versions 2–4 render as before (byte-identical exports).

Since 2026-10-13: absent light fields render with a new light's values
(intensity 25, rotation 135, shadows 100, colour `#fff1db`; engine
`relight::DEFAULT_*`, parity-tested against `createRelightLight`). Earlier
recipes with fields missing therefore render with these values. The editor
writes `sourceX`/`sourceY`/`patternRotation`/`distance` explicitly when it loads
a recipe (same rendering). Optional light fields: `number` (stable display
number), `temperature` (Kelvin the colour came from; `color` stays
authoritative), `maskId` (limit the light to that mask layer; a hidden or empty
layer switches the light off, a deleted one no longer limits it), and
`pattern: "custom"` with `customPattern: {id, data, name}` — a 512×512 grayscale
PNG data URL made from the chosen image's central square (white transmits
light), copied with the light. A custom light without its bitmap is
unpatterned. `relightDepth.cacheKey` names an analysis: previews send its
bitmap and landmarks once and then only the name, as with AI patches; files
without a key are named by a hash of the bitmap when loaded.
