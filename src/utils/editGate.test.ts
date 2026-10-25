import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { debouncedSave, debouncedSetHistory, updateAdjustments, useEditorActions } from '../hooks/useEditorActions';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { INITIAL_ADJUSTMENTS, type Adjustments } from './adjustments';
import { Invokes } from '../components/ui/AppProperties';
import { EDIT_TOOL_STATES, RECIPE_WRITES, ViewOnlyRefusal, editsRefused, invokeRecipeWrite } from './editGate';

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
  it.each(EDITS)('refuses %s', async (_name, edit) => {
    const { result } = renderHook(useEditorActions);
    act(() => useUIStore.setState(ui));
    expect(editsRefused()).toBe(true);
    await act(async () => {
      await edit(result.current);
    });
    debouncedSetHistory.flush();
    const editor = useEditorStore.getState();
    expect(editor.adjustments).toBe(shown);
    expect(editor.historyIndex).toBe(1);
    expect(editor.history).toEqual([earlier, shown, later]);
    expect(recipeWrites()).toEqual([]);
  });

  it.each(EDIT_TOOL_STATES)('keeps %s from turning on', (key) => {
    act(() => useUIStore.setState(ui));
    useEditorStore.getState().setEditor({ [key]: 'on' } as never);
    useEditorStore.setState({ [key]: 'on' } as never);
    expect(useEditorStore.getState()[key]).toBeNull();
  });

  it.each(RECIPE_WRITES)('refuses the recipe write %s without asking the backend', async (command) => {
    act(() => useUIStore.setState(ui));
    await expect(invokeRecipeWrite(command, { paths: ['/s/a.dng'] })).rejects.toBeInstanceOf(ViewOnlyRefusal);
    expect(invoke).not.toHaveBeenCalled();
  });

  it('lets the next photograph load and a tool be turned off', () => {
    useEditorStore.setState({ isSpotToolActive: true });
    act(() => useUIStore.setState(ui));
    useEditorStore.getState().setEditor({ isSpotToolActive: false });
    expect(useEditorStore.getState().isSpotToolActive).toBe(false);
    // Published with its source (useImageLoader), or before it is ready.
    useEditorStore.getState().setEditor({ selectedImage: photo('/s/c.dng', false) });
    useEditorStore.getState().setEditor({ adjustments: earlier });
    expect(useEditorStore.getState().adjustments).toBe(earlier);
    useEditorStore.getState().setEditor({ selectedImage: photo('/s/c.dng'), adjustments: later, history: [later], historyIndex: 0 });
    expect(useEditorStore.getState()).toMatchObject({ adjustments: later, historyIndex: 0 });
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
