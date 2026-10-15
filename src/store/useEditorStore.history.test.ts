import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { debouncedSetHistory, useEditorStore } from './useEditorStore';
import { INITIAL_ADJUSTMENTS, type Adjustments } from '../utils/adjustments';

const state = () => useEditorStore.getState();
const recipe = (value: number): Adjustments => ({ ...INITIAL_ADJUSTMENTS, relightSkinReflections: value });
const edit = (value: number) => {
  const adjustments = recipe(value);
  state().setEditor({ adjustments });
  debouncedSetHistory(adjustments);
};
beforeEach(() => {
  vi.useFakeTimers();
  debouncedSetHistory.cancel();
  useEditorStore.setState(useEditorStore.getInitialState());
  state().resetHistory(recipe(0));
});
afterEach(() => {
  debouncedSetHistory.cancel();
  vi.useRealTimers();
});

it('undoes the first edit immediately and keeps it available for redo', () => {
  edit(65);
  expect(state().historyIndex).toBe(0);
  expect(state().historyPending).toBe(true);
  state().undo();
  expect(state().adjustments.relightSkinReflections).toBe(0);
  expect(state().historyPending).toBe(false);
  vi.advanceTimersByTime(1000);
  expect(state().history).toHaveLength(2);
  expect(state().historyIndex).toBe(0);
  state().redo();
  expect(state().adjustments.relightSkinReflections).toBe(65);
});
it('coalesces slider changes but settles only their latest value before undo', () => {
  edit(10);
  vi.advanceTimersByTime(100);
  edit(20);
  vi.advanceTimersByTime(100);
  edit(75);
  state().undo();
  expect(state().history.map((r) => r.relightSkinReflections)).toEqual([0, 75]);
  vi.advanceTimersByTime(1000);
  expect(state().adjustments.relightSkinReflections).toBe(0);
  state().redo();
  expect(state().adjustments.relightSkinReflections).toBe(75);
});
it('flushes a branch edit before redo instead of resurrecting the discarded branch', () => {
  edit(25); debouncedSetHistory.flush();
  state().undo();
  edit(80);
  state().redo();
  expect(state().history.map((r) => r.relightSkinReflections)).toEqual([0, 80]);
  expect(state().adjustments.relightSkinReflections).toBe(80);
  vi.advanceTimersByTime(1000);
  expect(state().history).toHaveLength(2);
});
it('cancels pending history on reset and when selecting another photo', () => {
  edit(65);
  state().resetHistory(recipe(30));
  vi.advanceTimersByTime(1000);
  expect(state().history.map((r) => r.relightSkinReflections)).toEqual([30]);
  edit(70);
  state().setEditor({ selectedImage: { path: '/fixture/another.jpg' } as NonNullable<ReturnType<typeof state>['selectedImage']> });
  vi.advanceTimersByTime(1000);
  expect(state().history.map((r) => r.relightSkinReflections)).toEqual([30]);
  expect(state().historyPending).toBe(false);
});
it('commits naturally once after idle and does not duplicate on undo', () => {
  edit(65);
  vi.advanceTimersByTime(500);
  expect(state().history).toHaveLength(2);
  expect(state().historyPending).toBe(false);
  state().undo();
  expect(state().history).toHaveLength(2);
});
it('keeps the clicked history entry when committing shifts the 50-entry window', () => {
  const history = Array.from({ length: 50 }, (_, i) => recipe(i));
  state().setEditor({ history, historyIndex: 49, adjustments: history[49] });
  edit(80);
  state().goToHistoryIndex(10);
  expect(state().history).toHaveLength(50);
  expect(state().historyIndex).toBe(9);
  expect(state().adjustments).toBe(history[10]);
  vi.advanceTimersByTime(1000);
  expect(state().historyIndex).toBe(9);
});
it('direct history commits cancel the older scheduled edit', () => {
  edit(65);
  const newer = recipe(90);
  state().setEditor({ adjustments: newer });
  state().pushHistory(newer);
  vi.advanceTimersByTime(1000);
  expect(state().history.map((r) => r.relightSkinReflections)).toEqual([0, 90]);
});

it('can still select the oldest snapshot evicted by flushing pending history', () => {
  const history = Array.from({ length: 50 }, (_, i) => recipe(i));
  state().setEditor({ history, historyIndex: 49, adjustments: history[49] });
  edit(80);
  state().goToHistoryIndex(0);
  expect(state().adjustments).toBe(history[0]);
  expect(state().history).toHaveLength(50);
  state().undo();
  expect(state().adjustments.relightSkinReflections).toBe(80);
});

it('makes a drag one step however long it pauses, pushed when it ends', () => {
  edit(10);
  state().setEditor({ isSliderDragging: true });
  edit(20);
  vi.advanceTimersByTime(800);
  edit(30);
  vi.advanceTimersByTime(800);
  // The edit before the drag is its own step; nothing of the drag yet.
  expect(state().history.map((r) => r.relightSkinReflections)).toEqual([0, 10]);
  expect(state().historyPending).toBe(true);
  state().setEditor({ isSliderDragging: false });
  vi.advanceTimersByTime(500);
  expect(state().history.map((r) => r.relightSkinReflections)).toEqual([0, 10, 30]);
  expect(state().historyPending).toBe(false);
});

it('settles a drag in progress on undo and drops it on a photo switch', () => {
  state().setEditor({ isSliderDragging: true });
  edit(40);
  state().undo();
  expect(state().history.map((r) => r.relightSkinReflections)).toEqual([0, 40]);
  expect(state().adjustments.relightSkinReflections).toBe(0);
  edit(50);
  state().setEditor({ selectedImage: { path: '/fixture/other.jpg' } as NonNullable<ReturnType<typeof state>['selectedImage']> });
  state().setEditor({ isSliderDragging: false });
  vi.advanceTimersByTime(1000);
  expect(state().history.map((r) => r.relightSkinReflections)).toEqual([0, 40]);
});
