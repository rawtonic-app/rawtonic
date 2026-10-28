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
  const { history, historyIndex } = historyFor('/a.nef', recipe(1.85), 42);
  expect(history.map((step) => step.exposure)).toEqual([0, 0.7, 1.85]);
  expect(historyIndex).toBe(2);
  // The batch step that wrote it (undoes with it).
  expect(editUndoOrder(history, 2)).toBe(42);
});

it('a photograph never left starts with its loaded recipe', () => {
  const loaded = recipe(0.3);
  expect(historyFor('/new.nef', loaded)).toEqual({ history: [loaded], historyIndex: 0 });
});
