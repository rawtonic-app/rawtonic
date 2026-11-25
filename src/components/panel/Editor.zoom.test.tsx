import { act, cleanup, fireEvent, render } from '@testing-library/react';
import '../../i18n';
import { createRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import Editor, { type TransformWrapperHandle } from './Editor';
import { useEditorStore } from '../../store/useEditorStore';
import { useUIStore } from '../../store/useUIStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import { ContextMenuProvider } from '../../context/ContextMenuContext';
import { Invokes, type AppSettings } from '../ui/AppProperties';
import { INITIAL_ADJUSTMENTS } from '../../utils/adjustments';

// Keep the real viewer and targeted pointer surface; replace the renderer.
// Zoom tests also check the preview size requested at animation start.
// The photo pane ImageCanvas last got (`photoHost`).
const canvasProps = vi.hoisted(() => ({ photoHost: undefined as HTMLElement | null | undefined }));
vi.mock('./editor/ImageCanvas', async () => {
  const { default: TargetedAdjustmentOverlay } = await import('./editor/TargetedAdjustmentOverlay');
  // The frame list the preview parks (`switchPreviewLayers`).
  return {
    default: ({ photoHost }: { photoHost?: HTMLElement | null }) => {
      canvasProps.photoHost = photoHost;
      return useEditorStore.getState().tatTarget ? (
        <TargetedAdjustmentOverlay rect={{ left: 0, top: 0, width: 400, height: 300 }} />
      ) : null;
    }
  };
});
// The viewer's fit; a test changes it as the viewer's size would change.
const fit = vi.hoisted(() => ({
  width: 400,
  height: 300,
  scale: 0.1,
  offsetX: 0,
  offsetY: 0,
  containerWidth: 400,
  containerHeight: 300,
  margin: 0
}));
vi.mock('../../hooks/useImageRenderSize', () => ({
  useImageRenderSize: () => ({ ...fit })
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

describe('Editor zoom', () => {
  const platform = useSettingsStore.getState().osPlatform;
  beforeEach(() => {
    vi.useFakeTimers();
    // The animation's frames never run: what is reported comes from its start.
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
    // jsdom has no pointer capture.
    Object.assign(HTMLElement.prototype, {
      setPointerCapture: () => {},
      releasePointerCapture: () => {},
      hasPointerCapture: () => false
    });
    vi.stubGlobal(
      'PointerEvent',
      class extends MouseEvent {
        pointerId = 1;
        pointerType = 'mouse';
      }
    );
    useEditorStore.setState({
      tatTarget: null,
      isWbPickerActive: false,
      colorPickerTarget: null,
      selectedImage: { path: '/a.raw', width: 4000, height: 3000, isReady: true } as never,
      displaySize: { width: 400, height: 300 },
      baseRenderSize: { width: 0, height: 0, offsetX: 0, offsetY: 0, containerWidth: 0, containerHeight: 0 },
      browserFullZoom: false,
      zoom: 1,
      splitView: false
    });
  });

  afterEach(() => {
    // Unmount while the stubs still stand, so no React work outlives the file's jsdom.
    cleanup();
    useSettingsStore.setState({ osPlatform: platform });
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it.each([
    { activeView: 'editor', libraryLoupe: false },
    { activeView: 'library', libraryLoupe: true }
  ] as const)('applies the saved color around the native photo in %j', async (view) => {
    vi.useRealTimers();
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => setTimeout(() => callback(0), 0));
    vi.stubGlobal('cancelAnimationFrame', (id: number) => clearTimeout(id));
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue(new DOMRect(10, 20, 500, 300));
    useUIStore.setState(view);
    useSettingsStore.setState({ osPlatform: 'macos', appSettings: { keybinds: {} } as AppSettings });
    useEditorStore.setState({ hasRenderedFirstFrame: true });
    vi.mocked(invoke).mockClear();
    const viewer = createRef<TransformWrapperHandle>();
    const { getByText, unmount } = render(
      <ContextMenuProvider>
        <Editor onContextMenu={() => {}} transformWrapperRef={viewer} />
      </ContextMenuProvider>
    );
    try {
      const transforms = () =>
        vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.UpdateWgpuTransform);
      await vi.waitFor(() => expect(transforms().length).toBeGreaterThan(0));
      const initial = (transforms().at(-1)?.[1] as { payload: Record<string, unknown> }).payload;
      expect(initial.bgSecondary).toEqual([38 / 255, 38 / 255, 38 / 255, 1]);

      fireEvent.contextMenu(viewer.current!.instance.wrapperComponent!, { clientX: 460, clientY: 45 });
      fireEvent.click(getByText('White'));
      expect(invoke).toHaveBeenCalledWith(Invokes.SaveSettings, {
        settings: expect.objectContaining({ viewerBackground: 'white' })
      });
      await vi.waitFor(() => {
        expect(transforms().at(-1)?.[1]).toEqual({ payload: { ...initial, bgSecondary: [1, 1, 1, 1] } });
      });
      act(() => useUIStore.setState({ surroundings: 'black' }));
      await vi.waitFor(() => {
        expect(transforms().at(-1)?.[1]).toEqual({ payload: { ...initial, bgSecondary: [0, 0, 0, 1] } });
      });
    } finally {
      unmount();
      useEditorStore.setState({ hasRenderedFirstFrame: false });
      useUIStore.setState({ activeView: 'editor', libraryLoupe: false, surroundings: 'normal' });
      useSettingsStore.setState({ appSettings: null });
    }
  });

  it('draws the photograph beside the zoomed viewer on Linux, moved with it', () => {
    useSettingsStore.setState({ osPlatform: 'linux' });
    const ref = createRef<TransformWrapperHandle>();
    render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    const viewer = ref.current!.instance.contentComponent!;
    const pane = canvasProps.photoHost!;
    // Not inside the viewer (its canvases make WebKitGTK composite it), but
    // in the same box, below it.
    expect(pane).toBeInstanceOf(HTMLElement);
    expect(viewer.contains(pane)).toBe(false);
    expect(pane.parentElement).toBe(viewer.parentElement);
    expect(pane.compareDocumentPosition(viewer) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    act(() => ref.current!.setTransform(-120, -80, 4));
    expect(pane.style.transform).toBe(viewer.style.transform);
    expect(viewer.style.transform).toContain('scale(4)');
    // Composited while it moves, as the viewer, and painted at the zoom at rest.
    expect(pane.style.willChange).toBe('transform');
    act(() => vi.advanceTimersByTime(1000));
    expect(pane.style.willChange).toBe('');
  });

  it('keeps the photograph in the viewer on macOS and Windows', () => {
    useSettingsStore.setState({ osPlatform: 'macos' });
    const ref = createRef<TransformWrapperHandle>();
    render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    expect(canvasProps.photoHost).toBeNull();
  });

  it("asks for the frame of an animated zoom's end when the animation starts", () => {
    const ref = createRef<TransformWrapperHandle>();
    render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    act(() => ref.current!.zoomIn(Math.log(4), 200));
    // Still at the start of the animation…
    expect(ref.current!.instance.transformState.scale).toBe(1);
    expect(ref.current!.instance.targetTransformState.scale).toBeCloseTo(4);
    // …the preview already knows the size it ends at, without waiting for
    // the animation and the display size's settle (about 0.35 s).
    expect(useEditorStore.getState().displaySize).toEqual({ width: 1600, height: 1200 });
  });

  it("forgets the animation's end when another transform replaces it", () => {
    const ref = createRef<TransformWrapperHandle>();
    render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    act(() => ref.current!.zoomIn(Math.log(4), 200));
    act(() => ref.current!.setTransform(0, 0, 2));
    expect(ref.current!.instance.targetTransformState.scale).toBe(2);
    expect(ref.current!.instance.transformState.scale).toBe(2);
  });

  it("forgets the animation's end when a press stops it without moving", () => {
    const ref = createRef<TransformWrapperHandle>();
    render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    act(() => ref.current!.zoomIn(Math.log(4), 200));
    const wrapper = ref.current!.instance.wrapperComponent!;
    // A middle-button press stops the animation where it is; nothing moves.
    act(() => {
      fireEvent.pointerDown(wrapper, { pointerType: 'mouse', button: 1, pointerId: 1, clientX: 10, clientY: 10 });
      fireEvent.pointerUp(wrapper, { pointerType: 'mouse', button: 1, pointerId: 1, clientX: 10, clientY: 10 });
    });
    expect(ref.current!.instance.targetTransformState).toEqual(ref.current!.instance.transformState);
  });
  it.each(['curve', 'regionCurve', 'hue', 'saturation', 'luminance', 'grayMixer'] as const)(
    'gives %s the first drag without capturing, panning or click zooming',
    async (tatTarget) => {
      useEditorStore.setState({ tatTarget });
      const ref = createRef<TransformWrapperHandle>();
      const { container } = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
      const wrapper = ref.current!.instance.wrapperComponent!;
      const overlay = container.querySelector('[data-testid="targeted-adjustment"]')!;
      const viewerCapture = vi.spyOn(wrapper, 'setPointerCapture');
      const overlayCapture = vi.spyOn(overlay, 'setPointerCapture');
      const initial = { ...ref.current!.instance.transformState };
      await act(async () => {
        fireEvent.pointerDown(overlay, { button: 0, clientX: 100, clientY: 100 });
        fireEvent.pointerMove(overlay, { button: 0, clientX: 150, clientY: 80 });
        fireEvent.pointerUp(overlay, { button: 0, clientX: 150, clientY: 80 });
        fireEvent.click(overlay);
      });
      expect(overlayCapture).toHaveBeenCalled();
      expect(invoke).toHaveBeenCalledWith(
        tatTarget === 'curve' || tatTarget === 'regionCurve' ? 'sample_curve_input' : 'sample_preview_white_balance',
        expect.objectContaining({ path: '/a.raw' })
      );
      expect(viewerCapture).not.toHaveBeenCalled();
      expect(ref.current!.instance.transformState).toEqual(initial);
      expect(ref.current!.instance.targetTransformState).toEqual(initial);
      expect(wrapper.style.cursor).toBe('ns-resize');
    }
  );

  it('pans both side-by-side panes by a drag in the before pane, as in the after pane', () => {
    useUIStore.setState({ activeView: 'editor', activePanel: null });
    useEditorStore.setState({ splitView: true, splitOrientation: 'side-by-side' });
    // jsdom lays nothing out: each pane is the viewer's 400 x 300.
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(400);
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(300);
    const ref = createRef<TransformWrapperHandle>();
    const { container } = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    const pane = container.querySelector('[data-testid="before-pane"]')!;
    expect(pane).toBeTruthy();
    act(() => ref.current!.setTransform(-400, -300, 4));
    // Separate acts: the viewer renders between the events, as it does
    // between real pointer events.
    act(() => {
      fireEvent.pointerDown(pane, { button: 0, clientX: 100, clientY: 100 });
    });
    act(() => {
      fireEvent.pointerMove(pane, { button: 0, clientX: 140, clientY: 80 });
    });
    act(() => {
      fireEvent.pointerUp(pane, { button: 0, clientX: 140, clientY: 80 });
    });
    // One transform for both panes: the drag moved it as in the after pane.
    expect(ref.current!.instance.transformState).toMatchObject({ positionX: -360, positionY: -320, scale: 4 });
    // Modifier drags belong to the edit's tools (straighten, crop pan): not
    // from the before pane.
    act(() => {
      fireEvent.pointerDown(pane, { button: 0, shiftKey: true, clientX: 100, clientY: 100 });
    });
    act(() => {
      fireEvent.pointerMove(pane, { button: 0, shiftKey: true, clientX: 160, clientY: 100 });
    });
    act(() => {
      fireEvent.pointerUp(pane, { button: 0, shiftKey: true, clientX: 160, clientY: 100 });
    });
    expect(ref.current!.instance.transformState).toMatchObject({ positionX: -360, positionY: -320 });
    expect(useEditorStore.getState().adjustments.rotation ?? 0).toBe(0);
  });

  it('zooms both side-by-side panes about the point clicked in the before pane', () => {
    useUIStore.setState({ activeView: 'editor', activePanel: null });
    useEditorStore.setState({ splitView: true, splitOrientation: 'side-by-side' });
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(400);
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(300);
    const ref = createRef<TransformWrapperHandle>();
    const { container } = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    const pane = container.querySelector('[data-testid="before-pane"]')!;
    // The pane sits left of the after pane, each 400 px wide.
    vi.spyOn(pane, 'getBoundingClientRect').mockReturnValue(new DOMRect(0, 0, 400, 300));
    act(() => {
      fireEvent.pointerDown(pane, { button: 0, clientX: 100, clientY: 75 });
    });
    act(() => {
      fireEvent.pointerUp(pane, { button: 0, clientX: 100, clientY: 75 });
      fireEvent.click(pane, { button: 0, clientX: 100, clientY: 75 });
    });
    // 2x about the clicked point of the pane: that point stays where it is.
    expect(ref.current!.instance.targetTransformState).toMatchObject({ scale: 2, positionX: -100, positionY: -75 });
  });

  describe('camera preview (Shift+\\)', () => {
    const rotated = { ...INITIAL_ADJUSTMENTS, rotation: 5 };

    beforeEach(() => {
      useEditorStore.setState({
        selectedImage: { path: '/a.raw', width: 4000, height: 3000, isReady: true, isRaw: true } as never,
        adjustments: rotated,
        cameraPreview: null
      });
      // Develop, where the camera preview can be on (its guard ends it elsewhere).
      useUIStore.setState({ activeView: 'editor', activePanel: null });
    });

    /** Cmd-double-click (resets crop and straighten), Shift-drag
     * (straightens), a drag (pans) and a click and pinch (zoom). */
    function editGestures(target: Element) {
      act(() => {
        for (let i = 0; i < 2; i++) {
          fireEvent.pointerDown(target, { button: 0, metaKey: true, clientX: 60, clientY: 60 });
          fireEvent.pointerUp(target, { button: 0, metaKey: true, clientX: 60, clientY: 60 });
        }
      });
      act(() => {
        fireEvent.pointerDown(target, { button: 0, shiftKey: true, clientX: 50, clientY: 50 });
        fireEvent.pointerMove(target, { button: 0, shiftKey: true, clientX: 250, clientY: 120 });
        fireEvent.pointerUp(target, { button: 0, shiftKey: true, clientX: 250, clientY: 120 });
      });
      act(() => {
        fireEvent.pointerDown(target, { button: 0, clientX: 50, clientY: 50 });
        fireEvent.pointerMove(target, { button: 0, clientX: 150, clientY: 120 });
        fireEvent.pointerUp(target, { button: 0, clientX: 150, clientY: 120 });
        fireEvent.click(target, { button: 0, clientX: 150, clientY: 120 });
        fireEvent.wheel(target, { deltaY: -100, clientX: 100, clientY: 100, ctrlKey: true });
      });
    }

    it('leaves the hidden edit inert: no Cmd-double-click reset, Shift-drag straighten, pan or zoom', () => {
      const ref = createRef<TransformWrapperHandle>();
      const { container } = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
      act(() => useEditorStore.getState().setEditor({ cameraPreview: { path: '/a.raw' } }));
      const blocker = container.querySelector('.studio-camera-preview-blocker')!;
      expect(blocker).toBeTruthy();
      expect(container.querySelector('[data-bench-id="camera-preview"]')?.textContent).toContain('Camera preview');
      const transform = { ...ref.current!.instance.transformState };
      editGestures(blocker);
      expect(useEditorStore.getState().adjustments).toBe(rotated);
      expect(ref.current!.instance.transformState).toEqual(transform);
      // The same gestures reach the edit once it is back (the test sees them).
      act(() => useEditorStore.getState().setEditor({ cameraPreview: null }));
      expect(container.querySelector('.studio-camera-preview-blocker')).toBeNull();
      editGestures(ref.current!.instance.wrapperComponent!);
      expect(useEditorStore.getState().adjustments.rotation).not.toBe(5);
    });

    it("places the camera JPEG natively in the viewer to fit, whatever the edit's zoom", async () => {
      vi.useRealTimers();
      vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => setTimeout(() => callback(0), 0));
      vi.stubGlobal('cancelAnimationFrame', (id: number) => clearTimeout(id));
      vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
        left: 10,
        top: 20,
        width: 400,
        height: 300,
        right: 410,
        bottom: 320,
        x: 10,
        y: 20
      } as DOMRect);
      useEditorStore.setState({ hasRenderedFirstFrame: true });
      const transforms = () =>
        vi
          .mocked(invoke)
          .mock.calls.filter(([command]) => command === 'update_wgpu_transform')
          .map(([, args]) => (args as { payload: { width: number; viewerArea: number[] } }).payload);
      const ref = createRef<TransformWrapperHandle>();
      const { unmount } = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
      await vi.waitFor(() => expect(transforms().length).toBeGreaterThan(0));
      const dpr = window.devicePixelRatio || 1;
      const area = [10 * dpr, 20 * dpr, 400 * dpr, 300 * dpr];
      expect(transforms().at(-1)!.viewerArea).toEqual(area);
      // Zoomed in, the edit's frame is larger than the viewer; the camera
      // preview's area (where the native display fits it) stays the viewer.
      act(() => ref.current!.setTransform(0, 0, 3));
      await vi.waitFor(() => expect(transforms().at(-1)!.width).toBeCloseTo(1200 * dpr));
      act(() => useEditorStore.getState().setEditor({ cameraPreview: { path: '/a.raw' } }));
      await new Promise((resolve) => setTimeout(resolve, 20));
      expect(transforms().at(-1)!.viewerArea).toEqual(area);
      // No frame or sync left running after the test (real timers).
      unmount();
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
  });

  it('keeps middle-button pan available while targeting', () => {
    useEditorStore.setState({ tatTarget: 'curve' });
    const ref = createRef<TransformWrapperHandle>();
    const { container } = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    const wrapper = ref.current!.instance.wrapperComponent!;
    const overlay = container.querySelector('[data-testid="targeted-adjustment"]')!;
    const capture = vi.spyOn(wrapper, 'setPointerCapture');
    fireEvent.pointerDown(overlay, { button: 1, clientX: 100, clientY: 100 });
    expect(capture).toHaveBeenCalled();
    expect(wrapper.style.cursor).toBe('grabbing');
    fireEvent.pointerUp(overlay, { button: 1, clientX: 100, clientY: 100 });
    expect(wrapper.style.cursor).toBe('ns-resize');
  });

  it.each([false, true])('keeps wheel zoom available while targeting (trackpad pinch: %s)', (ctrlKey) => {
    useEditorStore.setState({ tatTarget: 'curve' });
    const ref = createRef<TransformWrapperHandle>();
    render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    const wrapper = ref.current!.instance.wrapperComponent!;
    fireEvent.wheel(wrapper, { deltaY: -100, clientX: 100, clientY: 100, ctrlKey });
    expect(ref.current!.instance.transformState.scale).toBeGreaterThan(1);
  });

  it('keeps the magnification and the centred detail when the before pane halves the viewer', () => {
    // 4000 × 3000 photo shown in an 800 × 300 viewer: fitted by height, 400 × 300 centred.
    Object.assign(fit, {
      width: 400,
      height: 300,
      offsetX: 200,
      offsetY: 0,
      containerWidth: 800,
      containerHeight: 300
    });
    const size = (dim: 'clientWidth' | 'clientHeight') =>
      Object.defineProperty(HTMLElement.prototype, dim, {
        configurable: true,
        get: () => (dim === 'clientWidth' ? fit.containerWidth : fit.containerHeight)
      });
    size('clientWidth');
    size('clientHeight');
    const ref = createRef<TransformWrapperHandle>();
    const view = render(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />);
    // Zoom to 3x with the photo point (0.25, 0.5) at the viewer's centre.
    const [a, b] = [0.25, 0.5];
    act(() => ref.current!.setTransform(400 - 3 * (200 + a * 400), 150 - 3 * (0 + b * 300), 3));
    expect(ref.current!.instance.transformState.scale).toBe(3);
    // The before pane opens: the layout flips first, the viewer's new size arrives after it.
    act(() => useEditorStore.setState({ splitView: true, splitOrientation: 'side-by-side' } as never));
    Object.assign(fit, { width: 400, height: 150, offsetX: 0, offsetY: 0, containerWidth: 400, containerHeight: 150 });
    act(() => view.rerender(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />));
    const { scale, positionX, positionY } = ref.current!.instance.transformState;
    // CSS pixels per photo pixel stay: 400 px wide before, 400 px wide now (the fit's width is unchanged).
    expect(scale).toBeCloseTo(3);
    // The same photo point still sits at the new viewer's centre.
    expect((fit.containerWidth / 2 - positionX) / scale / fit.width).toBeCloseTo(a);
    expect((fit.containerHeight / 2 - positionY) / scale / fit.height).toBeCloseTo(b);
    // The pane closes again: back to the first framing.
    act(() => useEditorStore.setState({ splitView: false } as never));
    Object.assign(fit, {
      width: 400,
      height: 300,
      offsetX: 200,
      offsetY: 0,
      containerWidth: 800,
      containerHeight: 300
    });
    act(() => view.rerender(<Editor onContextMenu={() => {}} transformWrapperRef={ref} />));
    const back = ref.current!.instance.transformState;
    expect(back.scale).toBeCloseTo(3);
    expect((400 - back.positionX) / back.scale - 200).toBeCloseTo(a * 400);
    expect((150 - back.positionY) / back.scale).toBeCloseTo(b * 300);
    delete (HTMLElement.prototype as { clientWidth?: number }).clientWidth;
    delete (HTMLElement.prototype as { clientHeight?: number }).clientHeight;
  });
});
