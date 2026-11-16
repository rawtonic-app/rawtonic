import { beforeEach, expect, it } from 'vitest';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';
import { historyFor, sameSavedEdit } from './photoHistory';
import { editUndoOrder } from './undoOrder';

const recipe = (exposure: number): Adjustments => ({ ...INITIAL_ADJUSTMENTS, exposure });
const open = (path: string) => useEditorStore.setState({ selectedImage: { path, isReady: true } as never });

beforeEach(() => {
  useEditorStore.setState(useEditorStore.getInitialState());
  useLibraryStore.setState({ sessionDirectory: '/job' });
});

it('a photograph keeps its History when another one is opened and back (C02)', () => {
  open('/a.nef');
  useEditorStore.getState().pushHistory(recipe(0.7));
  useEditorStore.getState().pushHistory(recipe(0.8));
  const orders = [1, 2].map((index) => editUndoOrder(useEditorStore.getState().history, index));
  open('/b.nef');
  useEditorStore.getState().resetHistory(recipe(0));
  // Back on the first one, loaded again from its saved recipe.
  const loaded = recipe(0.8);
  const { history, historyIndex } = historyFor('/a.nef', loaded);
  expect(history.map((step) => step.exposure)).toEqual([0, 0.7, 0.8]);
  expect(historyIndex).toBe(2);
  expect(history[2]).toBe(loaded);
  expect([1, 2].map((index) => editUndoOrder(history, index))).toEqual(orders);
});

it('reading a saved edit back adds no History step (HIST1)', () => {
  // A never-edited RAF's camera lens data, as the editor shows it.
  const lens = { model: 3, k1: 0, tca_vr: 1.0004769346969977, table_radii: [0, 0.3629483687877655] };
  open('/a.raf');
  useEditorStore.getState().pushHistory({
    ...recipe(0),
    shadows: 100,
    lensCorrectionMode: 'camera',
    lensDistortionParams: lens
  } as Adjustments);
  open('/b.raf');
  // As a session photograph's saved recipe reads back: with the lens stamp
  // its save adds, its colour label (library metadata, "" without one) and
  // the backend's key order.
  const sorted = <T>(value: T): T =>
    value && typeof value === 'object' && !Array.isArray(value)
      ? (Object.fromEntries(
          Object.entries(value)
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([key, item]) => [key, sorted(item)])
        ) as T)
      : value;
  const saved = {
    ...recipe(0),
    shadows: 100,
    lensCorrectionMode: 'camera',
    lensProfileFor: '["camera",1,{"model":3}]',
    colorLabel: '',
    hsl: sorted(INITIAL_ADJUSTMENTS.hsl),
    colorGrading: sorted(INITIAL_ADJUSTMENTS.colorGrading),
    // A camera's lens data as the backend's JSON parser reads it back
    // (serde_json: 1.0004769346969977 → 1.0004769346969975).
    lensDistortionParams: { ...lens, tca_vr: 1.0004769346969975 }
  } as unknown as Adjustments;
  const { history, historyIndex } = historyFor('/a.raf', saved);
  expect(history.map((step) => step.shadows)).toEqual([0, 100]);
  expect(historyIndex).toBe(1);
  expect(history[1]).toBe(saved);
});

it('a recipe read back differs from the held one only by what the save adds (HIST1)', () => {
  const held = { ...recipe(0.5), lensDistortionParams: { k1: 0.0123456789012345 } } as Adjustments;
  const read = (values: Partial<Adjustments>) => ({ ...held, colorLabel: '', ...values }) as Adjustments;
  // One unit in the last place off, as the backend's parser may read it.
  const k1 = 0.0123456789012345 * (1 + Number.EPSILON);
  expect(k1).not.toBe(0.0123456789012345);
  expect(sameSavedEdit(held, read({ lensDistortionParams: { k1 } } as never))).toBe(true);
  // An edit made meanwhile, however small, is a change.
  expect(sameSavedEdit(held, read({ lensDistortionParams: { k1: 0.0124 } } as never))).toBe(false);
  expect(sameSavedEdit(held, read({ exposure: 0.51 }))).toBe(false);
  expect(sameSavedEdit(held, read({ lensDistortionParams: null } as never))).toBe(false);
});

it('a recipe changed while another photo was open becomes a new step (C02)', () => {
  open('/a.nef');
  useEditorStore.getState().pushHistory(recipe(0.7));
  open('/b.nef');
  const { history, historyIndex } = historyFor('/a.nef', recipe(1.85), [{ order: 42, recipe: recipe(1.85) }]);
  expect(history.map((step) => step.exposure)).toEqual([0, 0.7, 1.85]);
  expect(historyIndex).toBe(2);
  // The batch step that wrote it (undoes with it).
  expect(editUndoOrder(history, 2)).toBe(42);
});

it('each batch step that wrote a closed photo becomes its own entry (C02)', () => {
  open('/a.nef');
  useEditorStore.getState().pushHistory(recipe(1));
  open('/b.nef');
  const { history, historyIndex } = historyFor('/a.nef', recipe(3), [
    { order: 41, recipe: recipe(2) },
    { order: 42, recipe: recipe(3) }
  ]);
  expect(history.map((step) => step.exposure)).toEqual([0, 1, 2, 3]);
  expect(historyIndex).toBe(3);
  expect([2, 3].map((index) => editUndoOrder(history, index))).toEqual([41, 42]);
});

it('a photograph never left starts with its loaded recipe', () => {
  const loaded = recipe(0.3);
  expect(historyFor('/new.nef', loaded)).toEqual({ history: [loaded], historyIndex: 0 });
});

it('a batch still writing when its photograph was left becomes its entry once written (C02)', async () => {
  const { recipeBatchAction, recipeStepsSince } = await import('./appUndo');
  useEditorStore.setState({ history: [recipe(0)], historyIndex: 0 });
  open('/a.nef');
  useEditorStore.getState().pushHistory(recipe(1));
  let written!: () => void;
  const done = recipeBatchAction(
    'sync',
    () =>
      new Promise((resolve) => {
        written = () => resolve({ failed: [], changes: [{ path: '/a.nef', before: recipe(1), after: recipe(2) }] });
      }),
    ['/a.nef']
  );
  // Left while the Sync (its order given out already) still writes it.
  open('/b.nef');
  written();
  await done;
  const { keptSince } = await import('./photoHistory');
  const steps = recipeStepsSince('/a.nef', keptSince('/a.nef'));
  expect(steps.map((step) => step.after)).toEqual([recipe(2)]);
});

it('the lens stamp its save added is not a change of its own (C02)', () => {
  open('/a.nef');
  useEditorStore.getState().pushHistory(recipe(0.3));
  open('/b.nef');
  const saved = { ...recipe(0.3), lensProfileFor: '["camera",1,{}]' };
  const { history, historyIndex } = historyFor('/a.nef', saved);
  expect(history.map((step) => step.exposure)).toEqual([0, 0.3]);
  expect(historyIndex).toBe(1);
  expect(history[1]).toBe(saved);
});
