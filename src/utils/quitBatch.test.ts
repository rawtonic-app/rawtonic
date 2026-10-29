// Quitting during a long batch command (Auto, face retouch): each
// photograph the command writes is progress for the quit's save deadline,
// photo by photo - never on a timer - and each one not written is kept for
// the unsaved-changes prompt.
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { BatchCommand, COMMANDS, registerDrain, sent, turn, waitFor } from './quit.test.support';
import { handleQuitRequest, registerQuitFlush } from './quit';
import { flushRecipeSaves } from '../hooks/useEditorActions';
import { allPhotoWritesSettled } from './photoWrites';
import { Invokes } from '../components/ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));

const unregistered: Array<() => void> = [];
let open: BatchCommand | undefined;
beforeEach(() => {
  // The app's editor drain (its recipe flush, which waits for every queued
  // write), registered as the app registers it.
  unregistered.push(registerQuitFlush(async () => (await flushRecipeSaves()).map((path) => `Edits of ${path}`)));
});
afterEach(async () => {
  unregistered.splice(0).forEach((done) => done());
  vi.clearAllMocks();
  vi.useRealTimers();
  // A command a test leaves open is answered so nothing dangles.
  open?.finish(0);
  open = undefined;
  await allPhotoWritesSettled().catch(() => undefined);
});

/** A long Auto batch on `photos`, its write started and waiting for the
 * test's next photograph. */
async function autoBatch(photos: string[], kind = 'auto'): Promise<BatchCommand> {
  const command = new BatchCommand(photos, kind);
  open = command;
  await command.started();
  await turn();
  return command;
}

it('tells the backend it is saving after every photograph a long batch wrote', async () => {
  const command = await autoBatch(['/1.dng', '/2.dng', '/3.dng']);
  const progressAfter = vi.fn();
  // The command writes its photographs while the quit's drain listens: each
  // report is the quit's progress ("still saving"), no timer is.
  unregistered.push(
    registerDrain(registerQuitFlush, async () => {
      for (const [at, photo] of ['/1.dng', '/2.dng', '/3.dng'].entries()) {
        await command.next();
        // Each report crossed: the quit answered the backend's save signal
        // once more.
        await waitFor(() => sent(Invokes.QuitSaving) >= at + 2, `progress after ${photo}`);
        progressAfter(photo);
        await turn();
      }
      command.finish(3);
    }),
  );
  await handleQuitRequest(vi.fn());
  expect(progressAfter.mock.calls).toEqual([['/1.dng'], ['/2.dng'], ['/3.dng']]);
  expect(COMMANDS()).toEqual([...Array<number>(4).fill(0)].map(() => Invokes.QuitSaving).concat(Invokes.FinishQuit));
});

it('never counts a timer as progress', async () => {
  const command = await autoBatch(['/slow.dng']);
  const quit = handleQuitRequest(vi.fn());
  await waitFor(() => sent(Invokes.QuitSaving) === 1, 'the request');
  vi.useFakeTimers();
  for (let second = 0; second < 120; second += 1) await vi.advanceTimersByTimeAsync(1000);
  vi.useRealTimers();
  // Nothing reported: no second answer, though two minutes passed.
  expect(COMMANDS()).toEqual([Invokes.QuitSaving]);
  // Its photograph was written before it answered (its count holds its last
  // ones back to one a command may report later).
  command.finish(1);
  await quit;
  await command.opened;
  expect(COMMANDS().at(-1)).toBe(Invokes.FinishQuit);
});

it('counts a photograph written before the quit without failing it', async () => {
  const command = await autoBatch(['/a.dng']);
  await command.next();
  command.finish(1);
  await command.opened;
  await turn();
  await handleQuitRequest(vi.fn());
  await turn();
  expect(COMMANDS()).toEqual([Invokes.QuitSaving, Invokes.FinishQuit]);
});

it('reports a batch photograph that failed, though the batch goes on and ends without failing', async () => {
  const command = await autoBatch(['/x.dng', '/y.dng']);
  const ask = vi.fn();
  unregistered.push(
    // Its answer counts one written (the photograph it could not write).
    registerDrain(registerQuitFlush, () => command.drive(async (photo) => (photo === '/y.dng' ? 'disk full' : undefined), 1)),
  );
  await handleQuitRequest(ask);
  await turn();
  expect(ask).toHaveBeenCalledTimes(1);
  expect(ask.mock.calls[0][0]).toEqual(['y.dng (disk full)']);
  expect(COMMANDS().at(-1)).toBe(Invokes.CancelQuit);
});

it('counts a batch whose command failed as not written though it reported some, and keeps what it reported', async () => {
  const command = await autoBatch(['/m.dng', '/n.dng']);
  const ask = vi.fn();
  unregistered.push(
    // The test writes the first photograph; the command fails on the next.
    registerDrain(registerQuitFlush, async () => {
      await command.next();
      command.failWith('Auto adjustments failed: the photos are gone');
      await command.opened.catch(() => undefined);
    }),
  );
  await handleQuitRequest(ask);
  await turn();
  expect(ask).toHaveBeenCalledTimes(1);
  expect(ask.mock.calls[0][0]).toEqual(['n.dng (not written (Auto adjustments failed: the photos are gone))']);
});

it('reports a batch still running when the quit starts as far as it got', async () => {
  const command = await autoBatch(['/p.dng', '/q.dng']);
  // A photograph written a while ago (reported before the quit), the next
  // not yet - and the command still writing it when the quit starts: the
  // quit counts what it heard (the drain waits for the command).
  await command.next();
  await turn();
  const ask = vi.fn();
  unregistered.push(
    registerDrain(registerQuitFlush, () => {
      // (It writes its last one now, as a slow command may.)
      command.finish(2);
      return command.opened;
    }),
  );
  await handleQuitRequest(ask);
  await turn();
  expect(ask).toHaveBeenCalledTimes(1);
  expect(ask.mock.calls[0][0]).toEqual(['Auto adjustments not finished (1 of 2 photos saved)']);
});
