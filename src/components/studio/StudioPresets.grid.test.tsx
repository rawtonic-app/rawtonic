import '../../i18n';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import StudioPresets from './StudioPresets';
import { useEditorStore } from '../../store/useEditorStore';
import { useLibraryStore } from '../../store/useLibraryStore';
import { ContextMenuProvider } from '../../context/ContextMenuContext';
import { INITIAL_ADJUSTMENTS } from '../../utils/adjustments';
import { resetPresetLibrary, type UserPreset } from '../../hooks/usePresets';
import { resetBuiltinPresets } from '../../utils/builtinPresets';
import { useSettingsStore } from '../../store/useSettingsStore';
import type { AppSettings } from '../ui/AppProperties';
import { clearPresetThumbnailCache, recipeChanges } from './presetThumbnails';
import { presetPreviewOf } from './StudioPresets';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../../hooks/useEditorActions', () => ({
  useEditorActions: () => ({ applyAdjustmentsToPaths: vi.fn(), setAdjustments: vi.fn() }),
  openPasteTarget: (_path: string, recipe: unknown) => ({ recipe, isNew: false })
}));
const mockInvoke = vi.mocked(invoke);

const stored: UserPreset[] = [
  { preset: { id: 'warm', name: 'Warm evening', adjustments: { temperature: 12 }, presetType: 'tool' } },
  {
    folder: {
      id: 'bw',
      name: 'Black & white',
      children: [{ id: 'hc', name: 'High contrast', adjustments: { contrast: 40, saturation: -100 } }]
    }
  }
];
const builtins = [
  {
    id: 'bw-high-contrast',
    version: 1,
    name: 'B&W high contrast',
    presetType: 'style',
    adjustments: { blackAndWhite: true, contrast: 35 }
  }
];

/** A framed answer: one 2-byte "JPEG" per variant (length 0 where `missing`). */
const framed = (count: number, missing: number[] = []) => {
  const out = new Uint8Array(count * 6);
  let at = 0;
  for (let i = 0; i < count; i++) {
    if (missing.includes(i)) {
      out.set([0, 0, 0, 0], at);
      at += 4;
    } else {
      out.set([2, 0, 0, 0, 0xff, 0xd8], at);
      at += 6;
    }
  }
  return out.buffer.slice(0, at);
};

type TileCall = { variants: Record<string, unknown>[]; generation: number; path: string };
let tileCalls: TileCall[] = [];
let answer: (call: TileCall) => Promise<ArrayBuffer> = async (call) => framed(call.variants.length);

beforeEach(() => {
  tileCalls = [];
  answer = async (call) => framed(call.variants.length);
  clearPresetThumbnailCache();
  resetBuiltinPresets();
  resetPresetLibrary();
  const handleSettingsChange = vi.fn(async (settings: AppSettings) => {
    useSettingsStore.setState({ appSettings: settings });
  });
  useSettingsStore.setState({ appSettings: { presetLayout: 'grid' } as AppSettings, handleSettingsChange } as never);
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command, args) => {
    if (command === 'load_presets') return stored;
    if (command === 'load_builtin_presets') return builtins;
    if (command === 'preset_thumbnails') {
      const a = args as Record<string, unknown>;
      const call = {
        variants: a.variants as Record<string, unknown>[],
        generation: a.generation as number,
        path: a.path as string
      };
      tileCalls.push(call);
      return answer(call);
    }
    return undefined;
  });
  let n = 0;
  URL.createObjectURL = vi.fn(() => `blob:tile-${n++}`);
  URL.revokeObjectURL = vi.fn();
  useEditorStore.setState({
    selectedImage: { path: '/job/a.arw', isReady: true } as never,
    adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 0.5 },
    isSliderDragging: false,
    previewOverride: null
  });
  useLibraryStore.setState({ multiSelectedPaths: ['/job/a.arw'] });
});
afterEach(cleanup);

// Several rounds: a frame finds the tiles on screen, the next round's timer asks for them.
const settle = async () => {
  for (let i = 0; i < 3; i++) await act(async () => void (await new Promise((resolve) => setTimeout(resolve, 80))));
};

const renderPanel = async (open = true) => {
  render(
    <ContextMenuProvider>
      <StudioPresets />
    </ContextMenuProvider>
  );
  await act(async () => {});
  if (open) setPanelOpen(true);
  await settle();
};

const setPanelOpen = (open: boolean) => {
  const details = document.querySelector('details')!;
  details.open = open;
  fireEvent(details, new Event('toggle'));
};

const tileNames = (call: TileCall) =>
  call.variants.map((v) =>
    v.temperature === 12 ? 'warm' : v.saturation === -100 ? 'hc' : v.blackAndWhite ? 'bw' : '?'
  );

