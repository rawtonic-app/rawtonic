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

export const isUneditedTransition = (from: Adjustments, to: Adjustments) =>
  shownNotEdited.has(to) || refinedOpenedAspectFrom.get(to) === from;
