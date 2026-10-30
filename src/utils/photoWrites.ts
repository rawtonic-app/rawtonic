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
  /** A batch command reports its photographs as it writes them; `paths`
   * are the photographs it writes. */
  batch?: boolean;
};

/** A batch command whose progress is reported photo by photo
 * (`batch_write_settled` in src-tauri: Auto adjustments, face retouch):
 * quitting then hears each photograph as it is written instead of waiting
 * blind for the whole command. */
export type PhotoBatch = {
  /** The write queue entry it belongs to (what quitting has to wait for). */
  id: number;
  /** What it writes (`PhotoWrite.kind`). */
  kind: string;
  /** The photographs it writes. */
  paths: string[];
  /** How many: `paths.length`, kept plain so this module loads without them. */
  total: number;
  /** The tag the command is given, which it reports its photographs with. */
  tag: string;
  /** Photographs its command reported failed so far (why), for quitting. */
  photoFailures?: Map<string, string>;
};

/** The batch of each queue entry a batch command runs in (the oldest
 * finished ones are pruned, `BATCHES_KEPT`). */
const batches = new Map<number, PhotoBatch>();
/** The batches whose write is still queued. */
const running = new Set<number>();
/** The photographs of a batch no progress event reported yet. */
const unsettled = new WeakMap<PhotoBatch, Set<string>>();
/** Kept settled batches (beyond twice as many, the oldest finished ones are
 * dropped; a quit lists the batches after it started). */
const BATCHES_KEPT = 64;
/** What a photograph of a batch not written is called when the batch's
 * count does not name it. */
const NOT_WRITTEN = 'not written';

/** The write queue entries a batch command runs in, and their batches. */
export function photoBatches(): Map<number, PhotoBatch> {
  return batches;
}

/** What a batch has written so far: its photographs no report reached count
 * as written (a command's count may hold back its last ones). */
export function batchPhotoCount(batch: PhotoBatch): { done: number; total: number } {
  return { done: batch.total - (unsettled.get(batch)?.size ?? 0), total: batch.total };
}

/** The photographs of `batch` no progress event has reported yet. */
export function unsettledPhotos(batch: PhotoBatch): string[] {
  const left = unsettled.get(batch);
  return left ? batch.paths.filter((path) => left.has(path)) : [];
}

/** Calls `handler` for each photograph a batch command reports written
 * (`BATCH_WRITE_SETTLED`, src-tauri/src/file_management/edits.rs);
 * returns the unsubscribe. */
export function onBatchWriteSettled(
  handler: (settled: { tag: string; path: string; ok?: string; also: string[] }) => void,
): () => void {
  const listener = (event: Event) => {
    const detail = (event as CustomEvent<{ tag?: unknown; path?: unknown; ok?: unknown; also?: unknown }>).detail;
    if (typeof detail?.tag !== 'string' || typeof detail.path !== 'string') return;
    handler({
      tag: detail.tag,
      path: detail.path,
      ok: typeof detail.ok === 'string' ? detail.ok : undefined,
      also: Array.isArray(detail.also) ? detail.also.filter((path): path is string => typeof path === 'string') : [],
    });
  };
  window.addEventListener('rawtonic:batch-write-settled', listener);
  return () => window.removeEventListener('rawtonic:batch-write-settled', listener);
}

/** A photograph of the batch of queue entry `entry` settled: written, or
 * not (`failure`). */
export function batchPhotoSettled(entry: number, path: string, failure?: string): void {
  const batch = batches.get(entry);
  if (!batch) return;
  const left = unsettled.get(batch);
  if (!left?.delete(path)) return;
  if (failure !== undefined) (batch.photoFailures ??= new Map()).set(path, failure);
}

/** Reports the unreported photographs of the batch of `entry` (`failure`:
 * those that did not settle). */
export function batchPhotosReported(entry: number, failure?: string): void {
  const batch = batches.get(entry);
  if (!batch) return;
  for (const path of unsettledPhotos(batch)) batchPhotoSettled(entry, path, failure);
}

