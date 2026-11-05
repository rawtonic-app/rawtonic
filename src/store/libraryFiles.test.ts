import { beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { ALBUMS_WRITE_NAME, albumSaves, makeSaveQueue, PRESETS_WRITE_NAME, presetSaves } from './libraryFiles';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-os', () => ({ platform: () => 'macos' }));

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const tree = (name: string) => [{ type: 'album', id: name, name, images: [] }];

beforeEach(() => {
  vi.clearAllMocks();
  albumSaves.reset();
  presetSaves.reset();
});

it('sends a save at once, and the next only after the earlier one finished', async () => {
  const finish: Array<(value?: unknown) => void> = [];
  vi.mocked(invoke).mockImplementation(() => new Promise((resolve) => finish.push(resolve)));
  void albumSaves.save(tree('First'));
  expect(invoke).toHaveBeenCalledTimes(1);
  void albumSaves.save(tree('Second'));
  void albumSaves.save(tree('Third'));
  await flush();
  // The later snapshots wait: the backend writes them in this order.
  expect(invoke).toHaveBeenCalledTimes(1);
  finish[0]();
  await flush();
  expect(invoke).toHaveBeenCalledTimes(2);
  finish[1]();
  await flush();
  finish[2]();
  await flush();
  const sent = vi.mocked(invoke).mock.calls.map(([, args]) => (args as { tree: { name: string }[] }).tree[0].name);
  expect(sent).toEqual(['First', 'Second', 'Third']);
  expect(vi.mocked(invoke).mock.calls.every(([command]) => command === Invokes.SaveAlbums)).toBe(true);
});

it('sends a failed save’s successor anyway, and the last written save decides', async () => {
  const reject: Array<(reason: unknown) => void> = [];
  const finish: Array<(value?: unknown) => void> = [];
  vi.mocked(invoke).mockImplementation(
    () => new Promise((resolve, rejectFn) => (finish.push(resolve), reject.push(rejectFn)))
  );
  const first = albumSaves.save(tree('First'));
  const second = albumSaves.save(tree('Second'));
  await flush();
  reject[0]('disk full');
  await expect(first).rejects.toBe('disk full');
  await flush();
  expect(invoke).toHaveBeenCalledTimes(2);
  finish[1]();
  await second;
  // The newer save made disk equal to what is wanted: nothing is retried.
  expect(await albumSaves.flush()).toEqual([]);
  expect(invoke).toHaveBeenCalledTimes(2);
});

it('a flush waits for the pending saves and retries a failed one once', async () => {
  const finish: Array<(value?: unknown) => void> = [];
  vi.mocked(invoke).mockImplementation(() => new Promise((resolve) => finish.push(resolve)));
  void albumSaves.save(tree('Late'));
  let flushed: string[] | null = null;
  const flushing = albumSaves.flush().then((failures) => (flushed = failures));
  await flush();
  // Still being written: the quit has not been told everything is saved.
  expect(flushed).toBeNull();
  finish[0]();
  await flushing;
  expect(flushed).toEqual([]);

  vi.mocked(invoke).mockRejectedValue('disk full');
  await expect(presetSaves.save([{ preset: { id: 'p', name: 'P', adjustments: {} } }])).rejects.toBe('disk full');
  expect(await presetSaves.flush()).toEqual([PRESETS_WRITE_NAME]);
  // Written once more, and still failing: reported, not retried forever.
  expect(invoke).toHaveBeenCalledTimes(3);
  expect(await presetSaves.flush()).toEqual([PRESETS_WRITE_NAME]);
  expect(invoke).toHaveBeenCalledTimes(4);
  // A retry that succeeds leaves nothing unsaved.
  vi.mocked(invoke).mockResolvedValue(undefined);
  expect(await presetSaves.flush()).toEqual([]);
});

it('a flush sends what the caller still holds back first', async () => {
  vi.mocked(invoke).mockResolvedValue(undefined);
  const beforeFlush = vi.fn();
  const queue = makeSaveQueue('presets', () => invoke<void>(Invokes.SavePresets, {}), { beforeFlush });
  await queue.flush();
  expect(beforeFlush).toHaveBeenCalledTimes(1);
  queue.reset();
});

it('a hung save is reported after the flush’s bound', async () => {
  vi.useFakeTimers();
  try {
    vi.mocked(invoke).mockImplementation(() => new Promise(() => undefined));
    void albumSaves.save(tree('Stuck'));
    const flushing = albumSaves.flush(5_000);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(await flushing).toEqual([ALBUMS_WRITE_NAME]);
  } finally {
    vi.useRealTimers();
  }
});
