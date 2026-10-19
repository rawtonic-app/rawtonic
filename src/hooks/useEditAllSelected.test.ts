import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { EDIT_ALL_LIVE_MS, EDIT_ALL_SETTLE_MS, livePatch, useEditAllSelected } from './useEditAllSelected';
import { useEditorStore } from '../store/useEditorStore';
import { updateAdjustments, useEditorActions, waitForPendingMetadataSave } from './useEditorActions';
import { useLibraryStore } from '../store/useLibraryStore';
import { useMultiViewStore } from '../store/useMultiViewStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes } from '../components/ui/AppProperties';
import { INITIAL_ADJUSTMENTS, type Adjustments } from '../utils/adjustments';
import { registerSliderDraftRenderer } from '../utils/sliderDraft';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const selection = ['/s/a.CR3', '/s/b.CR3', '/s/c.CR3'];
const pasted = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([command]) => command === Invokes.ApplyAdjustmentsToPaths)
    .map(([, args]) => args as { paths: string[]; adjustments: Partial<Adjustments> });

const live = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([command]) => command === Invokes.SetMultiViewLive)
    .map(([, args]) => (args as { patch: Partial<Adjustments> | null }).patch);

function edit(change: Partial<Adjustments>) {
  act(() =>
    useEditorStore.setState((state) => ({ adjustments: { ...state.adjustments, ...change } as Adjustments })),
  );
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  useSettingsStore.setState({ osPlatform: 'macos' });
  useLibraryStore.setState({ multiSelectedPaths: selection });
  useEditorStore.setState({
    selectedImage: { path: '/s/b.CR3', isReady: true, width: 6000, height: 4000 } as never,
    adjustments: { ...INITIAL_ADJUSTMENTS } as Adjustments,
  });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('Edit all selected', () => {
  it('saves Linux browser cells and refreshes them without native live-display IPC', async () => {
    useSettingsStore.setState({ osPlatform: 'linux' });
    const before = useMultiViewStore.getState().recipeRevision;
    renderHook(() => useEditAllSelected(true, selection));
    edit({ exposure: 0.5 });
    expect(live()).toEqual([]);
    await act(async () => { vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10); });
    expect(pasted()).toEqual([{ paths: ['/s/a.CR3', '/s/c.CR3'], adjustments: { exposure: 0.5 } }]);
    expect(useMultiViewStore.getState().recipeRevision).toBe(before + 1);
    expect(live()).toEqual([]);
  });
  it('applies what an edit of the primary changed to the other selected photographs', () => {
    renderHook(() => useEditAllSelected(true, selection));
    edit({ exposure: 0.5 });
    edit({ exposure: 0.8, contrast: 10 });
    expect(pasted()).toEqual([]);
    act(() => vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10));
    expect(pasted()).toEqual([
      { paths: ['/s/a.CR3', '/s/c.CR3'], adjustments: { exposure: 0.8, contrast: 10 } },
    ]);
    // The primary keeps its own edit (and its own history).
    expect(useEditorStore.getState().adjustments.exposure).toBe(0.8);
  });

  it("does not give the others a batch's reloaded result for the primary", () => {
    renderHook(() => useEditAllSelected(true, selection));
    // Batch Auto straighten holds the selection and reloads the primary's own answer.
    act(() => useEditorStore.setState({ heldRecipePaths: [...selection] }));
    edit({ rotation: -2.5, crop: { unit: 'px', x: 10, y: 8, width: 5900, height: 3900 } });
    act(() => useEditorStore.setState({ heldRecipePaths: [] }));
    act(() => vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10));
    expect(pasted()).toEqual([]);
    // An edit afterwards still goes to the others, without the rotation.
    edit({ exposure: 0.4 });
    act(() => vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10));
    expect(pasted()).toEqual([{ paths: ['/s/a.CR3', '/s/c.CR3'], adjustments: { exposure: 0.4 } }]);
  });

  it('writes a pending edit before a batch starts, so the batch results stay', async () => {
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === Invokes.ApplyAutoStraightenToPaths
        ? { straightened: [], noLines: [], changed: [], failed: [] }
        : command === Invokes.LoadMetadata
          ? { adjustments: useEditorStore.getState().adjustments }
          : undefined,
    );
    renderHook(() => useEditAllSelected(true, selection));
    const { result } = renderHook(useEditorActions);
    edit({ rotation: 1.5 });
    // Batch-straighten the selection before the settle time is over.
    await act(async () => {
      await result.current.handleAutoStraightenPaths(selection);
    });
    const order = vi
      .mocked(invoke)
      .mock.calls.map(([command]) => command)
      .filter((c) => c === Invokes.ApplyAdjustmentsToPaths || c === Invokes.ApplyAutoStraightenToPaths);
    expect(order).toEqual([Invokes.ApplyAdjustmentsToPaths, Invokes.ApplyAutoStraightenToPaths]);
    // Nothing is written after the batch.
    await act(async () => { vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10); });
    expect(pasted()).toHaveLength(1);
  });

  it('passes a slider draft pending when a batch starts on to the others first', async () => {
    vi.mocked(invoke).mockImplementation(async (command) =>
      command === Invokes.ApplyAutoStraightenToPaths
        ? { straightened: [], noLines: [], changed: [], failed: [] }
        : command === Invokes.LoadMetadata
          ? { adjustments: useEditorStore.getState().adjustments }
          : undefined,
    );
    renderHook(() => useEditAllSelected(true, selection));
    const { result } = renderHook(useEditorActions);
    // A slider is being dragged: its moves are a draft not yet committed.
    act(() => useEditorStore.setState({ isSliderDragging: true }));
    const unregister = registerSliderDraftRenderer(() => true);
    act(() => updateAdjustments({ exposure: 4 }));
    // Rendered as a draft, not committed yet.
    expect(useEditorStore.getState().adjustments.exposure).not.toBe(4);
    await act(async () => {
      await result.current.handleAutoStraightenPaths(selection);
    });
    unregister();
    act(() => useEditorStore.setState({ isSliderDragging: false }));
    const order = vi
      .mocked(invoke)
      .mock.calls.map(([command]) => command)
      .filter((c) => c === Invokes.ApplyAdjustmentsToPaths || c === Invokes.ApplyAutoStraightenToPaths);
    expect(order).toEqual([Invokes.ApplyAdjustmentsToPaths, Invokes.ApplyAutoStraightenToPaths]);
    expect(pasted()[0]).toMatchObject({ paths: ['/s/a.CR3', '/s/c.CR3'], adjustments: { exposure: 4 } });
  });

  it('has the other cells developed again once their recipes are written', async () => {
    const before = useMultiViewStore.getState().recipeRevision;
    renderHook(() => useEditAllSelected(true, selection));
    edit({ exposure: 0.3 });
    act(() => vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10));
    vi.useRealTimers();
    await vi.waitFor(() => expect(useMultiViewStore.getState().recipeRevision).toBe(before + 1));
  });

  it('does nothing while off (the default)', () => {
    renderHook(() => useEditAllSelected(false, selection));
    edit({ exposure: 0.5 });
    act(() => vi.advanceTimersByTime(1000));
    expect(pasted()).toEqual([]);
  });

  it('does not copy a photograph being opened or its recipe arriving', () => {
    renderHook(() => useEditAllSelected(true, selection));
    act(() =>
      useEditorStore.setState({
        selectedImage: { path: '/s/a.CR3', isReady: false } as never,
        adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 2 } as Adjustments,
      }),
    );
    act(() =>
      useEditorStore.setState({
        selectedImage: { path: '/s/a.CR3', isReady: true } as never,
        adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 1 } as Adjustments,
      }),
    );
    act(() => vi.advanceTimersByTime(1000));
    expect(pasted()).toEqual([]);
  });

  it('does not copy the default aspect ratio the full decode adapts on a ready photograph', () => {
    renderHook(() => useEditAllSelected(true, selection));
    act(() =>
      useEditorStore.setState((state) => ({
        selectedImage: { ...state.selectedImage!, width: 6024, height: 4020, isDecoded: true },
        adjustments: { ...state.adjustments, aspectRatio: 6024 / 4020 },
      })),
    );
    act(() => vi.advanceTimersByTime(1000));
    expect(pasted()).toEqual([]);
    expect(live()).toEqual([]);
  });

  it('leaves per-photograph retouching on the primary', () => {
    renderHook(() => useEditAllSelected(true, selection));
    edit({ spots: [{ id: 's' }] } as unknown as Partial<Adjustments>);
    act(() => vi.advanceTimersByTime(1000));
    expect(pasted()).toEqual([]);
  });

  it('writes what was gathered when it is switched off', () => {
    const { rerender } = renderHook(({ on }) => useEditAllSelected(on, selection), {
      initialProps: { on: true },
    });
    edit({ saturation: 20 });
    rerender({ on: false });
    expect(pasted()).toEqual([{ paths: ['/s/a.CR3', '/s/c.CR3'], adjustments: { saturation: 20 } }]);
  });

  it('writes a gathered edit to all the others when the primary switches to one of them', () => {
    renderHook(() => useEditAllSelected(true, selection));
    edit({ exposure: 0.5 });
    act(() => useEditorStore.setState({ selectedImage: { path: '/s/a.CR3', isReady: false } as never }));
    expect(pasted()).toEqual([{ paths: ['/s/a.CR3', '/s/c.CR3'], adjustments: { exposure: 0.5 } }]);
    // The new primary reads its recipe after that write.
    expect(waitForPendingMetadataSave('/s/a.CR3')).toBeDefined();
    act(() => vi.advanceTimersByTime(1000));
    expect(pasted()).toHaveLength(1);
  });

  it('writes to the photographs selected when the edit was made, though the selection changed', () => {
    const { rerender } = renderHook(({ on, paths }) => useEditAllSelected(on, paths), {
      initialProps: { on: true, paths: selection },
    });
    edit({ exposure: 0.5 });
    rerender({ on: false, paths: ['/s/b.CR3'] });
    expect(pasted()).toEqual([{ paths: ['/s/a.CR3', '/s/c.CR3'], adjustments: { exposure: 0.5 } }]);
  });

  it('sends the primary\'s sharpening response with sharpening edits, and response 1 for an undo to none', async () => {
    useEditorStore.setState((state) => ({ adjustments: { ...state.adjustments, sharpenVersion: 2 } as Adjustments }));
    renderHook(() => useEditAllSelected(true, selection));
    edit({ sharpness: 80 });
    // Live and written alike, so cells of response-1 photographs do not jump.
    expect(live()).toEqual([{ sharpness: 80, sharpenVersion: 2 }]);
    await act(async () => { vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10); });
    expect(pasted()).toEqual([{ paths: ['/s/a.CR3', '/s/c.CR3'], adjustments: { sharpness: 80, sharpenVersion: 2 } }]);
    // Undo to a recipe from before the key: the others go back to response 1 too.
    edit({ sharpenVersion: undefined });
    await act(async () => { vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10); });
    await act(async () => { await Promise.resolve(); });
    expect(pasted().at(-1)).toEqual({ paths: ['/s/a.CR3', '/s/c.CR3'], adjustments: { sharpenVersion: 1 } });
  });

  it('does not send a sharpening response live for a mask-only edit', () => {
    renderHook(() => useEditAllSelected(true, selection));
    edit({ masks: [{ id: 'm', adjustments: { sharpness: 30 }, subMasks: [] }] } as unknown as Partial<Adjustments>);
    // Drawn masks are written framed, not shown live: nothing to send.
    expect(live().filter((patch) => patch && 'sharpenVersion' in patch)).toEqual([]);
  });

  describe('live on the other cells', () => {
    it('sends the changes not yet written while the primary is edited', () => {
      renderHook(() => useEditAllSelected(true, selection));
      edit({ exposure: 0.5 });
      expect(live()).toEqual([{ exposure: 0.5 }]);
      // At most one update per display frame; the newest values win.
      edit({ exposure: 0.6 });
      edit({ exposure: 0.7, contrast: 5 });
      expect(live()).toHaveLength(1);
      act(() => vi.advanceTimersByTime(EDIT_ALL_LIVE_MS));
      expect(live().at(-1)).toEqual({ exposure: 0.7, contrast: 5 });
    });

    it('keeps them live until they are written, then lets the recipes show them', async () => {
      let written!: () => void;
      vi.mocked(invoke).mockImplementation((command) =>
        command === Invokes.ApplyAdjustmentsToPaths
          ? new Promise<void>((resolve) => {
              written = resolve;
            })
          : Promise.resolve(),
      );
      renderHook(() => useEditAllSelected(true, selection));
      edit({ exposure: 0.5 });
      act(() => vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10));
      expect(pasted()).toHaveLength(1);
      // Being written: still live, with the next edit added.
      edit({ contrast: 8 });
      act(() => vi.advanceTimersByTime(EDIT_ALL_LIVE_MS));
      expect(live().at(-1)).toEqual({ exposure: 0.5, contrast: 8 });
      vi.useRealTimers();
      await act(async () => written());
      await vi.waitFor(() => expect(live().at(-1)).toEqual({ contrast: 8 }));
      vi.mocked(invoke).mockImplementation(() => Promise.resolve());
    });

    it('clears the live changes when it is switched off and they are written', async () => {
      const { rerender } = renderHook(({ on }) => useEditAllSelected(on, selection), {
        initialProps: { on: true },
      });
      edit({ exposure: 0.4 });
      rerender({ on: false });
      vi.useRealTimers();
      await vi.waitFor(() => expect(live().at(-1)).toBeNull());
    });

    it('never shows drawn masks live (they are framed per photograph)', () => {
      expect(livePatch({ masks: [] } as Partial<Adjustments>)).toBeNull();
      expect(livePatch({ masks: [], exposure: 1 } as Partial<Adjustments>)).toEqual({ exposure: 1 });
    });
  });
});
