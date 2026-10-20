import { invoke, type InvokeArgs } from '@tauri-apps/api/core';

let last: Promise<unknown> = Promise.resolve();

/**
 * Invokes a rating, colour label or keyword write after the ones given
 * before it have finished. These commands run on blocking workers (the
 * catalog may be locked for seconds; the main thread must not wait), so two
 * quick key presses would otherwise race and the older value could land
 * last. A failed write does not stop the ones after it.
 */
export function invokeInOrder<T>(command: string, args: InvokeArgs): Promise<T> {
  const run = last.then(() => invoke<T>(command, args));
  last = run.catch(() => undefined);
  void run.finally(catalogChanged).catch(() => undefined);
  return run;
}

/** Fired (on `window`) after a write to the library's catalog settled, so
 * searches, value lists and smart albums show what it holds now. */
export const CATALOG_CHANGED = 'rawtonic:catalog-changed';

export function catalogChanged() {
  window.dispatchEvent(new Event(CATALOG_CHANGED));
}
