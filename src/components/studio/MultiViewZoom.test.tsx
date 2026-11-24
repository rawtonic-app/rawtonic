import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import MultiViewStage from './MultiViewStage';
import { useEditorStore } from '../../store/useEditorStore';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useMultiViewStore } from '../../store/useMultiViewStore';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useUIStore } from '../../store/useUIStore';
import { useSessionBar } from '../session/sessionBar';
import { Invokes, type AppSettings, type ImageFile } from '../ui/AppProperties';
import type { CellZoomsPayload, SharedZoom } from '../../utils/multiView';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve(true)) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock('../../activity/notify', () => ({ notify: { error: vi.fn(), success: vi.fn() } }));

const file = (path: string): ImageFile => ({
  path,
  is_edited: false,
  modified: 0,
  rating: 0,
  tags: null,
  exif: null,
  is_virtual_copy: false,
  is_cloud_placeholder: false,
  is_raw: true,
  group_id: null
});
const paths = ['/s/a.CR3', '/s/b.CR3', '/s/c.CR3'];
const files = paths.map(file);
const SIZE = { width: 6000, height: 4000 };

let frames: FrameRequestCallback[] = [];
const runFrame = () => act(() => frames.splice(0).forEach((frame) => frame(0)));
const transform = { scale: 1, positionX: 0, positionY: 0 };
/** The editor's count of applied transforms (`instance.applied`). */
const editorState = { applied: 0 };
/** The editor moved by the photographer (wheel, keys, pan). */
const moveEditor = (t: Partial<typeof transform>) => {
  editorState.applied += 1;
  Object.assign(transform, t);
};
const editorHandle = {
  current: {
    resetTransform: vi.fn(() => moveEditor({ scale: 1, positionX: 0, positionY: 0 })),
    // The editor keeps its transform within its bounds: never below fit.
    setTransform: vi.fn((positionX: number, positionY: number, scale: number) =>
      moveEditor(scale < 1 ? { scale: 1, positionX: 0, positionY: 0 } : { scale, positionX, positionY })
    ),
    instance: {
      transformState: transform,
      get applied() {
        return editorState.applied;
      }
    }
  }
};

const zoomCalls = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([command]) => command === Invokes.SetMultiViewZoom)
    .map(([, args]) => args as { zoom: SharedZoom | null; cells: Record<string, SharedZoom> | null });

function stage() {
  const view = render(
    <MultiViewStage
      files={files}
      onOpen={(path) => open(path)}
      onRate={vi.fn()}
      transformWrapperRef={editorHandle as never}
    >
      <div data-testid="editor" />
    </MultiViewStage>
  );
  measureEditor(view.container);
  runFrame();
  return view;
}

/** The editor measures itself in the primary's cell (as its resize
 * observer does a frame or more after the multi view starts). */
function measureEditor(container: HTMLElement, size = SIZE) {
  const primary = container.querySelector<HTMLElement>('.studio-mv-primary')!;
  const width = parseFloat(primary.style.width);
  const height = parseFloat(primary.style.height);
  const fit = Math.min(width / size.width, height / size.height);
  useEditorStore.setState({
    baseRenderSize: {
      width: size.width * fit,
      height: size.height * fit,
      offsetX: (width - size.width * fit) / 2,
      offsetY: (height - size.height * fit) / 2,
      containerWidth: width,
      containerHeight: height
    }
  });
}

function open(path: string, size = SIZE) {
  useEditorStore.setState({
    selectedImage: { path, isReady: true, width: size.width, height: size.height } as never
  });
}

/** A secondary cell's press area and its photograph area in window px. */
function cellOf(container: HTMLElement, path: string) {
  const cell = container.querySelector<HTMLElement>(`.studio-mv-cell[data-path="${path}"]`)!;
  const select = cell.querySelector<HTMLElement>('.studio-mv-select')!;
  const x = 100 + parseFloat(cell.style.left);
  const y = 50 + parseFloat(cell.style.top);
  const width = parseFloat(cell.style.width);
  const height = parseFloat(select.style.height);
  return { cell, select, center: { clientX: x + width / 2, clientY: y + height / 2 } };
}

