import { expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { CATALOG_CHANGED, invokeInOrder } from './catalogWrites';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

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
  const { allPhotoWritesSettled, photoWorkPending, submitPhotoWrite } = await import('./photoWrites');
  let rated: (value?: unknown) => void = () => {};
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementationOnce(() => new Promise((resolve) => (rated = resolve)));
  const rating = invokeInOrder('set_rating_for_paths', { paths: ['/a'], rating: 5 });
  let exifWritten: () => void = () => {};
  // An EXIF write that fails still settles.
  const exif = submitPhotoWrite(['/b'], () => new Promise<void>((_, reject) => (exifWritten = () => reject('locked'))), { kind: 'exif' });
  exif.catch(() => {});
  let settled = false;
  const waiting = allPhotoWritesSettled().then(() => (settled = true));
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
