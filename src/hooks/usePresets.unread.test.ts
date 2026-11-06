import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { loadPresetLibrary, resetPresetLibrary, usePresetStore } from './usePresets';
import { libraryChanges, PRESETS_UNREAD_NAME } from '../store/libraryFiles';
import { relaunchAfterSettingsSaves } from '../store/useSettingsStore';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'linux' }));

beforeEach(() => {
  resetPresetLibrary();
  libraryChanges.reset();
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === Invokes.LoadPresets) throw 'presets.json is unreadable';
    return [];
  });
});

it('an unread preset file without edits allows Save and Relaunch and is never written', async () => {
  await loadPresetLibrary();
  const relaunch = vi.fn(async () => {});
  expect(await relaunchAfterSettingsSaves(relaunch)).toEqual([]);
  expect(relaunch).toHaveBeenCalledOnce();
  expect(invoke).not.toHaveBeenCalledWith(Invokes.SavePresets, expect.anything());
});

it('an edit after a failed read is reported without writing over the unread file', async () => {
  await loadPresetLibrary();
  usePresetStore.getState().setPresets([{ preset: { id: 'new', name: 'New', adjustments: {} } }]);
  const relaunch = vi.fn(async () => {});
  expect(await relaunchAfterSettingsSaves(relaunch)).toEqual([PRESETS_UNREAD_NAME]);
  expect(relaunch).not.toHaveBeenCalled();
  expect(invoke).not.toHaveBeenCalledWith(Invokes.SavePresets, expect.anything());
});
