import { afterAll, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { CATALOG_CHANGED, invokeInOrder } from './catalogWrites';
import { PHOTO_WRITES_SETTLED } from './photoWrites';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

// Nothing of one test may still wait in the queue of the next.
afterAll(async () => (await import('./photoWrites')).allPhotoWritesSettled());

it('writes ratings in the order they were given, also after a failure', async () => {
  const answers: Array<(value?: unknown) => void> = [];
  const fails: Array<(reason: unknown) => void> = [];
  vi.mocked(invoke).mockImplementation(
    () =>
      new Promise((resolve, reject) => {
        answers.push(resolve);
        fails.push(reject);
      }),
  );
  const first = invokeInOrder('set_rating_for_paths', { paths: ['/a'], rating: 1 });
  const second = invokeInOrder('set_rating_for_paths', { paths: ['/a'], rating: 2 });
  const third = invokeInOrder('set_rating_for_paths', { paths: ['/a'], rating: 3 });
  // Only the first is asked for until it has answered.
  await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(invoke).toHaveBeenCalledTimes(1);
  fails[0]('database is locked');
  await expect(first).rejects.toBe('database is locked');
  await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
  expect(vi.mocked(invoke).mock.calls[1][1]).toEqual({ paths: ['/a'], rating: 2 });
  answers[1]();
  await second;
  await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(3));
  answers[2]('done');
  await expect(third).resolves.toBe('done');
});

it('tells searches and smart albums after each write settled, also a failed one', async () => {
  const changed = vi.fn();
  window.addEventListener(CATALOG_CHANGED, changed);
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockResolvedValueOnce(undefined).mockRejectedValueOnce('locked');
  await invokeInOrder('add_tag_for_paths', { paths: ['/a'], tag: 'user:x' });
  await vi.waitFor(() => expect(changed).toHaveBeenCalledTimes(1));
  await expect(invokeInOrder('set_rating_for_paths', { paths: ['/a'], rating: 2 })).rejects.toBe('locked');
  await vi.waitFor(() => expect(changed).toHaveBeenCalledTimes(2));
  window.removeEventListener(CATALOG_CHANGED, changed);
});

it('remembers the photographs this window rated, not failed ratings nor labels', async () => {
  const { metadataWritten } = await import('./catalogWrites');
  vi.mocked(invoke).mockImplementation(async (command: string, args?: unknown) => {
    if ((args as { rating?: number }).rating === 9) throw new Error('refused');
    return undefined;
  });
  await invokeInOrder('set_rating_for_paths', { paths: ['/r'], rating: 2 });
  await invokeInOrder('set_color_label_for_paths', { paths: ['/l'], color: 'red' });
  await invokeInOrder('set_rating_for_paths', { paths: ['/f'], rating: 9 }).catch(() => {});
  await vi.waitFor(() => expect(metadataWritten('/f')).toBe(false));
  expect([metadataWritten('/r'), metadataWritten('/l')]).toEqual([true, false]);
  // Saved for one, failed for the other: the saved one stays protected.
  vi.mocked(invoke).mockImplementation(async () => {
    throw { message: 'one failed', failed: ['/y'] };
  });
  await invokeInOrder('set_rating_for_paths', { paths: ['/x', '/y'], rating: 0 }).catch(() => {});
  await vi.waitFor(() => expect(metadataWritten('/y')).toBe(false));
  expect(metadataWritten('/x')).toBe(true);
});

