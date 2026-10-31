import type { Adjustments } from './adjustments';

/**
 * The frame of the RAW a photo develops (`sourceFrameVersion`, ADR 0020)
 * belongs to the photo and never changes once it is edited (decision
 * 2026-10-30, "new photos only"): a never-edited photo opens with the
 * newest frame and its first edit keeps it; Reset, undo, paste and presets
 * keep the frame the photo was edited in, so a photo never changes its size
 * while it is edited.
 */

/** `recipe` (a Reset, paste or preset result for the open photo) with the
 * frame of `current`, the photo's recipe before it: its key, or none (an
 * edit from before the key develops the first frame). */
export function withSourceFrameOf<T extends Partial<Adjustments>>(
  recipe: T,
  current: Pick<Partial<Adjustments>, 'sourceFrameVersion'>,
): T {
  const out = { ...recipe };
  if (current.sourceFrameVersion === undefined || current.sourceFrameVersion === null) {
    delete out.sourceFrameVersion;
  } else {
    out.sourceFrameVersion = current.sourceFrameVersion;
  }
  return out;
}

/** A never-edited photo's first recipe, `recipe`, for a source that shows
 * frame version `newEditFrame` (image_loader.rs `new_edit_frame`: set only
 * offline, on previews of an older frame than the newest, which no decode
 * replaces): edited in the frame it shows. */
export function withNewEditFrame<T extends Partial<Adjustments>>(recipe: T, newEditFrame: number | null | undefined): T {
  return newEditFrame === null || newEditFrame === undefined ? recipe : { ...recipe, sourceFrameVersion: newEditFrame };
}
