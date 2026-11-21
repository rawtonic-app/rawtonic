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
/** A recipe without what saving and reading it back add that is no edit:
 * the lens stamp its save adds (`lensProfileFor`,
 * `lens_recipe::lens_profile_stamp`) and the colour label a session
 * photograph's recipe is read with (`colorLabel`, library metadata;
 * crates/catalog `apply_editor_metadata`): the same edit before and after
 * saving. */
// The save's stamp of the photo size (utils/frameSize.ts) is no edit either.
const withoutReadStamps = (recipe: Adjustments) => ({
  ...recipe,
  lensProfileFor: undefined,
  colorLabel: undefined,
  frameSize: undefined
});

/** Whether two numbers are the same as a save keeps them: they may differ in
 * their last digits, since the backend's JSON parser rounds some 17-digit
 * decimals to the neighbouring double (a never-edited Fujifilm RAF's camera
 * lens data reads back one unit in the last place off). */
export const sameSavedNumber = (a: number, b: number): boolean =>
  a === b || Math.abs(a - b) <= 1e-12 * Math.max(Math.abs(a), Math.abs(b));

/** Whether two values are the same as a save keeps them (`sameSavedNumber`). */
function sameAsSaved(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a === 'number' && typeof b === 'number') return sameSavedNumber(a, b);
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((item, i) => sameAsSaved(item, b[i]));
  }
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
  const keys = Object.keys(left).filter((key) => left[key] !== undefined);
  return (
    keys.length === Object.keys(right).filter((key) => right[key] !== undefined).length &&
    keys.every((key) => sameAsSaved(left[key], right[key]))
  );
}

/** Whether `read` (a recipe as read back from its save) holds the same edit
 * as `held` (the editor's): nobody edited it in between, so it is no step of
 * its History. */
export function sameSavedEdit(held: Adjustments, read: Adjustments): boolean {
  const left = withoutReadStamps(held);
  const right = withoutReadStamps(read);
  return sameAdjustmentValue(left, right) || sameAsSaved(left, right);
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
  batches: { order: number; recipe: Adjustments }[] = []
): { history: Adjustments[]; historyIndex: number } {
  const entry = kept.get(path);
  if (!entry) return { history: [shown], historyIndex: 0 };
  kept.delete(path);
  if (sameSavedEdit(entry.history[entry.index], shown)) {
    const history = [...entry.history];
    history[entry.index] = shown;
    setHistoryOrders(history, entry.orders);
    return { history, historyIndex: entry.index };
  }
  const steps = batches.length ? batches : [{ order: nextUndoOrder(), recipe: shown }];
  const history = [
    ...entry.history.slice(0, entry.index + 1),
    ...steps.slice(0, -1).map((step) => step.recipe),
    shown
  ].slice(-HISTORY_LIMIT);
  const orders = [...entry.orders.slice(0, entry.index + 1), ...steps.map((step) => step.order)].slice(-HISTORY_LIMIT);
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
