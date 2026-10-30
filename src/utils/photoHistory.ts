import type { Adjustments } from './adjustments';
import { sameAdjustmentValue } from './adjustments';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { historyOrders, nextUndoOrder, setHistoryOrders } from './undoOrder';

// Each photograph keeps its History while another one is open, as
// Lightroom's per-photo Develop history (ui-review-2026-10-28 C02).
interface Kept {
  history: Adjustments[];
  index: number;
  orders: number[];
  /** The batch steps (by undo order) that had written it when it was left;
   * a step still writing then is not among them. */
  written: ReadonlySet<number>;
}
/** Photographs whose History is kept (least recently left dropped first). */
const KEPT_PHOTOS = 100;
const HISTORY_LIMIT = 50;
const kept = new Map<string, Kept>();

/** The batch steps that had written `path` when it was left, if its History
 * is kept (`recipeStepsSince` gives those that wrote it since). */
export const keptSince = (path: string) => kept.get(path)?.written;

/** The undo orders of the batch steps that wrote `path` (appUndo.ts). */
let writtenSteps: (path: string) => number[] = () => [];
export function registerWrittenSteps(steps: (path: string) => number[]) {
  writtenSteps = steps;
}

/** The History `path` opens with: the one it had when it was left, with
 * `shown` (its recipe as loaded now) as its current entry. When its recipe
 * changed meanwhile, each batch step that wrote it since (`batches`, oldest
 * first, with the recipe it wrote) becomes an entry with its undo order, so
 * each undoes together with its step; the last one is `shown`. A change
 * by none (another editor) is a new entry of its own. */
export function historyFor(
  path: string,
  shown: Adjustments,
  batches: { order: number; recipe: Adjustments }[] = [],
): { history: Adjustments[]; historyIndex: number } {
  const entry = kept.get(path);
  if (!entry) return { history: [shown], historyIndex: 0 };
  kept.delete(path);
  if (sameAdjustmentValue(entry.history[entry.index], shown)) {
    const history = [...entry.history];
    history[entry.index] = shown;
    setHistoryOrders(history, entry.orders);
    return { history, historyIndex: entry.index };
  }
  const steps = batches.length ? batches : [{ order: nextUndoOrder(), recipe: shown }];
  const history = [
    ...entry.history.slice(0, entry.index + 1),
    ...steps.slice(0, -1).map((step) => step.recipe),
    shown,
  ].slice(-HISTORY_LIMIT);
  const orders = [...entry.orders.slice(0, entry.index + 1), ...steps.map((step) => step.order)].slice(
    -HISTORY_LIMIT,
  );
  setHistoryOrders(history, orders);
  return { history, historyIndex: history.length - 1 };
}

/** A batch step with undo order `order` travelled `direction` on `path`
 * while another photograph is open: a kept History holding the step's entry
 * moves with it (other kept Histories are compared when they open). */
export function travelKeptHistory(path: string, order: number, direction: 'undo' | 'redo') {
  const entry = kept.get(path);
  if (!entry) return;
  if (direction === 'undo' && entry.index > 0 && entry.orders[entry.index] === order) entry.index--;
  else if (direction === 'redo' && entry.orders[entry.index + 1] === order) entry.index++;
}

function keep(path: string, history: Adjustments[], index: number) {
  kept.delete(path);
  kept.set(path, { history, index, orders: historyOrders(history), written: new Set(writtenSteps(path)) });
  while (kept.size > KEPT_PHOTOS) kept.delete(kept.keys().next().value!);
}

// A loaded photograph's History is kept when another one opens (or it
// closes); one still loading has none of its own yet.
useEditorStore.subscribe((state, previous) => {
  const left = previous.selectedImage;
  if (!left?.isReady || state.selectedImage?.path === left.path) return;
  if (previous.history.length > 1) keep(left.path, previous.history, previous.historyIndex);
});
// Histories belong to one open library.
useLibraryStore.subscribe((state, previous) => {
  if (state.sessionDirectory !== previous.sessionDirectory) kept.clear();
});
