import type { Crop } from 'react-image-crop';
import { useEditorStore } from '../store/useEditorStore';
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
    return { key, rect, ...centre(next) };
  }
  return { key, rect: { unit: 'px', x: next.x, y: next.y, width: next.width, height: next.height }, ...centre(next) };
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

/** The photo, oriented frame and rotation the editor shows now. */
function current() {
  const { selectedImage, adjustments } = useEditorStore.getState();
  if (!selectedImage?.width || !selectedImage?.height) return null;
  const { width, height } = getOrientedDimensions(
    selectedImage.width,
    selectedImage.height,
    adjustments.orientationSteps || 0
  );
  return {
    key: cropReferenceKey(selectedImage.path, adjustments as unknown as Record<string, unknown>),
    width,
    height,
    rotation: adjustments.rotation || 0
  };
}

/** Records a crop the user drew, resized or moved by hand (not one a ratio
 * change, a rotation or a reset fitted). */
export function noteHandDrawnCrop(previous: Crop | null | undefined, next: Crop) {
  const now = current();
  if (!now || !(next.width > 0 && next.height > 0)) return;
  const reference = useEditorStore.getState().cropReference;
  useEditorStore.setState({
    cropReference: referenceAfterHandEdit(reference, now.key, previous, next, now.width, now.height, now.rotation)
  });
}

/** The crop for a new ratio on the photo the editor shows (null: none fits). */
export function cropForRatio(aspectRatio: number): Crop | null {
  const now = current();
  const { selectedImage, adjustments, cropReference } = useEditorStore.getState();
  if (!now || !selectedImage) return null;
  const reference = referenceForRatioChange(
    cropReference,
    now.key,
    adjustments.crop,
    now.width,
    now.height,
    adjustments.aspectRatio
  );
  if (reference !== cropReference) useEditorStore.setState({ cropReference: reference });
  return fitRatioCrop(
    selectedImage.width,
    selectedImage.height,
    adjustments.orientationSteps || 0,
    aspectRatio,
    now.rotation,
    reference
  );
}
