# ADR 0020: When the developed source changes its frame

- Status: Proposed (task SD, G71, 2026-10-26) — design only, nothing implemented; the user decides

## Context

The editor assumes that the open photograph's source (the decoded RAW, before
any adjustment) keeps its size for the whole session. Every recipe key that
changes the decode today (`DecodeVariant`: AI Denoise, the colour noise
filter's base, the dead-photosite test) leaves the size alone, so the
in-session re-develop (`useImageProcessing`'s source-develop effect →
`apply_ai_denoise`) swaps the pixels and keeps `selectedImage`'s width and
height, the overlays and every size-derived cache.

Three measured lens-correction gaps (G71) need a different frame and are
parked because of it:

| Case | Today | Wanted | Change of the shown photo |
|---|---|---|---|
| EOS R5 Mark II, APS-C crop | 4970 × 3392, ≈ 54 px off-centre (the CR3 active area ends 118 px before its recorded crop) | 5088 × 3392 as the camera JPEG and Camera Raw (parked rawler fix, `nielsfranke/g71c-cr3-apsc-frame`) | grows 118 px on the right; content stays where it is |
| Sony fixed-lens 16:9 (HX95, ZV-1 II, RX0 II; ZV-E10 a little) | the aspect crop is cut at decode, the warp runs on the 16:9 frame and lacks the pixels beyond it, so these frames keep version 0's reading (2.0–11.9 camera-JPEG px) | warp the active area, then cut the 16:9 frame, as the camera and Camera Raw do | same size; content moves by the better warp |
| Samsung EX2F in 16:9 | the whole 4:3 sensor, warp on it (shape 0.23 px vs Camera Raw) | the camera's 16:9 frame | shrinks to 16:9 |

The key that would select these frames (`cameraGeometryVersion`, or
`renderVersion`) changes inside a session: Reset, undo/redo, paste, presets,
sync, choosing the camera lens mode. So the open source would change size
mid-session, and the review of the R5 II fix (Codex r1) blocked it because
existing edits would change without a guard. A size change must be followed
by everything that holds pixel coordinates or sizes:

- editor state: `selectedImage` width/height, the crop and mask overlays,
  `patchesSentToBackend`, the preview queue's resolution;
- recipe coordinates: `crop` (pixels of the photo as shown), drawn shapes and
  strokes (`ShapeFrame`, older shapes without a frame), spots, `aiPatches`
  (`offsetX`/`offsetY`/`width`/`height`), face retouch (`crop`, blemishes,
  `frame`), AI mask bitmaps and their `warp`, colour samples, relight depth
  geometry, `guidedPerspective` lines (0–1 of the uncropped photo);
- caches and other consumers of one source: the decoded-image cache, preview
  proxies (`ProxyMemory`, `stands_for`), thumbnails, multi-view and snapshot
  compare (two recipes of one photo), the export's reuse of the open decode,
  deferred multi-recipe exports (`DEFER_OUTPUT`: one main pass per photo);
- the camera's embedded still shown before the decode (`developed_frame`,
  placed in the developed frame) and offline photos (previews and
  `source_facts` of the old frame).

## Options

**A. Frame fixed when the photo opens.** The editor decodes the frame of the
recipe it opens with and keeps it until the photo is opened again. Touches
little, but breaks the rule that what is shown is what exports: after an undo
or paste across the key the export (which reads the recipe) would decode
another frame than the editor shows. Rejected on its own.

**B. A per-photo frame key, changed only by an explicit update that reloads.**
A new key `sourceFrameVersion` (not `cameraGeometryVersion`) says which frame
the decode gives. It belongs to the photo like `spots` and `aiPatches`: not
copied by paste, presets or sync, kept by Reset, written into new edits and
by `apply_default`. It changes in only two ways: an explicit "Update frame"
action for the photo (one history step, which also translates the recipe's
coordinates, below), and undo/redo across that step. The frontend compares
`frameKey(adjustments)` with the source's, as `sourceDevelopToApply` does, and
on a difference reopens the photo through the normal open path (as if another
photo was selected) instead of `apply_ai_denoise`. The open path already sets
everything size-dependent; `apply_ai_denoise` keeps its "same size"
assumption and asserts it. Undo restores the whole recipe snapshot, so the old
coordinates come back with the old frame. Touches: `DecodeVariant` (+ `frame`),
cache and proxy keys, `developed_frame` (takes the variant), `source_facts`
(records the frame; offline photos answer only their own frame), multi-view /
snapshot compare and `DEFER_OUTPUT` grouping by frame, one migration function
in the engine (used by the action and by importers), the editor reload hook.

