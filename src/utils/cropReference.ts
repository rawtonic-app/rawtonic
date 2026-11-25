import type { Crop } from 'react-image-crop';
import {
  REFRAMING_KEYS,
  calculateCenteredCrop,
  forceCropInBounds,
  getOrientedDimensions,
  isCropWithinBounds,
  moveCropInsideBounds
} from './cropUtils';

/** What a crop ratio is fitted into (ASP1, like Capture One and Lightroom):
 * the crop the user last drew by hand, or else the whole photo, around the
 * centre the crop had when the ratio changes began. A ratio switch never
 * fits into the crop the previous switch made, so the result does not depend
 * on the ratios chosen in between (3:2 → 1:1 → 16:9 is 3:2 → 16:9). */
export interface CropReference {
  /** The photo and the frame the crop lies in; another value drops it. */
  key: string;
  /** The crop drawn by hand (oriented pixels), or null for the whole photo. */
  rect: Crop | null;
  cx: number;
  cy: number;
}

const FRAME_KEYS = REFRAMING_KEYS.filter((key) => key !== 'crop' && key !== 'aspectRatio');

/** The photo and the inputs that decide its frame, without the crop. */
export function cropReferenceKey(path: string, adjustments: Record<string, unknown>): string {
  return JSON.stringify([path, ...FRAME_KEYS.map((key) => adjustments[key] ?? null)]);
}

const copy = (crop: Crop): Crop => ({ unit: 'px', x: crop.x, y: crop.y, width: crop.width, height: crop.height });

const centre = (crop: Crop) => ({ cx: crop.x + crop.width / 2, cy: crop.y + crop.height / 2 });

const within = (crop: Crop, rect: Crop) =>
  crop.x >= rect.x - 1 &&
  crop.y >= rect.y - 1 &&
  crop.x + crop.width <= rect.x + rect.width + 1 &&
  crop.y + crop.height <= rect.y + rect.height + 1;

/** The reference a ratio change uses: the kept one for this photo and frame,
 * or the whole photo around the current crop's centre. */
export function referenceForRatioChange(
  reference: CropReference | null,
  key: string,
  crop: Crop | null | undefined,
  orientedW: number,
  orientedH: number,
  /** The recipe's ratio now: a whole-photo crop without one is a reset. */
  aspectRatio: number | null = null
): CropReference {
  const hasCrop = !!crop && crop.width > 0 && crop.height > 0;
  // Kept while the crop is one this reference made or the user moved within
  // it; a reset (no crop, the whole photo), an undo or a refit drops it.
  if (reference && reference.key === key && hasCrop) {
    const inside = reference.rect
      ? within(crop, reference.rect)
      : !!aspectRatio || !(crop.x <= 0 && crop.y <= 0 && crop.width >= orientedW && crop.height >= orientedH);
    if (inside) return reference;
  }
  const c = hasCrop ? centre(crop) : { cx: orientedW / 2, cy: orientedH / 2 };
  return { key, rect: null, ...c };
}

/** The reference after the user changed the crop by hand: a moved crop
 * moves the reference with it, a drawn or resized one becomes it. */
export function referenceAfterHandEdit(
  reference: CropReference | null,
  key: string,
  previous: Crop | null | undefined,
  next: Crop,
  orientedW: number,
  orientedH: number,
  rotation: number
): CropReference {
  const moved =
    reference && reference.key === key && previous && previous.width === next.width && previous.height === next.height;
  if (moved) {
    const rect = reference.rect
      ? forceCropInBounds(
          {
            ...reference.rect,
            x: reference.rect.x + next.x - previous.x,
            y: reference.rect.y + next.y - previous.y
          },
          orientedW,
          orientedH,
          rotation
        )
      : null;
    // Moved back inside a straightened photo, the drawn crop may no longer
    // hold the moved one: the moved crop is the reference then.
    return { key, rect: rect && !within(next, rect) ? copy(next) : rect, ...centre(next) };
  }
  return { key, rect: copy(next), ...centre(next) };
}

/** The largest crop of `aspectRatio` inside the reference (the drawn crop,
 * or the photo at its straightening), as near the reference's centre as it
 * fits. */
export function fitRatioCrop(
  imageWidth: number,
  imageHeight: number,
  orientationSteps: number,
  aspectRatio: number,
  rotation: number,
  reference: CropReference
): Crop | null {
  if (!(aspectRatio > 0)) return null;
  const { width: W, height: H } = getOrientedDimensions(imageWidth, imageHeight, orientationSteps);
  const r = reference.rect;
  if (r && r.width > 0 && r.height > 0 && isCropWithinBounds(r, W, H, rotation)) {
    const width = Math.min(r.width, Math.round(Math.min(r.width, r.height * aspectRatio)));
    const height = Math.min(r.height, Math.round(width / aspectRatio));
    const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));
    return {
      unit: 'px',
      x: clamp(Math.round(reference.cx - width / 2), r.x, r.x + r.width - width),
      y: clamp(Math.round(reference.cy - height / 2), r.y, r.y + r.height - height),
      width,
      height
    };
  }
  const largest = calculateCenteredCrop(imageWidth, imageHeight, orientationSteps, aspectRatio, rotation);
  if (!largest) return null;
  const dx = Math.round(reference.cx - (largest.x + largest.width / 2));
  const dy = Math.round(reference.cy - (largest.y + largest.height / 2));
  // Centred within rounding: the largest crop as it is.
  if (Math.abs(dx) <= 1 && Math.abs(dy) <= 1) return largest;
  if (rotation === 0) {
    // Exact: slide it as far towards the centre as the photo allows.
    const moved = forceCropInBounds({ ...largest, x: largest.x + dx, y: largest.y + dy }, W, H, 0);
    return { ...moved, x: Math.round(moved.x), y: Math.round(moved.y) };
  }
  return moveCropInsideBounds(largest, dx, dy, W, H, rotation);
}

type CropState = { crop?: Crop | null; aspectRatio?: number | null };

/** The reference after an undo, a redo or a jump in the history: kept when
 * the crop did not change; a restored free crop (no ratio) becomes it, as if
 * drawn again; else none (the whole photo). */
export function referenceAfterHistoryStep(
  reference: CropReference | null,
  path: string | null | undefined,
  from: CropState,
  to: CropState
): CropReference | null {
  if (JSON.stringify(from.crop ?? null) === JSON.stringify(to.crop ?? null)) return reference;
  const crop = to.crop;
  if (!path || !crop || !(crop.width > 0 && crop.height > 0) || to.aspectRatio) return null;
  return { key: cropReferenceKey(path, to as Record<string, unknown>), rect: copy(crop), ...centre(crop) };
}

/** The reference each history entry had when it was recorded: an undo or a
 * redo returns to it, so a ratio change after an undo fits into the crop the
 * user had drawn at that step. */
const historyReferences = new WeakMap<object, CropReference | null>();

export function rememberCropReference(entry: object, reference: CropReference | null) {
  historyReferences.set(entry, reference);
}

/** The reference after an undo, a redo or a jump to `to`: the one recorded
 * with that entry, else (an entry from before the photo was opened)
 * `referenceAfterHistoryStep`. */
export function referenceForHistoryEntry(
  reference: CropReference | null,
  path: string | null | undefined,
  from: CropState,
  to: CropState
): CropReference | null {
  if (historyReferences.has(to)) return historyReferences.get(to) ?? null;
  return referenceAfterHistoryStep(reference, path, from, to);
}
