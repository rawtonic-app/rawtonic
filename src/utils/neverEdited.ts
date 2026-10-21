import {
  AUTO_CA_VERSION,
  CAMERA_CA_VERSION,
  FRINGE_VERSION,
  initialAdjustmentsFor,
  normalizeLoadedAdjustments,
  type Adjustments,
} from './adjustments';

/** The camera's own lens data for a photograph (`get_camera_lens_profile`). */
export type CameraLens = { params: Adjustments['lensDistortionParams'] } | null;

/** Whether a saved recipe is one of a photograph never edited (none, or one
 * without a lens mode at all: a batch paste or Auto filled it). */
export const neverEdited = (saved: unknown): boolean =>
  !saved || Boolean((saved as { is_null?: boolean }).is_null) || (saved as Partial<Adjustments>).lensCorrectionMode === undefined;

/** `recipe` (the editor's recipe of `saved`) with what a never-edited RAW
 * starts with, as its export renders it (edge_fringe.rs, lateral_ca.rs,
 * embedded_lens.rs): the edge-fringe pass and the automatic lateral CA
 * unless the recipe states them, and the camera's lens data (`camera`). */
export function applyNeverEditedDefaults(recipe: Adjustments, saved: unknown, isRaw: boolean, camera: CameraLens): Adjustments {
  if (!isRaw || !neverEdited(saved)) return recipe;
  recipe.fringeVersion ??= FRINGE_VERSION;
  recipe.autoCaVersion ??= AUTO_CA_VERSION;
  if (camera?.params) {
    recipe.lensCorrectionMode = 'camera';
    recipe.lensDistortionParams = camera.params;
    // With the camera's data its current CA version, as embedded_lens.rs.
    recipe.cameraCaVersion = CAMERA_CA_VERSION;
  }
  return recipe;
}

/** `applyNeverEditedDefaults` asking for the camera's lens data only when it
 * is needed. */
export async function withNeverEditedDefaults(
  recipe: Adjustments,
  saved: unknown,
  isRaw: boolean,
  camera: () => Promise<CameraLens>,
): Promise<Adjustments> {
  if (!isRaw || !neverEdited(saved)) return recipe;
  return applyNeverEditedDefaults(recipe, saved, isRaw, await camera());
}

/** The recipe the editor opens a saved recipe `saved` (none: never edited)
 * of a photograph `width` × `height` with: normalized, the photo's aspect
 * ratio when it has no crop, a never-edited RAW's defaults (`camera`: its
 * lens data, asked for by the caller when `neverEdited`). */
export function editorRecipe(saved: unknown, isRaw: boolean, width: number, height: number, camera: CameraLens): Adjustments {
  const recipe =
    saved && !(saved as { is_null?: boolean }).is_null
      ? normalizeLoadedAdjustments(saved as Adjustments, isRaw)
      : initialAdjustmentsFor(isRaw);
  if (!recipe.aspectRatio && !recipe.crop) recipe.aspectRatio = width / height;
  return applyNeverEditedDefaults(recipe, saved, isRaw, camera);
}
