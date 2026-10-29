// Helpers for the quit tests (quitBatch.test.ts): what the webview told the
// backend, a long batch command the test drives step by step as the app runs
// one, and the app's own listener hook mounted over a stand-in for Tauri's
// `listen`. A test support file, not app code.
//
// A batch command as the app runs one (Auto adjustments, face retouch): its
// write goes through the real queue (`submitPhotoWrite`, the command's kind,
// `batch: true`) and is given the batch's tag. src-tauri reports each
// photograph it finishes as the Tauri event `batch-write-settled`
// ({ batch, path, ok: null | string, also }, src-tauri/src/file_management/
// edits.rs); the app's `useTauriListeners` turns it into the webview's
// `rawtonic:batch-write-settled` ({ tag, path, ok?, also }), which quitting
// listens to. The test delivers each report as the backend delivers it, so
// only the real listener's normalization, filtering and per-photograph
// forwarding count a photograph for its batch and for the quit. Nothing
// settles on its own: the test delivers every photograph (`photo`) and
// answers the command (`settle`, `reject`) - no timer stands for progress.
//
// (The earlier form of this file dispatched an invented `rawtonic:batch-
// photo-settled` { entry, failure } that no production code listens to, and
// its drain flush waited for every queued write before the test could drive
// them.)
import { act, renderHook } from '@testing-library/react';
import { expect, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { allPhotoWritesSettled, photoBatches, submitPhotoWrite, type PhotoBatch } from './photoWrites';
import { useTauriListeners } from '../hooks/useTauriListeners';
import { Invokes } from '../components/ui/AppProperties';

/** What the mounted hook asks the backend as it starts (its interrupted
 * exports): not a quit's command. */
const HOOK_STARTUP: string[] = [Invokes.ExportQueueJobs];

/** The commands the webview sent the backend (`invoke` is a mock), the
 * listener hook's own start excluded. */
export function COMMANDS(): string[] {
  const commands = vi.mocked(invoke).mock.calls.map(([command]) => command);
  return commands.filter((command) => !HOOK_STARTUP.some((own) => own === command));
}

/** How often `command` was sent. */
export function sent(command: string): number {
  return COMMANDS().filter((each) => each === command).length;
}

/** Lets the tasks due run (a report or a progress invoke is a task). */
export async function turn(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

/** Runs `work` in React's act scope: the app's listener hook turns a Tauri
 * report into the webview's event (a state-less callback, but a report that
 * starts renders is act's). */
async function inAct(work: () => void): Promise<void> {
  await act(async () => {
    work();
  });
}

/** Waits until `done` holds (real time passes: a progress invoke, a report
 * or a command's answer is a task). */
export async function waitFor(done: () => boolean, what = 'the condition'): Promise<void> {
  await vi.waitFor(() => expect(done(), `waited for ${what}`).toBe(true));
}

/** `value()` once it holds (`when`): the microtasks the queue's promises
 * take (a write of one photograph starts a task after its submission), then
 * the tasks due. */
async function until<T>(value: () => T, when = (it: T): boolean => !!it): Promise<T> {
  for (let round = 0; round < 1000; round += 1) {
    const now = value();
    if (when(now)) return now;
    for (let micro = 0; micro < 4; micro += 1) await Promise.resolve();
    await turn();
  }
  throw new Error('a condition never held');
}

/** Registers the quit's drain flush through the app's own registry
 * (`registerQuitFlush`) and returns its unregistration. The flush does not
 * wait for the queue itself (the app's drain does, in `flushRecipeSaves`);
 * `whileDraining` is where the test drives the batch while the quit listens,
 * which must happen beside the flush, not behind it. */
export function registerDrain(
  registerQuitFlush: (flush: () => Promise<string[]>) => () => void,
  whileDraining: () => Promise<unknown> = async () => [],
): () => void {
  let ran = false;
  // (The while is excluded from the flush's promise: the flushes run in
  // Promise.allSettled, which reads each promise's handlers only after all
  // of them were called - a flush waiting inside the call would block the
  // others, the app's editor drain included.)
  return registerQuitFlush(() => {
    if (ran) return Promise.resolve([]);
    ran = true;
    return new Promise<string[]>((resolve, reject) => {
      void (async () => {
        try {
          await whileDraining();
          resolve([]);
        } catch (error) {
          reject(error);
        }
      })();
    });
  });
}

/** Mounts the app's own listener hook (`useTauriListeners`) as
 * src/hooks/useBatchWriteProgress.test.ts does; Tauri's `listen` is the
 * test's stand-in (`vi.mock('@tauri-apps/api/event')`: one callback per
 * event name, removed by its unlisten). Returns an unmount that settles once
 * the hook's listeners are gone, so the next test may mount its own. */
export async function mountAppListeners(): Promise<() => Promise<void>> {
  vi.mocked(listen).mockClear();
  const { unmount } = renderHook(() =>
    useTauriListeners({
      refreshAllFolderTrees: vi.fn(),
      handleSelectSubfolder: vi.fn(),
      refreshImageList: vi.fn(),
      markGenerated: vi.fn(),
    }),
  );
  await until(() => vi.mocked(listen).mock.calls.some(([name]) => name === 'batch-write-settled'));
  return async () => {
    unmount();
    // (It unlistens through the promises of its `listen`s - tasks of real
    // time, which a test's fake timers must not hold back.)
    for (let task = 0; task < 3; task += 1) await new Promise((resolve) => setTimeout(resolve, 0));
    vi.mocked(listen).mockClear();
  };
}

/** A long batch command as the app runs one on `photos` (`kind`: Auto
 * adjustments `auto`, face retouch `faceRetouch`): its write goes through
 * the queue as a batch and is given the batch's tag. Each photograph it
 * finishes is reported by src-tauri as `batch-write-settled` (`ok: null`:
 * written; a reason: not); the app's listener hook hears each one and
 * quitting counts it up. Its own answer comes last: resolves with the
 * photographs it did not write (`failed`) - src-tauri's `SaveFailure.failed`
 * - or rejects with its `SaveFailure`. Until the test answers it, it stays
 * pending, as a long command does. */
const commands = new Set<BatchCommand>();

export class BatchCommand {
  /** Its write queue entry (its batch's id). */
  entry = 0;
  /** The tag it reports its photographs with (its batch's). */
  tag = '';
  /** Its write: settles as the command answers (see `settle`, `reject`). */
  readonly opened: Promise<string[]>;
  /** The photographs it reported not written (`ok`). */
  readonly notWritten = new Set<string>();
  /** Its answer, once the command waits for it. */
  private answer: { resolve: (failed: string[]) => void; reject: (error: unknown) => void } | undefined;
  /** The Tauri callback of the app's `batch-write-settled` listener. */
  private report: ((event: { payload: unknown }) => void) | null = null;
  private readonly told = new Set<string>();
  /** Its report delivery, queued so reports arrive in the order given. */
  private delivering: Promise<void> = Promise.resolve();

  /** Answered by the test (`settle`, `reject`), or by its cleanup. */
  answered = false;
  private readonly photos: string[];

  private constructor(photos: string[], readonly kind: string) {
    this.photos = photos;
    this.opened = submitPhotoWrite(photos, (id) => this.run(id), { kind, batch: true });
    // Answered by the test: no unhandled rejection before it is awaited.
    this.opened.catch(() => undefined);
    commands.add(this);
  }

  /** Submits the command on `photos` and waits until the queue runs its
   * write (its entry and tag are known then). */
  static async start(photos: string[], kind = 'auto'): Promise<BatchCommand> {
    const command = new BatchCommand(photos, kind);
    // (The mount above keeps the Tauri callback of the event reports come
    // through.)
    command.report = listenerOf();
    const batch = await until(() => batchOf(command.entry) ?? undefined);
    command.tag = batch.tag;
    return command;
  }

  /** The batch behind its entry, as the app reads it (its count and
   * failures). */
  get batch(): PhotoBatch {
    const batch = batchOf(this.entry);
    if (!batch) throw new Error(`no batch for entry ${this.entry}`);
    return batch;
  }

  /** Its write runs the command: its answer is taken here, and the command
   * then reports its photographs (the test's reports) and waits for it -
   * the photographs are checked beside it, as Auto's are decoded beside
   * their writes. */
  private async run(id: number): Promise<string[]> {
    this.entry = id;
    const answer = new Promise<string[]>((resolve, reject) => {
      this.answer = { resolve, reject };
    });
    return answer;
  }

  /** Delivers src-tauri's report for `photo` (written, or `why` not) of
   * this command's batch through the app's listener hook. */
  private deliver(photo: string, why?: string): void {
    (this.report ?? listenerOf())({ payload: { batch: this.tag, path: photo, ok: why ?? null, also: [] } });
  }

  /** The command finishes `photo` (`why`: it could not write it). The
   * report reaches the webview as the app's listener passes it on; the
   * tasks it starts run. */
  async photo(photo: string, why?: string): Promise<void> {
    this.delivering = this.delivering.catch(() => undefined).then(async () => {
      expect(this.photos, `reported photo ${photo} belongs to the command`).toContain(photo);
      expect(this.told, `${photo} is reported once`).not.toContain(photo);
      this.told.add(photo);
      if (why !== undefined) this.notWritten.add(photo);
      await inAct(() => this.deliver(photo, why));
      await turn();
    });
    return this.delivering;
  }

  /** The command's photographs not reported yet, one after the other (`why`:
   * which of them it could not write). */
  async writeWhile(photos: string[] = this.unreported(), why: (photo: string) => string | undefined = () => undefined): Promise<void> {
    for (const photo of photos) await this.photo(photo, why(photo));
  }

  /** Its photographs, as submitted. */
  get paths(): string[] {
    return [...this.photos];
  }

  /** A command the test left unanswered answers now (its photographs not
   * reported count as written, as a command that answers without naming
   * any). */
  answerIt(): void {
    this.answered = true;
    this.reject({ message: 'left unanswered by the test', failed: [] });
  }

  /** Its photographs no report has settled yet. */
  unreported(): string[] {
    return this.photos.filter((photo) => !this.told.has(photo));
  }

  /** The command answers: it wrote every photograph it did not report as
   * not written (Auto's answers the ones it could not with them, as
   * `savedBatch` does). */
  async settle(): Promise<string[]> {
    const written = this.photos.filter((photo) => !this.notWritten.has(photo));
    return this.close(written);
  }

  /** The command fails outright with `SaveFailure` `error` (its reports so
   * far stay heard as progress). */
  async reject(error: unknown = { message: 'Auto adjustments failed', failed: [...this.photos] }): Promise<void> {
    this.answered = true;
    await this.delivering;
    const answer = await until(() => this.answer);
    await act(async () => {
      answer.reject(error);
    });
    await this.settled();
  }

  /** Its answer (src-tauri's `SaveFailure.failed`: the photographs it did
   * not write), once the command waits for it. */
  private async close(written: string[]): Promise<string[]> {
    this.answered = true;
    await this.delivering;
    const answer = await until(() => this.answer);
    await act(async () => {
      answer.resolve(written);
    });
    await this.settled();
    return written;
  }

  /** Its write has settled (the queue announced it). */
  async settled(): Promise<void> {
    await this.opened.then(
      () => undefined,
      () => undefined,
    );
    await turn();
  }
}

/** Puts the queue back to "nothing queued" for the test: forgetting the
 * batches earlier tests left finished (they are kept, unreported photographs
 * and all, as the app keeps them for 64) takes a fresh copy of the app's
 * queue modules, whose writes are all settled by then. What a test submitted
 * is emptied in its cleanup - which must stay short: the command a test
 * leaves unanswered and its batch go with it - and the mounted listener hook
 * is unmounted first, so it holds nothing of the old copy. */
export async function freshQueue(): Promise<void> {
  vi.resetModules();
  renderHook(() =>
    useTauriListeners({
      refreshAllFolderTrees: vi.fn(),
      handleSelectSubfolder: vi.fn(),
      refreshImageList: vi.fn(),
      markGenerated: vi.fn(),
    }),
  ).unmount();
  for (let task = 0; task < 2; task += 1) await new Promise((resolve) => setTimeout(resolve, 0));
}

/** Whether the queue has a write pending (it announces every one). */
function pendingWrites(): boolean {
  let settled = false;
  void allPhotoWritesSettled().then(() => {
    settled = true;
  });
  return !settled;
}

/** The Tauri callback the mounted listener hook registered for
 * `batch-write-settled`. */
function listenerOf(): (event: { payload: unknown }) => void {
  const callback = vi.mocked(listen).mock.calls.find(([name]) => name === 'batch-write-settled')?.[1];
  if (!callback) throw new Error('the app listener hook is not mounted');
  return callback;
}

/** The batch of a queue entry, from the app's own registry. */
function batchOf(entry: number): PhotoBatch | undefined {
  return photoBatches().get(entry);
}

/** A command the test left unanswered answers (it wrote what it reported),
 * so nothing waits in the queue into the next test; bounded, a leftover that
 * does not settle is not driven forever. */
export async function settleEverything(): Promise<void> {
  for (let round = 0; round < 5; round += 1) {
    for (const command of commands) if (!command.answered) command.answerIt();
    for (let micro = 0; micro < 4; micro += 1) await Promise.resolve();
    await turn();
    if (!pendingWrites()) break;
  }
}
