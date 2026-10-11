import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import PreviewSettings from './PreviewSettings';
import { type AppSettings, Invokes } from '../ui/AppProperties';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

beforeEach(() => {
  vi.mocked(invoke).mockImplementation((command) =>
    Promise.resolve(
      command === Invokes.PreviewCacheUsage
        ? { session: null, outside: null }
        : 0,
    ),
  );
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it('offers a size limit for Standard previews, 10 GB unless chosen, and applies a new one at once', async () => {
  const onSettingsChange = vi.fn();
  render(<PreviewSettings appSettings={{} as AppSettings} onSettingsChange={onSettingsChange} />);
  await act(async () => {});
  expect(screen.getByText('previews.settings.standardLimit')).toBeTruthy();
  // The limit's dropdown shows the default.
  const trigger = screen.getByText('10 GB');
  fireEvent.click(trigger);
  fireEvent.click(screen.getByText('previews.settings.noLimit'));
  await act(async () => {});
  expect(onSettingsChange).toHaveBeenCalledWith({ standardPreviewsLimitGb: 0 });
  expect(vi.mocked(invoke).mock.calls.some(([command]) => command === Invokes.ApplyPreviewLimits)).toBe(true);
});
