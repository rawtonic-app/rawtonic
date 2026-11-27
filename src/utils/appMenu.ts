/**
 * The macOS menu bar, generated from the shortcut table (keyboardUtils) and
 * the app's commands (appCommands): this file only places the commands in
 * menus. Labels come from the locale files (`appMenu.*`, else the shortcut's
 * description), shortcuts from the user's keys, so the menu, the keys and
 * the shortcut list in Preferences cannot drift apart. src-tauri's
 * app_menu.rs builds the native menu from `buildMenuSpec`; Windows and Linux
 * show the same tree in the ⋯ menu (commandPalette's `overflowCommands`).
 *
 * Every item shows its key, single keys included (UXR4M-05, UX4-06), named
 * as the active keyboard layout types it (keyboardLayout.ts, UXR4M-06).
 *
 * Grouping (G53, taken from Lightroom Classic's and Capture One's menus
 * where each is clearer): the photo commands in one Photo menu (Lightroom;
 * Capture One spreads them over Image and Select), the views and the zoom
 * in View (both), Copy/Paste Settings in Edit (Capture One's Adjustments
 * menu splits them from Undo), File for libraries, import and export.
 */
import { comboKeyChar, effectiveCombos, formatCombo, KEYBIND_DEFINITIONS, shortcutHint } from './keyboardUtils';
import { appCommand, appCommandEnabled, appMenuCommands } from './appCommands';

export type NativeRole =
  | 'services'
  | 'hide'
  | 'hideOthers'
  | 'showAll'
  | 'quit'
  | 'cut'
  | 'copy'
  | 'paste'
  | 'minimize'
  | 'zoom'
  | 'fullscreen'
  | 'bringAllToFront'
  | 'closeWindow';

type Entry =
  | 'separator'
  | {
      command: string;
      /** A check item: the command's `checked()` shows a check mark. */
      check?: boolean;
      /** The key the menu shows when the action has several (Redo: ⇧⌘Z). */
      preferCombo?: string[];
    }
  | { dynamic: 'recent' | 'recipes' }
  | { submenu: string; items: Entry[] }
  | { native: NativeRole };

interface TopMenu {
  menu: 'app' | 'file' | 'edit' | 'photo' | 'view' | 'window' | 'help';
  role?: 'window' | 'help';
  items: Entry[];
}

const c = (command: string, extra: Omit<Extract<Entry, { command: string }>, 'command'> = {}): Entry => ({
  command,
  ...extra
});