/** The editor's transform for 1:1 (at 2 device px per CSS px), centred. */
function oneToOne() {
  const fit = useEditorStore.getState().baseRenderSize;
  const scale = SIZE.width / (fit.width * 2);
  const [cx, cy] = [fit.containerWidth / 2, fit.containerHeight / 2];
  return { scale, positionX: cx - cx * scale, positionY: cy - cy * scale };
}

const badge = (container: HTMLElement, path: string) =>
  container.querySelector(`.studio-mv-cell[data-path="${path}"] .studio-mv-zoom`)?.textContent ?? null;

beforeEach(() => {
  vi.clearAllMocks();
  frames = [];
  Object.assign(transform, { scale: 1, positionX: 0, positionY: 0 });
  vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => frames.push(cb));
  vi.stubGlobal('cancelAnimationFrame', () => {});
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    }
  );
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
    left: 100,
    top: 50,
    width: 1200,
    height: 800,
    right: 1300,
    bottom: 850,
    x: 100,
    y: 50,
    toJSON: () => ({})
  });
  Object.defineProperty(window, 'devicePixelRatio', { value: 2, configurable: true });
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings, osPlatform: 'macos' });
  useUIStore.setState({ activeView: 'editor' });
  useMultiViewStore.setState({
    enabled: true,
    editAll: false,
    unavailable: false,
    active: false,
    reference: null,
    zoomLock: false,
    cellZooms: {},
    cellSizes: Object.fromEntries(paths.map((path) => [path, SIZE]))
  });
  useLibraryStore.setState({ multiSelectedPaths: paths, imageRatings: {}, imageFlags: {} });
  useSessionBar.setState({ mark: null });
  useEditorStore.setState({
    baseRenderSize: { width: 600, height: 400, offsetX: 0, offsetY: 50, containerWidth: 600, containerHeight: 500 },
    adjustments: { ...useEditorStore.getState().adjustments, crop: null, orientationSteps: 0 }
  });
  open('/s/b.CR3');
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('individual zoom (zoom lock off)', () => {
  it('zooms only the cell under the wheel, which keeps its zoom', () => {
    const { container } = stage();
    const a = cellOf(container, '/s/a.CR3');
    fireEvent.wheel(a.select, { deltaY: -400, ...a.center });
    runFrame();
    const zoom = useMultiViewStore.getState().cellZooms['/s/a.CR3'];
    expect(zoom.scale).toBeGreaterThan(0.2);
    // Only that cell is zoomed natively; the others (and the primary) stay.
    expect(zoomCalls().at(-1)).toEqual({ zoom: null, cells: { '/s/a.CR3': zoom } });
    expect(badge(container, '/s/a.CR3')).toMatch(/%/);
    expect(badge(container, '/s/c.CR3')).toBeNull();
    expect(badge(container, '/s/b.CR3')).toBeNull();
    expect(transform.scale).toBe(1);
    // Wheeling the other way returns it to fit.
    fireEvent.wheel(a.select, { deltaY: 4000, ...a.center });
    runFrame();
    expect(useMultiViewStore.getState().cellZooms['/s/a.CR3']).toBeUndefined();
    expect(zoomCalls().at(-1)).toEqual({ zoom: null, cells: {} });
  });

  it('zooms the hovered cell with the zoom keys and shows 100 %', () => {
    const { container } = stage();
    const c = cellOf(container, '/s/c.CR3');
    fireEvent.pointerEnter(c.select, c.center);
    let handled = false;
    act(() => {
      handled = useMultiViewStore.getState().zoomHoveredCell!('cycle');
    });
    expect(handled).toBe(true);
    expect(useMultiViewStore.getState().cellZooms['/s/c.CR3'].scale).toBeCloseTo(1);
    expect(badge(container, '/s/c.CR3')).toBe('100%');
    act(() => {
      useMultiViewStore.getState().zoomHoveredCell!('fit');
    });
    expect(badge(container, '/s/c.CR3')).toBeNull();
    // Not over another cell: the keys go to the primary.
    fireEvent.pointerLeave(c.select);
    expect(useMultiViewStore.getState().zoomHoveredCell!('cycle')).toBe(false);
  });

  it('pans a zoomed cell with a drag, without making it the primary', () => {
    const { container } = stage();
    const a = cellOf(container, '/s/a.CR3');
    fireEvent.pointerEnter(a.select, a.center);
    act(() => {
      useMultiViewStore.getState().zoomHoveredCell!('actual');
    });
    const before = useMultiViewStore.getState().cellZooms['/s/a.CR3'];
    fireEvent.pointerDown(a.select, { button: 0, pointerId: 1, ...a.center });
    fireEvent.pointerMove(a.select, { pointerId: 1, clientX: a.center.clientX + 40, clientY: a.center.clientY });
    fireEvent.pointerUp(a.select, { pointerId: 1 });
    fireEvent.click(a.select);
    const after = useMultiViewStore.getState().cellZooms['/s/a.CR3'];
    expect(after.scale).toBe(before.scale);
    // The photograph moved right with the pointer: the centre moved left.
    expect(after.center[0]).toBeLessThan(before.center[0]);
    expect(useEditorStore.getState().selectedImage?.path).toBe('/s/b.CR3');
    // A plain click still makes it the primary.
    fireEvent.click(a.select);
    expect(useEditorStore.getState().selectedImage?.path).toBe('/s/a.CR3');
  });

  it('hands each photograph its own zoom when it becomes or stops being the primary', () => {
    const { container, rerender } = stage();
    // The primary is zoomed in the editor (5 × fit = 1:1, centred).
    moveEditor(oneToOne());
    runFrame();
    expect(badge(container, '/s/b.CR3')).toBe('100%');
    // Cell a was zoomed on its own.
    useMultiViewStore.getState().setCellZoom('/s/a.CR3', { scale: 0.5, center: [0.25, 0.5] });
    act(() => open('/s/a.CR3'));
    rerender(
      <MultiViewStage
        files={files}
        onOpen={(path) => open(path)}
        onRate={vi.fn()}
        transformWrapperRef={editorHandle as never}
      >
        <div data-testid="editor" />
      </MultiViewStage>
    );
    runFrame();
    // b keeps 1:1 as its cell's zoom; the editor shows a's own zoom.
    const zooms = useMultiViewStore.getState().cellZooms;
    expect(zooms['/s/b.CR3'].scale).toBeCloseTo(1);
    expect(editorHandle.current.setTransform).toHaveBeenCalled();
    expect(transform.scale).toBeCloseTo((0.5 * 6000) / (useEditorStore.getState().baseRenderSize.width * 2));
    runFrame();
    expect(zoomCalls().at(-1)!.cells!['/s/b.CR3'].scale).toBeCloseTo(1);
    expect(zoomCalls().at(-1)!.cells!['/s/a.CR3']).toBeUndefined();
    // A photograph without a zoom of its own opens at fit.
    act(() => open('/s/c.CR3'));
    rerender(
      <MultiViewStage
        files={files}
        onOpen={(path) => open(path)}
        onRate={vi.fn()}
        transformWrapperRef={editorHandle as never}
      >
        <div data-testid="editor" />
      </MultiViewStage>
    );
    expect(editorHandle.current.resetTransform).toHaveBeenCalled();
    expect(useMultiViewStore.getState().cellZooms['/s/a.CR3'].scale).toBeCloseTo(0.5);
  });
});

