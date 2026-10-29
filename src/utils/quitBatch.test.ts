// Quitting during a long batch command (Auto adjustments, face retouch): each
// photograph the command writes is progress for the quit's save deadline,
// photo by photo - never on a timer - and each one it did not write is kept
// for the unsaved-changes prompt. The command runs beside the quit and is
// driven, finished or rejected by the test (quit.test.support); its reports
// reach the webview only through the app's own listener hook.
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import {
  BatchCommand,
  COMMANDS,
  freshQueue,
  mountAppListeners,
  registerDrain,
  sent,
  settleEverything,
  turn,
  waitFor,
} from './quit.test.support';
import { handleQuitRequest, registerQuitFlush } from './quit';
import { flushRecipeSaves } from '../hooks/useEditorActions';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));
// Tauri's event system, as src/hooks/useBatchWriteProgress.test.ts stands in
// for it: one callback per event name until its unlisten.
const callbacks = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, callback: (event: { payload: unknown }) => void) => {
    callbacks.set(name, callback);
    return Promise.resolve(() => callbacks.delete(name));
  }),
}));

// A command promise caught by the queue is no unhandled rejection in the app;
// Vitest's own report would fail the run for one (and it restores the
// handler between tests, so this one is put back in `beforeEach`).
const unhandled = (_reason: unknown) => {};
process.on('unhandledRejection', unhandled);

let unmount: () => Promise<void> = async () => {};
/** The editor drain: waits for the queued writes, then writes its recipes. */
let unregisterEditorDrain: () => void = () => {};
beforeEach(async () => {
  process.on('unhandledRejection', unhandled);
  await freshQueue();
  unmount = await mountAppListeners();
  unregisterEditorDrain = registerQuitFlush(async () => (await flushRecipeSaves()).map((path) => `Edits of ${path}`));
});
afterEach(async () => {
  process.off('unhandledRejection', unhandled);
  await unmount();
  unregisterEditorDrain();
  vi.clearAllMocks();
  vi.useRealTimers();
  // A command the test left unanswered is answered, so nothing waits in the
  // queue or runs into a later test: its batch settles with what it reported.
  await settleEverything();
});

/** A long Auto batch on `photos`, its write running and its command waiting
 * for the test's next photograph and answer. */
const autoBatch = (photos: string[], kind = 'auto') => BatchCommand.start(photos, kind);

/** The progress invokes sent so far (`quit_saving`). */
const progress = () => sent(Invokes.QuitSaving);

it('tells the backend it is saving after every photograph a long batch wrote', async () => {
  const command = await autoBatch(['/1.dng', '/2.dng', '/3.dng']);
  const progressAfter = vi.fn();
  registerDrain(registerQuitFlush, async () => {
    await turn();
    await turn();
    for (const [at, photo] of command.photos.entries()) {
      // The command finishes this photograph: src-tauri reports it, the
      // app's listener passes it on, and the quit answers the backend once
      // more.
      // (Two answers more: this photograph's, and the batch's write - which
      // settles once its command answers - is told separately below.)
      await waitFor(() => progress() >= at + 3, `progress after ${photo}`);
      progressAfter(photo);
    }
    await command.settle();
  });
  await handleQuitRequest(vi.fn());
  expect(progressAfter.mock.calls).toEqual([['/1.dng'], ['/2.dng'], ['/3.dng']]);
  expect(COMMANDS()).toEqual([...Array<number>(4).fill(0)].map(() => Invokes.QuitSaving).concat(Invokes.FinishQuit));
});

