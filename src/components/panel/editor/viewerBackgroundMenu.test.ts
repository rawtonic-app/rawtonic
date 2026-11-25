import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { viewerBackgroundMenu } from './viewerBackgroundMenu';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { AppSettings, Invokes } from '../../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings });
});

it('offers the seven backgrounds with a check mark on the current one', () => {
  useSettingsStore.setState({ appSettings: { viewerBackground: 'light' } as unknown as AppSettings });
  const menu = viewerBackgroundMenu();
  expect(menu).toHaveLength(7);
  expect(menu.filter((option) => option.checked)).toHaveLength(1);
  expect(menu[2].checked).toBe(true);
  expect(menu.every((option) => option.checked !== undefined)).toBe(true);
});

it('checks Medium for settings that had the neutral grey canvas on', () => {
  useSettingsStore.setState({ appSettings: { editorNeutralGreyBg: true } as unknown as AppSettings });
  expect(viewerBackgroundMenu()[3].checked).toBe(true);
});

it('saves the chosen background in the app settings', () => {
  viewerBackgroundMenu()[6].onClick!();
  expect(useSettingsStore.getState().appSettings?.viewerBackground).toBe('black');
  expect(invoke).toHaveBeenCalledWith(Invokes.SaveSettings, {
    settings: expect.objectContaining({ viewerBackground: 'black' })
  });
});

it('saves an explicit Default even when legacy neutral grey was enabled', () => {
  useSettingsStore.setState({ appSettings: { editorNeutralGreyBg: true } as unknown as AppSettings });
  viewerBackgroundMenu()[0].onClick!();
  expect(viewerBackgroundMenu()[0].checked).toBe(true);
  expect(invoke).toHaveBeenCalledWith(Invokes.SaveSettings, {
    settings: expect.objectContaining({ viewerBackground: 'default' })
  });
});
