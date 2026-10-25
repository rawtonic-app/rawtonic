import type { Adjustments } from './adjustments';

// A monotonic input order, shared by catalog actions and this photo's edits.
let order = 0;
const edits = new WeakMap<Adjustments, number>();
const listeners = new Set<() => void>();
export const nextUndoOrder = () => ++order;
export const editUndoOrder = (recipe: Adjustments | undefined) => (recipe ? (edits.get(recipe) ?? 0) : 0);
export function recordEditOrder(recipe: Adjustments) {
  edits.set(recipe, nextUndoOrder());
  for (const listener of listeners) listener();
}
export function onNewEdit(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
