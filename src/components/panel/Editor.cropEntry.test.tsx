import { act, cleanup, render } from '@testing-library/react';
import '../../i18n';
import { createRef } from 'react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import Editor, { type TransformWrapperHandle } from './Editor';
import { useEditorStore } from '../../store/useEditorStore';
import { useUIStore } from '../../store/useUIStore';
import { Panel } from '../ui/AppProperties';
import { INITIAL_ADJUSTMENTS } from '../../utils/adjustments';
import type { Crop, PercentCrop } from 'react-image-crop';

// The canvas is replaced; the test plays ReactCrop, which reports the crop
// it is first given as complete (componentDidUpdate).
type CanvasProps = { crop: PercentCrop | null; handleCropComplete(c: Crop, pc: PercentCrop): void };
const canvas = vi.hoisted(() => ({ props: null as null | CanvasProps }));
vi.mock('./editor/ImageCanvas', () => ({
  default: (props: CanvasProps) => {
    canvas.props = props;
    return null;
  }
}));
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
  // The whole frame is shown, and ReactCrop reports it as complete.
  const shown = canvas.props!.crop!;
  expect([shown.x, shown.y, shown.width, shown.height]).toEqual([0, 0, 100, 100]);
  act(() => canvas.props!.handleCropComplete({ unit: 'px', x: 0, y: 0, width: 400, height: 300 }, shown));
  act(() => useUIStore.setState({ activePanel: Panel.Adjustments }));
  const state = useEditorStore.getState();
  expect(state.adjustments.crop).toBeNull();
  expect(state.history).toHaveLength(1);
  expect(state.historyIndex).toBe(0);
});

it('a ratio chosen in the Crop tab still writes its crop', () => {
  render(<Editor onContextMenu={() => {}} transformWrapperRef={createRef<TransformWrapperHandle>()} />);
  act(() => useUIStore.setState({ activePanel: Panel.Crop }));
  act(() => useEditorStore.getState().setEditor((s) => ({ ...s, adjustments: { ...s.adjustments, aspectRatio: 1 } })));
  const crop = useEditorStore.getState().adjustments.crop;
  expect(crop).not.toBeNull();
  expect(Math.round(crop!.width)).toBe(Math.round(crop!.height));
});

it('a frame moved in the Crop tab is an edit', () => {
  render(<Editor onContextMenu={() => {}} transformWrapperRef={createRef<TransformWrapperHandle>()} />);
  act(() => useUIStore.setState({ activePanel: Panel.Crop }));
  const moved: PercentCrop = { unit: '%', x: 10, y: 10, width: 80, height: 80 };
  act(() => canvas.props!.handleCropComplete({ unit: 'px', x: 40, y: 30, width: 320, height: 240 }, moved));
  expect(useEditorStore.getState().adjustments.crop).toMatchObject({ x: 400, y: 300, width: 3200, height: 2400 });
});