export const MENU_LAYOUT: TopMenu[] = [
  {
    menu: 'app',
    items: [
      c('about'),
      c('check_updates'),
      'separator',
      c('open_settings'),
      'separator',
      { native: 'services' },
      'separator',
      { native: 'hide' },
      { native: 'hideOthers' },
      { native: 'showAll' },
      'separator',
      { native: 'quit' }
    ]
  },
  {
    menu: 'file',
    items: [
      c('new_session'),
      c('open_session'),
      { dynamic: 'recent' },
      'separator',
      c('import_photos'),
      c('tether_capture'),
      c('find_duplicates'),
      'separator',
      c('toggle_export'),
      c('export_previous'),
      { dynamic: 'recipes' },
      c('proof_sheet'),
      'separator',
      { native: 'closeWindow' }
    ]
  },
  {
    menu: 'edit',
    items: [
      // Shared metadata/edit/file history; a focused text field keeps its own.
      c('undo'),
      c('redo', { preferCombo: ['ctrl', 'shift', 'KeyZ'] }),
      'separator',
      { native: 'cut' },
      { native: 'copy' },
      { native: 'paste' },
      'separator',
      c('select_all'),
      c('select_none'),
      'separator',
      c('copy_adjustments'),
      c('paste_adjustments'),
      c('paste_from_previous'),
      c('sync_settings'),
      c('reset_adjustments'),
      c('reset_to_neutral'),
      'separator',
      c('copy_image_path')
    ]
  },
  {
    menu: 'photo',
    items: [
      {
        submenu: 'rating',
        items: [c('rate_0'), c('rate_1'), c('rate_2'), c('rate_3'), c('rate_4'), c('rate_5')]
      },
      c('toggle_auto_advance', { check: true }),
      { submenu: 'flag', items: [c('flag_pick'), c('flag_unflag'), c('flag_reject')] },
      {
        submenu: 'colorLabel',
        items: [
          c('color_label_none'),
          'separator',
          c('color_label_red'),
          c('color_label_yellow'),
          c('color_label_green'),
          c('color_label_blue'),
          c('color_label_purple')
        ]
      },
      {
        submenu: 'stacking',
        items: [
          c('stack_group'),
          c('stack_unstack'),
          c('stack_remove'),
          c('stack_move_top'),
          'separator',
          c('stack_split'),
          c('stack_move_up'),
          c('stack_move_down'),
          'separator',
          c('stack_toggle'),
          c('stack_collapse_all'),
          c('stack_expand_all'),
          'separator',
          c('auto_stack')
        ]
      },
      'separator',
      c('rotate_left'),
      c('rotate_right'),
      'separator',
      c('match_light'),
      c('shift_capture_time'),
      c('auto_adjust'),
      c('auto_white_balance'),
      'separator',
      c('open_with'),
      c('edit_with'),
      'separator',
      c('virtual_copy'),
      c('move_to_selects'),
      c('show_in_finder'),
      'separator',
      c('delete_selected')
    ]
  },
  {
    menu: 'view',
    items: [
      c('command_palette'),
      c('show_activity'),
      'separator',
      c('view_grid', { check: true }),
      c('view_loupe', { check: true }),
      c('view_develop', { check: true }),
      'separator',
      c('toggle_fullscreen', { check: true }),
      c('toggle_metadata', { check: true }),
      c('toggle_split_view', { check: true }),
      c('compare_left_right', { check: true }),
      c('compare_top_bottom', { check: true }),
      c('compare_side_by_side', { check: true }),
      c('toggle_original', { check: true }),
      c('toggle_multi_view', { check: true }),
      'separator',
      c('toggle_analytics', { check: true }),
      c('toggle_clipping', { check: true }),
      c('cycle_surroundings', { check: true }),
      c('toggle_pointer_readout', { check: true }),
      // Shift+\ stays the webview's alone (no ⌘: not a key equivalent).
      c('toggle_camera_preview', { check: true }),
      c('toggle_soft_proof', { check: true }),
      c('toggle_proof_margin', { check: true }),
      'separator',
      {
        submenu: 'panels',
        items: [
          c('toggle_side_panels'),
          c('toggle_all_panels'),
          'separator',
          c('toggle_left_panel'),
          c('toggle_right_panel'),
          c('toggle_bottom_panel'),
          'separator',
          c('dock_filmstrip_bottom', { check: true }),
          c('dock_filmstrip_left', { check: true }),
          c('dock_filmstrip_right', { check: true })
        ]
      },
      'separator',
      c('zoom_in'),
      c('zoom_out'),
      c('zoom_fit'),
      c('zoom_100'),
      c('zoom_toggle')
    ]
  },
  {
    menu: 'window',
    role: 'window',
    items: [
      { native: 'minimize' },
      { native: 'zoom' },
      'separator',
      c('toggle_second_window', { check: true }),
      'separator',
      { native: 'fullscreen' },
      'separator',
      { native: 'bringAllToFront' }
    ]
  },
  {
    menu: 'help',
    role: 'help',
    items: [c('keyboard_shortcuts'), c('website'), 'separator', c('report_problem')]
  }
];

/** The native items' keys, so a command never takes one of them. */
const NATIVE_COMBOS: Partial<Record<NativeRole, string>> = {
  hide: 'ctrl+KeyH',
  hideOthers: 'ctrl+alt+KeyH',
  quit: 'ctrl+KeyQ',
  cut: 'ctrl+KeyX',
  copy: 'ctrl+KeyC',
  paste: 'ctrl+KeyV',
  minimize: 'ctrl+KeyM',
  closeWindow: 'ctrl+KeyW'
};

/** A key as AppKit takes it (app_menu.rs `Shortcut`): the character typed
 * or a named key, with ⌘ (`command`), ⇧ and ⌥. */
export interface NativeShortcut {
  key: string;
  command?: boolean;
  shift?: boolean;
  option?: boolean;
}