/** The batch a queue entry is, once it settled (null: not a batch). */
export function batchOfEntry(entry: number): PhotoBatch | null {
  return batches.get(entry) ?? null;
}

/** What the photographs of `batch` not written failed with (for quitting). */
export function batchFailures(batch: PhotoBatch): string[] {
  return [...(batch.photoFailures?.entries() ?? [])].map(([path, failure]) => `${nameOf(path)} (${failure})`);
}

function nameOf(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** The photograph a path's sidecar and XMP belong to: virtual copies of one
 * file share its XMP, so they share its queue. */
export function photoWriteKey(path: string): string {
  return path.split('?vc=')[0];
}

const tails = new Map<string, Promise<void>>();
let nextId = 1;
let nextBatchTag = 1;

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
export function submitPhotoWrite<T>(paths: string[], run: (id: number) => Promise<T>, write: PhotoWrite<T>): Promise<T> {
  const id = nextId++;
  const batch: PhotoBatch | undefined = write.batch
    ? { id, kind: write.kind, total: paths.length, tag: `batch-${nextBatchTag++}`, paths: [...paths] }
    : undefined;
  if (batch) {
    batches.set(id, batch);
    unsettled.set(batch, new Set(paths));
    running.add(id);
    // (Finished ones go when too many pile up in one session, oldest
    // first; a running batch is never dropped.)
    for (const kept of batches.keys()) {
      if (batches.size <= BATCHES_KEPT * 2) break;
      if (running.has(kept)) continue;
      batches.delete(kept);
    }
  }
  const keys = [...new Set(paths.map(photoWriteKey))];
  const before = keys.map((key) => tails.get(key)).filter((tail): tail is Promise<void> => !!tail);
  if (write.after) before.push(write.after.then(() => undefined, () => undefined));
  // Nothing to wait for: it starts at once, as a direct invoke would.
  const start = () => run(id);
  const result = before.length > 0 ? Promise.all(before).then(start) : new Promise<T>((resolve) => resolve(start()));
  const settled = result.then(
    (value) => (write.failedOf ? write.failedOf(value) : []),
    (error: unknown) => failedOfError(error, paths),
  ).catch((error: unknown) => {
    console.error('Reading which photos a write failed for failed:', error);
    return paths;
  }).then((failed) => {
    for (const key of keys) if (tails.get(key) === settled) tails.delete(key);
    if (write.announce === false) return;
    if (batch) {
      batchPhotosReported(id, NOT_WRITTEN);
      running.delete(id);
    }
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

/** Steps that submit their writes later (`photoWorkPending`). */
const steps = new Set<Promise<void>>();

/** Counts `work`, a step that submits its writes once earlier work is done
 * (a catalog undo step), among what `photoWritesAndStepsSettled` waits for
 * from now on; returns it. Its writes still join the queue themselves. */
export function photoWorkPending<T>(work: Promise<T>): Promise<T> {
  const settled = work.then(() => undefined, () => undefined);
  steps.add(settled);
  void settled.then(() => steps.delete(settled));
  return work;
}

/** Settles once no write is pending for any photograph, also those
 * submitted while waiting (quitting, switching library, file operations).
 * Steps not yet writing are not waited for: one may wait for the very
 * operation that drains (a rating queued behind a file undo). */
export async function allPhotoWritesSettled(): Promise<void> {
  while (tails.size > 0) await Promise.all(tails.values());
}

/** `allPhotoWritesSettled`, and also every step counted with
 * `photoWorkPending` (an export takes the metadata a rating step given just
 * before it writes). Never from inside such a step's dependencies. */
export async function photoWritesAndStepsSettled(): Promise<void> {
  while (tails.size > 0 || steps.size > 0) await Promise.all([...tails.values(), ...steps]);
}

/** Calls `handler` with each settled write; returns the unsubscribe. */
export function onPhotoWritesSettled(handler: (settled: PhotoWritesSettled) => void): () => void {
  const listener = (event: Event) => handler((event as CustomEvent<PhotoWritesSettled>).detail);
  window.addEventListener(PHOTO_WRITES_SETTLED, listener);
  return () => window.removeEventListener(PHOTO_WRITES_SETTLED, listener);
}
