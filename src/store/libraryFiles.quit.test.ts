import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { libraryChanges } from './libraryFiles';
import { flushBeforeQuit } from '../utils/quit';
import { registerAppWrites } from '../utils/appWrites';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

const tree = (name: string) => [{ type: 'album', id: name, name, images: [] }];

/** Unregisters the app's queues; quitting runs them as `main.tsx` has. */
let unregisterAppWrites: () => void = () => undefined;

beforeEach(() => {
  vi.clearAllMocks();
  libraryChanges.reset();
  unregisterAppWrites();
  unregisterAppWrites = registerAppWrites();
});

// The queue is registered with quitting (utils/quit.ts): quitting runs a
// `library_flush` after every change sent, and the backend answers with what
// it could not write.
it('quitting waits for the album and preset changes through one library flush', async () => {
  const finishAlbums: Array<() => void> = [];
  let flushes = 0;
  vi.mocked(invoke).mockImplementation(((command: Invokes) => {
    if (command === Invokes.SaveAlbums)
      return new Promise<undefined>((resolve) => finishAlbums.push(() => resolve(undefined)));
    if (command === Invokes.LibraryFlush) {
      flushes += 1;
      return Promise.resolve(['Preset list could not be saved: disk full']);
    }
    return Promise.resolve(undefined);
  }) as typeof invoke);
  void libraryChanges.enqueue(() => invoke(Invokes.SaveAlbums, { tree: tree('Queued before quitting') }));
  vi.useFakeTimers();
  let done: string[] | null = null;
  const quitting = flushBeforeQuit().then((failures) => (done = failures));
  try {
    await vi.advanceTimersByTimeAsync(0);
    // Waiting for the album save, not done yet.
    expect(finishAlbums).toHaveLength(1);
    expect(done).toBeNull();
    finishAlbums.forEach((resolve) => resolve());
    await vi.advanceTimersByTimeAsync(0);
  } finally {
    vi.useRealTimers();
  }
  await quitting;
  expect(done).toEqual(['Preset list could not be saved: disk full']);
  expect(flushes).toBe(1);
  const calls = vi.mocked(invoke).mock.calls.map(([command]) => command);
  expect(calls.indexOf(Invokes.SaveAlbums)).toBeLessThan(calls.indexOf(Invokes.LibraryFlush));
});