const layoutCalls = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([command]) => command === Invokes.SetMultiView)
    .map(([, args]) => args as { primary: string | null; zooms: CellZoomsPayload | null });

describe('switching the primary', () => {
  const switchTo = (rerender: (ui: React.ReactElement) => void, path: string) => {
    act(() => open(path));
    rerender(
      <MultiViewStage
        files={files}
        onOpen={(path) => open(path)}
        onRate={vi.fn()}
        transformWrapperRef={editorHandle as never}
      >
        <div data-testid="editor" />
      </MultiViewStage>
    );
  };

  it.each([false, true])("sends the outgoing primary's zoom with the layout itself (zoom lock %s)", (zoomLock) => {
    useMultiViewStore.setState({ zoomLock });
    const { rerender } = stage();
    // b, the primary, at 1:1 in the editor.
    moveEditor(oneToOne());
    runFrame();
    const zoomsBefore = zoomCalls().length;
    switchTo(rerender, '/s/a.CR3');
    // The layout that makes a the primary carries b's 1:1 before any
    // zoom call: sent after it, b's cell showed its last frame to fit
    // until its own render (G74, WMV3).
    const layout = layoutCalls().find((call) => call.primary === '/s/a.CR3')!;
    expect(layout).toBeDefined();
    expect(zoomCalls()).toHaveLength(zoomsBefore);
    const zoomOfB = zoomLock ? layout.zooms!.zoom : layout.zooms!.cells!['/s/b.CR3'];
    expect(zoomOfB!.scale).toBeCloseTo(1);
    expect(zoomOfB!.center[0]).toBeCloseTo(0.5);
    if (zoomLock) expect(layout.zooms!.cells).toBeNull();
    else expect(layout.zooms!.cells!['/s/a.CR3']).toBeUndefined();
    // The zoom sent next agrees with it (natively a repeat is ignored).
    runFrame();
    const next = zoomCalls().at(-1)!;
    if (zoomLock) expect(next.zoom!.scale).toBeCloseTo(1);
    else expect(next.cells!['/s/b.CR3'].scale).toBeCloseTo(1);
  });
});

