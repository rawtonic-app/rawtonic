import { afterEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { flushBeforeQuit, handleQuitRequest, registerQuitFlush } from './quit';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));

const unregister: Array<() => void> = [];
const register = (flush: () => Promise<string[]>) => unregister.push(registerQuitFlush(flush));
const commands = () => vi.mocked(invoke).mock.calls.map(([command]) => command);

afterEach(() => {
  unregister.splice(0).forEach((done) => done());
  vi.clearAllMocks();
});

it('quits only after every pending save finished', async () => {
  let finishSave!: (failures: string[]) => void;
  register(() => new Promise((resolve) => { finishSave = resolve; }));
  const ask = vi.fn();
  const quitting = handleQuitRequest(ask);
  await Promise.resolve();
  await Promise.resolve();
  expect(commands()).toEqual([Invokes.QuitSaving]);
  finishSave([]);
  await quitting;
  expect(commands()).toEqual([Invokes.QuitSaving, Invokes.FinishQuit]);
  expect(ask).not.toHaveBeenCalled();
});

it('keeps the app open and lets the user decide when a save failed', async () => {
  register(async () => ['Edits of a.dng']);
  register(async () => { throw new Error('disk full'); });
  register(async () => []);
  const ask = vi.fn();
  await handleQuitRequest(ask);
  expect(commands()).toEqual([Invokes.QuitSaving, Invokes.CancelQuit]);
  expect(ask).toHaveBeenCalledWith(['Edits of a.dng', 'Error: disk full'], expect.any(Function));
  ask.mock.calls[0][1]();
  expect(commands()).toEqual([Invokes.QuitSaving, Invokes.CancelQuit, Invokes.FinishQuit]);
});

it('runs each flush once for a save that keeps failing, and never quits without the user\'s choice', async () => {
  const failing = vi.fn(async () => ['Edits of a.dng']);
  const saved = vi.fn(async () => []);
  register(failing);
  register(saved);
  const ask = vi.fn();
  await handleQuitRequest(ask);
  // Its failures are not pending work: no flush is run again.
  expect(failing).toHaveBeenCalledTimes(1);
  expect(saved).toHaveBeenCalledTimes(1);
  expect(commands()).toEqual([Invokes.QuitSaving, Invokes.CancelQuit]);
  expect(ask).toHaveBeenCalledTimes(1);
  expect(ask.mock.calls[0][0]).toEqual(['Edits of a.dng']);
  // Stayed open: nothing asked for more saves or quit on its own.
  await new Promise((done) => setTimeout(done, 0));
  expect(failing).toHaveBeenCalledTimes(1);
  expect(commands()).toEqual([Invokes.QuitSaving, Invokes.CancelQuit]);
  // The user decides: quit anyway.
  ask.mock.calls[0][1]();
  await new Promise((done) => setTimeout(done, 0));
  expect(commands()).toEqual([Invokes.QuitSaving, Invokes.CancelQuit, Invokes.FinishQuit]);
});

it('ignores a second request while the first one saves', async () => {
  let finishSave!: (failures: string[]) => void;
  const flush = vi.fn(() => new Promise<string[]>((resolve) => { finishSave = resolve; }));
  register(flush);
  const first = handleQuitRequest(vi.fn());
  await handleQuitRequest(vi.fn());
  await Promise.resolve();
  await Promise.resolve();
  expect(flush).toHaveBeenCalledTimes(1);
  finishSave([]);
  await first;
  expect(commands().filter((command) => command === Invokes.FinishQuit)).toHaveLength(1);
});

it('no longer runs unregistered flushes', async () => {
  const flush = vi.fn(async () => ['x']);
  registerQuitFlush(flush)();
  expect(await flushBeforeQuit()).toEqual([]);
  expect(flush).not.toHaveBeenCalled();
});

it('tells the backend it is still saving each time a queued photo write settles', async () => {
  const { submitPhotoWrite } = await import('./photoWrites');
  let finishBatch!: () => void;
  const batch = submitPhotoWrite(['/a.dng'], () => new Promise<void>((resolve) => { finishBatch = resolve; }), { kind: 'auto' });
  const save = submitPhotoWrite(['/a.dng'], async () => {}, { kind: 'recipe' });
  register(async () => { await save; return []; });
  const quitting = handleQuitRequest(vi.fn());
  await Promise.resolve();
  expect(commands()).toEqual([Invokes.QuitSaving]);
  finishBatch();
  await batch;
  await quitting;
  // Twice more: the batch and the save behind it, each progress; then quit.
  expect(commands()).toEqual([Invokes.QuitSaving, Invokes.QuitSaving, Invokes.QuitSaving, Invokes.FinishQuit]);
});
