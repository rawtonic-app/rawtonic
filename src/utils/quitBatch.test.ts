// Real per-photo bridge events must keep an active quit alive, retain failures,
// and never let it finish while the actual command is still pending.
import '../i18n';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { BatchCommand, COMMANDS, mountAppListeners } from './quit.test.support';
import { QUIT_NOTICE_ID, QUIT_NOTICE_MS, handleQuitRequest, registerQuitFlush } from './quit';
import { toast } from 'react-toastify';
import { allPhotoWritesSettled, photoBatches } from './photoWrites';
import { debouncedSave, flushRecipeSaves } from '../hooks/useEditorActions';
import { INITIAL_ADJUSTMENTS } from './adjustments';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn((name) => Promise.resolve(['quit_saving', 'cancel_quit', 'finish_quit'].includes(String(name)) ? true : [])) }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));
vi.mock('react-toastify', () => ({ toast: Object.assign(vi.fn(), { info: vi.fn(), update: vi.fn(), dismiss: vi.fn(), error: vi.fn() }) }));
const callbacks = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn((name, callback) => {
  callbacks.set(name, callback);
  return Promise.resolve(() => { callbacks.delete(name); });
}) }));

let unmount: () => void;
let unregister: () => void;
let nextRequest = 100;
const commands: BatchCommand[] = [];
const quits: Promise<void>[] = [];
const progress = () => COMMANDS().filter((name) => name === Invokes.QuitSaving).length;
const quit = (ask = vi.fn()) => {
  const pending = handleQuitRequest(ask, ++nextRequest);
  quits.push(pending);
  return pending;
};
const batch = async (paths: string[]) => {
  const command = new BatchCommand(paths);
  commands.push(command);
  await command.started;
  return command;
};

beforeEach(async () => {
  unmount = await mountAppListeners();
  vi.mocked(invoke).mockClear();
  unregister = registerQuitFlush(async () => (await flushRecipeSaves()).map((path) => `Edits of ${path}`));
});
afterEach(async () => {
  vi.useRealTimers();
  for (const command of commands.splice(0)) if (!command.answered) command.finish();
  await allPhotoWritesSettled();
  await Promise.all(quits.splice(0));
  unregister();
  unmount();
  await new Promise((resolve) => setTimeout(resolve, 0));
  // Test isolation only: the production registry deliberately retains history.
  photoBatches().clear();
});

it('reports real progress after every photo, separately from the whole write settling', async () => {
  const command = await batch(['/1.dng', '/2.dng', '/3.dng']);
  const done = quit();
  await vi.waitFor(() => expect(progress()).toBe(1));
  for (const [index, path] of command.paths.entries()) {
    command.photo(path);
    expect(progress()).toBe(index + 2);
  }
  expect(COMMANDS()).not.toContain(Invokes.FinishQuit);
  command.finish();
  await done;
  expect(progress()).toBe(5); // acknowledgement, three photos, whole write
  expect(COMMANDS().at(-1)).toBe(Invokes.FinishQuit);
});

it('does not turn elapsed time into save progress', async () => {
  const command = await batch(['/slow.dng']);
  const done = quit();
  await vi.waitFor(() => expect(progress()).toBe(1));
  vi.useFakeTimers();
  await vi.advanceTimersByTimeAsync(120_000);
  expect(COMMANDS()).toEqual([Invokes.QuitSaving]);
  vi.useRealTimers();
  command.photo('/slow.dng');
  command.finish();
  await done;
});

it('does not count photos saved before this quit as failures or new progress', async () => {
  const command = await batch(['/a.dng']);
  command.photo('/a.dng');
  command.finish();
  await allPhotoWritesSettled();
  const ask = vi.fn();
  await quit(ask);
  expect(ask).not.toHaveBeenCalled();
  expect(COMMANDS()).toEqual([Invokes.QuitSaving, Invokes.FinishQuit]);
});

