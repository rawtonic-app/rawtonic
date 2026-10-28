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
}
/** Photographs whose History is kept (least recently left dropped first). */
const KEPT_PHOTOS = 100;
const HISTORY_LIMIT = 50;
const kept = new Map<string, Kept>();

/** The History `path` opens with: the one it had when it was left, with
 * `shown` (its recipe as loaded now) as its current entry; when its recipe
 * changed meanwhile (a batch edit, another editor), `shown` becomes a new
 * entry with the undo order `changedOrder` (the batch step that wrote it,
 * so both undo together; otherwise a new one). */
export function historyFor(
  path: string,
  shown: Adjustments,
  changedOrder?: number,
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
  const history = [...entry.history.slice(0, entry.index + 1), shown].slice(-HISTORY_LIMIT);
  const orders = [...entry.orders.slice(0, entry.index + 1), changedOrder ?? nextUndoOrder()].slice(
    -HISTORY_LIMIT,
  );
  setHistoryOrders(history, orders);
  return { history, historyIndex: history.length - 1 };
}

function keep(path: string, history: Adjustments[], index: number) {
  kept.delete(path);
  kept.set(path, { history, index, orders: historyOrders(history) });
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
