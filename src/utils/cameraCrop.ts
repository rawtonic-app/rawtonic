import type { Crop } from 'react-image-crop';
import type { Adjustments } from './adjustments';

/**
 * The aspect ratio set in the camera as the crop new edits start with (ADR
 * 0020, option C; crates/engine/src/camera_crop.rs). A RAW that keeps the
 * whole sensor although the camera was set to another aspect ratio (Samsung
 * EX2F 16:9, Canon 1:1 / 4:3 / 16:9) opens cropped to the camera's frame, as
 * in Lightroom: an ordinary crop the photographer can widen or reset. Only
 * new recipes get it (never edited, or a Reset); an edit keeps its own crop.
 */

/** The camera's crop for each frame version (`get_camera_crops`, index =
 * `sourceFrameVersion`): null where the camera set no other aspect ratio;
 * null or empty when nothing tells it. */
export type CameraCrops = (Crop | null)[] | null;

/** The camera's crop of the frame `frameVersion` develops, if any. */
export function cameraCropFor(crops: CameraCrops | undefined, frameVersion: number | null | undefined): Crop | null {
  return crops?.[frameVersion ?? 0] ?? null;
}

/** `recipe`, a new recipe of a `width` × `height` photo, starting with the
 * camera's crop of its frame (its aspect ratio locked to it, Lightroom's As
 * Shot), else uncropped at the photo's own aspect ratio. */
export function withCameraCrop<T extends Partial<Adjustments>>(
  recipe: T,
  crops: CameraCrops | undefined,
  width: number,
  height: number,
): T {
  const crop = cameraCropFor(crops, recipe.sourceFrameVersion);
  if (crop) return { ...recipe, crop: { ...crop }, aspectRatio: crop.width / crop.height };
  return { ...recipe, crop: null, aspectRatio: width && height ? width / height : null };
}

/** The crop and aspect ratio a crop reset returns to on the open photo
 * (`image`, its recipe's frame `frameVersion`, turned `orientationSteps`
 * quarter turns): the camera's crop, turned with the photo (it is centred,
 * so a quarter turn swaps its axes), as Lightroom's crop reset returns to
 * As Shot; else none. */
export function cropResetValues(
  image: { isRaw?: boolean; cameraCrops?: CameraCrops } | null | undefined,
  frameVersion: number | null | undefined,
  orientationSteps: number | null | undefined,
): Pick<Adjustments, 'crop' | 'aspectRatio'> {
  const crop = image?.isRaw ? cameraCropFor(image.cameraCrops, frameVersion) : null;
  if (!crop) return { crop: null, aspectRatio: null };
  const turned = (orientationSteps ?? 0) % 2 === 1;
  const shown: Crop = turned
    ? { unit: 'px', x: crop.y, y: crop.x, width: crop.height, height: crop.width }
    : { ...crop };
  return { crop: shown, aspectRatio: shown.width / shown.height };
}
