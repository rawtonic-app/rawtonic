// A long batch command (Auto adjustments, face retouch) writes its
// photographs one at a time, and the backend reports each as it finishes:
// `batch-write-settled` with { batch, path, ok: string | null, also } (the
// `BatchWriteSettled` serde shape in src-tauri/src/file_management/edits.rs,
// emitted by edits.rs and ai_faces.rs). The app's listener hook
// (useTauriListeners) counts each for the command's batch (`photoBatches`,
// what quitting reads) and passes it on once, in the webview's own form, as
// `rawtonic:batch-write-settled` (photoWrites.onBatchWriteSettled - the event
// quitting's drain answers the backend's `quit_saving` from). Reports of no
// queued batch, repeated ones and late ones reach the webview not at all; a
// report passes on only this batch's photographs still waited for, once
// each, and no foreign or repeated one.
import { act, renderHook } from '@testing-library/react';
import { beforeAll, expect, it, vi } from 'vitest';
import { useTauriListeners } from './useTauriListeners';

const callbacks = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>());
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, callback: (event: { payload: unknown }) => void) => {
    callbacks.set(name, callback);
    // A listener once in the backend's event system until it unlistens.
    return Promise.resolve(() => callbacks.delete(name));
  }),
}));
vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (path: string) => `asset://${path}`,
  invoke: vi.fn(() => Promise.resolve([])),
}));

type Writes = typeof import('../utils/photoWrites');
type Report = { batch: string; path: string; ok: string | null; also: string[] };

let writes: Writes;
const unmount = { current: () => {} };

beforeAll(async () => {
  writes = await import('../utils/photoWrites');
  unmount.current = renderHook(() =>
    useTauriListeners({
      refreshAllFolderTrees: vi.fn(), handleSelectSubfolder: vi.fn(),
      refreshImageList: vi.fn(), markGenerated: vi.fn(),
    }),
  ).unmount;
  await vi.waitFor(() => expect(callbacks.has('batch-write-settled')).toBe(true));
});

/** A batch command of `photos` queued as `recipeWrite(..., { batch: true })`
 * submits it: its write queue entry and the tag it reports its photos with. */
async function queuedBatch(photos: string[]): Promise<{ id: number; tag: string; finish: () => void }> {
  let opened!: () => void;
  const started = new Promise<void>((resolve) => { opened = resolve; });
  let finish: () => void = () => {};
  let entry = 0;
  const answered = new Promise<void>((resolve) => { finish = resolve; });
  const write = writes.submitPhotoWrite(
    photos,
    (id) => {
      entry = id;
      opened();
      return answered;
    },
    { kind: 'auto', batch: true },
  );
  write.catch(() => undefined);
  await started;
  return { id: entry, tag: writes.photoBatches().get(entry)!.tag, finish };
}

/** Delivers `payload` as Tauri delivers the backend's report, and returns
 * what the webview heard meanwhile (the event quitting listens to). */
function report(payload: Partial<Report> | null) {
  const heard: Array<{ tag: string; path: string; ok?: string; also: string[] }> = [];
  const listener = (event: Event) => heard.push((event as CustomEvent).detail);
  window.addEventListener('rawtonic:batch-write-settled', listener);
  try {
    act(() => callbacks.get('batch-write-settled')!({ payload }));
  } finally {
    window.removeEventListener('rawtonic:batch-write-settled', listener);
  }
  return heard;
}

const count = (id: number) => writes.batchPhotoCount(writes.batchOfEntry(id)!);
const failures = (id: number) => writes.batchFailures(writes.batchOfEntry(id)!);