describe('the zoom lock across a primary switch', () => {
  it('keeps the image scale when the next primary has another size and shape', () => {
    useMultiViewStore.setState({ zoomLock: true });
    const { container, rerender } = stage();
    // b, the primary, at 1:1 in the editor: every cell at 100 %.
    moveEditor(oneToOne());
    runFrame();
    expect(zoomCalls().at(-1)!.zoom!.scale).toBeCloseTo(1);
    const before = zoomCalls().length;
    // a is a 4:3 photograph of another resolution (8256 × 6192 beside
    // 6000 × 4000): its fit in the cell differs, the editor keeps its
    // transform (a factor over the fit) until told otherwise.
    const A = { width: 8256, height: 6192 };
    act(() => open('/s/a.CR3', A));
    rerender(
      <MultiViewStage
        files={files}
        onOpen={(path) => open(path)}
        onRate={vi.fn()}
        transformWrapperRef={editorHandle as never}
      >
        <div data-testid="editor" />
      </MultiViewStage>
    );
    measureEditor(container, A);
    runFrame();
    runFrame();
    // The lock keeps 1 image px per device px in every cell (WMV4: every
    // cell went 100 % -> 127 % about 1 s after the switch).
    for (const call of zoomCalls().slice(before - 1)) expect(call.zoom!.scale).toBeCloseTo(1);
    expect(badge(container, '/s/b.CR3')).toBe('100%');
    expect(badge(container, '/s/c.CR3')).toBe('100%');
    // The editor shows the new primary at 1:1 too.
    const fit = useEditorStore.getState().baseRenderSize;
    expect((fit.width * transform.scale * 2) / A.width).toBeCloseTo(1);
  });
});

