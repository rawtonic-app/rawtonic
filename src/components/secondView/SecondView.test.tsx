import '../../i18n';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import SecondView from './SecondView';
import { useNeedsCompact } from '../../window/useNeedsCompact';

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
vi.mock('../../window/useNeedsCompact', () => ({ useNeedsCompact: vi.fn(() => false) }));

beforeEach(() => {
  vi.mocked(invoke).mockImplementation(async (command) =>
    command === 'second_window_state'
      ? { path: '/photos/IMG_0001.NEF', locked: false, zoom: null, shown: null, pageDraws: false }
      : null
  );
});
afterEach(() => {
  cleanup();
  vi.mocked(useNeedsCompact).mockReturnValue(false);
});

it('keeps every toolbar label on one line and tooltips on the global tooltip', async () => {
  render(<SecondView />);
  const toolbar = await screen.findByRole('toolbar');
  await within(toolbar).findByText('IMG_0001.NEF');
  expect(toolbar.className).toContain('whitespace-nowrap');
  expect(toolbar.className).toContain('max-w-[calc(100%-1rem)]');
  expect(within(screen.getByRole('toolbar')).getByText('100 %')).toBeTruthy();
  const lock = screen.getByRole('button', { name: 'Lock' });
  expect(lock.getAttribute('data-tooltip')).toContain('Keep showing this photo');
  expect(lock.getAttribute('title')).toBeNull();
});

it('below the width the toolbar needs: no file name, Lock as an icon with its name', async () => {
  vi.mocked(useNeedsCompact).mockReturnValue(true);
  render(<SecondView />);
  const lock = await screen.findByRole('button', { name: 'Lock' });
  await waitFor(() => expect(within(screen.getByRole('toolbar')).queryByText('IMG_0001.NEF')).toBeNull());
  expect(lock.textContent).toBe('');
  expect(lock.getAttribute('data-tooltip')).toBeTruthy();
  expect(within(screen.getByRole('toolbar')).getByText('100 %')).toBeTruthy();
});
