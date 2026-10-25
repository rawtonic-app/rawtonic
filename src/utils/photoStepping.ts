/**
 * Stepping through photographs in Develop: which neighbours to prepare in
 * the background, and the step after a rating or flag (auto-advance).
 */

/** Asked for by a rating or flag key in Develop (`AutoAdvanceRequest`). */
export const AUTO_ADVANCE_EVENT = "rawtonic:auto-advance";

/** The photograph rated or flagged, and how to give it back its previous
 * rating or flag (Undo right after the step). */
export interface AutoAdvanceRequest {
  rated: string;
  restore: () => void;
}

/**
 * The direction of a step from index `from` to `to` in a list of `length`
 * photographs: 1 forward, -1 back (the arrows wrap at the ends), null for a
 * jump (a click elsewhere in the filmstrip, a new list).
 */
export function stepDirection(from: number, to: number, length: number): 1 | -1 | null {
  if (from < 0 || to < 0 || from === to || length < 2) return null;
  if (to === from + 1 || (from === length - 1 && to === 0)) return 1;
  if (to === from - 1 || (from === 0 && to === length - 1)) return -1;
  return null;
}

/**
 * The photographs to prepare beside the open one at `index`, the likeliest
 * next first: two ahead in the direction of the last step and one behind
 * (forward after a jump), as culling mostly goes one way.
 */
export function neighboursToPrepare(paths: string[], index: number, direction: 1 | -1 | null): string[] {
  if (index < 0) return [];
  const ahead = direction ?? 1;
  return [ahead, 2 * ahead, -ahead]
    .map((step) => paths[index + step])
    .filter((path): path is string => Boolean(path));
}

/**
 * The photograph auto-advance steps to after `rated` was rated or flagged:
 * the next one, none at the end of the list (no wrap, so the last rating
 * does not jump back to the first photograph), none once another photograph
 * is open (the step happened already).
 */
export function autoAdvanceTarget(paths: string[], open: string | null, rated: string): string | null {
  if (open !== rated) return null;
  const at = paths.indexOf(rated);
  return at >= 0 && at + 1 < paths.length ? paths[at + 1] : null;
}

/**
 * After a rating or flag key in Develop: asks for the step to the next
 * photograph, which happens when auto-advance is on (Preferences). The
 * rating or flag is bound to `rated` before the step; `restore` gives it
 * back its previous one.
 */
export function requestAutoAdvance(rated: string | undefined, restore: () => void) {
  if (rated) {
    window.dispatchEvent(new CustomEvent<AutoAdvanceRequest>(AUTO_ADVANCE_EVENT, { detail: { rated, restore } }));
  }
}