export type MenuNode =
  | {
      kind: 'item';
      id: string;
      label: string;
      /** The native menu's key (macOS). */
      shortcut?: NativeShortcut;
      /** The key as the in-window menu shows it ("Ctrl+Shift+G"). */
      shortcutLabel?: string;
      enabled: boolean;
      checked?: boolean;
    }
  | { kind: 'submenu'; label: string; role?: 'window' | 'help'; items: MenuNode[] }
  | { kind: 'separator' }
  | { kind: 'native'; role: NativeRole; label?: string };

export interface MenuSpec {
  menus: MenuNode[];
}

export interface BuiltMenu {
  spec: MenuSpec;
  /** Command → the key combo ("ctrl+shift+KeyC") its item holds. */
  combos: Map<string, string>;
  /** Commands shown with a check mark. */
  checks: Set<string>;
  commands: string[];
  liveLabels: Map<string, () => string>;
}

export const MENU_ID_PREFIX = 'cmd:';

/** i18next's `t`, untyped keys (they are built from command names). */
export type Translate = (key: string | string[], options?: Record<string, unknown>) => string;

/** Keys AppKit names by themselves (app_menu.rs `NAMED_KEYS`). */
const NAMED_KEYS = new Set([
  'ArrowUp',
  'ArrowDown',
  'ArrowLeft',
  'ArrowRight',
  'Enter',
  'Tab',
  'Space',
  'Escape',
  'Home',
  'End',
  'PageUp',
  'PageDown'
]);

/** A combo as the native menu's key, or null for a key it cannot name
 * (numpad keys, Caps Lock). The table's Delete is ⌫ on a Mac. */
export function nativeShortcut(combo: string[]): NativeShortcut | null {
  if (combo.length === 0) return null;
  const code = combo[combo.length - 1];
  const modifiers = combo.slice(0, -1);
  if (!modifiers.every((part) => part === 'ctrl' || part === 'shift' || part === 'alt')) return null;
  let key: string | null;
  if (/^Key[A-Z]$/.test(code) || /^Digit[0-9]$/.test(code)) key = code.slice(-1).toLowerCase();
  else if (code === 'Delete' || code === 'Backspace') key = 'Backspace';
  else if (NAMED_KEYS.has(code) || /^F([1-9]|1[0-9]|2[0-4])$/.test(code)) key = code;
  else key = comboKeyChar(code, modifiers.includes('ctrl'))?.toLowerCase() ?? null;
  if (!key) return null;
  return {
    key,
    ...(modifiers.includes('ctrl') ? { command: true } : {}),
    ...(modifiers.includes('shift') ? { shift: true } : {}),
    ...(modifiers.includes('alt') ? { option: true } : {})
  };
}

