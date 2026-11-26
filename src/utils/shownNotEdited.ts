import type { Adjustments } from './adjustments';

/** Recipes the editor shows that are no edit of the photograph (a never-edited
 * one showing new develop defaults, `showNewDevelopDefaults`): not saved, so
 * it keeps following them. */
export const shownNotEdited = new WeakSet<Adjustments>();

/** The full decode can refine the aspect of a recipe shown on a proxy.
 * That transition is no edit. A later Undo to the same object still saves. */
const refinedOpenedAspectFrom = new WeakMap<Adjustments, Adjustments>();

export function markOpenedAspectRefinement(from: Adjustments, to: Adjustments) {
  refinedOpenedAspectFrom.set(to, from);
}

/** Recipes a load just published from the saved one (`publishSource`): the
 * photograph as it is on disk, no edit, whichever recipe the editor showed
 * before — also when the same photograph loads again (selected again, back
 * after an undone Move), whose new recipe object only equals the old one.
 * Saving it would rewrite the sidecars beside a linked original with the
 * full recipe (WQA7). Forgotten once the editor has taken it
 * (`forgetLoadedRecipe`), so a later Undo back to it still saves. */
const loadedNotEdited = new WeakSet<Adjustments>();

export function markLoadedRecipe(recipe: Adjustments) {
  loadedNotEdited.add(recipe);
}

export function forgetLoadedRecipe(recipe: Adjustments) {
  loadedNotEdited.delete(recipe);
}

export const isUneditedTransition = (from: Adjustments, to: Adjustments) =>
  shownNotEdited.has(to) || loadedNotEdited.has(to) || refinedOpenedAspectFrom.get(to) === from;
