import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { EDIT_ALL_LIVE_MS, EDIT_ALL_SETTLE_MS, livePatch, useEditAllSelected } from './useEditAllSelected';
import { useEditorStore } from '../store/useEditorStore';
import { flushRecipeSaves, waitForPendingMetadataSave } from './useEditorActions';
import { useLibraryStore } from '../store/useLibraryStore';
import { useMultiViewStore } from '../store/useMultiViewStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes } from '../components/ui/AppProperties';
import { switchLibraryAfterSaves, type UnsavedChoice } from '../utils/quit';
import { INITIAL_ADJUSTMENTS, type Adjustments } from '../utils/adjustments';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), success: vi.fn() } }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));

const selection = ['/s/a.CR3', '/s/b.CR3', '/s/c.CR3'];
const pasted = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([command]) => command === Invokes.ApplyAdjustmentsToPaths)
    .map(([, args]) => {
      const { paths, adjustments } = args as { paths: string[]; adjustments: Partial<Adjustments> };
      return { paths, adjustments };
    });
/** Where each write said its settings come from. */
const pastedFrom = () =>
  vi
    .mocked(invoke)
    .mock.calls.filter(([command]) => command === Invokes.ApplyAdjustmentsToPaths)
    .map(([, args]) => (args as { paste: unknown }).paste);

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
    // The others get its layers, their AI detections made on each of them.
    expect(pastedFrom()).toEqual([{ source: '/s/b.CR3', replaceLayers: true }]);
  });

  it('does not send an AI detection made on the primary to the others', () => {
    const sky = (data: string | null, pending: boolean) => ({
      id: 'l', name: 'Sky', visible: true, invert: false, opacity: 100, adjustments: {},
      subMasks: [{ id: 's', type: 'ai-sky', visible: true, invert: false, opacity: 100, mode: 'additive',
        parameters: { maskDataBase64: data, ...(pending ? { detectPending: true } : {}) } }],
    });
    act(() => useEditorStore.setState({ adjustments: { ...useEditorStore.getState().adjustments, masks: [sky(null, true)] as never } }));
    renderHook(() => useEditAllSelected(true, selection));
    edit({ masks: [sky('data:own', false)] as never });
    act(() => vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10));
    expect(pasted()).toEqual([]);
  });

  it("sends an AI detection made on the primary to its virtual copies, which share its pixels", () => {
    const sky = (data: string | null) => ({
      id: 'l', name: 'Sky', visible: true, invert: false, opacity: 100, adjustments: {},
      subMasks: [{ id: 's', type: 'ai-sky', visible: true, invert: false, opacity: 100, mode: 'additive',
        parameters: { maskDataBase64: data } }],
    });
    act(() => useEditorStore.setState({ adjustments: { ...useEditorStore.getState().adjustments, masks: [sky(null)] as never } }));
    renderHook(() => useEditAllSelected(true, [...selection, '/s/b.CR3?vc=c0ffee']));
    edit({ masks: [sky('data:own')] as never });
    act(() => vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10));
    expect(pasted()).toEqual([{ paths: ['/s/b.CR3?vc=c0ffee'], adjustments: { masks: [sky('data:own')] } }]);
  });

  it('takes away from the others a setting the primary no longer has', () => {
    act(() =>
      useEditorStore.setState({
        adjustments: { ...useEditorStore.getState().adjustments, whiteBalance: 'custom', wbTemperature: 2850 } as never,
      }),
    );
    renderHook(() => useEditAllSelected(true, selection));
    const asShot = { ...useEditorStore.getState().adjustments } as Record<string, unknown>;
    delete asShot.whiteBalance;
    delete asShot.wbTemperature;
    act(() => useEditorStore.setState({ adjustments: asShot as never }));
    act(() => vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10));
    expect(pasted()[0].adjustments).toMatchObject({ whiteBalance: null, wbTemperature: null });
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

  it('sends the primary\'s Clarity response with Clarity edits', async () => {
    useEditorStore.setState((state) => ({ adjustments: { ...state.adjustments, clarityVersion: 2 } as Adjustments }));
    renderHook(() => useEditAllSelected(true, selection));
    edit({ clarity: 30 });
    expect(live()).toEqual([{ clarity: 30, clarityVersion: 2 }]);
    await act(async () => { vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10); });
    expect(pasted()).toEqual([{ paths: ['/s/a.CR3', '/s/c.CR3'], adjustments: { clarity: 30, clarityVersion: 2 } }]);
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

  describe('at a library switch', () => {
    // The backend as the write command sees it: written into the library
    // open now.
    let active: string;
    let written: Record<string, string[]>;
    let failing: boolean;
    let opened: () => void;
    beforeEach(() => {
      active = 'A';
      written = { A: [], B: [] };
      failing = false;
      vi.mocked(invoke).mockImplementation(async (command, args) => {
        if (command === Invokes.ApplyAdjustmentsToPaths) {
          if (failing) throw new Error('database is locked');
          written[active].push(...(args as { paths: string[] }).paths);
        }
        return undefined;
      });
    });
    const openB = () =>
      new Promise<string>((resolve) => {
        opened = () => {
          active = 'B';
          resolve('B');
        };
      });

    it('writes what was gathered into the library being left, and nothing made during the switch into the next', async () => {
      renderHook(() => useEditAllSelected(true, selection));
      edit({ exposure: 0.5 });
      // Switched within the settle time: the edit is written into A first.
      const switched = switchLibraryAfterSaves(openB, async () => 'cancel');
      await act(async () => {
        await vi.waitFor(() => expect(written.A).toEqual(['/s/a.CR3', '/s/c.CR3']));
      });
      // An edit of the photograph still shown while B opens belongs to A.
      edit({ exposure: 0.9 });
      await act(async () => {
        opened();
        expect(await switched).toBe('B');
        vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10);
      });
      expect(written.B).toEqual([]);
      expect(pasted()).toHaveLength(1);
    });

    it('is written before an export reads the recipes, and an export is told what failed', async () => {
      renderHook(() => useEditAllSelected(true, selection));
      edit({ exposure: 0.5 });
      // Exported within the settle time: written first.
      await act(async () => {
        expect(await flushRecipeSaves()).toEqual([]);
      });
      expect(written.A).toEqual(['/s/a.CR3', '/s/c.CR3']);
      failing = true;
      edit({ exposure: 0.7 });
      let unsaved: string[] = [];
      await act(async () => {
        unsaved = await flushRecipeSaves();
      });
      expect(unsaved).toEqual(['/s/a.CR3', '/s/c.CR3']);
      // Tried again by the next flush.
      failing = false;
      await act(async () => {
        unsaved = await flushRecipeSaves();
      });
      expect(unsaved).toEqual([]);
      expect(written.A).toEqual(['/s/a.CR3', '/s/c.CR3', '/s/a.CR3', '/s/c.CR3']);
    });

    it('keeps a failed write after it is switched off, until it is saved', async () => {
      const { rerender } = renderHook(({ on }) => useEditAllSelected(on, selection), { initialProps: { on: true } });
      failing = true;
      edit({ exposure: 0.5 });
      await act(async () => {
        vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10);
      });
      rerender({ on: false });
      // Back in the Library, the selection is exported: the export is told.
      let unsaved: string[] = [];
      await act(async () => {
        unsaved = await flushRecipeSaves();
      });
      expect(unsaved).toEqual(['/s/a.CR3', '/s/c.CR3']);
      failing = false;
      await act(async () => {
        unsaved = await flushRecipeSaves();
      });
      expect(unsaved).toEqual([]);
      expect(written.A).toEqual(['/s/a.CR3', '/s/c.CR3']);
      // Saved: nothing is written again.
      await act(async () => {
        await flushRecipeSaves();
      });
      expect(pasted()).toHaveLength(5);
    });

    it('never writes a failed edit over a newer one saved since, also from a later run', async () => {
      const exposures = () =>
        pasted()
          .filter(({ paths }) => paths.includes('/s/c.CR3'))
          .map(({ adjustments }) => adjustments.exposure);
      const { rerender } = renderHook(({ on }) => useEditAllSelected(on, selection), { initialProps: { on: true } });
      failing = true;
      edit({ exposure: 0.5 });
      await act(async () => {
        vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10);
      });
      rerender({ on: false });
      failing = false;
      // Switched on again, a newer Exposure is saved to the others.
      rerender({ on: true });
      edit({ exposure: 0.9 });
      await act(async () => {
        vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10);
      });
      let unsaved: string[] = [];
      await act(async () => {
        unsaved = await flushRecipeSaves();
      });
      expect(unsaved).toEqual([]);
      // The failed 0.5 is not written again over the saved 0.9.
      expect(exposures()).toEqual([0.5, 0.9]);
    });

    it('gives a failed write to the editor when its photo opens; a flush never changes the editor', async () => {
      const { rerender } = renderHook(({ on }) => useEditAllSelected(on, selection), { initialProps: { on: true } });
      failing = true;
      edit({ exposure: 0.5 });
      await act(async () => {
        vi.advanceTimersByTime(EDIT_ALL_SETTLE_MS + 10);
      });
      rerender({ on: false });
      // c.CR3 opens: it shows the edit that failed to be written, as an
      // edit of it the editor saves.
      act(() =>
        useEditorStore.setState({
          selectedImage: { path: '/s/c.CR3', isReady: true, width: 6000, height: 4000 } as never,
          adjustments: { ...INITIAL_ADJUSTMENTS } as Adjustments,
        }),
      );
      expect(useEditorStore.getState().adjustments.exposure).toBe(0.5);
      failing = false;
      const shown = useEditorStore.getState().adjustments;
      let unsaved: string[] = [];
      await act(async () => {
        unsaved = await flushRecipeSaves();
      });
      expect(unsaved).toEqual([]);
      // a.CR3 is written to its file; the open c.CR3 is saved as the editor
      // shows it (also without an edit after opening), and the flush did not
      // change the editor.
      expect(written.A).toEqual(['/s/a.CR3']);
      const saves = vi
        .mocked(invoke)
        .mock.calls.filter(([command]) => command === Invokes.SaveMetadataAndUpdateThumbnail)
        .map(([, args]) => args as { path: string; adjustments: Adjustments });
      expect(saves.map(({ path, adjustments }) => [path, adjustments.exposure])).toEqual([['/s/c.CR3', 0.5]]);
      expect(useEditorStore.getState().adjustments).toBe(shown);
    });

    it('holds the switch while the write failed: Retry writes it into the library it was made in', async () => {
      renderHook(() => useEditAllSelected(true, selection));
      edit({ exposure: 0.5 });
      failing = true;
      const ask = vi.fn(async (): Promise<UnsavedChoice> => {
        failing = false;
        return 'retry';
      });
      const switched = switchLibraryAfterSaves(openB, ask);
      await act(async () => {
        await vi.waitFor(() => expect(ask).toHaveBeenCalledTimes(1));
      });
      expect(ask).toHaveBeenCalledWith(['Edits of a.CR3', 'Edits of c.CR3']);
      await act(async () => {
        await vi.waitFor(() => expect(written.A).toEqual(['/s/a.CR3', '/s/c.CR3']));
        opened();
        expect(await switched).toBe('B');
      });
      expect(written.B).toEqual([]);
    });
  });
});
