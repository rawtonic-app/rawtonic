import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useTranslation } from 'react-i18next';
import { Invokes } from '../components/ui/AppProperties';
import { appCommandEnabled, onAppCommandsChanged, runAppCommand } from '../utils/appCommands';
import { buildMenuSpec, MENU_ID_PREFIX, type BuiltMenu, type MenuItemState, menuItemStates, type Translate } from '../utils/appMenu';
import { normalizeCombo } from '../utils/keyboardUtils';
import { useSettingsStore } from '../store/useSettingsStore';
import { isAnyModalOpen, useUIStore } from '../store/useUIStore';
import { useUpdateStore } from '../store/useUpdateStore';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useMultiViewStore } from '../store/useMultiViewStore';

/** The event app_menu.rs sends with the chosen command's name. */
export const APP_MENU_COMMAND_EVENT = 'app-menu-command';

/** WebKit offers a ⌘ key to the page first and passes it on to the menu
 * only when the page did not take it (no preventDefault; measured on macOS
 * 2026-10-25: a taken key never reached the menu, a key left to a text
 * field chose its item 2–26 ms later). Each declined key with an item's
 * combo is owed one menu echo: the next command with that combo consumes
 * it and goes to the text field or nowhere, never runs the command the
 * webview declined, however late it arrives. A disabled item gets no key
 * from AppKit, so only a key whose item the menu shows enabled is owed an
 * echo. A debt older than this lapses (a state change on its way to the
 * menu), so a later click runs. */
export const KEY_ECHO_MS = 2000;

/** Edit commands that act on a focused text field instead of the app. */
const TEXT_ACTIONS: Record<string, string> = { undo: 'undo', redo: 'redo', select_all: 'selectAll' };

const TEXT_INPUTS = ['text', 'search', 'email', 'url', 'tel', 'password', 'number', ''];

/** A field with text (sliders, checkboxes and buttons have none); Select
 * All also takes a read-only one, Undo and Redo only an editable one. */
export function textFieldFocused(command = 'undo'): boolean {
  const element = document.activeElement;
  const editable = command !== 'select_all';
  if (element instanceof HTMLTextAreaElement) return !editable || !element.readOnly;
  if (element instanceof HTMLInputElement) {
    return TEXT_INPUTS.includes(element.type) && (!editable || !element.readOnly);
  }
  return element instanceof HTMLElement && element.isContentEditable;
}

/** Help and About stay reachable behind a dialog (they open their own). */
const ALWAYS_AVAILABLE = new Set(['about', 'website', 'report_problem', 'keyboard_shortcuts']);

/** Preferences, a dialog or a modal holds the app's commands. */
function commandsHeld(): boolean {
  return (
    useUIStore.getState().isSettingsOpen ||
    !!document.querySelector('dialog[open], [aria-modal="true"]') ||
    isAnyModalOpen(useUIStore.getState())
  );
}

interface MenuDispatch {
  menu: BuiltMenu | null;
  /** ⌘ keys the webview saw lately; their handlers have run by the time a
   * menu command arrives (`defaultPrevented` tells whether one took it). */
  keys: { combo: string; at: number; event: KeyboardEvent }[];
  /** Whether the native menu shows each command enabled (what was sent). */
  shown: Map<string, boolean>;
}

/** Edit commands go to a focused text field; the rest only while no
 * dialog or Preferences holds the app's commands (as their keys). */
export function menuCommandEnabled(command: string): boolean {
  if (command in TEXT_ACTIONS && textFieldFocused(command)) return true;
  return (ALWAYS_AVAILABLE.has(command) || !commandsHeld()) && appCommandEnabled(command);
}

/** Runs a command chosen in the menu bar: the handler its key runs, once. */
export function dispatchMenuCommand(
  command: string,
  dispatch: MenuDispatch,
  now = performance.now(),
): void {
  const combo = dispatch.menu?.combos.get(command);
  dispatch.keys = dispatch.keys.filter((key) => now - key.at < KEY_ECHO_MS);
  const owed = dispatch.keys.findIndex((key) => key.combo === combo && !key.event.defaultPrevented);
  const echo = owed >= 0;
  if (echo) dispatch.keys.splice(owed, 1);
  if (command in TEXT_ACTIONS && textFieldFocused(command)) {
    void invoke(Invokes.AppMenuTextAction, { action: TEXT_ACTIONS[command] }).catch(() => {});
    return;
  }
  if (echo || !menuCommandEnabled(command)) return;
  runAppCommand(command);
}

