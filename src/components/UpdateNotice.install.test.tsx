// SPDX-License-Identifier: AGPL-3.0-or-later
// "Install update" in the update notice: available → downloading → ready to
// restart, the failures, the fallbacks and the guards before the restart.
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-shell';
import { relaunch } from '@tauri-apps/plugin-process';
import UpdateNotice from './UpdateNotice';
import { type UpdateInfo, useUpdateStore } from '../store/useUpdateStore';
import { relaunchAfterSettingsSaves, useSettingsStore } from '../store/useSettingsStore';
import { runningWork } from '../utils/quit';
import { type AppSettings, Invokes } from './ui/AppProperties';

type Progress = { payload: { version: string; downloaded: number; total: number | null } };
const progress: { handler: ((event: Progress) => void) | null } = { handler: null };

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (_name: string, handler: (event: Progress) => void) => {
    progress.handler = handler;
    return () => {
      progress.handler = null;
    };
  })
}));
vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/plugin-process', () => ({ relaunch: vi.fn().mockResolvedValue(undefined) }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));
vi.mock('../store/useSettingsStore', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../store/useSettingsStore')>()),
  relaunchAfterSettingsSaves: vi.fn()
}));
vi.mock('../utils/quit', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../utils/quit')>()),
  runningWork: vi.fn(() => [])
}));
vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    i18n: { language: 'en' },
    t: (key: string, args?: Record<string, unknown>) => (args ? `${key} ${JSON.stringify(args)}` : key)
  })
}));

const release: UpdateInfo = {
  version: '0.2.0',
  releaseUrl: 'https://github.com/rawtonic-app/rawtonic/releases/tag/v0.2.0',
  appImage: false,
  downloadUrl: 'https://github.com/rawtonic-app/rawtonic/releases/download/v0.2.0/Rawtonic_0.2.0_aarch64.dmg',
  assetName: 'Rawtonic_0.2.0_aarch64.dmg',
  assetSize: 47_000_000,
  notes: '',
  inApp: true,
  fallback: null
};

/** The commands' answers; a test replaces one to fail or hold it. */
let answers: Partial<Record<Invokes, () => Promise<unknown>>>;
const calls = (cmd: Invokes) => vi.mocked(invoke).mock.calls.filter(([c]) => c === cmd);

beforeEach(() => {
  vi.clearAllMocks();
  progress.handler = null;
  useUpdateStore.setState({
    update: null,
    checking: false,
    checked: false,
    failed: false,
    started: false,
    install: { phase: 'idle' },
    installVersion: null
  });
  useSettingsStore.setState({ appSettings: {} as AppSettings });
  answers = {
    [Invokes.CheckUpdates]: async () => release,
    [Invokes.UpdateDownload]: async () => undefined,
    [Invokes.UpdateInstall]: async () => undefined
  };
  vi.mocked(invoke).mockImplementation(async (cmd) => answers[cmd as Invokes]?.());
  vi.mocked(runningWork).mockReturnValue([]);
  vi.mocked(relaunchAfterSettingsSaves).mockImplementation(async (restart) => {
    await restart();
    return [];
  });
});
afterEach(cleanup);

async function shown() {
  render(<UpdateNotice />);
  await act(async () => {});
}

it('installs only after the click: download with progress, then Restart now installs and restarts', async () => {
  let finish: () => void = () => undefined;
  answers[Invokes.UpdateDownload] = () => new Promise<void>((resolve) => (finish = resolve));
  await shown();
  expect(screen.getByText('updates.inAppDetail')).toBeTruthy();
  // Nothing is fetched before the click.
  expect(calls(Invokes.UpdateDownload)).toEqual([]);
  fireEvent.click(screen.getByText('updates.install'));
  await act(async () => {});
  expect(calls(Invokes.UpdateDownload)).toEqual([[Invokes.UpdateDownload, { version: '0.2.0' }]]);
  act(() => progress.handler?.({ payload: { version: '0.2.0', downloaded: 23_500_000, total: 47_000_000 } }));
  expect(screen.getByText(/updates.downloadingOf .*"percent":50/)).toBeTruthy();
  expect((document.querySelector('progress') as HTMLProgressElement).value).toBe(23_500_000);
  // A stray event for another version changes nothing.
  act(() => progress.handler?.({ payload: { version: '0.3.0', downloaded: 1, total: 2 } }));
  expect(screen.getByText(/"percent":50/)).toBeTruthy();
  await act(async () => finish());
  expect(screen.getByText(/updates.ready .*0\.2\.0/)).toBeTruthy();
  expect(calls(Invokes.UpdateInstall)).toEqual([]);
  fireEvent.click(screen.getByText('updates.restartNow'));
  await act(async () => {});
  expect(relaunchAfterSettingsSaves).toHaveBeenCalledTimes(1);
  expect(calls(Invokes.UpdateInstall)).toEqual([[Invokes.UpdateInstall, { version: '0.2.0' }]]);
  expect(relaunch).toHaveBeenCalledTimes(1);
});

it('a failed download says why, leaves the app as it was and offers a retry and the browser', async () => {
  answers[Invokes.UpdateDownload] = async () => Promise.reject({ kind: 'network' });
  await shown();
  fireEvent.click(screen.getByText('updates.install'));
  await act(async () => {});
  expect(screen.getByRole('alert').textContent).toBe('updates.failure.network');
  fireEvent.click(screen.getByText('updates.downloadInstead'));
  expect(open).toHaveBeenCalledWith(release.downloadUrl);
  answers[Invokes.UpdateDownload] = async () => undefined;
  fireEvent.click(screen.getByText('updates.tryAgain'));
  await act(async () => {});
  expect(calls(Invokes.UpdateDownload)).toHaveLength(2);
  expect(screen.getByText('updates.restartNow')).toBeTruthy();
});

