import '../i18n';
import i18n from 'i18next';
import { catalogAction, undo, getUndoLabel, getRedoLabel, useCatalogUndoStore } from '../utils/appUndo';
import { debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useAppMenu, APP_MENU_COMMAND_EVENT, KEY_ECHO_MS, menuCommandEnabled } from './useAppMenu';
import { useKeyboardShortcuts } from './useKeyboardShortcuts';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useUIStore } from '../store/useUIStore';
import { AppSettings, Invokes, Panel } from '../components/ui/AppProperties';
import { menuCommands } from '../utils/appMenu';
import { appCommand, registerAppCommands } from '../utils/appCommands';

const listeners = new Map<string, (event: { payload: unknown }) => void>();
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, callback: (event: { payload: unknown }) => void) => {
    listeners.set(name, callback);
    return Promise.resolve(() => listeners.delete(name));
  }),
}));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), success: vi.fn(), info: vi.fn(), dismiss: vi.fn(), update: vi.fn(), isActive: vi.fn(() => false) } }));

const props = {
  sortedImageList: [],
  handleBackToLibrary: vi.fn(),
  handleDeleteSelected: vi.fn(),
  handleUndoFileOperation: vi.fn(),
  handleRedoFileOperation: vi.fn(),
  handleImageSelect: vi.fn(),
  handlePasteFiles: vi.fn(),
  handleZoomChange: vi.fn(),
};

/** A key as WebKit hands it to the page, before the menu sees it. */
const press = (key: string, modifiers: KeyboardEventInit = {}) =>
  act(() => {
    (document.activeElement ?? window).dispatchEvent(
      new KeyboardEvent('keydown', { key, code: `Key${key.toUpperCase()}`, bubbles: true, cancelable: true, ...modifiers }),
    );
  });
/** The menu item AppKit chose (app_menu.rs sends its command). */
const choose = (command: string) => act(() => listeners.get(APP_MENU_COMMAND_EVENT)!({ payload: command }));

let clock = 1_000;
beforeEach(async () => {
  vi.clearAllMocks();
  debouncedSetHistory.cancel();
  useCatalogUndoStore.setState(useCatalogUndoStore.getInitialState());
  useEditorStore.setState(useEditorStore.getInitialState());
  clock = 1_000;
  vi.spyOn(performance, 'now').mockImplementation(() => clock);
  useSettingsStore.setState({ appSettings: { keybinds: {} } as unknown as AppSettings, osPlatform: 'macos' });
  useLibraryStore.setState({ imageList: [], multiSelectedPaths: [] });
  useUIStore.setState({ activeView: 'library', isSettingsOpen: false, activePanel: Panel.Adjustments });
  renderHook(() => {
    useAppMenu();
    useKeyboardShortcuts(props);
  });
  // The menu shows the registered commands' state (first update).
  await act(() => new Promise((resolve) => setTimeout(resolve, 150)));
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  document.body.innerHTML = '';
});

