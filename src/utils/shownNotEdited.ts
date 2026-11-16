import type { Adjustments } from './adjustments';

/** Recipes the editor shows that are no edit of the photograph (a never-edited
 * one showing new develop defaults, `showNewDevelopDefaults`): not saved, so
 * it keeps following them. */
export const shownNotEdited = new WeakSet<Adjustments>();