describe("the zoom lock and the editor's bounds", () => {
  const again = (rerender: (ui: React.ReactElement) => void) =>
    rerender(
      <MultiViewStage
        files={files}
        onOpen={(path) => open(path)}
        onRate={vi.fn()}
        transformWrapperRef={editorHandle as never}
      >
        <div data-testid="editor" />
      </MultiViewStage>
    );

  it('keeps the zoom when the next primary clamps it to its fit (a tight crop)', () => {
    useMultiViewStore.setState({ zoomLock: true });
    const { container, rerender } = stage();
    // b slightly above fit: 1.5 x its fit.
    const fit = useEditorStore.getState().baseRenderSize;
    moveEditor({
      scale: 1.5,
      positionX: (fit.containerWidth / 2) * -0.5,
      positionY: (fit.containerHeight / 2) * -0.5
    });
    runFrame();
    const shared = zoomCalls().at(-1)!.zoom!;
    expect(shared.scale).toBeCloseTo((fit.width * 1.5 * 2) / 6000);
    const before = zoomCalls().length;
    // a is shown cropped to 1500 x 1000: that zoom is below its fit, so the
    // editor stays at fit.
    act(() => {
      useEditorStore.setState({
        adjustments: {
          ...useEditorStore.getState().adjustments,
          crop: { unit: 'px', x: 0, y: 0, width: 1500, height: 1000 }
        }
      });
      open('/s/a.CR3');
    });
    again(rerender);
    measureEditor(container);
    runFrame();
    runFrame();
    expect(transform.scale).toBe(1);
    // The other cells keep the shared zoom (not fit).
    for (const call of zoomCalls().slice(before)) expect(call.zoom!.scale).toBeCloseTo(shared.scale);
    expect(badge(container, '/s/c.CR3')).not.toBeNull();
    // Fit chosen there (the editor stays where it is): every cell to fit.
    act(() => editorHandle.current.resetTransform());
    runFrame();
    expect(zoomCalls().at(-1)!.zoom).toBeNull();
    // The editor's own zoom from there is read again.
    moveEditor(oneToOne());
    runFrame();
    expect(zoomCalls().at(-1)!.zoom!.scale).toBeCloseTo(
      (useEditorStore.getState().baseRenderSize.width * transform.scale * 2) / 1500
    );
  });

  it('reads the zoom afresh when the multi view starts again', () => {
    useMultiViewStore.setState({ zoomLock: true });
    const { container, rerender } = stage();
    moveEditor(oneToOne());
    runFrame();
    expect(zoomCalls().at(-1)!.zoom!.scale).toBeCloseTo(1);
    // Leave the multi view, go to fit in the single view, resize the window.
    act(() => useMultiViewStore.setState({ enabled: false }));
    again(rerender);
    runFrame();
    moveEditor({ scale: 1, positionX: 0, positionY: 0 });
    vi.mocked(HTMLElement.prototype.getBoundingClientRect).mockReturnValue({
      left: 100,
      top: 50,
      width: 1000,
      height: 700,
      right: 1100,
      bottom: 750,
      x: 100,
      y: 50,
      toJSON: () => ({})
    });
    editorHandle.current.setTransform.mockClear();
    const sent = zoomCalls().length;
    const laid = layoutCalls().length;
    act(() => useMultiViewStore.setState({ enabled: true }));
    again(rerender);
    measureEditor(container);
    runFrame();
    runFrame();
    // The fit chosen meanwhile stays: no old zoom put back.
    expect(editorHandle.current.setTransform).not.toHaveBeenCalled();
    expect(transform.scale).toBe(1);
    for (const call of zoomCalls().slice(sent)) expect(call.zoom).toBeNull();
    // Nor in the first layout sent on entry.
    expect(layoutCalls().length).toBeGreaterThan(laid);
    for (const call of layoutCalls().slice(laid)) expect(call.zooms?.zoom ?? null).toBeNull();
  });

  it('records the zoom of a photograph that adopts its own after an unlocked switch', () => {
    useMultiViewStore.setState({ zoomLock: true });
    const { container, rerender } = stage();
    moveEditor(oneToOne());
    runFrame();
    act(() => open('/s/a.CR3'));
    again(rerender);
    runFrame();
    runFrame();
    act(() => useMultiViewStore.getState().setZoomLock(false));
    runFrame();
    // c's own zoom (the shared 1:1) puts the editor where it already is.
    act(() => open('/s/c.CR3'));
    again(rerender);
    runFrame();
    runFrame();
    expect(badge(container, '/s/c.CR3')).toBe('100%');
    act(() => open('/s/b.CR3'));
    again(rerender);
    expect(useMultiViewStore.getState().cellZooms['/s/c.CR3']?.scale).toBeCloseTo(1);
  });
});

