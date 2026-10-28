import { afterEach, expect, it, vi } from 'vitest';
import {
  allPhotoWritesSettled,
  onPhotoWritesSettled,
  photoWritePending,
  photoWritesSettled,
  submitPhotoWrite,
  type PhotoWritesSettled,
} from './photoWrites';

type Gate = { promise: Promise<unknown>; open(value?: unknown): void; fail(error: unknown): void };
const gate = (): Gate => {
  let open!: (value?: unknown) => void;
  let fail!: (error: unknown) => void;
  const promise = new Promise((resolve, reject) => { open = resolve; fail = reject; });
  return { promise, open, fail };
};
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

afterEach(async () => {
  await allPhotoWritesSettled();
});

it('starts a write at once when nothing is pending for its photographs', () => {
  const run = vi.fn(async () => 'done');
  void submitPhotoWrite(['/a'], run, { kind: 'recipe' });
  expect(run).toHaveBeenCalledTimes(1);
});

it('runs the writes of one photograph in submission order, also after a failure', async () => {
  const order: string[] = [];
  const first = gate();
  const one = submitPhotoWrite(['/a'], () => { order.push('one'); return first.promise; }, { kind: 'x' });
  const two = submitPhotoWrite(['/a', '/b'], async () => { order.push('two'); }, { kind: 'x' });
  const three = submitPhotoWrite(['/b'], async () => { order.push('three'); }, { kind: 'x' });
  await tick();
  expect(order).toEqual(['one']);
  first.fail(new Error('disk full'));
  await expect(one).rejects.toThrow('disk full');
  await two;
  await three;
  expect(order).toEqual(['one', 'two', 'three']);
});

it('lets writes of other photographs run beside each other', async () => {
  const slow = gate();
  void submitPhotoWrite(['/a'], () => slow.promise, { kind: 'x' });
  const other = vi.fn(async () => {});
  await submitPhotoWrite(['/b'], other, { kind: 'x' });
  expect(other).toHaveBeenCalled();
  slow.open();
});

it('keeps virtual copies of one file in its queue (they share its XMP)', async () => {
  const slow = gate();
  void submitPhotoWrite(['/a.dng'], () => slow.promise, { kind: 'x' });
  const copy = vi.fn(async () => {});
  void submitPhotoWrite(['/a.dng?vc=2'], copy, { kind: 'x' });
  expect(photoWritePending('/a.dng?vc=3')).toBe(true);
  await tick();
  expect(copy).not.toHaveBeenCalled();
  slow.open();
  await vi.waitFor(() => expect(copy).toHaveBeenCalled());
});

it('announces each write once, naming the photographs it did not write', async () => {
  const seen: PhotoWritesSettled[] = [];
  const stop = onPhotoWritesSettled((settled) => seen.push(settled));
  try {
    await submitPhotoWrite(['/a', '/b'], async () => ['/b'], { kind: 'paste', failedOf: (failed) => failed, step: 7 });
    await submitPhotoWrite(['/c'], async () => { throw { message: 'locked', failed: ['/c'] }; }, { kind: 'x' }).catch(() => {});
    await submitPhotoWrite(['/d', '/e'], async () => { throw new Error('gone'); }, { kind: 'x' }).catch(() => {});
    await submitPhotoWrite(['/f'], async () => [], { kind: 'list', announce: false });
    await tick();
    expect(seen.map(({ kind, paths, failed, step }) => ({ kind, paths, failed, step }))).toEqual([
      { kind: 'paste', paths: ['/a', '/b'], failed: ['/b'], step: 7 },
      { kind: 'x', paths: ['/c'], failed: ['/c'], step: undefined },
      { kind: 'x', paths: ['/d', '/e'], failed: ['/d', '/e'], step: undefined },
    ]);
    expect(seen[0].id).toBeLessThan(seen[1].id);
  } finally {
    stop();
  }
});

it('settles the queue of a photograph only when its last write did', async () => {
  const first = gate();
  void submitPhotoWrite(['/a'], () => first.promise, { kind: 'x' });
  const second = gate();
  void submitPhotoWrite(['/a'], () => second.promise, { kind: 'x' });
  const read = photoWritesSettled('/a')!;
  let done = false;
  void read.then(() => { done = true; });
  first.open();
  await tick();
  expect(done).toBe(false);
  second.open();
  await vi.waitFor(() => expect(done).toBe(true));
  expect(photoWritesSettled('/a')).toBeUndefined();
});

it('waits for writes submitted while waiting before it reports everything settled', async () => {
  const first = gate();
  void submitPhotoWrite(['/a'], () => first.promise, { kind: 'x' });
  let late = false;
  const all = allPhotoWritesSettled();
  void submitPhotoWrite(['/b'], async () => { await tick(); late = true; }, { kind: 'x' });
  first.open();
  await all;
  expect(late).toBe(true);
});