describe('presets grid (PRESET-THUMB)', () => {
  it('stays a list by default and remembers the grid', async () => {
    useSettingsStore.setState({ appSettings: {} as AppSettings });
    await renderPanel();
    expect(document.querySelector('.studio-preset-tiles')).toBeNull();
    expect(tileCalls).toHaveLength(0);
    const toggle = screen.getByRole('button', { name: 'Show as thumbnails of this photo' });
    expect(toggle.getAttribute('aria-pressed')).toBe('false');
    fireEvent.click(toggle);
    expect(useSettingsStore.getState().appSettings?.presetLayout).toBe('grid');
    expect(toggle.getAttribute('aria-pressed')).toBe('true');
    await settle();
    expect(document.querySelectorAll('.studio-preset-tile')).toHaveLength(3);
    fireEvent.click(toggle);
    expect(useSettingsStore.getState().appSettings?.presetLayout).toBe('list');
  });

  it('renders only the tiles on screen, with each preset as its hover preview shows it', async () => {
    await renderPanel(false);
    // A closed panel shows no tiles: nothing is rendered.
    expect(tileCalls).toHaveLength(0);
    // A closed folder's presets are not on screen either.
    fireEvent.click(screen.getByRole('button', { name: /Black & white/ }));
    setPanelOpen(true);
    await settle();
    expect(tileCalls).toHaveLength(1);
    expect(tileNames(tileCalls[0]).sort()).toEqual(['bw', 'warm']);
    const warm = tileCalls[0].variants.find((v) => v.temperature === 12)!;
    // Only the preset's changes travel, not the whole recipe.
    expect(warm).not.toHaveProperty('exposure');
    expect(tileCalls[0].path).toBe('/job/a.arw');
    const images = [...document.querySelectorAll('.studio-preset-thumb img')].map((img) => img.getAttribute('src'));
    expect(images).toEqual(['blob:tile-0', 'blob:tile-1']);
    // Opening the folder asks for its tile only.
    fireEvent.click(screen.getByRole('button', { name: /Black & white/ }));
    await settle();
    expect(tileCalls).toHaveLength(2);
    expect(tileNames(tileCalls[1])).toEqual(['hc']);
  });

  it('keeps rendered tiles per photo and recipe: showing them again asks for nothing', async () => {
    await renderPanel();
    expect(tileCalls).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Show as thumbnails of this photo' }));
    await settle();
    fireEvent.click(screen.getByRole('button', { name: 'Show as thumbnails of this photo' }));
    await settle();
    expect(tileCalls).toHaveLength(1);
    expect(document.querySelectorAll('.studio-preset-thumb img')).toHaveLength(3);
    // An edit renders them again; the old tiles stay until then.
    act(() => useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 1 } }));
    expect(document.querySelectorAll('.studio-preset-thumb img')).toHaveLength(3);
    await settle();
    expect(tileCalls).toHaveLength(2);
  });

  it('renders nothing during a slider drag, and once after it', async () => {
    await renderPanel();
    act(() => useEditorStore.setState({ isSliderDragging: true }));
    act(() => useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 0.8 } }));
    await settle();
    act(() => useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 0.9 } }));
    await settle();
    expect(tileCalls).toHaveLength(1);
    act(() => useEditorStore.setState({ isSliderDragging: false }));
    await settle();
    expect(tileCalls).toHaveLength(2);
  });

  it('leaves the remaining batches until a drag that started meanwhile ends', async () => {
    const many: UserPreset[] = Array.from({ length: 12 }, (_, i) => ({
      preset: { id: `p${i}`, name: `Look ${i}`, adjustments: { contrast: i + 1 }, presetType: 'tool' as const }
    }));
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === 'load_presets') return many;
      if (command === 'load_builtin_presets') return [];
      if (command === 'preset_thumbnails') {
        const a = args as Record<string, unknown>;
        tileCalls.push({ variants: a.variants as never, generation: a.generation as number, path: a.path as string });
        // The drag starts while the first batch renders.
        useEditorStore.setState({ isSliderDragging: true });
        return framed((a.variants as unknown[]).length);
      }
      return undefined;
    });
    await renderPanel();
    expect(tileCalls).toHaveLength(1);
    expect(tileCalls[0].variants).toHaveLength(8);
    act(() => useEditorStore.setState({ isSliderDragging: false }));
    await settle();
    expect(tileCalls.slice(1).map((call) => call.variants.length)).toEqual([4]);
  });

  it('stops the batch in flight when a drag starts and renders its tiles after the drag', async () => {
    const pending: ((value: ArrayBuffer) => void)[] = [];
    answer = () => new Promise<ArrayBuffer>((resolve) => pending.push(resolve));
    await renderPanel();
    expect(tileCalls).toHaveLength(1);
    act(() => useEditorStore.setState({ isSliderDragging: true }));
    expect(mockInvoke).toHaveBeenCalledWith('cancel_preset_thumbnails', { generation: tileCalls[0].generation });
    // The stopped batch answers with no tiles: they are not marked as undrawable.
    await act(async () => pending[0](framed(3, [0, 1, 2])));
    expect(document.querySelectorAll('.studio-preset-thumb.is-unavailable')).toHaveLength(0);
    answer = async (call) => framed(call.variants.length);
    act(() => useEditorStore.setState({ isSliderDragging: false }));
    await settle();
    expect(tileCalls).toHaveLength(2);
    expect(document.querySelectorAll('.studio-preset-thumb img')).toHaveLength(3);
  });

  it('a pointer held on Amount holds the tiles (no new batch, the one in flight stops) until it comes up', async () => {
    await renderPanel();
    // A tool preset applied to the open photo gets its Amount.
    fireEvent.click(screen.getByRole('button', { name: 'Warm evening' }));
    const amount = screen.getByRole('slider', { name: 'Preset amount' });
    const pending: ((value: ArrayBuffer) => void)[] = [];
    answer = () => new Promise<ArrayBuffer>((resolve) => pending.push(resolve));
    act(() => useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 2 } }));
    await settle();
    const calls = tileCalls.length;
    expect(pending).toHaveLength(1);
    fireEvent.pointerDown(amount, { button: 0 });
    expect(mockInvoke).toHaveBeenCalledWith('cancel_preset_thumbnails', { generation: tileCalls.at(-1)!.generation });
    // The Amount is not a draft drag: it changes nothing else.
    expect(useEditorStore.getState().isSliderDragging).toBe(false);
    await act(async () => pending[0](framed(3, [0, 1, 2])));
    act(() => useEditorStore.setState({ adjustments: { ...INITIAL_ADJUSTMENTS, exposure: 2.5 } }));
    await settle();
    expect(tileCalls).toHaveLength(calls);
    answer = async (call) => framed(call.variants.length);
    fireEvent.pointerUp(window);
    await settle();
    expect(tileCalls).toHaveLength(calls + 1);
    expect(document.querySelectorAll('.studio-preset-thumb.is-unavailable')).toHaveLength(0);
  });

  it('cancels the tiles still rendering when another photo opens or the panel closes', async () => {
    const pending: ((value: ArrayBuffer) => void)[] = [];
    answer = () => new Promise<ArrayBuffer>((resolve) => pending.push(resolve));
    await renderPanel();
    expect(tileCalls).toHaveLength(1);
    const first = tileCalls[0].generation;
    act(() =>
      useEditorStore.setState({
        selectedImage: { path: '/job/b.arw', isReady: true } as never,
        adjustments: { ...INITIAL_ADJUSTMENTS }
      })
    );
    expect(mockInvoke).toHaveBeenCalledWith('cancel_preset_thumbnails', { generation: first });
    await settle();
    expect(tileCalls.at(-1)!.path).toBe('/job/b.arw');
    const second = tileCalls.at(-1)!.generation;
    setPanelOpen(false);
    await settle();
    expect(mockInvoke).toHaveBeenCalledWith('cancel_preset_thumbnails', { generation: second });
    // The first photo's late answer is not shown for the second.
    await act(async () => pending.forEach((resolve) => resolve(framed(3))));
    expect(document.querySelectorAll('.studio-preset-thumb img')).toHaveLength(0);
  });

  it('marks a tile that cannot be drawn on the shown frame and does not ask again', async () => {
    answer = async (call) => framed(call.variants.length, [0]);
    await renderPanel();
    expect(document.querySelectorAll('.studio-preset-thumb.is-unavailable')).toHaveLength(1);
    fireEvent.click(screen.getByRole('button', { name: 'Show as thumbnails of this photo' }));
    await settle();
    fireEvent.click(screen.getByRole('button', { name: 'Show as thumbnails of this photo' }));
    await settle();
    expect(tileCalls).toHaveLength(1);
  });
});

describe('recipeChanges', () => {
  it('sends changed keys and null for dropped ones', () => {
    const base = { exposure: 1, contrast: 5, masks: [{ id: 'a' }], lutPath: 'x.cube' } as never;
    const next = { exposure: 1, contrast: 10, masks: [{ id: 'a' }], blackAndWhite: true } as never;
    expect(recipeChanges(base, next)).toEqual({ contrast: 10, blackAndWhite: true, lutPath: null });
  });
});

describe('presetPreviewOf', () => {
  it('shows nothing for an imported preset whose settings are not an object', () => {
    const preset = { id: 'bad', name: 'Bad', adjustments: null } as never;
    expect(presetPreviewOf(preset, { ...INITIAL_ADJUSTMENTS }, { path: '/job/a.arw' })).toBeNull();
  });
});