it('counts each backend photo once for its batch and passes it on in the webview form', async () => {
  const { id, tag, finish } = await queuedBatch(['/1.dng', '/2.dng', '/3.dng']);
  expect(count(id)).toEqual({ done: 0, total: 3 });
  // The Rust shape: ok is null for a written photo.
  expect(report({ batch: tag, path: '/1.dng', ok: null, also: [] })).toEqual([{ tag, path: '/1.dng', also: [] }]);
  expect(count(id)).toEqual({ done: 1, total: 3 });
  // A real failure keeps its reason (what quitting's prompt lists).
  expect(report({ batch: tag, path: '/2.dng', ok: 'disk full', also: ['/2.dng'] }))
    .toEqual([{ tag, path: '/2.dng', ok: 'disk full', also: [] }]);
  expect(failures(id)).toEqual(['2.dng (disk full)']);
  expect(count(id)).toEqual({ done: 2, total: 3 });
  // A face retouch names the failed photo in `also` too: no second photo.
  expect(report({ batch: tag, path: '/3.dng', ok: null, also: ['/3.dng'] })).toHaveLength(1);
  expect(count(id)).toEqual({ done: 3, total: 3 });
  finish();
  await writes.allPhotoWritesSettled();
});

it('writes the photos a report settled with it (`also`) as it wrote the photo, foreign and repeated ones dropped', async () => {
  const mine = await queuedBatch(['/a.dng', '/b.dng']);
  const other = await queuedBatch(['/x.dng', '/y.dng']);
  // Another batch's photo is that batch's business.
  expect(report({ batch: other.tag, path: '/x.dng', ok: null, also: [] }))
    .toEqual([{ tag: other.tag, path: '/x.dng', also: [] }]);
  expect(count(other.id)).toEqual({ done: 1, total: 2 });
  expect(count(mine.id)).toEqual({ done: 0, total: 2 });
  // `also` with no failure: those photos were written too (no failure made
  // up). A photo of no such batch (`/elsewhere.dng`), another batch's
  // (`/y.dng`) and the report's own photo told twice (`/a.dng`) stay out of
  // the webview form, which hears each settled photo once and only this
  // batch's.
  expect(report({ batch: mine.tag, path: '/a.dng', ok: null, also: ['/b.dng', '/elsewhere.dng', '/y.dng', '/a.dng'] }))
    .toEqual([
      { tag: mine.tag, path: '/a.dng', also: [] },
      { tag: mine.tag, path: '/b.dng', also: [] },
    ]);
  expect(count(mine.id)).toEqual({ done: 2, total: 2 });
  expect(count(other.id)).toEqual({ done: 1, total: 2 });
  expect(failures(mine.id)).toEqual([]);
  expect(failures(other.id)).toEqual([]);
  mine.finish();
  other.finish();
  await writes.allPhotoWritesSettled();
});

it('writes the photos a failed report settled with it (`also`) the same failure', async () => {
  const { id, tag, finish } = await queuedBatch(['/f1.dng', '/f2.dng', '/f3.dng']);
  // The failure is said once per photo, and only about this batch's.
  expect(report({ batch: tag, path: '/f1.dng', ok: 'no space left', also: ['/f2.dng', '/f1.dng', '/f2.dng', '/elsewhere.dng'] }))
    .toEqual([
      { tag, path: '/f1.dng', ok: 'no space left', also: [] },
      { tag, path: '/f2.dng', ok: 'no space left', also: [] },
    ]);
  expect(count(id)).toEqual({ done: 2, total: 3 });
  expect(failures(id)).toEqual(['f1.dng (no space left)', 'f2.dng (no space left)']);
  finish();
  await writes.allPhotoWritesSettled();
});

