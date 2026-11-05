import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  libraryChanges,
  PRESETS_UNREAD_NAME,
  saveAlbums,
  setPresetsUnread,
  UNREPORTED_WRITE_NAME
} from './libraryFiles';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

const album = (id: string) => ({ type: 'album', id, name: id, images: [] });

/** The backend: `SaveAlbums` answers when the test says so. */
function backend() {
  const saves: Array<{ tree: ReturnType<typeof album>[]; answer: (() => void) | null }> = [];
  const flushes: unknown[][] = [];
  vi.mocked(invoke).mockImplementation(((command: Invokes, args: Record<string, unknown> = {}) => {
    if (command === Invokes.SaveAlbums)
      return new Promise<unknown>((resolve) =>
        saves.push({ tree: args.tree as ReturnType<typeof album>[], answer: () => resolve(undefined) })
      );
    if (command === Invokes.LibraryFlush) {
      flushes.push([]);
      return Promise.resolve([]);
    }
    return Promise.resolve([]);
  }) as typeof invoke);
  return { saves, flushes };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

beforeEach(() => {
  vi.clearAllMocks();
  libraryChanges.reset();
  setPresetsUnread(false);
});

it('sends every change one after another: the next waits for the earlier one', async () => {
  const { saves } = backend();
  void saveAlbums([album('First')]);
  void saveAlbums([album('Second')]);
  await settle();
  expect(saves.map(({ tree }) => tree[0].id)).toEqual(['First']);
  saves[0].answer!();
  await settle();
  expect(saves.map(({ tree }) => tree[0].id)).toEqual(['First', 'Second']);
});

it('a change is sent after an earlier one that failed: a failure never drops the next change', async () => {
  const refuse: Array<() => void> = [];
  const sent: string[] = [];
  vi.mocked(invoke).mockImplementation(((command: Invokes, args: Record<string, unknown> = {}) => {
    if (command !== Invokes.SaveAlbums) return Promise.resolve([]);
    sent.push((args.tree as ReturnType<typeof album>[])[0].id);
    if (sent.length === 1) return new Promise((_, reject) => refuse.push(() => reject('disk full')));
    return Promise.resolve(args.tree);
  }) as typeof invoke);
  const first = saveAlbums([album('Failing')]);
  const second = saveAlbums([album('After')]);
  await settle();
  expect(sent).toEqual(['Failing']);
  refuse[0]();
  await expect(first).rejects.toBe('disk full');
  await second;
  expect(sent).toEqual(['Failing', 'After']);
});

it('a flush sends the changes made so far, waits for them, then asks the backend', async () => {
  const { saves, flushes } = backend();
  void saveAlbums([album('Late')]);
  let flushed: string[] | null = null;
  const flushing = libraryChanges.flush().then((failures) => (flushed = failures));
  await settle();
  expect(flushed).toBeNull();
  saves[0].answer!();
  await flushing;
  expect(flushed).toEqual([]);
  expect(flushes).toHaveLength(1);
});

it('a flush answers with what the backend could not write, and with an unread preset list', async () => {
  backend();
  vi.mocked(invoke).mockImplementation(((command: Invokes) =>
    Promise.resolve(
      command === Invokes.LibraryFlush ? ['Album list could not be saved: disk full'] : []
    )) as typeof invoke);
  expect(await libraryChanges.flush()).toEqual(['Album list could not be saved: disk full']);
  // A presets.json that could not be read keeps its changes unsaved: every
  // quit, Save & Relaunch and library switch asks about it.
  setPresetsUnread(true);
  vi.mocked(invoke).mockImplementation(((command: Invokes) =>
    Promise.resolve(command === Invokes.LibraryFlush ? [] : [])) as typeof invoke);
  expect(await libraryChanges.flush()).toEqual([PRESETS_UNREAD_NAME]);
  setPresetsUnread(false);
  expect(await libraryChanges.flush()).toEqual([]);
});

it('a flush the backend does not answer reports it instead of waiting forever', async () => {
  vi.useFakeTimers();
  try {
    vi.mocked(invoke).mockImplementation(((command: Invokes) =>
      command === Invokes.LibraryFlush ? new Promise(() => undefined) : Promise.resolve([])) as typeof invoke);
    const flushing = libraryChanges.flush(5_000);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(await flushing).toEqual([UNREPORTED_WRITE_NAME]);
    // A backend that cannot be asked at all is reported too, not swallowed.
  } finally {
    vi.useRealTimers();
  }
  vi.mocked(invoke).mockRejectedValue('IPC closed');
  expect(await libraryChanges.flush()).toEqual([UNREPORTED_WRITE_NAME]);
});