it('a bad signature is reported and nothing is installed', async () => {
  answers[Invokes.UpdateDownload] = async () => Promise.reject({ kind: 'signature' });
  await shown();
  fireEvent.click(screen.getByText('updates.install'));
  await act(async () => {});
  expect(screen.getByRole('alert').textContent).toBe('updates.failure.signature');
  expect(calls(Invokes.UpdateInstall)).toEqual([]);
});

it('Cancel stops the download and offers the update again', async () => {
  answers[Invokes.UpdateDownload] = () => new Promise(() => undefined);
  answers[Invokes.UpdateCancelDownload] = async () => undefined;
  await shown();
  fireEvent.click(screen.getByText('updates.install'));
  await act(async () => {});
  fireEvent.click(screen.getByText('updates.cancel'));
  await act(async () => {});
  expect(calls(Invokes.UpdateCancelDownload)).toHaveLength(1);
  expect(screen.getByText('updates.install')).toBeTruthy();
});

it('asks before a restart would stop running exports, and Keep Working installs nothing', async () => {
  vi.mocked(runningWork).mockReturnValue(['Web JPEGs (exporting)']);
  useUpdateStore.setState({ update: release, install: { phase: 'ready' }, installVersion: '0.2.0', started: true });
  const asked: { go?: () => void; cancel?: () => void; work?: string[] } = {};
  const ask = (work: string[], go: () => void, cancel: () => void) => Object.assign(asked, { work, go, cancel });
  const first = useUpdateStore.getState().restart(ask);
  await act(async () => {});
  expect(asked.work).toEqual(['Web JPEGs (exporting)']);
  asked.cancel?.();
  await act(async () => first);
  expect(relaunchAfterSettingsSaves).not.toHaveBeenCalled();
  expect(calls(Invokes.UpdateInstall)).toEqual([]);
  expect(useUpdateStore.getState().install.phase).toBe('ready');
  const second = useUpdateStore.getState().restart(ask);
  await act(async () => {});
  asked.go?.();
  await act(async () => second);
  expect(calls(Invokes.UpdateInstall)).toHaveLength(1);
});

it('edits that cannot be saved stop the restart before anything is installed', async () => {
  vi.mocked(relaunchAfterSettingsSaves).mockResolvedValue(['Edits of a.dng']);
  useUpdateStore.setState({ update: release, install: { phase: 'ready' }, installVersion: '0.2.0', started: true });
  await shown();
  fireEvent.click(screen.getByText('updates.restartNow'));
  await act(async () => {});
  expect(calls(Invokes.UpdateInstall)).toEqual([]);
  expect(relaunch).not.toHaveBeenCalled();
  expect(screen.getByRole('alert').textContent).toContain('Edits of a.dng');
});

it('a failed install keeps the downloaded update for another try', async () => {
  answers[Invokes.UpdateInstall] = async () => Promise.reject({ kind: 'disk' });
  useUpdateStore.setState({ update: release, install: { phase: 'ready' }, installVersion: '0.2.0', started: true });
  await shown();
  fireEvent.click(screen.getByText('updates.restartNow'));
  await act(async () => {});
  expect(screen.getByRole('alert').textContent).toBe('updates.failure.disk');
  expect(relaunch).not.toHaveBeenCalled();
  answers[Invokes.UpdateInstall] = async () => undefined;
  fireEvent.click(screen.getByText('updates.tryAgain'));
  await act(async () => {});
  // From the kept package: no second download.
  expect(calls(Invokes.UpdateDownload)).toEqual([]);
  expect(calls(Invokes.UpdateInstall)).toHaveLength(2);
  expect(relaunch).toHaveBeenCalledTimes(1);
});

it('Later hides a downloaded update without saving a dismissal', async () => {
  useUpdateStore.setState({ update: release, install: { phase: 'ready' }, installVersion: '0.2.0', started: true });
  await shown();
  fireEvent.click(screen.getByText('updates.later'));
  await act(async () => {});
  expect(screen.queryByRole('status')).toBeNull();
  expect(calls(Invokes.SaveSettings)).toEqual([]);
  // A manual check brings it back, still ready.
  await act(async () => useUpdateStore.getState().check(true));
  expect(screen.getByText('updates.restartNow')).toBeTruthy();
});

it('an all-users Windows copy links to its installer with a note, never Install update', async () => {
  answers[Invokes.CheckUpdates] = async () => ({
    ...release,
    inApp: false,
    fallback: 'allUsers',
    downloadUrl: 'https://github.com/rawtonic-app/rawtonic/releases/download/v0.2.0/Rawtonic_0.2.0_x64.msi',
    assetName: 'Rawtonic_0.2.0_x64.msi'
  });
  await shown();
  expect(screen.queryByText('updates.install')).toBeNull();
  expect(screen.getByText('updates.fallback.allUsers')).toBeTruthy();
  fireEvent.click(screen.getByText('updates.download'));
  expect(open).toHaveBeenCalledWith(
    'https://github.com/rawtonic-app/rawtonic/releases/download/v0.2.0/Rawtonic_0.2.0_x64.msi'
  );
  await act(async () => useUpdateStore.getState().download());
  expect(calls(Invokes.UpdateDownload)).toEqual([]);
});

it('a release without an update package keeps the plain download link and no note', async () => {
  answers[Invokes.CheckUpdates] = async () => ({ ...release, inApp: false, fallback: 'noManifest' });
  await shown();
  expect(screen.queryByText('updates.install')).toBeNull();
  expect(screen.queryByText(/updates.fallback/)).toBeNull();
  expect(screen.getByText('updates.download')).toBeTruthy();
});
