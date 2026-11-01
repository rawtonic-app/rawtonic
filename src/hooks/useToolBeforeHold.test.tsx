import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { TOOL_BEFORE_HOLD_MS, useToolBeforeHold } from './useToolBeforeHold';
import { updateAdjustments } from './useEditorActions';
import { useEditorStore } from '../store/useEditorStore';
import { INITIAL_ADJUSTMENTS } from '../utils/adjustments';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => new Promise(() => {})) }));

const resetExposure = vi.fn(() => updateAdjustments((prev) => ({ ...prev, exposure: 0 })));
function Reset() {
  const hold = useToolBeforeHold('Exposure', resetExposure);
  return <button type="button" {...hold}>reset</button>;
}

beforeEach(() => {
  vi.useFakeTimers();
  resetExposure.mockClear();
  const adjustments = { ...INITIAL_ADJUSTMENTS, exposure: 2 };
  useEditorStore.setState({
    selectedImage: { path: '/a.dng', isReady: true, width: 10, height: 10 } as never,
    adjustments, history: [adjustments], historyIndex: 0,
    showOriginal: false, toolBefore: null, previewOverride: null, isSliderDragging: false,
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const button = () => document.querySelector('button')!;

it('a short click resets the tool', () => {
  render(<Reset />);
  fireEvent.pointerDown(button(), { button: 0 });
  fireEvent.pointerUp(button());
  fireEvent.click(button());
  expect(useEditorStore.getState().adjustments.exposure).toBe(0);
  expect(useEditorStore.getState().toolBefore).toBeNull();
});

it('Alt + press shows the photograph without the tool until released, and does not reset', () => {
  render(<Reset />);
  fireEvent.pointerDown(button(), { button: 0, altKey: true });
  expect(useEditorStore.getState()).toMatchObject({ toolBefore: 'Exposure', previewOverride: { exposure: 0 } });
  fireEvent.pointerUp(button());
  fireEvent.click(button(), { altKey: true });
  expect(useEditorStore.getState()).toMatchObject({ toolBefore: null, previewOverride: null });
  expect(useEditorStore.getState().adjustments.exposure).toBe(2);
});

it('a long plain press shows it too; its release does not reset', () => {
  render(<Reset />);
  fireEvent.pointerDown(button(), { button: 0 });
  expect(useEditorStore.getState().toolBefore).toBeNull();
  act(() => vi.advanceTimersByTime(TOOL_BEFORE_HOLD_MS));
  expect(useEditorStore.getState().toolBefore).toBe('Exposure');
  fireEvent.pointerUp(button());
  fireEvent.click(button());
  expect(useEditorStore.getState()).toMatchObject({ toolBefore: null, previewOverride: null });
  expect(useEditorStore.getState().adjustments.exposure).toBe(2);
});

it('a focus loss ends it', () => {
  render(<Reset />);
  fireEvent.pointerDown(button(), { button: 0, altKey: true });
  act(() => void window.dispatchEvent(new Event('blur')));
  expect(useEditorStore.getState().toolBefore).toBeNull();
});
