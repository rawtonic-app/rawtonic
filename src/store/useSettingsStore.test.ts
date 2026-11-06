import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { flushSettingsSaves, relaunchAfterSettingsSaves, saveSettings, SETTINGS_WRITE_NAME } from './useSettingsStore';
import { libraryChanges, PRESETS_UNREAD_NAME, saveAlbums, setPresetsUnread } from './libraryFiles';
import { AppSettings, Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

const snapshot = (theme: string) => ({ theme }) as unknown as AppSettings;
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

beforeEach(() => {
  vi.clearAllMocks();
  libraryChanges.reset();
  setPresetsUnread(false);
});

it('sends a save at once, and the next only after the earlier one finished', async () => {
  const finish: Array<(value?: unknown) => void> = [];
  const fail: Array<(reason: unknown) => void> = [];
  vi.mocked(invoke).mockImplementation(
    () =>
      new Promise((resolve, reject) => {
        finish.push(resolve);
        fail.push(reject);
      })
  );
  const first = saveSettings(snapshot('light'));
  expect(invoke).toHaveBeenCalledTimes(1);
  const second = saveSettings(snapshot('dark'));
  const third = saveSettings(snapshot('grey'));
  await flush();
  expect(invoke).toHaveBeenCalledTimes(1);
  // A failed save still lets the next one go, and reports its own failure.
  fail[0]('disk full');
  await expect(first).rejects.toBe('disk full');
  await flush();
  expect(invoke).toHaveBeenCalledTimes(2);
  finish[1]();
  await second;
  await flush();
  finish[2]();
  await third;
  expect(vi.mocked(invoke).mock.calls.map(([, args]) => (args as { settings: AppSettings }).settings.theme)).toEqual([
    'light',
    'dark',
    'grey'
  ]);
  expect(vi.mocked(invoke).mock.calls.every(([command]) => command === Invokes.SaveSettings)).toBe(true);
  // Nothing under way any more: the next save goes out at once.
  await flush();
  const fourth = saveSettings(snapshot('black'));
  expect(invoke).toHaveBeenCalledTimes(4);
  finish[3]();
  await fourth;
});

it('quitting waits for a delayed settings save', async () => {
  let finish!: (value?: unknown) => void;
  vi.mocked(invoke).mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      })
  );
  void saveSettings(snapshot('dark'));
  let flushed: string[] | null = null;
  const flushing = flushSettingsSaves().then((failures) => (flushed = failures));
  await flush();
  // Still writing: the quit has not been told everything is saved.
  expect(flushed).toBeNull();
  finish();
  await flushing;
  expect(flushed).toEqual([]);
});

it('quitting writes a failed settings save once more and reports it when that fails too', async () => {
  vi.mocked(invoke).mockRejectedValue('disk full');
  await expect(saveSettings(snapshot('dark'))).rejects.toBe('disk full');
  expect(await flushSettingsSaves()).toEqual([SETTINGS_WRITE_NAME]);
  expect(vi.mocked(invoke).mock.calls.map(([, args]) => (args as { settings: AppSettings }).settings.theme)).toEqual([
    'dark',
    'dark'
  ]);
  // The retry succeeds: nothing is left unsaved.
  vi.mocked(invoke).mockResolvedValue(undefined);
  expect(await flushSettingsSaves()).toEqual([]);
  expect(invoke).toHaveBeenCalledTimes(3);
  expect(await flushSettingsSaves()).toEqual([]);
  expect(invoke).toHaveBeenCalledTimes(3);
});

it('Save and Relaunch keeps the app open when the settings cannot be written', async () => {
  vi.mocked(invoke).mockRejectedValue('disk full');
  await expect(saveSettings(snapshot('dark'))).rejects.toBe('disk full');
  const relaunch = vi.fn(() => Promise.resolve());
  // The album and preset queue is asked too; it could not be asked here, so
  // it is reported as well and the app stays open.
  const failed = await relaunchAfterSettingsSaves(relaunch);
  expect(failed[0]).toBe(SETTINGS_WRITE_NAME);
  expect(relaunch).not.toHaveBeenCalled();
  // Once the disk takes it again, the retry writes it and the app relaunches.
  vi.mocked(invoke).mockImplementation(((command: Invokes) =>
    Promise.resolve(command === Invokes.QueueTicket ? 1 : [])) as typeof invoke);
  expect(await relaunchAfterSettingsSaves(relaunch)).toEqual([]);
  expect(relaunch).toHaveBeenCalledTimes(1);
});

it('Save and Relaunch waits for an album save and keeps the app open when it cannot be written', async () => {
  let finishAlbum!: (value: undefined) => void;
  let flushAnswer: string[] = [];
  vi.mocked(invoke).mockImplementation(((command: Invokes, args: Record<string, unknown> = {}) => {
    if (command === Invokes.SaveSettings) return Promise.resolve(undefined);
    if (command === Invokes.QueueTicket) return Promise.resolve(Number(args.ticket ?? 0) + 1);
    if (command === Invokes.SaveAlbums) return new Promise<undefined>((resolve) => (finishAlbum = resolve));
    if (command === Invokes.LibraryFlush) return Promise.resolve(flushAnswer);
    return Promise.resolve(undefined);
  }) as typeof invoke);
  // The album change is being written when Save & Relaunch is clicked.
  void saveAlbums([{ type: 'album', id: 'a', name: 'A', images: [] }]).catch(() => undefined);
  const relaunch = vi.fn(() => Promise.resolve());
  let flushed: string[] | null = null;
  const relaunching = relaunchAfterSettingsSaves(relaunch).then((failures) => (flushed = failures));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(flushed).toBeNull();
  expect(relaunch).not.toHaveBeenCalled();
  // The backend could not write it: the app stays open with the message, as
  // for a settings save, and the relaunch never ran.
  flushAnswer = ['Album list could not be saved: disk full'];
  finishAlbum(undefined);
  await relaunching;
  expect(flushed).toEqual(['Album list could not be saved: disk full']);
  expect(relaunch).not.toHaveBeenCalled();
  // Saved now: Save & Relaunch writes it and restarts the app.
  flushAnswer = [];
  expect(await relaunchAfterSettingsSaves(relaunch)).toEqual([]);
  expect(relaunch).toHaveBeenCalledTimes(1);
});

it('Save and Relaunch keeps the app open while preset changes cannot be written (unread presets.json)', async () => {
  vi.mocked(invoke).mockImplementation(((command: Invokes) =>
    Promise.resolve(command === Invokes.QueueTicket ? 1 : [])) as typeof invoke);
  const relaunch = vi.fn(() => Promise.resolve());
  setPresetsUnread(true);
  expect(await relaunchAfterSettingsSaves(relaunch)).toEqual([PRESETS_UNREAD_NAME]);
  expect(relaunch).not.toHaveBeenCalled();
});

it('quitting waits for a hung settings save only so long, then reports it', async () => {
  vi.useFakeTimers();
  try {
    vi.mocked(invoke).mockImplementation(() => new Promise(() => undefined));
    void saveSettings(snapshot('dark'));
    const flushing = flushSettingsSaves(5_000);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(await flushing).toEqual([SETTINGS_WRITE_NAME]);
  } finally {
    vi.useRealTimers();
  }
});
