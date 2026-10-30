import { act, cleanup, render } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import PreviewBuildStatus from './PreviewBuildStatus';
import { Invokes, type PreviewBuildProgress } from '../ui/AppProperties';
import { resetActivityStore, useActivityStore } from '../../store/useActivityStore';
import '../../i18n';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

let emit: (progress: PreviewBuildProgress) => void;
const progress = (patch: Partial<PreviewBuildProgress>): PreviewBuildProgress => ({
  done: 0, total: 0, running: false, stopped: false, skipped: 0, failedCount: 0, failed: [], ...patch,
});
const entry = () => useActivityStore.getState().entries.find((e) => e.id === 'previews:build');

beforeEach(() => {
  resetActivityStore();
  vi.mocked(listen).mockImplementation((_event, handler) => {
    emit = (payload) => (handler as (event: { payload: PreviewBuildProgress }) => void)({ payload });
    return Promise.resolve(() => {});
  });
  vi.mocked(invoke).mockImplementation((command) =>
    Promise.resolve(command === Invokes.PreviewBuildProgress ? progress({}) : 0),
  );
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it('runs in the activity centre with progress and Cancel, then goes when all went well', async () => {
  const { container } = render(<PreviewBuildStatus />);
  await act(async () => {});
  expect(container.innerHTML).toBe('');
  expect(entry()).toBeUndefined();
  // Expired 1:1 previews are discarded when the library opens.
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === Invokes.ApplyPreviewLimits)).toBe(true);

  await act(async () => emit(progress({ running: true, done: 3, total: 10 })));
  expect(entry()).toMatchObject({ status: 'running', title: 'Building previews 3 / 10', progress: { done: 3, total: 10 } });
  await act(async () => entry()!.cancel!());
  expect(invoke).toHaveBeenCalledWith(Invokes.StopPreviewBuild);

  await act(async () => emit(progress({ done: 10, total: 10 })));
  expect(entry()).toBeUndefined();
});

it('keeps what was not built under Needs attention until dismissed', async () => {
  render(<PreviewBuildStatus />);
  await act(async () => {});
  vi.useFakeTimers();
  await act(async () =>
    emit(progress({
      done: 4, total: 4, failedCount: 1,
      failed: [{ path: '/s/Capture/A.RAF', reason: 'The RAW file is missing (offline)' }],
    })),
  );
  expect(entry()).toMatchObject({
    status: 'warning',
    title: 'Previews not built: 1',
    detail: 'A.RAF: The RAW file is missing (offline)',
    paths: ['/s/Capture/A.RAF'],
  });
  act(() => vi.advanceTimersByTime(60 * 60 * 1000));
  expect(entry()).toBeTruthy();
  vi.useRealTimers();
  useActivityStore.getState().dismiss('previews:build');
  expect(entry()).toBeUndefined();
});