/** The macOS menu bar (app_menu.rs): its tree follows the language and the
 * user's keys, its enabled/checked state the app (at most every 100 ms, only
 * what changed). Other platforms keep their behaviour (no menu bar). */
export function useAppMenu(): void {
  const { t, i18n } = useTranslation();
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const keybinds = useSettingsStore((s) => s.appSettings?.keybinds);
  const enabled = osPlatform === 'macos';

  useEffect(() => {
    if (!enabled) return;
    const dispatch: MenuDispatch = { menu: buildMenuSpec(t as unknown as Translate, keybinds, menuCommandEnabled), keys: [], shown: new Map() };
    // The state the tree was built with; changes are sent from there.
    const sent = new Map<string, MenuItemState>(menuItemStates(dispatch.menu!, menuCommandEnabled).map((state) => [state.id, state]));
    const show = (state: MenuItemState) => dispatch.shown.set(state.id.slice(MENU_ID_PREFIX.length), state.enabled);
    sent.forEach(show);
    // The command each key equivalent belongs to.
    const commandOf = new Map([...dispatch.menu!.combos].map(([command, combo]) => [combo, command]));
    let timer: ReturnType<typeof setTimeout> | null = null;
    let disposed = false;

    const menu = dispatch.menu!;
    void invoke(Invokes.SetAppMenu, { spec: menu.spec })
      .then(() => schedule())
      .catch((e) => console.warn('Could not set the menu bar:', e));

    const update = () => {
      timer = null;
      if (disposed) return;
      const changed = menuItemStates(menu, menuCommandEnabled).filter((state) => {
        const before = sent.get(state.id);
        return !before || before.enabled !== state.enabled || before.checked !== state.checked;
      });
      if (changed.length === 0) return;
      changed.forEach((state) => {
        sent.set(state.id, state);
        show(state);
      });
      void invoke(Invokes.SetAppMenuState, { items: changed }).catch(() => {});
    };
    const schedule = () => {
      if (timer === null && !disposed) timer = setTimeout(update, 100);
    };
    const unsubscribers = [
      useUIStore.subscribe(schedule),
      useEditorStore.subscribe(schedule),
      useLibraryStore.subscribe(schedule),
      useMultiViewStore.subscribe(schedule),
      useSettingsStore.subscribe(schedule),
      useUpdateStore.subscribe(schedule),
      onAppCommandsChanged(schedule),
    ];
    // Text focus (Edit's Undo/Redo/Select All) and dialogs opening or
    // closing change what is available without a store change.
    window.addEventListener('focusin', schedule);
    window.addEventListener('focusout', schedule);
    const dialogs = new MutationObserver(schedule);
    dialogs.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ['open', 'aria-modal'] });

    // Capture: seen before any handler can stop the key.
    const onKeyDown = (event: KeyboardEvent) => {
      // The menu's key equivalents hold ⌘ (CmdOrCtrl on macOS); a ⌃ key
      // never chooses one.
      if (!event.metaKey || event.ctrlKey) return;
      const combo = normalizeCombo(event, 'macos').join('+');
      const command = commandOf.get(combo);
      // Only an enabled item's key equivalent can come back from AppKit.
      if (!command || !dispatch.shown.get(command)) return;
      const at = performance.now();
      dispatch.keys = [...dispatch.keys.filter((key) => at - key.at < KEY_ECHO_MS), { combo, at, event }];
    };
    window.addEventListener('keydown', onKeyDown, true);
    const unlisten = listen<string>(APP_MENU_COMMAND_EVENT, (event) => {
      dispatchMenuCommand(event.payload, dispatch);
      // AppKit flips a check item's mark when it is chosen, also when the
      // command changed nothing (Develop chosen in Develop): send its state.
      sent.delete(`${MENU_ID_PREFIX}${event.payload}`);
      schedule();
    });

    return () => {
      disposed = true;
      if (timer !== null) clearTimeout(timer);
      unsubscribers.forEach((stop) => stop());
      window.removeEventListener('keydown', onKeyDown, true);
      window.removeEventListener('focusin', schedule);
      window.removeEventListener('focusout', schedule);
      dialogs.disconnect();
      void unlisten.then((stop) => stop());
    };
  }, [enabled, t, i18n.language, keybinds]);
}
