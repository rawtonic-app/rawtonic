import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { albumSaves, PRESETS_WRITE_NAME, presetSaves } from './libraryFiles';
import { flushBeforeQuit } from '../utils/quit';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

const tree = (name: string) => [{ type: 'album', id: name, name, images: [] }];

beforeEach(() => {
  vi.clearAllMocks();
  albumSaves.reset();
  presetSaves.reset();
});

// The queue is registered with quitting (utils/quit.ts). This file imports
// nothing else that touches the quit registry, so the settings' own flush
// (registered at import) runs here too and resolves at once.
it('quitting runs the queues’ flushes: a pending save is waited for, a hung one reported', async () => {
  // `flushBeforeQuit` is what quitting runs (utils/quit.ts). It waits for
  // the album save queued here, then for the preset save, which never
  // finishes: that one is reported after its bound, the album save kept.
  const finishAlbums: Array<() => void> = [];
  vi.mocked(invoke).mockImplementation((command) =>
    command === Invokes.SaveAlbums
      ? new Promise((resolve) => finishAlbums.push(() => resolve(undefined)))
      : new Promise<string[]>(() => undefined)
  );
  // The queue reads `invoke` when it sends.
  albumSaves.reset();
  presetSaves.reset();
  void albumSaves.save(tree('Queued before quitting'));
  void presetSaves.save([{ preset: { id: 'p', name: 'P', adjustments: {} } }]);
  vi.useFakeTimers();
  let done: string[] | null = null;
  const quitting = flushBeforeQuit().then((failures) => (done = failures));
  try {
    await vi.advanceTimersByTimeAsync(0);
    // Waiting for the album save, not done yet.
    expect(invoke).toHaveBeenCalledWith(Invokes.SaveAlbums, { tree: tree('Queued before quitting') });
    expect(finishAlbums).toHaveLength(1);
    expect(done).toBeNull();
    finishAlbums.forEach((resolve) => resolve());
    await vi.advanceTimersByTimeAsync(10_000);
  } finally {
    vi.useRealTimers();
  }
  await quitting;
  expect(done).toEqual([PRESETS_WRITE_NAME]);
  expect(invoke).toHaveBeenCalledWith(Invokes.SaveAlbums, { tree: tree('Queued before quitting') });
});
