// SPDX-License-Identifier: AGPL-3.0-or-later
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { getVersion } from '@tauri-apps/api/app';
import WhatsNewAfterUpdate from './WhatsNewAfterUpdate';
import { useSettingsStore } from '../store/useSettingsStore';
import { type AppSettings, Invokes } from './ui/AppProperties';

vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));
vi.mock('react-i18next', () => ({ useTranslation: () => ({ t: (key: string, args?: { version: string }) => (args ? `${key} ${args.version}` : key) }) }));

const saved = () => vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === Invokes.SaveSettings).map(([, args]) => (args as { settings: AppSettings }).settings);

async function start(version: string, settings: AppSettings) {
  vi.mocked(getVersion).mockResolvedValue(version);
  useSettingsStore.setState({ appSettings: settings });
  render(<WhatsNewAfterUpdate />);
  // getVersion, the settings save and the CHANGELOG chunk.
  await act(() => new Promise((resolve) => setTimeout(resolve, 200)));
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(invoke).mockResolvedValue(undefined);
});
afterEach(cleanup);

it('a first install records the version and shows nothing', async () => {
  await start('0.1.0-alpha.7.1', {} as AppSettings);
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(saved()).toEqual([{ lastRunVersion: '0.1.0-alpha.7.1' }]);
});

it('the first start of a newer version shows its bundled CHANGELOG section once', async () => {
  await start('0.1.0-alpha.7.1', { lastRunVersion: '0.1.0-alpha.7', theme: 'dark' } as AppSettings);
  const dialog = await screen.findByRole('dialog');
  expect(screen.getByText('updates.notesTitle 0.1.0-alpha.7.1')).toBeTruthy();
  expect(dialog.textContent).toContain('AI masks show on the first open of a photo.');
  expect(dialog.textContent).not.toContain('The seventh alpha');
  expect(saved()).toEqual([{ lastRunVersion: '0.1.0-alpha.7.1', theme: 'dark' }]);
  fireEvent.click(screen.getAllByText('updates.close').at(-1)!);
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(saved()).toHaveLength(1);
  cleanup();
  // The next start of the same version.
  await start('0.1.0-alpha.7.1', useSettingsStore.getState().appSettings!);
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(saved()).toHaveLength(1);
});

it('"Don\'t show after updates" turns the preference off', async () => {
  await start('0.1.0-alpha.7.1', { lastRunVersion: '0.1.0-alpha.6.1' } as AppSettings);
  await screen.findByRole('dialog');
  fireEvent.click(screen.getByLabelText('updates.dontShowAgain'));
  fireEvent.click(screen.getAllByText('updates.close').at(-1)!);
  await act(async () => {});
  expect(saved().at(-1)).toEqual({ lastRunVersion: '0.1.0-alpha.7.1', showWhatsNew: false });
});

it('stays quiet when turned off, after a downgrade and for a version without a section', async () => {
  for (const [version, settings] of [
    ['0.1.0-alpha.7.1', { lastRunVersion: '0.1.0-alpha.7', showWhatsNew: false }],
    ['0.1.0-alpha.7', { lastRunVersion: '0.1.0-alpha.7.1' }],
    ['9.0.0', { lastRunVersion: '0.1.0-alpha.7.1' }],
  ] as const) {
    vi.mocked(invoke).mockClear();
    await start(version, settings as AppSettings);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(saved().at(-1)?.lastRunVersion).toBe(version);
    cleanup();
  }
});