describe('the macOS menu bar', () => {
  it('updates Undo/Redo action labels and availability as metadata history changes', async () => {
    act(() => useUIStore.setState({ activeView: 'editor' }));
    const latest = (id: string) => vi.mocked(invoke).mock.calls
      .filter(([name]) => name === Invokes.SetAppMenuState)
      .flatMap(([, args]) => (args as { items: { id: string; enabled: boolean; label?: string }[] }).items)
      .filter((item) => item.id === id).at(-1);
    await act(() => new Promise((resolve) => setTimeout(resolve, 150)));
    expect(latest('cmd:undo')).toMatchObject({ enabled: false });
    await act(async () => catalogAction('rating', async () => [{
      path: '/a.nef', before: { kind: 'rating', value: 0 }, after: { kind: 'rating', value: 4 },
    }]));
    await act(() => new Promise((resolve) => setTimeout(resolve, 150)));
    expect(latest('cmd:undo')).toMatchObject({ enabled: true, label: getUndoLabel() });
    vi.mocked(invoke).mockResolvedValueOnce([]);
    await act(async () => undo());
    await act(() => new Promise((resolve) => setTimeout(resolve, 150)));
    expect(latest('cmd:undo')).toMatchObject({ enabled: false });
    expect(latest('cmd:redo')).toMatchObject({ enabled: true, label: getRedoLabel() });
    const field = document.createElement('input');
    document.body.append(field);
    act(() => field.focus());
    await act(() => new Promise((resolve) => setTimeout(resolve, 150)));
    expect(latest('cmd:redo')).toMatchObject({ enabled: true, label: i18n.t('appMenu.items.redo') });
  });

  it('sends the tree built from the shortcut table', () => {
    const call = vi.mocked(invoke).mock.calls.find(([name]) => name === Invokes.SetAppMenu);
    expect(call).toBeTruthy();
    const spec = (call![1] as { spec: { menus: unknown[] } }).spec;
    expect(spec.menus).toHaveLength(7);
  });

  it('runs the handler the key runs, from the menu and from the key alike', () => {
    choose('undo');
    expect(props.handleUndoFileOperation).toHaveBeenCalledOnce();
    clock += 5_000;
    press('z', { metaKey: true });
    expect(props.handleUndoFileOperation).toHaveBeenCalledTimes(2);
  });

  it('runs a key equivalent once', () => {
    // A key the webview took never reaches the menu (WebKit): an item
    // chosen right after it is a click and runs again.
    press('z', { metaKey: true });
    expect(props.handleUndoFileOperation).toHaveBeenCalledOnce();
    clock += 30;
    choose('undo');
    expect(props.handleUndoFileOperation).toHaveBeenCalledTimes(2);
    // A key the webview left (behind Preferences) and its menu echo: neither runs.
    useUIStore.setState({ isSettingsOpen: true });
    press('z', { metaKey: true });
    useUIStore.setState({ isSettingsOpen: false });
    clock += 20;
    choose('undo');
    expect(props.handleUndoFileOperation).toHaveBeenCalledTimes(2);
    // Later, the same item is a click.
    clock += KEY_ECHO_MS;
    choose('undo');
    expect(props.handleUndoFileOperation).toHaveBeenCalledTimes(3);
  });

  it('owes no echo for a ⌃ key (the menu holds ⌘ keys only)', () => {
    document.body.innerHTML = '<input type="text" />';
    document.querySelector('input')!.focus();
    press('e', { ctrlKey: true, shiftKey: true });
    clock += 300;
    choose('toggle_export');
    expect(useUIStore.getState().activePanel).toBe(Panel.Export);
  });

  it('owes no echo for a key whose item the menu shows disabled', async () => {
    const wait = () => act(() => new Promise((resolve) => setTimeout(resolve, 150)));
    useUIStore.setState({ isSettingsOpen: true });
    await wait();
    // AppKit sends nothing for the disabled item…
    press('z', { metaKey: true });
    useUIStore.setState({ isSettingsOpen: false });
    await wait();
    // …so the click right after Preferences closed is a click.
    clock += 300;
    choose('undo');
    expect(props.handleUndoFileOperation).toHaveBeenCalledOnce();
  });

  it('gives Undo, Redo and Select All to a focused text field', () => {
    document.body.innerHTML = '<input type="search" />';
    document.querySelector('input')!.focus();
    // Enabled for the field even where the app has nothing to undo.
    useUIStore.setState({ activeView: 'editor' });
    expect(menuCommandEnabled('undo')).toBe(true);
    press('z', { metaKey: true });
    choose('undo');
    expect(props.handleUndoFileOperation).not.toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledWith(Invokes.AppMenuTextAction, { action: 'undo' });
    choose('select_all');
    expect(invoke).toHaveBeenCalledWith(Invokes.AppMenuTextAction, { action: 'selectAll' });
    // A read-only text (a report to copy) can be selected, not undone.
    document.body.innerHTML = '<dialog open><textarea readonly>report</textarea></dialog>';
    document.querySelector('textarea')!.focus();
    expect(menuCommandEnabled('select_all')).toBe(true);
    expect(menuCommandEnabled('undo')).toBe(false);
  });

  it("leaves a text field's keys to it, while a click on the item still runs the command", () => {
    document.body.innerHTML = '<input type="text" />';
    document.querySelector('input')!.focus();
    press('e', { metaKey: true, shiftKey: true });
    // Its echo, however late (a busy main thread): still the field's key.
    clock += 600;
    choose('toggle_export');
    expect(useUIStore.getState().activePanel).toBe(Panel.Adjustments);
    clock += 5_000;
    choose('toggle_export');
    expect(useUIStore.getState().activePanel).toBe(Panel.Export);
  });

  it('greys out photo commands without a photograph', () => {
    for (const command of ['rate_3', 'color_label_red', 'delete_selected', 'move_to_selects', 'copy_adjustments']) {
      expect(appCommand(command)?.enabled?.(), command).toBe(false);
    }
    useLibraryStore.setState({ multiSelectedPaths: ['/s/a.raf'] });
    expect(appCommand('rate_3')?.enabled?.()).toBe(true);
    expect(appCommand('delete_selected')?.enabled?.()).toBe(true);
  });

  it('runs nothing behind Preferences or a dialog, as the keys', () => {
    let opened = 0;
    const stop = registerAppCommands({ open_session: { run: () => opened++ } });
    document.body.innerHTML = '<dialog open></dialog>';
    expect(menuCommandEnabled('open_session')).toBe(false);
    choose('open_session');
    expect(opened).toBe(0);
    document.body.innerHTML = '';
    choose('open_session');
    expect(opened).toBe(1);
    stop();
    useUIStore.setState({ isSettingsOpen: true });
    choose('undo');
    press('z', { metaKey: true });
    expect(props.handleUndoFileOperation).not.toHaveBeenCalled();
  });

  it('updates enabled and checked state, only what changed', async () => {
    const wait = () => act(() => new Promise((resolve) => setTimeout(resolve, 150)));
    const updates = () => vi.mocked(invoke).mock.calls.filter(([name]) => name === Invokes.SetAppMenuState);
    // The commands registered after the tree was built.
    await wait();
    expect(updates()).toHaveLength(1);
    expect(updates()[0][1]).toMatchObject({ items: expect.arrayContaining([expect.objectContaining({ id: 'cmd:undo', enabled: true })]) });
    useUIStore.setState({ isSettingsOpen: true });
    await wait();
    expect(updates()).toHaveLength(2);
    const items = (updates()[1][1] as { items: { id: string; enabled: boolean }[] }).items;
    expect(items).toContainEqual(expect.objectContaining({ id: 'cmd:undo', enabled: false }));
    expect(items.every((item) => item.enabled === false)).toBe(true);
    // Nothing changed since: nothing more is sent.
    useUIStore.setState({ gridColumns: 7 });
    await wait();
    expect(updates()).toHaveLength(2);
  });

  it('sends a chosen check item its state again (AppKit flipped its mark)', async () => {
    const wait = () => act(() => new Promise((resolve) => setTimeout(resolve, 150)));
    await wait();
    vi.mocked(invoke).mockClear();
    choose('toggle_split_view');
    await wait();
    const update = vi.mocked(invoke).mock.calls.find(([name]) => name === Invokes.SetAppMenuState);
    expect(update?.[1]).toEqual({ items: [{ id: 'cmd:toggle_split_view', enabled: false, checked: false }] });
  });

  it('builds the tree with the state it then keeps up to date', () => {
    // Registered and enabled on its own, but held behind Preferences.
    const stop = registerAppCommands({ view_grid: { run: () => {} }, about: { run: () => {} } });
    useUIStore.setState({ isSettingsOpen: true });
    cleanup();
    vi.mocked(invoke).mockClear();
    renderHook(() => useAppMenu());
    const call = vi.mocked(invoke).mock.calls.find(([name]) => name === Invokes.SetAppMenu)!;
    const items = JSON.stringify(call[1]);
    expect(items).toContain(JSON.stringify({ kind: 'item', id: 'cmd:view_grid', label: i18n.t('appMenu.items.view_grid'), enabled: false, checked: false }));
    expect(items).toContain(JSON.stringify({ kind: 'item', id: 'cmd:about', label: i18n.t('appMenu.items.about', { app: 'Rawtonic' }), enabled: true }));
    stop();
  });

  it('rebuilds live submenus when registered entries change', async () => {
    const stop = registerAppCommands({ 'recipe:web': { run: () => {}, menu: { group: 'recipes', label: 'Web JPEG' } } });
    await act(() => new Promise((resolve) => setTimeout(resolve, 150)));
    const trees = () => vi.mocked(invoke).mock.calls.filter(([name]) => name === Invokes.SetAppMenu);
    expect(JSON.stringify(trees().at(-1))).toContain('Web JPEG');
    stop();
    await act(() => new Promise((resolve) => setTimeout(resolve, 150)));
    expect(JSON.stringify(trees().at(-1))).not.toContain('Web JPEG');
  });

  it('has a handler for every shortcut-table command it places', () => {
    // The rest register in their owners (appMenu.test.ts checks them).
    const owners = new Set(['command_palette', 'about', 'check_updates', 'keyboard_shortcuts', 'website', 'report_problem', 'select_none',
      'clear_recent', 'show_in_finder', 'new_session', 'open_session', 'import_photos', 'flag_pick', 'flag_unflag', 'flag_reject',
      'view_grid', 'view_loupe', 'view_develop']);
    const missing = menuCommands().filter((command) => !owners.has(command) && !appCommand(command));
    expect(missing).toEqual([]);
  });
});

describe('table-driven view and flag keys', () => {
  it.each([
    ['view_grid', 'g'], ['view_loupe', 'e'], ['view_develop', 'd'],
    ['flag_pick', 'p'], ['flag_unflag', 'u'], ['flag_reject', 'x'],
  ])('uses the rebound command handler for %s', (action, original) => {
    const run = vi.fn();
    const stop = registerAppCommands({ [action]: { run } });
    useSettingsStore.setState({ appSettings: { keybinds: { [action]: ['ctrl', 'alt', 'KeyK'] } } as AppSettings });
    press(original, action === 'flag_reject' ? { shiftKey: true } : {});
    expect(run).not.toHaveBeenCalled();
    press('k', { metaKey: true, altKey: true });
    expect(run).toHaveBeenCalledOnce();
    stop();
  });
});