it('drops unknown, malformed, repeated and late reports before the webview hears them', async () => {
  const { id, tag, finish } = await queuedBatch(['/u1.dng', '/u2.dng']);
  // No queued batch has this tag, the batch waits for no such photo, and
  // unreadable reports (no tag, no photo, an empty tag or photo, and an
  // `ok` that is neither the backend's null nor a reason: not a write).
  for (const payload of [
    { batch: 'batch-9999', path: '/u1.dng', ok: null, also: [] },
    { batch: tag, path: '/elsewhere.dng', ok: null, also: [] },
    { path: '/u1.dng' },
    { batch: tag, path: '/u1.dng', also: [] },
    { batch: tag, path: '/u1.dng', ok: undefined, also: [] },
    { batch: '', path: '/u1.dng', ok: null, also: [] },
    { batch: tag, path: '', ok: null, also: [] },
    { batch: tag, path: 7, ok: null, also: [] },
    { batch: tag, path: '/u1.dng', ok: { Ok: 'written' }, also: [] },
    { batch: tag, path: '/u1.dng', ok: true, also: [] },
    { batch: tag, path: '/u1.dng', ok: 0, also: [] },
    null,
    undefined,
  ] as unknown as Array<Partial<Report> | null>) {
    expect(report(payload)).toEqual([]);
  }
  expect(count(id)).toEqual({ done: 0, total: 2 });
  // A photo once; the same photo again, or late with a failure, is no new
  // progress and no new failure.
  expect(report({ batch: tag, path: '/u1.dng', ok: null, also: [] })).toEqual([{ tag, path: '/u1.dng', also: [] }]);
  expect(report({ batch: tag, path: '/u1.dng', ok: null, also: [] })).toEqual([]);
  expect(report({ batch: tag, path: '/u1.dng', ok: 'disk full', also: [] })).toEqual([]);
  // A stale duplicate names a photo it settled before: it is dropped whole,
  // also the photos still waited for that it drags along.
  expect(report({ batch: tag, path: '/u1.dng', ok: null, also: ['/u2.dng'] })).toEqual([]);
  expect(report({ batch: tag, path: '/u1.dng', ok: 'disk full', also: ['/elsewhere.dng'] })).toEqual([]);
  expect(count(id)).toEqual({ done: 1, total: 2 });
  expect(failures(id)).toEqual([]);
  // The photo still waited for settles once, and once settled it is late too.
  expect(report({ batch: tag, path: '/u2.dng', ok: null, also: [] })).toEqual([{ tag, path: '/u2.dng', also: [] }]);
  expect(report({ batch: tag, path: '/u2.dng', ok: 'late failure', also: [] })).toEqual([]);
  expect(count(id)).toEqual({ done: 2, total: 2 });
  expect(failures(id)).toEqual([]);
  finish();
  await writes.allPhotoWritesSettled();
});

it('settles each photograph of a report once, a repeated `also` in it included', async () => {
  const { id, tag, finish } = await queuedBatch(['/s1.dng', '/s2.dng', '/s3.dng']);
  // Quitting records a failure per photograph it hears: a photo named twice
  // in `also` must not fail twice, and the report's own photo not again.
  expect(report({ batch: tag, path: '/s1.dng', ok: 'read only', also: ['/s2.dng', '/s2.dng', '/s1.dng'] }))
    .toEqual([
      { tag, path: '/s1.dng', ok: 'read only', also: [] },
      { tag, path: '/s2.dng', ok: 'read only', also: [] },
    ]);
  expect(count(id)).toEqual({ done: 2, total: 3 });
  expect(failures(id)).toEqual(['s1.dng (read only)', 's2.dng (read only)']);
  finish();
  await writes.allPhotoWritesSettled();
});

it('hears no report after it unmounted, and removes its listener', async () => {
  const { id, tag, finish } = await queuedBatch(['/z.dng']);
  await vi.waitFor(() => expect(callbacks.has('batch-write-settled')).toBe(true));
  unmount.current();
  await vi.waitFor(() => expect(callbacks.has('batch-write-settled')).toBe(false));
  // (Its listener is gone: a late report reaches neither the batch nor the
  // webview.)
  act(() => callbacks.get('batch-write-settled')?.({ payload: { batch: tag, path: '/z.dng', ok: null, also: [] } }));
  expect(count(id)).toEqual({ done: 0, total: 1 });
  finish();
  await writes.allPhotoWritesSettled();
});
