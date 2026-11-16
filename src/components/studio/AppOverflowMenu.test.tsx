import '../../i18n';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import AppOverflowMenu from './AppOverflowMenu';
import { ContextMenuProvider } from '../../context/ContextMenuContext';
import { useSettingsStore } from '../../store/useSettingsStore';
import { useUIStore } from '../../store/useUIStore';
import { registerAppCommands } from '../../utils/appCommands';
import { catalogAction, redo, undo, useCatalogUndoStore } from '../../utils/appUndo';
import { useLibraryStore } from '../../store/useLibraryStore';
import { invoke } from '@tauri-apps/api/core';

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => false, invoke: vi.fn(async () => []) }));
let stop = () => {};
beforeEach(() => {
  useUIStore.setState({ isSettingsOpen: false });
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      disconnect() {}
    }
  );
});
afterEach(() => {
  cleanup();
  stop();
  vi.unstubAllGlobals();
});
it.each(['linux', 'windows'])('shows and dispatches the shared command tree on %s', async (osPlatform) => {
  useSettingsStore.setState({ osPlatform });
  const run = vi.fn();
  stop = registerAppCommands({
    open_session: { run },
    new_session: { enabled: () => false, run: vi.fn() }
  });
  render(
    <ContextMenuProvider>
      <AppOverflowMenu />
    </ContextMenuProvider>
  );
  fireEvent.click(screen.getByRole('button', { name: 'More' }));
  for (const name of ['File', 'Edit', 'Photo', 'View', 'Help'])
    expect(screen.getByRole('menuitem', { name })).toBeTruthy();
  fireEvent.click(screen.getByRole('menuitem', { name: 'File' }));
  const open = await screen.findByRole('menuitem', {
    name: 'Open Session or Catalog…'
  });
  expect(screen.getByRole('menuitem', { name: 'New Session or Catalog…' }).hasAttribute('disabled')).toBe(true);
  fireEvent.click(open);
  expect(run).toHaveBeenCalledOnce();
});
it('is not shown on macOS, where the menu bar holds every command', () => {
  useSettingsStore.setState({ osPlatform: 'macos' });
  render(
    <ContextMenuProvider>
      <AppOverflowMenu />
    </ContextMenuProvider>
  );
  // The menu bar's Help holds Report a problem…: a one-item ⋯ menu is not shown.
  expect(screen.queryByRole('button', { name: 'More' })).toBeNull();
});

it('lists Report a problem… once, inside Help', async () => {
  useSettingsStore.setState({ osPlatform: 'linux' });
  render(
    <ContextMenuProvider>
      <AppOverflowMenu />
    </ContextMenuProvider>
  );
  fireEvent.click(screen.getByRole('button', { name: 'More' }));
  expect(screen.queryByRole('menuitem', { name: /report a problem/i })).toBeNull();
  fireEvent.click(screen.getByRole('menuitem', { name: 'Help' }));
  expect(await screen.findAllByRole('menuitem', { name: /report a problem/i })).toHaveLength(1);
});

it.each(['linux', 'windows'])(
  'names the action Undo and Redo will take in the Edit menu on %s, after every history change',
  async (osPlatform) => {
    useSettingsStore.setState({ osPlatform });
    useLibraryStore.setState({ sessionDirectory: '/job', imageRatings: {}, imageFlags: {}, imageList: [] });
    useCatalogUndoStore.setState(useCatalogUndoStore.getInitialState());
    vi.mocked(invoke).mockImplementation(async (_command, args) =>
      ((args as { values?: { path: string; value: unknown }[] })?.values ?? []).map(({ path, value }) => ({
        path,
        before: value,
        after: value
      }))
    );
    const rate = [
      { path: '/a.nef', before: { kind: 'rating' as const, value: 0 }, after: { kind: 'rating' as const, value: 4 } }
    ];
    const open = () => {
      fireEvent.click(screen.getByRole('button', { name: 'More' }));
      fireEvent.click(screen.getByRole('menuitem', { name: 'Edit' }));
    };
    render(
      <ContextMenuProvider>
        <AppOverflowMenu />
      </ContextMenuProvider>
    );
    await catalogAction('rating', async () => rate);
    open();
    expect(await screen.findByRole('menuitem', { name: 'Undo Set Rating' })).toBeTruthy();
    cleanup();
    render(
      <ContextMenuProvider>
        <AppOverflowMenu />
      </ContextMenuProvider>
    );
    await undo();
    open();
    expect(await screen.findByRole('menuitem', { name: 'Redo Set Rating' })).toBeTruthy();
    // Nothing left to undo: the plain label, disabled.
    expect(screen.getByRole('menuitem', { name: 'Undo' }).hasAttribute('disabled')).toBe(true);
    await redo();
  }
);