/** The menu tree for the app's language and the user's keys. */
export function buildMenuSpec(
  t: Translate,
  keybinds?: Record<string, string[]> | null,
  enabled: (command: string) => boolean = appCommandEnabled,
  app = 'Rawtonic',
  historyLabels: Partial<Record<'undo' | 'redo', string>> = {},
  platform = 'macos',
  native = platform === 'macos'
): BuiltMenu {
  const definitions = new Map(KEYBIND_DEFINITIONS.map((def) => [def.action, def]));
  const combos = new Map<string, string>();
  const checks = new Set<string>();
  const liveLabels = new Map<string, () => string>();
  const taken = new Set(Object.values(NATIVE_COMBOS));

  const item = (entry: Extract<Entry, { command: string }>): MenuNode => {
    const def = definitions.get(entry.command);
    const label = t(def ? [`appMenu.items.${entry.command}`, def.description] : `appMenu.items.${entry.command}`, {
      app
    });
    const node: Extract<MenuNode, { kind: 'item' }> = {
      kind: 'item',
      id: `${MENU_ID_PREFIX}${entry.command}`,
      label: entry.command === 'undo' || entry.command === 'redo' ? (historyLabels[entry.command] ?? label) : label,
      enabled: enabled(entry.command)
    };
    if (entry.command === 'toggle_analytics') {
      // A belongs to the webview so it can still toggle Auto Mask on a brush.
      const shortcut = shortcutHint(entry.command, keybinds, platform);
      const scopesLabel = () => {
        const label = t(
          appCommand(entry.command)?.checked?.() ? 'appMenu.items.hide_scopes' : 'appMenu.items.show_scopes'
        );
        return native && shortcut ? t('studio.workspace.withShortcut', { label, shortcut }) : label;
      };
      liveLabels.set(entry.command, scopesLabel);
      node.label = scopesLabel();
      if (shortcut) node.shortcutLabel = shortcut;
    } else if (def) {
      const bound = effectiveCombos(def, keybinds);
      const preferred = entry.preferCombo && bound.find((combo) => combo.join('+') === entry.preferCombo!.join('+'));
      for (const combo of preferred ? [preferred, ...bound] : bound) {
        const shortcut = nativeShortcut(combo);
        const key = combo.join('+');
        if (!shortcut || taken.has(key)) continue;
        taken.add(key);
        combos.set(entry.command, key);
        if (native) node.shortcut = shortcut;
        node.shortcutLabel = formatCombo(combo, platform);
        break;
      }
    }
    if (entry.check) {
      checks.add(entry.command);
      node.checked = !!appCommand(entry.command)?.checked?.();
    }
    return node;
  };
  const nodes = (entries: Entry[]): MenuNode[] =>
    entries.map((entry): MenuNode => {
      if (entry === 'separator') return { kind: 'separator' };
      if ('native' in entry)
        return { kind: 'native', role: entry.native, label: t(`appMenu.native.${entry.native}`, { app }) };
      if ('dynamic' in entry)
        return {
          kind: 'submenu',
          label: t(`appMenu.submenus.${entry.dynamic}`),
          items: [
            ...appMenuCommands(entry.dynamic).map(([command, handler]): MenuNode => ({
              kind: 'item',
              id: `${MENU_ID_PREFIX}${command}`,
              label: handler.menu!.label,
              enabled: enabled(command)
            })),
            ...(entry.dynamic === 'recent'
              ? [{ kind: 'separator' } as MenuNode, item({ command: 'clear_recent' })]
              : [])
          ]
        };
      if ('submenu' in entry)
        return { kind: 'submenu', label: t(`appMenu.submenus.${entry.submenu}`), items: nodes(entry.items) };
      return item(entry);
    });
  const menus = MENU_LAYOUT.map((top): MenuNode => ({
    kind: 'submenu',
    label: t(`appMenu.menus.${top.menu}`, { app }),
    ...(top.role ? { role: top.role } : {}),
    items: nodes(top.items)
  }));
  const commands: string[] = [];
  const collect = (nodes: MenuNode[]) =>
    nodes.forEach((node) => {
      if (node.kind === 'item') commands.push(node.id.slice(MENU_ID_PREFIX.length));
      if (node.kind === 'submenu') collect(node.items);
    });
  collect(menus);
  return { spec: { menus }, combos, checks, commands, liveLabels };
}

/** Every command the layout places. */
export function menuCommands(): string[] {
  const out: string[] = [];
  const walk = (entries: Entry[]) => {
    for (const entry of entries) {
      if (entry === 'separator' || 'native' in entry) continue;
      if ('dynamic' in entry) {
        if (entry.dynamic === 'recent') out.push('clear_recent');
      } else if ('submenu' in entry) walk(entry.items);
      else out.push(entry.command);
    }
  };
  MENU_LAYOUT.forEach((top) => walk(top.items));
  return out;
}

export interface MenuItemState {
  id: string;
  label?: string;
  enabled: boolean;
  checked?: boolean;
}

/** Live history labels and enabled/checked state of every command item. */
export function menuItemStates(
  menu: BuiltMenu,
  enabled: (command: string) => boolean = appCommandEnabled,
  historyLabels: Partial<Record<'undo' | 'redo', string>> = {}
): MenuItemState[] {
  return menu.commands.map((command) => {
    const state: MenuItemState = { id: `${MENU_ID_PREFIX}${command}`, enabled: enabled(command) };
    if (command === 'undo' || command === 'redo') {
      const label = historyLabels[command];
      if (label !== undefined) state.label = label;
    }
    const liveLabel = menu.liveLabels.get(command);
    if (liveLabel) state.label = liveLabel();
    if (menu.checks.has(command)) state.checked = !!appCommand(command)?.checked?.();
    return state;
  });
}