describe('entering the multi view', () => {
  it("waits for the editor to measure itself in the primary's cell", () => {
    useMultiViewStore.setState({ zoomLock: true });
    // The single view's size, as the editor holds it for a frame or more.
    useEditorStore.setState({
      baseRenderSize: { width: 1200, height: 800, offsetX: 0, offsetY: 0, containerWidth: 1200, containerHeight: 800 }
    });
    // Zoomed 2 × in the single view; the multi view keeps that zoom.
    moveEditor({ scale: 2, positionX: -600, positionY: -400 });
    const { container } = render(
      <MultiViewStage
        files={files}
        onOpen={(path) => open(path)}
        onRate={vi.fn()}
        transformWrapperRef={editorHandle as never}
      >
        <div data-testid="editor" />
      </MultiViewStage>
    );
    runFrame();
    // Nothing read from the single view's size: it gave 1200 × 2 × 2 /
    // 6000 = 0.8, not the cell's 2 × fit.
    expect(zoomCalls().every((call) => call.zoom === null)).toBe(true);
    measureEditor(container);
    runFrame();
    const fit = useEditorStore.getState().baseRenderSize;
    expect(zoomCalls().at(-1)!.zoom!.scale).toBeCloseTo((fit.width * 2 * 2) / 6000);
  });
});

describe('zoom lock switching', () => {
  it('keeps what every cell shows when unlocked, and follows the primary again when locked', () => {
    useMultiViewStore.setState({ zoomLock: true });
    const { container } = stage();
    moveEditor(oneToOne());
    runFrame();
    const shared = zoomCalls().at(-1)!;
    expect(shared.cells).toBeNull();
    expect(shared.zoom!.scale).toBeCloseTo(1);
    expect(badge(container, '/s/a.CR3')).toBe('100%');
    // Unlocked: each other cell keeps the shared zoom as its own.
    act(() => useMultiViewStore.getState().setZoomLock(false));
    runFrame();
    const own = zoomCalls().at(-1)!;
    expect(own.zoom).toBeNull();
    expect(Object.keys(own.cells!).sort()).toEqual(['/s/a.CR3', '/s/c.CR3']);
    expect(own.cells!['/s/a.CR3'].scale).toBeCloseTo(1);
    // A wheel now zooms one cell only.
    const a = cellOf(container, '/s/a.CR3');
    fireEvent.wheel(a.select, { deltaY: 300, ...a.center });
    runFrame();
    const after = zoomCalls().at(-1)!.cells!;
    expect(after['/s/a.CR3'].scale).toBeLessThan(1);
    expect(after['/s/c.CR3'].scale).toBeCloseTo(1);
    // Locked again: every cell shows the primary's zoom, own zooms dropped.
    act(() => useMultiViewStore.getState().setZoomLock(true));
    runFrame();
    expect(useMultiViewStore.getState().cellZooms).toEqual({});
    const locked = zoomCalls().at(-1)!;
    expect(locked.cells).toBeNull();
    expect(locked.zoom!.scale).toBeCloseTo(1);
    // With the lock, the wheel over another cell is left alone.
    fireEvent.wheel(a.select, { deltaY: -300, ...a.center });
    expect(useMultiViewStore.getState().cellZooms).toEqual({});
  });

  it('toggles the lock with Shift+N', async () => {
    const { KEYBIND_DEFINITIONS } = await import('../../utils/keyboardUtils');
    const lock = KEYBIND_DEFINITIONS.find((d) => d.action === 'toggle_zoom_lock');
    expect(lock?.defaultCombo).toEqual(['shift', 'KeyN']);
    const taken = KEYBIND_DEFINITIONS.filter(
      (d) => d.action !== 'toggle_zoom_lock' && JSON.stringify(d.defaultCombo) === JSON.stringify(['shift', 'KeyN'])
    );
    expect(taken).toEqual([]);
  });
});

describe('lock toggle', () => {
  it('names its shortcut and the gestures in its tooltip', async () => {
    const { default: MultiViewButtons } = await import('./MultiViewButtons');
    useMultiViewStore.setState({ active: true, zoomLock: false });
    const { getByLabelText } = render(<MultiViewButtons />);
    const title = getByLabelText('Lock zoom').getAttribute('title')!;
    expect(title).toMatch(/\((Shift \+ N|⇧ ?N)\)$/);
    expect(title).toMatch(/double-click/);
  });
});
