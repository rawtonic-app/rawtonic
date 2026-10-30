import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import PreviewBuildStatus from './PreviewBuildStatus';
import { Invokes, type PreviewBuildProgress } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));
vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, options?: Record<string, unknown>) =>
      options ? `${key} ${Object.values(options).join(' ')}` : key,
  }),
}));

let emit: (progress: PreviewBuildProgress) => void;
const progress = (patch: Partial<PreviewBuildProgress>): PreviewBuildProgress => ({
  done: 0, total: 0, running: false, stopped: false, skipped: 0, failedCount: 0, failed: [], ...patch,
});

beforeEach(() => {
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

it('shows progress with a Stop while previews are built, then nothing when all went well', async () => {
  render(<PreviewBuildStatus />);
  await act(async () => {});
  expect(screen.queryByRole('status')).toBeNull();
  // Expired 1:1 previews are discarded when the library opens.
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === Invokes.ApplyPreviewLimits)).toBe(true);

  await act(async () => emit(progress({ running: true, done: 3, total: 10 })));
  expect(screen.getByRole('status').textContent).toContain('previews.status.short 3 10');
  expect(screen.getByRole('status').getAttribute('title')).toBe('previews.status.building 3 10');
  fireEvent.click(screen.getByLabelText('previews.status.stop'));
  expect(invoke).toHaveBeenCalledWith(Invokes.StopPreviewBuild);

  await act(async () => emit(progress({ done: 10, total: 10 })));
  expect(screen.queryByRole('status')).toBeNull();
});

it('keeps failure details available until dismissed', async () => {
  render(<PreviewBuildStatus />);
  await act(async () => {});
  vi.useFakeTimers();
  await act(async () =>
    emit(progress({
      done: 4, total: 4, failedCount: 1,
      failed: [{ path: '/s/Capture/A.RAF', reason: 'The RAW file is missing (offline)' }],
    })),
  );
  const status = screen.getByRole('status');
  expect(status.textContent).toContain('previews.status.notBuilt 1');
  expect(screen.getByText('A.RAF: The RAW file is missing (offline)')).toBeTruthy();
  expect(status.querySelector('details')).toBeTruthy();
  act(() => vi.advanceTimersByTime(13000));
  expect(screen.getByRole('status')).toBeTruthy();
  vi.useRealTimers();
  fireEvent.click(screen.getByLabelText('previews.status.dismiss'));
  expect(screen.queryByRole('status')).toBeNull();
});