it('waits for every metadata write given before, and those they start meanwhile', async () => {
  const { photoWritesAndStepsSettled, photoWorkPending, submitPhotoWrite } = await import('./photoWrites');
  let rated: (value?: unknown) => void = () => {};
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementationOnce(() => new Promise((resolve) => (rated = resolve)));
  const rating = invokeInOrder('set_rating_for_paths', { paths: ['/a'], rating: 5 });
  let exifWritten: () => void = () => {};
  // An EXIF write that fails still settles.
  const exif = submitPhotoWrite(['/b'], () => new Promise<void>((_, reject) => (exifWritten = () => reject('locked'))), { kind: 'exif' });
  exif.catch(() => {});
  let settled = false;
  const waiting = photoWritesAndStepsSettled().then(() => (settled = true));
  await vi.waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
  // A step queued behind the rating starts its write once that is in.
  let labelled: () => void = () => {};
  const label = photoWorkPending(rating.then(() => new Promise<void>((resolve) => (labelled = resolve))));
  rated();
  exifWritten();
  await rating;
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(settled).toBe(false);
  labelled();
  await label;
  await waiting;
  expect(settled).toBe(true);
});

it('announces a keyword batch that settled, also one that failed, so the XMP count follows both', async () => {
  const announced: Array<{ kind: string; paths: string[]; failed: string[] }> = [];
  const settled = (event: Event) =>
    announced.push((event as CustomEvent<{ kind: string; paths: string[]; failed: string[] }>).detail);
  window.addEventListener(PHOTO_WRITES_SETTLED, settled);
  try {
    // The library saved the keyword for one photograph and not the other.
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockRejectedValueOnce({ message: 'Could not save 1 of 2 photos', failed: ['/b'] });
    await expect(invokeInOrder('add_tag_for_paths', { paths: ['/a', '/b'], tag: 'user:beach' }, 'keyword')).rejects.toMatchObject({ failed: ['/b'] });
    await vi.waitFor(() => expect(announced).toHaveLength(1));
    // A failed write changed nothing, but it still announces, as every
    // settled write does: the bar counts the sidecars again after it.
    expect(announced[0]).toMatchObject({ kind: 'keyword', paths: ['/a', '/b'], failed: ['/b'] });
    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    await invokeInOrder('remove_tag_for_paths', { paths: ['/a'], tag: 'user:beach' }, 'keyword');
    await vi.waitFor(() => expect(announced).toHaveLength(2));
    expect(announced[1]).toMatchObject({ kind: 'keyword', paths: ['/a'], failed: [] });
  } finally {
    window.removeEventListener(PHOTO_WRITES_SETTLED, settled);
  }
});

it('announces only the photographs a keyword batch could not save, the others as written', async () => {
  const announced: Array<{ kind: string; paths: string[]; failed: string[] }> = [];
  const settled = (event: Event) =>
    announced.push((event as CustomEvent<{ kind: string; paths: string[]; failed: string[] }>).detail);
  window.addEventListener(PHOTO_WRITES_SETTLED, settled);
  try {
    // add_tag_for_paths/remove_tag_for_paths report the photographs they
    // could not save (`SaveFailure.failed`); the rest have the keyword.
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockRejectedValueOnce({ message: 'Could not save 1 of 3 photos', failed: ['/b'] });
    await expect(
      invokeInOrder('add_tag_for_paths', { paths: ['/a', '/b', '/c'], tag: 'user:beach' }, 'keyword'),
    ).rejects.toMatchObject({ failed: ['/b'] });
    await vi.waitFor(() => expect(announced).toHaveLength(1));
    expect(announced[0]).toMatchObject({ kind: 'keyword', paths: ['/a', '/b', '/c'], failed: ['/b'] });
    // Only the photographs the command names failed; the others settle as
    // written, so the XMP status does not announce them as failed.
    vi.mocked(invoke).mockRejectedValueOnce({ message: 'Could not save 1 of 2 photos', failed: ['/y'] });
    await expect(
      invokeInOrder('remove_tag_for_paths', { paths: ['/x', '/y'], tag: 'user:beach' }, 'keyword'),
    ).rejects.toMatchObject({ failed: ['/y'] });
    await vi.waitFor(() => expect(announced).toHaveLength(2));
    expect(announced[1]).toMatchObject({ kind: 'keyword', paths: ['/x', '/y'], failed: ['/y'] });
  } finally {
    window.removeEventListener(PHOTO_WRITES_SETTLED, settled);
  }
});