**C. Keep the decode frame; express the camera frame as a crop.** Never change
the source per recipe; any smaller camera frame becomes a crop after the warp.
Fits the EX2F exactly: the source stays the 4:3 sensor the warp already
corrects, and new edits get the camera's 16:9 frame as their initial crop
(the user can widen it; never-edited exports get it through
`apply_default`). No size change, no reload, old edits untouched. It does not
fit the R5 II (the wanted frame is larger than today's source) nor the Sony
16:9 frames without a decode change: decoding the larger area for every
recipe moves the coordinates of every existing edit and breaks their byte
identity at the borders (demosaic and warp now see real pixels), so legacy
recipes would still need the old frame cut at decode — which is B's
mechanism.

**Implementation detail for B (from C):** decode once at the largest area and
cut each frame from it as a cheap sub-rectangle, so switching frames needs no
second decode — only if the cut equals today's decode byte for byte (border
demosaic); otherwise the frame joins the decode cache key and decodes again.

## Risks

- **Existing edits.** With B every recipe without `sourceFrameVersion` decodes
  today's frame: exports byte-identical by construction; to be shown with the
  fixture exports (R5 II APS-C, Sony 16:9, EX2F) old vs new commit. Nothing
  changes until the user updates a photo or makes a new edit.
- **Coordinate translation on update.** R5 II: the frame grows on the right,
  so crops and framed shapes stay on the content; shapes without `frame` on a
  turned photo must first get their frame (`keep_drawn_masks_on_photo`).
  Sony 16:9: size equal, content moves by the warp as with any lens change;
  AI masks follow through `warp` (add `sourceFrameVersion` to
  `GEOMETRY_KEYS` and the face-retouch frame key); spots and the crop behave
  as on any lens-correction change today. AI patches are pixels in the old frame:
  translate the offset where the frame only grows, otherwise hide them and
  ask to regenerate (never silently misplace generated content — the IPTC
  marking stays with the patch).
- **Review loop.** The decode-variant PR (#222) took several rounds because
  each review found another proxy consumer; this change has more consumers.
  Mitigation: list every consumer above in the PR with how it follows the
  frame, ship the mechanism together with its first case (R5 II: two sample
  files, grows right only — a key with no case would "guard nothing"), Sony
  16:9 and EX2F (C) as separate PRs; stop after ~4 same-class rounds.
- **Reload cost.** A reopen costs a full decode (not measured for this
  proposal) or a sub-rectangle cut; only on the explicit action and undo
  across it.

## Recommendation

B for the R5 II APS-C frame and the Sony 16:9 warp-before-crop, with the
single-decode cut where it is byte-identical; C for the EX2F (an initial crop
on new edits, no source change). Order: R5 II (mechanism + first case), then
Sony 16:9, then EX2F.

## Open questions for the user

1. Should Reset adopt the newest frame (as Lightroom's Reset adopts the newest
   process version) or keep the photo's frame? B proposes keep.
2. Where does "Update frame" live: a note in the lens panel only on photos it
   would change, also a batch command for selected photos, or both?
3. EX2F: initial crop the user may widen to 4:3 (proposed), or a hard frame
   like Camera Raw's?
4. AI patches on an update whose frame does not only grow: hide and ask to
   regenerate (proposed), or block the update while patches exist?
5. One key for all frame changes (`sourceFrameVersion`), or fold it into
   `renderVersion` (but then Reset/paste semantics of that key change)?