it.each(['linux', 'windows'])(
  'shows each command key and opens on F10 and a lone Alt, then walks with the arrows on %s (UXR4L-04)',
  async (osPlatform) => {
    useSettingsStore.setState({ osPlatform, appSettings: null });
    const run = vi.fn();
    stop = registerAppCommands({ toggle_export: { run } });
    const outside = vi.fn();
    window.addEventListener('keydown', outside);
    render(
      <ContextMenuProvider>
        <AppOverflowMenu />
      </ContextMenuProvider>
    );
    // F10 opens the menu with its first row focused; ↓ → walk into File.
    fireEvent.keyDown(window, { key: 'F10', code: 'F10' });
    const file = screen.getByRole('menuitem', { name: 'File' });
    expect(document.activeElement).toBe(file);
    fireEvent.keyDown(window, { key: 'ArrowDown', code: 'ArrowDown' });
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'Edit' }));
    fireEvent.keyDown(window, { key: 'ArrowUp', code: 'ArrowUp' });
    fireEvent.keyDown(window, { key: 'ArrowRight', code: 'ArrowRight' });
    const exportRow = await screen.findByRole('menuitem', { name: 'Export…' });
    // The key at the row's right, as the platform names it.
    expect(exportRow.getAttribute('aria-keyshortcuts')).toBe('Ctrl+Shift+E');
    expect(exportRow.textContent).toContain('Ctrl+Shift+E');
    expect(document.activeElement?.closest('[role="menu"]')?.getAttribute('data-menu-path')).toBe('0');
    // ← closes the submenu and goes back to File; the app never saw the keys.
    fireEvent.keyDown(window, { key: 'ArrowLeft', code: 'ArrowLeft' });
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'File' }));
    expect(outside).not.toHaveBeenCalled();
    // A letter goes to the row starting with it; Escape closes the menu.
    fireEvent.keyDown(window, { key: 'v', code: 'KeyV' });
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'View' }));
    fireEvent.keyDown(window, { key: 'Escape', code: 'Escape' });
    await vi.waitFor(() => expect(screen.queryByRole('menuitem', { name: 'File' })).toBeNull());
    // Alt pressed and released alone opens it; Alt+click (layer solo) does not.
    fireEvent.keyDown(window, { key: 'Alt', code: 'AltLeft', altKey: true });
    fireEvent.pointerDown(document.body);
    fireEvent.keyUp(window, { key: 'Alt', code: 'AltLeft' });
    expect(screen.queryByRole('menuitem', { name: 'File' })).toBeNull();
    fireEvent.keyDown(window, { key: 'Alt', code: 'AltLeft', altKey: true });
    fireEvent.keyUp(window, { key: 'Alt', code: 'AltLeft' });
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: 'File' }));
    // Enter on a row runs it.
    fireEvent.keyDown(window, { key: 'Enter', code: 'Enter' });
    const exportItem = await screen.findByRole('menuitem', { name: 'Export…' });
    exportItem.focus();
    fireEvent.keyDown(window, { key: 'Enter', code: 'Enter' });
    expect(run).toHaveBeenCalledOnce();
    window.removeEventListener('keydown', outside);
  }
);

it('opens nothing while a dialog holds the app', () => {
  useSettingsStore.setState({ osPlatform: 'windows' });
  useUIStore.setState({ isSettingsOpen: true });
  render(
    <ContextMenuProvider>
      <AppOverflowMenu />
    </ContextMenuProvider>
  );
  fireEvent.keyDown(window, { key: 'F10', code: 'F10' });
  expect(screen.queryByRole('menuitem', { name: 'File' })).toBeNull();
});
