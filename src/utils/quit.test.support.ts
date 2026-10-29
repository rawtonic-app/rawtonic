// Helpers for the quit tests (quitBatch.test.ts): what the webview told the
// backend; a long batch command as the app runs one (its write goes through
// the queue as a batch, it is given the batch's tag and reports each
// photograph it writes with `batch_write_settled`); and a registered drain
// flush, so a test can take the command through photographs while the
// quit's drain listens. A test support file, not app code.
import { expect, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { flushBeforeQuit } from './quit';
import { photoBatches, submitPhotoWrite } from './photoWrites';

/** The commands the webview sent the backend (`invoke` is a mock). */
export function COMMANDS(): string[] {
  return vi.mocked(invoke).mock.calls.map(([command]) => command);
}

/** How often `command` was sent. */
export function sent(command: string): number {
  return COMMANDS().filter((each) => each === command).length;
}

/** Lets the tasks due (the quit's progress invokes) run. */
export async function turn(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

/** Waits until `done` holds (real time passes: a progress invoke or a
 * report is a task). */
export async function waitFor(done: () => boolean, what = 'the condition'): Promise<void> {
  await vi.waitFor(() => expect(done(), `waited for ${what}`).toBe(true));
}

/** Registers the quit's drain flush for a test through the app's own
 * registry (`registerQuitFlush`, `flushBeforeQuit`): the flushes already
 * there run (as in the app), then `whileDraining` runs while the quit's
 * drain listens (the command writing its photographs), then the drain ends.
 * Returns it to remove (or put it in `unregistered`). */
export function registerDrain(
  registerQuitFlush: (flush: () => Promise<string[]>) => () => void,
  whileDraining: () => Promise<unknown> = async () => [],
): () => void {
  // (Once: the quit's saves submitted meanwhile call the flushes again -
  // their writes are what it waits for, not this step.)
  let ran = false;
  return registerQuitFlush(async () => {
    await flushBeforeQuit();
    if (!ran) {
      ran = true;
      await whileDraining();
    }
    return [];
  });
}

/** A long batch command as the app runs one on `photos`: its write goes
 * through the queue as a batch (`kind`: Auto adjustments, face retouch), it
 * is given the batch's tag, and the test lets each photograph be written
 * (`next`) or not (`next(why)`), then gives its answer (`finish`: how many
 * it wrote, `failWith`: why its photos may not be). */
export class BatchCommand {
  /** The tag the command was given (its batch's). */
  tag = '';
  /** Its write (settles when it answers). */
  readonly opened: Promise<unknown>;
  private readonly photos: string[];
  private readonly written = new Set<string>();
  private ahead!: () => void;
  private step: Promise<void> = new Promise((resolve) => { this.ahead = resolve; });
  private readonly answer: (written: number) => void;
  private readonly fail: (why: Error) => void;

  constructor(photos: string[], kind = 'auto') {
    this.photos = photos;
    const answering = new Promise<number>((resolve, reject) => {
      this.answer = resolve;
      this.fail = reject;
    });
    this.opened = submitPhotoWrite(
      photos,
      (id) => {
        this.tag = photoBatches().get(id)?.tag ?? '';
        return this.walk(answering);
      },
      { kind, batch: true },
    );
    this.opened.catch(() => undefined);
  }

  private async walk(answering: Promise<number>) {
    for (const photo of this.photos) {
      await this.step;
      this.step = new Promise((resolve) => { this.ahead = resolve; });
      this.written.add(photo);
    }
    const count = await answering;
    return { failed: count < this.photos.length ? this.photos.slice(count) : [] };
  }

  /** The command wrote its next photograph (`why`: it did not) - reported in
   * a task, as Tauri delivers it - and goes on to the next. */
  async next(why?: string): Promise<void> {
    const photo = this.photos.find((each) => !this.written.has(each))!;
    const tag = this.tag;
    await new Promise<void>((done) => {
      setTimeout(() => {
        window.dispatchEvent(
          new CustomEvent('rawtonic:batch-write-settled', { detail: { tag, path: photo, ...(why ? { ok: why } : {}), also: [] } }),
        );
        done();
      }, 0);
    });
    this.ahead();
  }

  /** It answers now (`written`: how many photographs it wrote; its answer
   * is the photograph it had to hold back to its last report). */
  finish(written = this.photos.length) {
    this.answer(written);
  }

  failWith(why: string) {
    this.fail(new Error(why));
  }
}