it('never counts a timer as progress', async () => {
  const command = await autoBatch(['/slow.dng']);
  const quit = handleQuitRequest(vi.fn());
  // (The drain waits for the command's write, which waits for its answer.)
  await waitFor(() => progress() === 1, 'the request');
  // Two minutes pass with no photograph finished: no second answer.
  vi.useFakeTimers();
  for (let second = 0; second < 120; second += 1) await vi.advanceTimersByTimeAsync(1000);
  vi.useRealTimers();
  expect(COMMANDS()).toEqual([Invokes.QuitSaving]);
  // Its photograph is finished and the command answers: then, and only
  // then, the quit goes on.
  await command.photo('/slow.dng');
  await command.settle();
  await quit;
  expect(COMMANDS()).toEqual([Invokes.QuitSaving, Invokes.QuitSaving, Invokes.FinishQuit]);
});

it('counts a photograph written before the quit without failing it', async () => {
  const command = await autoBatch(['/a.dng']);
  await command.photo('/a.dng');
  await command.settle();
  await handleQuitRequest(vi.fn());
  // (The report before the quit and the write settling during it: nothing
  // the prompt would list, so the app quits.)
  expect(COMMANDS()).toEqual([Invokes.QuitSaving, Invokes.QuitSaving, Invokes.FinishQuit]);
});

it('reports a batch photograph that failed, though the batch goes on and ends without failing', async () => {
  const command = await autoBatch(['/x.dng', '/y.dng']);
  const ask = vi.fn();
  registerDrain(registerQuitFlush, async () => {
    await command.photo('/x.dng');
    // The next photograph is not written (src-tauri's report says why); the
    // command answers with it as failed, as Auto's does: the saved photo of
    // the batch stays saved.
    await command.photo('/y.dng', 'disk full');
    await command.settle();
  });
  await handleQuitRequest(ask);
  await turn();
  expect(ask).toHaveBeenCalledTimes(1);
  expect(ask.mock.calls[0][0]).toEqual(['y.dng (disk full)']);
  expect(COMMANDS().at(-1)).toBe(Invokes.CancelQuit);
});

it('counts a batch whose command failed as not written though it reported some, and keeps what it reported', async () => {
  const command = await autoBatch(['/m.dng', '/n.dng']);
  const ask = vi.fn();
  registerDrain(registerQuitFlush, async () => {
    await command.photo('/m.dng');
    // The command fails outright (its SaveFailure names both photographs):
    // the one it reported stays saved, the other is not.
    await command.reject();
  });
  await handleQuitRequest(ask);
  await turn();
  expect(ask).toHaveBeenCalledTimes(1);
  expect(ask.mock.calls[0][0]).toEqual(['n.dng (not written (Auto adjustments failed))']);
});

it('reports a batch still running when the quit starts as far as it got', async () => {
  const command = await autoBatch(['/p.dng', '/q.dng']);
  // A photograph written a while ago (reported before the quit), the next
  // not yet - and the command still writing it when the quit starts: the
  // quit counts what it heard.
  await command.photo('/p.dng');
  const ask = vi.fn();
  registerDrain(registerQuitFlush, () => command.settle());
  await handleQuitRequest(ask);
  await turn();
  expect(ask).toHaveBeenCalledTimes(1);
  expect(ask.mock.calls[0][0]).toEqual(['Auto adjustments not finished (1 of 2 photos saved)']);
});

it('keeps the quit pending while a batch command runs, and saves what it wrote', async () => {
  const command = await autoBatch(['/r.dng', '/s.dng']);
  const ask = vi.fn();
  const quit = handleQuitRequest(ask);
  await waitFor(() => progress() === 1, 'the request');
  await command.photo('/r.dng');
  await waitFor(() => progress() === 2, 'the first photograph');
  // The command is still pending (its last photograph unwritten, its answer
  // not given): the quit must not settle it for the user - no prompt, no
  // quit.
  await turn();
  expect(ask).not.toHaveBeenCalled();
  expect(COMMANDS()).not.toContain(Invokes.FinishQuit);
  expect(command.batch).toMatchObject({ total: 2 });
  // Now the command settles: its written photographs are not failures.
  await command.photo('/s.dng');
  await command.settle();
  await quit;
  expect(ask).not.toHaveBeenCalled();
  expect(COMMANDS().at(-1)).toBe(Invokes.FinishQuit);
});
