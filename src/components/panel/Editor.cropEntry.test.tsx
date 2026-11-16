import { act, cleanup, render } from '@testing-library/react';
import '../../i18n';
import { createRef } from 'react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import Editor, { type TransformWrapperHandle } from './Editor';
import { useEditorStore } from '../../store/useEditorStore';
import { useUIStore } from '../../store/useUIStore';
import { Panel } from '../ui/AppProperties';
import { INITIAL_ADJUSTMENTS } from '../../utils/adjustments';

vi.mock('./editor/ImageCanvas', () => ({ default: () => null }));
vi.mock('../../hooks/useImageRenderSize', () => ({
  useImageRenderSize: () => ({
    width: 400,
    height: 300,
    scale: 0.1,
    offsetX: 0,
    offsetY: 0,
    containerWidth: 400,
    containerHeight: 300,
    margin: 0
  })
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

beforeEach(() => {
  vi.stubGlobal('requestAnimationFrame', () => 1);
  vi.stubGlobal('cancelAnimationFrame', () => {});
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    }
  );
  useUIStore.setState({ activePanel: Panel.Adjustments });
  useEditorStore.setState({
    selectedImage: { path: '/a.raw', width: 4000, height: 3000, isReady: true } as never,
    adjustments: INITIAL_ADJUSTMENTS,
    history: [INITIAL_ADJUSTMENTS],
    historyIndex: 0,
    displaySize: { width: 400, height: 300 }
  });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it('opening the Crop tab on an uncropped photo is no edit (UXR4M-18)', () => {
  // Lightroom and Capture One change nothing until the frame is moved: no
  // History step, no edited badge, no sidecar write.
  render(<Editor onContextMenu={() => {}} transformWrapperRef={createRef<TransformWrapperHandle>()} />);
  act(() => useUIStore.setState({ activePanel: Panel.Crop }));
  act(() => useUIStore.setState({ activePanel: Panel.Adjustments }));
  const state = useEditorStore.getState();
  expect(state.adjustments.crop).toBeNull();
  expect(state.history).toHaveLength(1);
  expect(state.historyIndex).toBe(0);
});

it('a ratio chosen in the Crop tab still writes its crop', () => {
  render(<Editor onContextMenu={() => {}} transformWrapperRef={createRef<TransformWrapperHandle>()} />);
  act(() => useUIStore.setState({ activePanel: Panel.Crop }));
  act(() =>
    useEditorStore.getState().setEditor((s) => ({ ...s, adjustments: { ...s.adjustments, aspectRatio: 1 } }))
  );
  const crop = useEditorStore.getState().adjustments.crop;
  expect(crop).not.toBeNull();
  expect(Math.round(crop!.width)).toBe(Math.round(crop!.height));
});
