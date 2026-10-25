import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { debouncedSave, debouncedSetHistory, updateAdjustments, useEditorActions } from '../hooks/useEditorActions';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';
import { Invokes } from '../components/ui/AppProperties';
import { registerSliderDraftRenderer } from './sliderDraft';
import { EDIT_TOOL_STATES, RECIPE_WRITES, ViewOnlyRefusal, acceptEdit, editsRefused, invokeRecipeWrite } from './editGate';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), warn: vi.fn() } }));

const shown = { ...INITIAL_ADJUSTMENTS, exposure: -2 } as Adjustments;
const earlier = { ...INITIAL_ADJUSTMENTS, exposure: -1 } as Adjustments;
const later = { ...INITIAL_ADJUSTMENTS, exposure: -3 } as Adjustments;
const photo = (path: string, isReady = true) => ({
  path, isReady, isRaw: true, width: 4000, height: 3000, exif: {}, thumbnailUrl: '',
});
// The ways the photograph is only shown (utils/viewerMode).
const MODES = {
  'the Library loupe': { activeView: 'library', libraryLoupe: true, isFullScreen: false },
  'the full screen from the Library': { activeView: 'library', libraryLoupe: false, isFullScreen: true },
  'the full screen from Develop': { activeView: 'editor', libraryLoupe: false, isFullScreen: true },
} as const;
const DEVELOP = { activeView: 'editor', libraryLoupe: false, isFullScreen: false } as const;

beforeEach(() => {
  vi.mocked(invoke).mockImplementation((command) =>
    Promise.resolve(command === Invokes.CalculateAutoAdjustments ? { ...INITIAL_ADJUSTMENTS, exposure: 4 } : undefined),
  );
  useUIStore.setState(DEVELOP);
  // Edited in Develop (one step to undo, one to redo), with several selected.
  useEditorStore.setState({
    selectedImage: photo('/s/b.dng'),
    adjustments: shown,
    history: [earlier, shown, later],
    historyIndex: 1,
    copiedAdjustments: { exposure: 3 } as Adjustments,
    copiedFrom: '/s/a.dng',
    ...Object.fromEntries(EDIT_TOOL_STATES.map((key) => [key, null])),
  });
  useLibraryStore.setState({ multiSelectedPaths: ['/s/a.dng', '/s/b.dng'], libraryActivePath: '/s/b.dng' });
});
afterEach(() => {
  cleanup();
  useUIStore.setState(DEVELOP);
  debouncedSave.cancel();
  debouncedSetHistory.cancel();
  vi.clearAllMocks();
});

/** `step` done as the user, while one of their events is handled. */
function asUser<T>(type: string, step: () => T): T {
  let out: T | undefined;
  const listener = () => {
    out = step();
  };
  window.addEventListener(type, listener, { once: true });
  window.dispatchEvent(new Event(type));
  return out as T;
}
// The user's ways in: a key, the wheel (the crop zoom), a click (a menu).
const INPUTS = ['keydown', 'wheel', 'click'] as const;

const recipeWrites = () =>
  vi.mocked(invoke).mock.calls.filter(([command]) => (RECIPE_WRITES as readonly string[]).includes(command));

// Every entry point that edits a recipe, however it is reached (a key, a
// wheel, a menu, a tool, a panel). None may change anything while shown only.
type Actions = ReturnType<typeof useEditorActions>;
const EDITS: Array<[string, (actions: Actions) => unknown]> = [
  ['updateAdjustments (object)', () => updateAdjustments({ exposure: 1 })],
  ['updateAdjustments (function: the crop wheel, drags, Quick Adjust)', () =>
    updateAdjustments((prev) => ({ ...prev, crop: { x: 1, y: 1, width: 10, height: 10, unit: 'px' } as never }))],
  ['setAdjustments', (a) => a.setAdjustments({ exposure: 1 })],
  ['setEditor({ adjustments })', () => useEditorStore.getState().setEditor({ adjustments: later })],
  ['setEditor(state => adjustments)', () =>
    useEditorStore.getState().setEditor((s) => ({ adjustments: { ...s.adjustments, contrast: 9 } }))],
  ['useEditorStore.setState({ adjustments })', () => useEditorStore.setState({ adjustments: later })],
  ['undo', () => useEditorStore.getState().undo()],
  ['redo', () => useEditorStore.getState().redo()],
  ['goToHistoryIndex', () => useEditorStore.getState().goToHistoryIndex(2)],
  ['resetHistory', () => useEditorStore.getState().resetHistory(later)],
  ['handleRotate', (a) => a.handleRotate(90)],
  ['handleAutoAdjustments', (a) => a.handleAutoAdjustments()],
  ['handleLutSelect', (a) => a.handleLutSelect('/l.cube')],
  ['handleResetAdjustments (selection)', (a) => a.handleResetAdjustments()],
  ['handleAutoLensCorrection (selection)', (a) => a.handleAutoLensCorrection()],
  ['applyAdjustmentsToPaths (selection)', (a) => a.applyAdjustmentsToPaths({ exposure: 2 })],
  ['handlePasteAdjustments (selection)', (a) => a.handlePasteAdjustments()],
  ['handlePasteAdjustments (shown photograph)', (a) => a.handlePasteAdjustments(['/s/b.dng'])],
];

