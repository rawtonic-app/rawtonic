import { cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useRawIsXTrans } from './useRawIsXTrans';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.mocked(invoke).mockReset();
});

it('is unknown until the sensor is read, then per photo', async () => {
  let answer: (value: boolean) => void = () => {};
  vi.mocked(invoke).mockImplementation(() => new Promise((resolve) => (answer = resolve as (v: boolean) => void)));
  const { result, rerender } = renderHook(({ path }) => useRawIsXTrans(path, true), {
    initialProps: { path: '/photos/a.RAF' }
  });
  expect(result.current).toBeNull();
  expect(invoke).toHaveBeenCalledWith(Invokes.RawIsXTrans, { path: '/photos/a.RAF' });
  answer(true);
  await waitFor(() => expect(result.current).toBe(true));
  // Another photo: unknown again until its own answer.
  rerender({ path: '/photos/b.NEF' });
  expect(result.current).toBeNull();
  answer(false);
  await waitFor(() => expect(result.current).toBe(false));
});

it('never asks for other photos and counts a failed read as not X-Trans', async () => {
  const { result } = renderHook(() => useRawIsXTrans('/photos/a.jpg', false));
  expect(result.current).toBe(false);
  expect(invoke).not.toHaveBeenCalled();
  vi.mocked(invoke).mockRejectedValue(new Error('unreadable'));
  vi.spyOn(console, 'error').mockImplementation(() => {});
  const failed = renderHook(() => useRawIsXTrans('/photos/c.RAF', true));
  await waitFor(() => expect(failed.result.current).toBe(false));
});
