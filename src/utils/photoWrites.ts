/**
 * The one write queue of each photograph: every write of its recipe,
 * sidecar or XMP (editor autosave, batch paste/preset/sync/reset, lens and
 * Auto corrections, undo/redo of recipe values, ratings, labels, flags,
 * keywords, EXIF, text fields, snapshots) is submitted here.
 *
 * - A write starts only after every write submitted before it for any of
 *   its photographs has settled. Its dependencies are taken when it is
 *   submitted, never looked up later, so writes of one photograph apply in
 *   submission order, a later write never loses to an earlier one, and no
 *   write can wait for one submitted after it (no wait cycle).
 * - A failed write does not stop the ones after it.
 * - Each settled write is announced once with `PHOTO_WRITES_SETTLED`
 *   (`PhotoWritesSettled`: which photographs it wrote and which failed),
 *   also when it failed for some or all of them; the XMP status and History
 *   listen to that one event.
 *
 * The backend makes each write's read-modify-write of a sidecar atomic
 * (`sidecar_field_edit`); the order of writes is the webview's, where the
 * user's actions are, so it is kept here.
 */

/** Fired on `window` (a `CustomEvent<PhotoWritesSettled>`) once per write. */
export const PHOTO_WRITES_SETTLED = 'rawtonic:photo-writes-settled';

export type PhotoWritesSettled = {
  /** The write's identity, increasing in submission order. */
  id: number;
  /** What was written (`PhotoWrite.kind`). */
  kind: string;
  /** Every photograph the write was for. */
  paths: string[];
  /** Those of `paths` not written (all of them when it failed outright). */
  failed: string[];
  /** The caller's tag (`PhotoWrite.step`), e.g. the History step it makes. */
  step?: unknown;
};

export type PhotoWrite<T> = {
  /** What is written, e.g. `recipe`, `paste`, `reset`, a command name. */
  kind: string;
  /** Those of the write's paths it did not write, from its result (default: none). */
  failedOf?: (result: T) => string[];
  /** Tag carried to the settled event. */
  step?: unknown;
  /** One more thing to wait for (submitted before, so no cycle). */
  after?: Promise<unknown>;
  /** False: a read that must see the writes before it, announced to no one. */
  announce?: boolean;
};

/** The photograph a path's sidecar and XMP belong to: virtual copies of one
 * file share its XMP, so they share its queue. */
export function photoWriteKey(path: string): string {
  return path.split('?vc=')[0];
}

const tails = new Map<string, Promise<void>>();
let nextId = 1;

/** Paths a rejected write did not write: those its error names
 * (`SaveFailure.failed`), else all of them. */
function failedOfError(error: unknown, paths: string[]): string[] {
  const failed = (error as { failed?: unknown } | null)?.failed;
  return Array.isArray(failed) ? paths.filter((path) => failed.includes(path)) : paths;
}

/**
 * Runs `run` once every write submitted before for any of `paths` settled
 * and resolves (or rejects) as it does. `run` must not wait for this
 * queue itself (`photoWritesSettled`): it would wait for itself.
 */
export function submitPhotoWrite<T>(paths: string[], run: () => Promise<T>, write: PhotoWrite<T>): Promise<T> {
  const id = nextId++;
  const keys = [...new Set(paths.map(photoWriteKey))];
  const before = keys.map((key) => tails.get(key)).filter((tail): tail is Promise<void> => !!tail);
  if (write.after) before.push(write.after.then(() => undefined, () => undefined));
  // Nothing to wait for: it starts at once, as a direct invoke would.
  const result = before.length > 0 ? Promise.all(before).then(run) : new Promise<T>((resolve) => resolve(run()));
  const settled = result.then(
    (value) => (write.failedOf ? write.failedOf(value) : []),
    (error: unknown) => failedOfError(error, paths),
  ).catch((error: unknown) => {
    console.error('Reading which photos a write failed for failed:', error);
    return paths;
  }).then((failed) => {
    for (const key of keys) if (tails.get(key) === settled) tails.delete(key);
    if (write.announce === false) return;
    const detail: PhotoWritesSettled = { id, kind: write.kind, paths, failed, ...(write.step !== undefined ? { step: write.step } : {}) };
    // (A listener that throws is reported by the browser; it stops nothing here.)
    window.dispatchEvent(new CustomEvent(PHOTO_WRITES_SETTLED, { detail }));
  });
  for (const key of keys) tails.set(key, settled);
  return result;
}

/** Settles after every write submitted so far for `path`; undefined when
 * none is pending (a read of its recipe may start at once). */
export function photoWritesSettled(path: string): Promise<void> | undefined {
  return tails.get(photoWriteKey(path));
}

/** Whether a write for `path` is pending. */
export function photoWritePending(path: string): boolean {
  return tails.has(photoWriteKey(path));
}

/** Settles once no write is pending for any photograph, also those
 * submitted while waiting (quitting, closing a library). */
export async function allPhotoWritesSettled(): Promise<void> {
  while (tails.size > 0) await Promise.all(tails.values());
}

/** Calls `handler` with each settled write; returns the unsubscribe. */
export function onPhotoWritesSettled(handler: (settled: PhotoWritesSettled) => void): () => void {
  const listener = (event: Event) => handler((event as CustomEvent<PhotoWritesSettled>).detail);
  window.addEventListener(PHOTO_WRITES_SETTLED, listener);
  return () => window.removeEventListener(PHOTO_WRITES_SETTLED, listener);
}
