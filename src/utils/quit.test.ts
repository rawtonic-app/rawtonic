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
