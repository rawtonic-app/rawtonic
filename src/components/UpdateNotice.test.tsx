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
vi.mock('react-i18next', () => ({ useTranslation: () => ({ i18n: { language: 'en' }, t: (key: string, args?: Record<string, unknown>) => key === 'updates.available' ? `Rawtonic ${args?.version} is available` : key === 'updates.fileWithSize' ? `${args?.file} (${args?.size} MB)` : key }) }));
const release = {
  version: '0.1.0-alpha.4',
  releaseUrl: 'https://github.com/rawtonic-app/rawtonic/releases/tag/v0.1.0-alpha.4',
  appImage: false,
  downloadUrl: 'https://github.com/rawtonic-app/rawtonic/releases/download/v0.1.0-alpha.4/Rawtonic_0.1.0-alpha.4_aarch64.dmg',
  assetName: 'Rawtonic_0.1.0-alpha.4_aarch64.dmg',
  assetSize: 412_345_678,
  notes: '## Fixed\n\n- **Masks** show. ![x](https://example.com/t.png) [More](https://github.com/rawtonic-app/rawtonic)',
};
beforeEach(() => {
  vi.clearAllMocks();
  useUpdateStore.setState({ update: null, checking: false, checked: false, failed: false, started: false });
  useSettingsStore.setState({ appSettings: {} as AppSettings });
  vi.mocked(invoke).mockImplementation(async (cmd) => cmd === Invokes.CheckUpdates ? release : undefined);
});
afterEach(cleanup);

it('checks startup once across remounts and Download opens this computer\'s installer in the browser', async () => {
  const view = render(<UpdateNotice />);
  await act(async () => {});
  expect(screen.getByText('Rawtonic 0.1.0-alpha.4 is available')).toBeTruthy();
  expect(screen.getByText('Rawtonic_0.1.0-alpha.4_aarch64.dmg (412 MB)')).toBeTruthy();
  expect(open).not.toHaveBeenCalled();
  fireEvent.click(screen.getByText('updates.download'));
  expect(open).toHaveBeenCalledWith(release.downloadUrl);
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

it('offers the release page when the release has no file for this computer', async () => {
  vi.mocked(invoke).mockResolvedValue({ ...release, downloadUrl: null, assetName: null, assetSize: null });
  render(<UpdateNotice />);
  await act(async () => {});
  expect(screen.queryByText('updates.download')).toBeNull();
  expect(screen.getByText('updates.noFile')).toBeTruthy();
  fireEvent.click(screen.getByText('updates.releasePage'));
  expect(open).toHaveBeenCalledWith(release.releaseUrl);
});

it("What's new shows the fetched notes without loading images, links open in the browser", async () => {
  render(<UpdateNotice />);
  await act(async () => {});
  fireEvent.click(screen.getByText('updates.whatsNew'));
  const dialog = screen.getByRole('dialog');
  expect(dialog.querySelector('h3')?.textContent).toBe('Fixed');
  expect(dialog.querySelector('strong')?.textContent).toBe('Masks');
  expect(dialog.querySelector('img')).toBeNull();
  expect(dialog.innerHTML).not.toContain('example.com');
  fireEvent.click(screen.getByText('More'));
  expect(open).toHaveBeenCalledWith('https://github.com/rawtonic-app/rawtonic');
  fireEvent.click(screen.getByText('updates.releasePage'));
  expect(open).toHaveBeenLastCalledWith(release.releaseUrl);
  fireEvent.click(screen.getByLabelText('updates.close'));
  expect(screen.queryByRole('dialog')).toBeNull();
  // The notice stays until Download or Not now.
  expect(screen.getByText('updates.notNow')).toBeTruthy();
});

it('defaults "What\'s new after an update" on and saves the opt-out', () => {
  const onSettingsChange = vi.fn();
  render(<UpdateSettings appSettings={{} as AppSettings} onSettingsChange={onSettingsChange} />);
  const toggle = screen.getByRole('checkbox', { name: 'updates.showWhatsNew' }) as HTMLInputElement;
  expect(toggle.checked).toBe(true);
  fireEvent.click(toggle);
  expect(onSettingsChange).toHaveBeenCalledWith({ showWhatsNew: false });
});
