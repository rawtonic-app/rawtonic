// SPDX-License-Identifier: AGPL-3.0-or-later
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-shell';
import UpdateNotice from './UpdateNotice';
import UpdateSettings from './panel/UpdateSettings';
import { useUpdateStore } from '../store/useUpdateStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { type AppSettings, Invokes } from './ui/AppProperties';

vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.1.0-alpha.3') }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key: string, args?: { version: string }) => key === 'updates.available' ? `Rawtonic ${args?.version} is available` : key }) }));
const release = { version: '0.1.0-alpha.4', releaseUrl: 'https://github.com/rawtonic-app/rawtonic/releases/tag/v0.1.0-alpha.4', appImage: false };
beforeEach(() => {
  vi.clearAllMocks();
  useUpdateStore.setState({ update: null, checking: false, checked: false, failed: false, started: false });
  useSettingsStore.setState({ appSettings: {} as AppSettings });
  vi.mocked(invoke).mockImplementation(async (cmd) => cmd === Invokes.CheckUpdates ? release : undefined);
});
afterEach(cleanup);

it('checks startup once across remounts and opens only the release page on Download', async () => {
  const view = render(<UpdateNotice />);
  await act(async () => {});
  expect(screen.getByText('Rawtonic 0.1.0-alpha.4 is available')).toBeTruthy();
  expect(open).not.toHaveBeenCalled();
  fireEvent.click(screen.getByText('updates.download'));
  expect(open).toHaveBeenCalledWith(release.releaseUrl);
  view.unmount();
  render(<UpdateNotice />);
  await act(async () => {});
  expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === Invokes.CheckUpdates)).toEqual([[Invokes.CheckUpdates, { manual: false }]]);
});
it('dismisses the notice and persists exactly that version', async () => {
  render(<UpdateNotice />);
  await act(async () => {});
  fireEvent.click(screen.getByText('updates.notNow'));
  await act(async () => {});
  expect(screen.queryByRole('status')).toBeNull();
  expect(invoke).toHaveBeenCalledWith(Invokes.SaveSettings, { settings: { dismissedUpdateVersion: release.version } });
});
it('shows AppImageUpdate only for an AppImage result', async () => {
  vi.mocked(invoke).mockResolvedValue({ ...release, appImage: true });
  render(<UpdateNotice />);
  await act(async () => {});
  expect(screen.getByText('updates.appImage')).toBeTruthy();
});
it('stays silent when a startup check fails', async () => {
  vi.mocked(invoke).mockRejectedValue(new Error('offline'));
  render(<UpdateNotice />);
  await act(async () => {});
  expect(screen.queryByRole('status')).toBeNull();
});
it('defaults the preference on, saves opt-out, and permits a manual check while off', async () => {
  const onSettingsChange = vi.fn();
  const view = render(<UpdateSettings appSettings={{} as AppSettings} onSettingsChange={onSettingsChange} />);
  const toggle = screen.getByRole('checkbox', { name: 'updates.startup' });
  expect((toggle as HTMLInputElement).checked).toBe(true);
  fireEvent.click(toggle);
  expect(onSettingsChange).toHaveBeenCalledWith({ checkForUpdates: false });
  view.rerender(<UpdateSettings appSettings={{ checkForUpdates: false } as AppSettings} onSettingsChange={onSettingsChange} />);
  fireEvent.click(screen.getByText('updates.checkNow'));
  await act(async () => {});
  expect(invoke).toHaveBeenCalledWith(Invokes.CheckUpdates, { manual: true });
});

it('defaults pre-releases on for alpha builds and saves an explicit choice', async () => {
  const onSettingsChange = vi.fn();
  render(<UpdateSettings appSettings={{} as AppSettings} onSettingsChange={onSettingsChange} />);
  await act(async () => {});
  const toggle = screen.getByRole('checkbox', { name: 'updates.prereleases' }) as HTMLInputElement;
  expect(toggle.checked).toBe(true);
  fireEvent.click(toggle);
  expect(onSettingsChange).toHaveBeenCalledWith({ updatePrereleases: false });
});

it('a failed manual check never claims there is no newer release', async () => {
  vi.mocked(invoke).mockRejectedValue(new Error('offline'));
  render(<UpdateSettings appSettings={{} as AppSettings} onSettingsChange={vi.fn()} />);
  fireEvent.click(screen.getByText('updates.checkNow'));
  await act(async () => {});
  expect(screen.queryByText('updates.noUpdate')).toBeNull();
  expect(screen.getByText('updates.failed')).toBeTruthy();
});

it('dismissing a known newer release never claims there is no newer release', async () => {
  render(<><UpdateNotice /><UpdateSettings appSettings={{} as AppSettings} onSettingsChange={vi.fn()} /></>);
  await act(async () => {});
  fireEvent.click(screen.getByText('updates.checkNow'));
  await act(async () => {});
  fireEvent.click(screen.getByText('updates.notNow'));
  await act(async () => {});
  expect(screen.queryByText('updates.noUpdate')).toBeNull();
});

it('reports no newer release only after a successful empty manual result', async () => {
  vi.mocked(invoke).mockResolvedValue(null);
  render(<UpdateSettings appSettings={{} as AppSettings} onSettingsChange={vi.fn()} />);
  fireEvent.click(screen.getByText('updates.checkNow'));
  await act(async () => {});
  expect(screen.getByText('updates.noUpdate')).toBeTruthy();
  expect(screen.queryByText('updates.failed')).toBeNull();
});

it('waits for settings before offering a dismissal that must be persisted', async () => {
  useSettingsStore.setState({ appSettings: null });
  render(<UpdateNotice />);
  await act(async () => {});
  expect(screen.queryByText('updates.notNow')).toBeNull();
  await act(async () => { useSettingsStore.setState({ appSettings: {} as AppSettings }); });
  fireEvent.click(screen.getByText('updates.notNow'));
  await act(async () => {});
  expect(invoke).toHaveBeenCalledWith(Invokes.SaveSettings, { settings: { dismissedUpdateVersion: release.version } });
});
