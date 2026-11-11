import '../i18n';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import WindowControls from './WindowControls';

const win = vi.hoisted(() => ({
  isMaximized: vi.fn(),
  onResized: vi.fn(),
  minimize: vi.fn(),
  toggleMaximize: vi.fn(),
  close: vi.fn()
}));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => win }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function setup(maximized: boolean) {
  win.isMaximized.mockResolvedValue(maximized);
  win.onResized.mockResolvedValue(() => {});
  win.minimize.mockResolvedValue(undefined);
  win.toggleMaximize.mockResolvedValue(undefined);
  win.close.mockResolvedValue(undefined);
  render(<WindowControls />);
}

it('has labelled minimize, maximize and close buttons that call the window', async () => {
  setup(false);
  fireEvent.click(screen.getByRole('button', { name: 'Minimize window' }));
  fireEvent.click(screen.getByRole('button', { name: 'Maximize window' }));
  fireEvent.click(screen.getByRole('button', { name: 'Close window' }));
  expect(win.minimize).toHaveBeenCalledOnce();
  expect(win.toggleMaximize).toHaveBeenCalledOnce();
  expect(win.close).toHaveBeenCalledOnce();
  await waitFor(() => expect(win.isMaximized).toHaveBeenCalled());
});

it('offers Restore while the window is maximized', async () => {
  setup(true);
  expect(await screen.findByRole('button', { name: 'Restore window' })).toBeTruthy();
});
