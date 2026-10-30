import { afterEach, expect, it, vi } from 'vitest';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useUIStore } from '../store/useUIStore';
import { invoke } from '@tauri-apps/api/core';
import { cancelQuitRequest, confirmQuitWithUnsaved, flushBeforeQuit, handleQuitRequest, registerQuitFlush } from './quit';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve(true)) }));
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
  const quitting = handleQuitRequest(ask, 1);
  await vi.waitFor(() => expect(finishSave).toBeTypeOf('function'));
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
  await handleQuitRequest(ask, 1);
  expect(commands()).toEqual([Invokes.QuitSaving, Invokes.CancelQuit]);
  expect(ask).toHaveBeenCalledWith(['Edits of a.dng', 'Error: disk full'], expect.any(Function), expect.any(Function));
  ask.mock.calls[0][1]();
  expect(commands()).toEqual([Invokes.QuitSaving, Invokes.CancelQuit, Invokes.FinishQuit]);
});

it('runs each flush once for a save that keeps failing, and never quits without the user\'s choice', async () => {
  const failing = vi.fn(async () => ['Edits of a.dng']);
  const saved = vi.fn(async () => []);
  register(failing);
  register(saved);
  const ask = vi.fn();
  await handleQuitRequest(ask, 1);
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
  const first = handleQuitRequest(vi.fn(), 1);
  await handleQuitRequest(vi.fn(), 1);
  await vi.waitFor(() => expect(flush).toHaveBeenCalledTimes(1));
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
  const quitting = handleQuitRequest(vi.fn(), 1);
  await Promise.resolve();
  expect(commands()).toEqual([Invokes.QuitSaving]);
  finishBatch();
  await batch;
  await quitting;
  // Twice more: the batch and the save behind it, each progress; then quit.
  expect(commands()).toEqual([Invokes.QuitSaving, Invokes.QuitSaving, Invokes.QuitSaving, Invokes.FinishQuit]);
});


it('keeps a cancelled request from quitting when its pending save finally finishes', async () => {
  let finish!: (failures: string[]) => void;
  const flush = vi.fn(() => new Promise<string[]>((resolve) => { finish = resolve; }));
  register(flush);
  const quitting = handleQuitRequest(vi.fn(), 10);
  await vi.waitFor(() => expect(flush).toHaveBeenCalledOnce());
  expect(cancelQuitRequest(10)).toBe(true);
  finish([]);
  await quitting;
  expect(commands()).not.toContain(Invokes.FinishQuit);
});

it('does not let an old cancellation or handler finally clear a newer request', async () => {
  const finishes: Array<(failures: string[]) => void> = [];
  const flush = vi.fn(() => new Promise<string[]>((resolve) => finishes.push(resolve)));
  register(flush);
  const old = handleQuitRequest(vi.fn(), 20);
  await vi.waitFor(() => expect(finishes).toHaveLength(1));
  cancelQuitRequest(20);
  const current = handleQuitRequest(vi.fn(), 21);
  await vi.waitFor(() => expect(finishes).toHaveLength(2));
  finishes[0]([]);
  await old;
  expect(cancelQuitRequest(20)).toBe(false);
  await handleQuitRequest(vi.fn(), 22);
  expect(flush).toHaveBeenCalledTimes(2);
  finishes[1]([]);
  await current;
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === Invokes.FinishQuit))
    .toEqual([[Invokes.FinishQuit, { requestId: 21, force: false }]]);
});

it('authorizes only the current explicit decision, and cancel invalidates its confirm callback', async () => {
  register(async () => ['unsaved']);
  const oldAsk = vi.fn();
  await handleQuitRequest(oldAsk, 30);
  const ask = vi.fn();
  await handleQuitRequest(ask, 31);
  oldAsk.mock.calls[0][1]();
  expect(commands()).not.toContain(Invokes.FinishQuit);
  ask.mock.calls[0][2]();
  ask.mock.calls[0][1]();
  expect(commands()).not.toContain(Invokes.FinishQuit);
  expect(vi.mocked(invoke).mock.calls.at(-1))
    .toEqual([Invokes.CancelQuit, { requestId: 31, allowQuitAnyway: false }]);
});

it('captures a synchronous flush throw as an unsaved change', async () => {
  register(() => { throw new Error('disk unavailable'); });
  const ask = vi.fn();
  await handleQuitRequest(ask, 40);
  expect(ask).toHaveBeenCalledWith(['Error: disk unavailable'], expect.any(Function), expect.any(Function));
  expect(vi.mocked(invoke).mock.calls.at(-1))
    .toEqual([Invokes.CancelQuit, { requestId: 40, allowQuitAnyway: true }]);
  ask.mock.calls[0][1]();
  expect(vi.mocked(invoke).mock.calls.at(-1))
    .toEqual([Invokes.FinishQuit, { requestId: 40, force: true }]);
});

it('does not drain or quit when the native acknowledgement rejects an expired request', async () => {
  const flush = vi.fn(async () => []);
  register(flush);
  vi.mocked(invoke).mockResolvedValueOnce(false);
  await handleQuitRequest(vi.fn(), 50);
  expect(flush).not.toHaveBeenCalled();
  expect(commands()).toEqual([Invokes.QuitSaving]);
});


it('cancels a dismissed confirmation, but closing after confirmation does not cancel the chosen quit', () => {
  vi.mocked(getCurrentWindow).mockReturnValue({
    unminimize: vi.fn(async () => {}), show: vi.fn(async () => {}), setFocus: vi.fn(async () => {}),
  } as unknown as ReturnType<typeof getCurrentWindow>);
  const quit = vi.fn();
  const cancel = vi.fn();
  confirmQuitWithUnsaved(['unsaved'], quit, cancel);
  const first = useUIStore.getState().confirmModalState;
  first.onCancel?.();
  first.onConfirm?.();
  expect(cancel).toHaveBeenCalledOnce();
  expect(quit).not.toHaveBeenCalled();
  confirmQuitWithUnsaved(['unsaved'], quit, cancel);
  const next = useUIStore.getState().confirmModalState;
  next.onConfirm?.();
  next.onCancel?.();
  expect(quit).toHaveBeenCalledOnce();
  expect(cancel).toHaveBeenCalledOnce();
});
