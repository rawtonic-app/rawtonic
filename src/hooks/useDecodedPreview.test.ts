import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useDecodedPreview } from './useDecodedPreview';

let finish: (() => void)[];
beforeEach(() => {
  finish = [];
  vi.stubGlobal('Image', class {
    src = '';
    onload = null;
    decode() { return new Promise<void>((resolve) => finish.push(resolve)); }
  });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
const decoded = (i: number) => act(async () => { finish[i](); });

it('replaces a frame atomically with its ROI coordinates, without a fade timer', async () => {
  const first = { url: 'blob:first', x: .1 };
  const next = { url: 'blob:next', x: .4 };
  const { result, rerender } = renderHook(({ frame }) => useDecodedPreview(frame, '/a.raw'),
    { initialProps: { frame: first } });
  expect(result.current).toBeNull();
  await decoded(0);expect(result.current).toBe(first);
  rerender({ frame: next });expect(result.current).toBe(first);
  await decoded(1);expect(result.current).toBe(next);
});

it('drops stale decode completions and never shows a previous photo in a new frame', async () => {
  const { result, rerender } = renderHook(({ frame, path }) => useDecodedPreview(frame, path),
    { initialProps: { frame: { url: 'blob:first' }, path: '/a.raw' } });
  await decoded(0);
  rerender({ frame: { url: 'blob:old-request' }, path: '/a.raw' });
  rerender({ frame: { url: 'blob:next-photo' }, path: '/b.raw' });
  expect(result.current).toBeNull();
  await decoded(2);expect(result.current?.url).toBe('blob:next-photo');
  await decoded(1);expect(result.current?.url).toBe('blob:next-photo');
});
