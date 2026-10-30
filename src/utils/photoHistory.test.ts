import { beforeEach, expect, it } from 'vitest';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';
import { historyFor } from './photoHistory';
import { editUndoOrder } from './undoOrder';

const recipe = (exposure: number): Adjustments => ({ ...INITIAL_ADJUSTMENTS, exposure });
const open = (path: string) =>
  useEditorStore.setState({ selectedImage: { path, isReady: true } as never });

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
    { order: 42, recipe: recipe(3) },
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
    ['/a.nef'],
  );
  // Left while the Sync (its order given out already) still writes it.
  open('/b.nef');
  written();
  await done;
  const { keptSince } = await import('./photoHistory');
  const steps = recipeStepsSince('/a.nef', keptSince('/a.nef'));
  expect(steps.map((step) => step.after)).toEqual([recipe(2)]);
});