describe.each(Object.entries(MODES))('in %s', (_mode, ui) => {
  it.each(INPUTS.flatMap((input) => EDITS.map(([name, edit]) => [name, input, edit] as const)))(
    'refuses %s on a %s',
    async (_name, input, edit) => {
    const { result } = renderHook(useEditorActions);
    act(() => useUIStore.setState(ui));
    expect(asUser(input, editsRefused)).toBe(true);
    await act(async () => {
      await asUser(input, () => edit(result.current));
    });
    debouncedSetHistory.flush();
    const editor = useEditorStore.getState();
    expect(editor.adjustments).toBe(shown);
    expect(editor.historyIndex).toBe(1);
    expect(editor.history).toEqual([earlier, shown, later]);
    expect(recipeWrites()).toEqual([]);
    },
  );

  it.each(EDIT_TOOL_STATES)('keeps the user from turning %s on', (key) => {
    act(() => useUIStore.setState(ui));
    asUser('click', () => {
      useEditorStore.getState().setEditor({ [key]: 'on' } as never);
      useEditorStore.setState({ [key]: 'on' } as never);
    });
    expect(useEditorStore.getState()[key]).toBeNull();
  });

  it.each(RECIPE_WRITES)('refuses the recipe write %s of the user without asking the backend', async (command) => {
    act(() => useUIStore.setState(ui));
    await expect(asUser('click', () => invokeRecipeWrite(command, { paths: ['/s/a.dng'] }))).rejects.toBeInstanceOf(
      ViewOnlyRefusal,
    );
    expect(invoke).not.toHaveBeenCalled();
  });

  it('lets the next photograph load on a key and a tool be turned off', () => {
    useEditorStore.setState({ isSpotToolActive: true });
    act(() => useUIStore.setState(ui));
    asUser('keydown', () => {
      useEditorStore.getState().setEditor({ isSpotToolActive: false });
      // Published with its source (useImageLoader), or before it is ready.
      useEditorStore.getState().setEditor({ selectedImage: photo('/s/c.dng', false) });
      useEditorStore.getState().setEditor({ adjustments: earlier });
    });
    expect(useEditorStore.getState()).toMatchObject({ isSpotToolActive: false, adjustments: earlier });
    asUser('keydown', () =>
      useEditorStore.getState().setEditor({ selectedImage: photo('/s/c.dng'), adjustments: later, history: [later], historyIndex: 0 }),
    );
    expect(useEditorStore.getState()).toMatchObject({ adjustments: later, historyIndex: 0 });
  });

  it("applies the editor's own updates outside the user's events (answers, retouch following the geometry)", () => {
    act(() => useUIStore.setState(ui));
    updateAdjustments({ exposure: 1 });
    expect(useEditorStore.getState().adjustments.exposure).toBe(1);
  });

  it('carries out an edit accepted before, also during a key event (Edit all turned off by F)', async () => {
    const accepted = acceptEdit()!;
    act(() => useUIStore.setState(ui));
    expect(asUser('keydown', () => acceptEdit())).toBeNull();
    await asUser('keydown', () =>
      accepted(() => {
        updateAdjustments({ exposure: 1 });
        return invokeRecipeWrite(Invokes.ApplyAdjustmentsToPaths, { paths: ['/s/a.dng'] });
      }),
    );
    expect(useEditorStore.getState().adjustments.exposure).toBe(1);
    expect(recipeWrites()).toHaveLength(1);
  });
});

