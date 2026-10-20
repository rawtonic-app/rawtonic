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
  await Promise.resolve();
  // Only the first is asked for until it has answered.
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
