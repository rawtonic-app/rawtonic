import { isNewRecipe, type Adjustments } from './adjustments';

/**
 * The editor stamps each recipe it saves with the photo's own size (full
 * resolution, before orientation) its pixel geometry was made on
 * (`frameSize`, crates/engine/src/recipe_frame.rs), so a later change of
 * the photo's decoded size can move crop, spots and AI bitmaps with the
 * photo instead of leaving them at their pixels. A new recipe (a Reset's
 * `{}`) stays new: it holds no geometry.
 */
export function withFrameSize<T extends Partial<Adjustments>>(recipe: T, width: number, height: number): T {
  if (!(width > 0 && height > 0) || isNewRecipe(recipe)) return recipe;
  const own = recipe.frameSize;
  if (own && own.width === width && own.height === height) return recipe;
  return { ...recipe, frameSize: { width, height } };
}