// Asked for in Develop, answered or written after F showed the photograph
// alone: what was accepted is done, and the editor shows what is on disk.
describe('work accepted before the photograph is only shown', () => {
  type Held = { command: string; resolve(value: unknown): void };
  let held: Held[];
  const answer = async (command: string, value?: unknown) => {
    await act(async () => {});
    const request = held.find((r) => r.command === command)!;
    held = held.filter((r) => r !== request);
    await act(async () => request.resolve(value));
    await act(async () => {});
  };
  const pressF = () => asUser('keydown', () => act(() => useUIStore.setState(MODES['the full screen from Develop'])));
  beforeEach(() => {
    held = [];
    vi.mocked(invoke).mockImplementation((command) => new Promise((resolve) => held.push({ command, resolve })));
  });

  it('commits a drag begun before F when it is released after (its draft keeps its acceptance)', () => {
    const stopDrafts = registerSliderDraftRenderer(() => true);
    try {
      useEditorStore.setState({ isSliderDragging: true });
      updateAdjustments({ exposure: 2 });
      expect(useEditorStore.getState().adjustments).toBe(shown);
      pressF();
      asUser('pointerup', () => useEditorStore.getState().setEditor({ isSliderDragging: false }));
      debouncedSetHistory.flush();
      const editor = useEditorStore.getState();
      expect(editor.adjustments.exposure).toBe(2);
      expect(editor.history.at(-1)).toBe(editor.adjustments);
      // A drag begun in the full screen draws no draft.
      useEditorStore.setState({ isSliderDragging: true });
      asUser('pointermove', () => updateAdjustments({ exposure: 3 }));
      asUser('pointerup', () => useEditorStore.getState().setEditor({ isSliderDragging: false }));
      expect(useEditorStore.getState().adjustments.exposure).toBe(2);
    } finally {
      stopDrafts();
    }
  });

  it('applies Auto answered after F', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => void result.current.handleAutoAdjustments());
    pressF();
    await answer(Invokes.CalculateAutoAdjustments, { ...INITIAL_ADJUSTMENTS, exposure: 4 });
    expect(useEditorStore.getState().adjustments.exposure).toBe(4);
  });

  it('shows the reset written after F, as one step', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => result.current.handleResetAdjustments(['/s/b.dng']));
    pressF();
    await answer(Invokes.ResetAdjustmentsForPaths);
    const editor = useEditorStore.getState();
    expect(editor.adjustments.exposure).toBe(0);
    expect(editor.history.at(-1)).toBe(editor.adjustments);
    expect(editor.historyIndex).toBe(editor.history.length - 1);
  });

  it('shows the auto lens correction written after F', async () => {
    const { result } = renderHook(useEditorActions);
    act(() => result.current.handleAutoLensCorrection(['/s/b.dng']));
    pressF();
    await answer(Invokes.ApplyAutoLensCorrectionToPaths);
    await answer(Invokes.LoadMetadata, { adjustments: { ...INITIAL_ADJUSTMENTS, lensMaker: 'Maker', lensModel: '35mm' } });
    expect(useEditorStore.getState().adjustments.lensModel).toBe('35mm');
  });
});

it('edits again once the photograph is no longer only shown', () => {
  useUIStore.setState(MODES['the full screen from Develop']);
  updateAdjustments({ exposure: 1 });
  useUIStore.setState(DEVELOP);
  updateAdjustments({ exposure: 1 });
  expect(useEditorStore.getState().adjustments.exposure).toBe(1);
});

// Every source file of the app (tests aside), as text.
const SOURCES = import.meta.glob<string>(['../**/*.{ts,tsx}', '!../**/*.test.{ts,tsx}'], {
  query: '?raw',
  import: 'default',
  eager: true,
});

it('writes recipes by path only through the gate', () => {
  const names = RECIPE_WRITES.map((command) => Object.entries(Invokes).find(([, value]) => value === command)![0]);
  const direct = new RegExp(`Invokes\\.(${names.join('|')})\\b`);
  const offenders = Object.entries(SOURCES)
    .filter(([file]) => !file.endsWith('/editGate.ts'))
    .flatMap(([file, text]) =>
      text
        .split('\n')
        .filter((line) => direct.test(line) && !line.includes('invokeRecipeWrite'))
        .map((line) => `${file}: ${line.trim()}`),
    );
  expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
  expect(offenders).toEqual([]);
});
