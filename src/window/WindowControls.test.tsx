import '../i18n';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
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
const ipc = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>()
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: ipc.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) => {
    ipc.listeners.set(name, handler);
    return Promise.resolve(() => ipc.listeners.delete(name));
  }
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function setup(maximized: boolean, snapLayouts = false) {
  win.isMaximized.mockResolvedValue(maximized);
  win.onResized.mockResolvedValue(() => {});
  win.minimize.mockResolvedValue(undefined);
  win.toggleMaximize.mockResolvedValue(undefined);
  win.close.mockResolvedValue(undefined);
  ipc.invoke.mockResolvedValue(undefined);
  return render(<WindowControls snapLayouts={snapLayouts} />);
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

it('reports nothing to the snap-layouts overlay unless asked (macOS, Linux)', async () => {
  setup(false);
  await waitFor(() => expect(win.isMaximized).toHaveBeenCalled());
  expect(ipc.invoke).not.toHaveBeenCalled();
});

it('places the snap-layouts overlay in physical pixels and draws its hover and press', async () => {
  vi.stubGlobal('devicePixelRatio', 1.25);
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    }
  );
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
    left: 800,
    top: 4,
    width: 32,
    height: 32
  } as DOMRect);
  const view = setup(false, true);
  expect(ipc.invoke).toHaveBeenCalledWith('set_maximize_button_rect', {
    rect: { x: 1000, y: 5, width: 40, height: 40 }
  });
  const button = screen.getByRole('button', { name: 'Maximize window' });
  await waitFor(() => expect(ipc.listeners.has('maximize-button-state')).toBe(true));
  act(() => ipc.listeners.get('maximize-button-state')!({ payload: { hovered: true, pressed: true } }));
  expect(button.className).toBe('is-hovered is-pressed');
  view.unmount();
  expect(ipc.invoke).toHaveBeenLastCalledWith('set_maximize_button_rect', { rect: null });
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
