import { cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useRawSensorKind } from './useRawSensorKind';

import wire from '../contracts/rawSensorKind.json';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.mocked(invoke).mockReset();
});

it('is unknown until the sensor is read, then per photo', async () => {
  let answer: (value: string) => void = () => {};
  vi.mocked(invoke).mockImplementation(() => new Promise((resolve) => (answer = resolve as (v: string) => void)));
  const { result, rerender } = renderHook(({ path }) => useRawSensorKind(path, true), {
    initialProps: { path: '/photos/a.RAF' }
  });
  expect(result.current).toBeNull();
  expect(invoke).toHaveBeenCalledWith(Invokes.RawSensorKind, { path: '/photos/a.RAF' });
  answer(wire[1]);
  await waitFor(() => expect(result.current).toBe('xTrans'));
  // Another photo: unknown again until its own answer.
  rerender({ path: '/photos/b.NEF' });
  expect(result.current).toBeNull();
  answer(wire[0]);
  await waitFor(() => expect(result.current).toBe('bayer'));
});

it('never asks for other photos and reports a failed sensor read', async () => {
  const { result } = renderHook(() => useRawSensorKind('/photos/a.jpg', false));
  expect(result.current).toBeNull();
  expect(invoke).not.toHaveBeenCalled();
  vi.mocked(invoke).mockRejectedValue(new Error('unreadable'));
  vi.spyOn(console, 'error').mockImplementation(() => {});
  const failed = renderHook(() => useRawSensorKind('/photos/c.RAF', true));
  await waitFor(() => expect(failed.result.current).toBe('unknown'));
});

it('renders the native unknown sensor status', async () => {
  vi.mocked(invoke).mockResolvedValue(wire[2]);
  const { result } = renderHook(() => useRawSensorKind('/photos/c.RAF', true));
  await waitFor(() => expect(result.current).toBe('unknown'));
});
