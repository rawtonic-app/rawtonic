import { AUTO_CA_VERSION, CAMERA_CA_VERSION, FRINGE_VERSION, type Adjustments } from './adjustments';

/** The camera's own lens data for a photograph (`get_camera_lens_profile`). */
export type CameraLens = { params: Adjustments['lensDistortionParams'] } | null;

/** Whether a saved recipe is one of a photograph never edited (none, or one
 * without a lens mode at all: a batch paste or Auto filled it). */
export const neverEdited = (saved: unknown): boolean =>
  !saved || Boolean((saved as { is_null?: boolean }).is_null) || (saved as Partial<Adjustments>).lensCorrectionMode === undefined;

/** `recipe` (the editor's recipe of `saved`) with what a never-edited RAW
 * starts with, as its export renders it (edge_fringe.rs, lateral_ca.rs,
 * embedded_lens.rs): the edge-fringe pass, the automatic lateral CA, and the
 * camera's lens data (`camera`, asked for only then). */
export async function withNeverEditedDefaults(
  recipe: Adjustments,
  saved: unknown,
  isRaw: boolean,
  camera: () => Promise<CameraLens>,
): Promise<Adjustments> {
  if (!isRaw || !neverEdited(saved)) return recipe;
  recipe.fringeVersion = FRINGE_VERSION;
  recipe.autoCaVersion = AUTO_CA_VERSION;
  const lens = await camera();
  if (lens?.params) {
    recipe.lensCorrectionMode = 'camera';
    recipe.lensDistortionParams = lens.params;
    recipe.cameraCaVersion = CAMERA_CA_VERSION;
  }
  return recipe;
}
