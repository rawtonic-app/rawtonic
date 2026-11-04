import '../../i18n';
import { afterEach, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import i18n from 'i18next';
import { invoke } from '@tauri-apps/api/core';
import { relaunch } from '@tauri-apps/plugin-process';
import SettingsPanel from './SettingsPanel';
import { useSettingsStore } from '../../store/useSettingsStore';
import { AppSettings, Invokes } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => false, invoke: vi.fn(), convertFileSrc: (p: string) => p }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn(() => Promise.resolve()) }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

Object.defineProperty(window, 'matchMedia', {
  configurable: true,
  value: () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

/** The panel as the app wires it: settings from the store, saved through it. */
function Wired() {
  const appSettings = useSettingsStore((s) => s.appSettings);
  const save = useSettingsStore((s) => s.handleSettingsChange);
  return (
    <SettingsPanel
      appSettings={appSettings as AppSettings}
      onBack={vi.fn()}
      onLibraryRefresh={vi.fn()}
      onSettingsChange={save}
      rootPaths={[]}
    />
  );
}

it('Save & Relaunch keeps the app open, says why and offers the button again when the settings cannot be written', async () => {
  vi.mocked(invoke).mockImplementation((command) =>
    command === Invokes.SaveSettings ? Promise.reject('disk full') : Promise.resolve([])
  );
  useSettingsStore.setState({ appSettings: { useWgpuRenderer: true } as AppSettings });
  const user = userEvent.setup();
  render(<Wired />);
  await user.click(screen.getByRole('button', { name: i18n.t('settings.categories.processing') }));
  await user.click(screen.getByLabelText(i18n.t('settings.processing.wgpuLabel')));
  await user.click(screen.getByRole('button', { name: i18n.t('settings.processing.saveRelaunch') }));
  expect((await screen.findByRole('alert')).textContent).toBe(i18n.t('settings.processing.saveRelaunchFailed'));
  expect(relaunch).not.toHaveBeenCalled();
  // The disk takes it again: the same button saves and relaunches.
  vi.mocked(invoke).mockImplementation(() => Promise.resolve([]));
  await user.click(screen.getByRole('button', { name: i18n.t('settings.processing.saveRelaunch') }));
  await vi.waitFor(() => expect(relaunch).toHaveBeenCalledTimes(1));
});