it('does not name an editor save its retry during the quit wrote', async () => {
  const errorLog = vi.spyOn(console, 'error').mockImplementation(() => {});
  const failOnce = vi.mocked(invoke).getMockImplementation()!;
  const saves: unknown[] = [];
  vi.mocked(invoke).mockImplementation((name, args) => {
    if (name !== Invokes.SaveMetadataAndUpdateThumbnail) return failOnce(name, args);
    saves.push((args as { path: string }).path);
    return saves.length === 1 ? Promise.reject(new Error('locked')) : failOnce(name, args);
  });
  debouncedSave('/a.dng', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
  const ask = vi.fn();
  // The quit flushes the save, which fails; the flush's retry writes it.
  await quit(ask);
  // (This file registers the recipe flush once more, so it may retry twice.)
  expect(saves.length).toBeGreaterThanOrEqual(2);
  expect(ask).not.toHaveBeenCalled();
  expect(COMMANDS().at(-1)).toBe(Invokes.FinishQuit);
  vi.mocked(invoke).mockImplementation(failOnce);
  errorLog.mockRestore();
});

it('does not ask again about a batch that failed and settled before this quit', async () => {
  // Its failure was told when it happened; quitting would lose nothing
  // that staying open could still save.
  const command = await batch(['/old.dng', '/ok.dng']);
  command.photo('/old.dng', 'disk full');
  command.photo('/ok.dng');
  command.finish();
  await allPhotoWritesSettled();
  const ask = vi.fn();
  await quit(ask);
  expect(ask).not.toHaveBeenCalled();
  expect(COMMANDS().at(-1)).toBe(Invokes.FinishQuit);
});

it('retains a failed photo even when its batch command resolves successfully', async () => {
  const command = await batch(['/x.dng', '/y.dng']);
  const ask = vi.fn();
  const done = quit(ask);
  await vi.waitFor(() => expect(progress()).toBe(1));
  command.photo('/x.dng');
  command.photo('/y.dng', 'disk full');
  command.finish();
  await done;
  expect(ask).toHaveBeenCalledOnce();
  expect(ask.mock.calls[0][0]).toEqual(['y.dng (disk full)']);
  expect(COMMANDS().at(-1)).toBe(Invokes.CancelQuit);
});

it('keeps the saved photo saved when the command fails before reporting the next', async () => {
  const command = await batch(['/m.dng', '/n.dng']);
  const ask = vi.fn();
  const done = quit(ask);
  await vi.waitFor(() => expect(progress()).toBe(1));
  command.photo('/m.dng');
  command.fail();
  await done;
  expect(ask).toHaveBeenCalledOnce();
  // The queue has no per-photo reason for an unreported photo; never invent it.
  expect(ask.mock.calls[0][0]).toEqual(['n.dng (not written)']);
});

it('waits for an already-running batch and does not fail its previously saved photo', async () => {
  const command = await batch(['/p.dng', '/q.dng']);
  command.photo('/p.dng');
  const ask = vi.fn();
  const done = quit(ask);
  await vi.waitFor(() => expect(progress()).toBe(1));
  expect(ask).not.toHaveBeenCalled();
  expect(COMMANDS()).not.toContain(Invokes.FinishQuit);
  command.photo('/q.dng');
  command.finish();
  await done;
  expect(ask).not.toHaveBeenCalled();
  expect(COMMANDS().at(-1)).toBe(Invokes.FinishQuit);
});

it('says it is saving once a quit waits, with how far the batch got, and clears that when done', async () => {
  const command = await batch(['/1.dng', '/2.dng']);
  vi.useFakeTimers();
  const done = quit();
  await vi.advanceTimersByTimeAsync(QUIT_NOTICE_MS - 1);
  expect(toast.info).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1);
  expect(toast.info).toHaveBeenCalledWith('Saving changes before Rawtonic quits… (0/2)', expect.objectContaining({ toastId: QUIT_NOTICE_ID }));
  vi.useRealTimers();
  command.photo('/1.dng');
  expect(toast.update).toHaveBeenLastCalledWith(QUIT_NOTICE_ID, { render: 'Saving changes before Rawtonic quits… (1/2)' });
  expect(toast.dismiss).not.toHaveBeenCalled();
  command.photo('/2.dng');
  command.finish();
  await done;
  expect(toast.dismiss).toHaveBeenCalledWith(QUIT_NOTICE_ID);
  expect(COMMANDS().at(-1)).toBe(Invokes.FinishQuit);
});
