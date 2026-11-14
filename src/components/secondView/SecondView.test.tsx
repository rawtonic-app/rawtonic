import '../../i18n';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import SecondView from './SecondView';
import { useCompactBar } from '../../window/useCompactBar';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    setTitle: async () => {},
    isFullscreen: async () => false,
    onResized: async () => () => {},
    onScaleChanged: async () => () => {},
    onFocusChanged: async () => () => {}
  })
}));
vi.mock('../../window/useCompactBar', () => ({ useCompactBar: vi.fn(() => false) }));

beforeEach(() => {
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === 'second_window_state'
      ? { path: '/photos/IMG_0001.NEF', locked: false, zoom: null, shown: null, pageDraws: false }
      : null
  );
});
afterEach(() => {
  cleanup();
  vi.mocked(useCompactBar).mockReturnValue(false);
});

it('keeps every toolbar label on one line and tooltips on the global tooltip', async () => {
  render(<SecondView />);
  const toolbar = await screen.findByRole('toolbar');
  await screen.findByText('IMG_0001.NEF');
  expect(toolbar.className).toContain('whitespace-nowrap');
  expect(toolbar.className).toContain('max-w-[calc(100%-1rem)]');
  expect(screen.getByText('100 %')).toBeTruthy();
  const lock = screen.getByRole('button', { name: 'Lock' });
  expect(lock.getAttribute('data-tooltip')).toContain('Keep showing this photo');
  expect(lock.getAttribute('title')).toBeNull();
});

it('below the width the toolbar needs: no file name, Lock as an icon with its name', async () => {
  vi.mocked(useCompactBar).mockReturnValue(true);
  render(<SecondView />);
  const lock = await screen.findByRole('button', { name: 'Lock' });
  await waitFor(() => expect(screen.queryByText('IMG_0001.NEF')).toBeNull());
  expect(lock.textContent).toBe('');
  expect(lock.getAttribute('data-tooltip')).toBeTruthy();
  expect(screen.getByText('100 %')).toBeTruthy();
});
