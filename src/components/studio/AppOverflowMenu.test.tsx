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
    name: 'Open Project or Catalog…'
  });
  expect(screen.getByRole('menuitem', { name: 'New Project or Catalog…' }).hasAttribute('disabled')).toBe(true);
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
