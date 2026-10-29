// Helpers for the quit tests (quitBatch.test.ts): what the webview told the
// backend; a long batch command as the app runs one (its write goes through
// the queue as a batch, it is given the batch's tag and settles its
// photographs one after the other, each reported as src-tauri reports it,
// `rawtonic:batch-photo-settled`); and a drain flush registered the app's
// way, so a test can take the command through its photographs while the
// quit's drain listens. A test support file, not app code.
import { expect, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { allPhotoWritesSettled, photoBatches, submitPhotoWrite } from './photoWrites';

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
 * registry (`registerQuitFlush`): the flushes already there settle, waiting
 * for the queued writes as the app's drain does (`allPhotoWritesSettled`),
 * then `whileDraining` runs while the quit's drain listens - once, as the
 * drain runs each flush once. Returns it to remove (or put it in
 * `unregistered`). */
export function registerDrain(
  registerQuitFlush: (flush: () => Promise<string[]>) => () => void,
  whileDraining: () => Promise<unknown> = async () => [],
): () => void {
  let ran = false;
  return registerQuitFlush(async () => {
    if (!ran) {
      ran = true;
      await allPhotoWritesSettled();
      await whileDraining();
    }
    return [];
  });
}

/** A long batch command as the app runs one on `photos` (`kind`: Auto
 * adjustments, face retouch): its write goes through the queue as a batch
 * and is given the batch's tag; it settles its photographs one after the
 * other, reporting each as src-tauri reports it (`rawtonic:batch-photo-
 * settled`: the entry, the photograph, and why it was not written), and
 * then answers counted (`finish`: how many it wrote; `failWith`: why its
 * photographs may not be). The test takes a photograph through it with
 * `next` (written) or `next(why)` (not). A photograph it settles without a
 * report is reported not written, so none is lost silently. */
export class BatchCommand {
  /** The tag it was given (its batch's). */
  tag = '';
  /** The write queue entry it is (`rawtonic:batch-photo-settled` names it). */
  entry = 0;
  /** Its write (settles when its photographs are settled, or it failed). */
  readonly opened: Promise<unknown>;
  private readonly photos: string[];
  /** The photographs its own reports covered. */
  private readonly told = new Set<string>();
  /** Its count (`finish`) or failure (`failWith`). */
  private count: { written: number; fail?: string } | undefined;
  /** Its write runs (what waits for it). */
  private starting: Array<() => void> = [];

  constructor(photos: string[], readonly kind = 'auto') {
    this.photos = [...photos];
    this.opened = submitPhotoWrite(photos, (id) => this.run(id), { kind, batch: true });
    this.opened.catch(() => undefined);
  }

  /** Whether its write runs (a write waits for those before it). */
  get running(): boolean {
    return this.entry > 0;
  }

  /** Settles once its write runs (a write waits for those before it). */
  started(): Promise<void> {
    return this.running ? Promise.resolve() : new Promise((resolve) => { this.starting.push(resolve); });
  }

  private async run(id: number): Promise<void> {
    this.entry = id;
    this.tag = photoBatches().get(id)?.tag ?? '';
    for (const resolve of this.starting.splice(0)) resolve();
    for (const [at, photo] of this.photos.entries()) {
      // Reported not written unless its count says written: a photograph no
      // report covered is never lost silently.
      await this.report(photo, at < (this.count?.written ?? 0) ? 'written' : 'not written');
    }
    if (this.count?.fail !== undefined) throw new Error(this.count.fail);
  }

  /** The photograph settles now, reported as src-tauri reports it - passed
   * on by the queue in a task. */
  private async report(photo: string, why: string): Promise<void> {
    const { entry } = this;
    window.dispatchEvent(
      new CustomEvent('rawtonic:batch-photo-settled', {
        detail: { entry, path: photo, ...(why !== 'written' ? { failure: why } : {}) },
      }),
    );
    this.told.add(photo);
    await turn();
  }

  /** The command wrote its next photograph (`why`: it did not), reported
   * before the queue's write settles. */
  async next(why?: string): Promise<void> {
    await this.started();
    const photo = this.photos.find((each) => !this.told.has(each));
    if (photo === undefined) return;
    await this.report(photo, why ?? 'written');
  }

  /** It answers counted (as `finish`) and the test writes its photographs
   * one after the other, as `how` lets them be written (`why`: not), while
   * the quit's drain listens - settles once it answered. */
  async drive(how: (photo: string, at: number) => Promise<string | undefined>, written = this.photos.length): Promise<void> {
    this.finish(written);
    for (const [at, photo] of this.photos.entries()) {
      if (at >= written || this.told.has(photo)) continue;
      await this.report(photo, (await how(photo, at)) ?? 'written');
    }
    await this.opened.catch(() => undefined);
    await turn();
  }

  /** It answers counted: `written` how many it wrote (the others not), or
   * its failure (`failWith`). */
  finish(written = this.photos.length) {
    this.count = { written };
  }

  failWith(why: string) {
    this.count = { written: 0, fail: why };
  }
}
