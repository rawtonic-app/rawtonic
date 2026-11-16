import i18next from 'i18next';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { buildMenuSpec, menuCommands, menuItemStates, nativeShortcut, type MenuNode, type Translate } from './appMenu';
import { setLayoutChars } from './keyboardLayout';
import { registerAppCommands } from './appCommands';
import { KEYBIND_DEFINITIONS } from './keyboardUtils';
import fixture from '../../src-tauri/tests/fixtures/app-menu-en.json?raw';
import keyboardShortcutsSource from '../hooks/useKeyboardShortcuts.ts?raw';

const locales = Object.fromEntries(
  Object.entries(
    import.meta.glob<Record<string, unknown>>('../i18n/locales/*.json', { eager: true, import: 'default' })
  ).map(([file, translation]) => [file.replace(/^.*\/|\.json$/g, ''), translation])
);
/** Every source file, for the commands registered by name. */
const sources = import.meta.glob<string>(['../**/*.{ts,tsx}', '!../**/*.test.{ts,tsx}'], {
  eager: true,
  query: '?raw',
  import: 'default'
});
/** The tree app_menu.rs's Rust tests read (`the_webview_tree_is_valid…`). */
const FIXTURE = 'src-tauri/tests/fixtures/app-menu-en.json';

const i18n = i18next.createInstance();
beforeAll(async () => {
  await i18n.init({
    lng: 'en',
    fallbackLng: false,
    resources: Object.fromEntries(Object.entries(locales).map(([lng, translation]) => [lng, { translation }])),
    interpolation: { escapeValue: false }
  });
});
const tr = (lng: string) => i18n.getFixedT(lng) as unknown as Translate;

const items = (nodes: MenuNode[]): Extract<MenuNode, { kind: 'item' }>[] =>
  nodes.flatMap((node) => (node.kind === 'item' ? [node] : node.kind === 'submenu' ? items(node.items) : []));
const find = (nodes: MenuNode[], command: string) => items(nodes).find((node) => node.id === `cmd:${command}`)!;

let unregister: (() => void) | null = null;
afterEach(() => {
  unregister?.();
  unregister = null;
});

describe('the menu bar tree', () => {
  it('is the tree the Rust tests check (UPDATE_MENU_FIXTURE=1 rewrites it)', async () => {
    const json = `${JSON.stringify(buildMenuSpec(tr('en'), {}).spec, null, 2)}\n`;
    if (
      import.meta.env.UPDATE_MENU_FIXTURE ||
      (globalThis as { process?: { env: Record<string, string> } }).process?.env.UPDATE_MENU_FIXTURE
    ) {
      const fs = await import(/* @vite-ignore */ 'node:fs' as string);
      fs.writeFileSync(FIXTURE, json);
      return;
    }
    expect(json).toBe(fixture);
  });

  it('places live recent libraries and recipes, with their handlers and state', () => {
    unregister = registerAppCommands({
      'recent:one': { run: () => {}, menu: { group: 'recent', label: 'One' } },
      'recipe:one': { run: () => {}, enabled: () => false, menu: { group: 'recipes', label: 'JPEG' } }
    });
    const menu = buildMenuSpec(tr('en'), {});
    expect(find(menu.spec.menus, 'recent:one')).toMatchObject({ label: 'One', enabled: true });
    expect(find(menu.spec.menus, 'recipe:one')).toMatchObject({ label: 'JPEG', enabled: false });
    expect(menuItemStates(menu)).toContainEqual({ id: 'cmd:recipe:one', enabled: false });
  });

  it('places each command once, with a unique id', () => {
    const ids = items(buildMenuSpec(tr('en'), {}).spec.menus).map((node) => node.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(ids.length).toBe(menuCommands().length);
  });

  it('labels every item and menu in every locale from its own strings', () => {
    for (const lng of Object.keys(locales)) {
      const { spec } = buildMenuSpec(tr(lng), {});
      const walk = (nodes: MenuNode[]) => {
        for (const node of nodes) {
          if (node.kind === 'separator') continue;
          const label = node.label ?? '';
          expect(label, `${lng} ${JSON.stringify(node)}`).toBeTruthy();
          expect(label, `${lng}`).not.toMatch(/appMenu\.|settings\.keybinds|\{\{/);
          if (node.kind === 'submenu') walk(node.items);
        }
      };
      walk(spec.menus);
    }
    const de = buildMenuSpec(tr('de'), {}).spec.menus;
    expect(de.map((menu) => menu.kind === 'submenu' && menu.label)).toEqual([
      'Rawtonic',
      'Ablage',
      'Bearbeiten',
      'Foto',
      'Darstellung',
      'Fenster',
      'Hilfe'
    ]);
    expect(find(de, 'undo').label).toBe('Widerrufen');
  });

  it('shows every key, single keys included, as the native menu and the ⋯ menu take it', () => {
    const { spec, combos } = buildMenuSpec(tr('en'), {});
    expect(find(spec.menus, 'undo').shortcut).toEqual({ key: 'z', command: true });
    // Redo shows ⇧⌘Z (the Mac's key); ⌘Y keeps working as the webview's.
    expect(find(spec.menus, 'redo').shortcut).toEqual({ key: 'z', command: true, shift: true });
    expect(find(spec.menus, 'redo').shortcutLabel).toBe('⇧⌘Z');
    expect(find(spec.menus, 'toggle_export').shortcut).toEqual({ key: 'e', command: true, shift: true });
    expect(find(spec.menus, 'paste_from_previous').shortcut).toEqual({ key: 'v', command: true, option: true });
    expect(find(spec.menus, 'zoom_in').shortcut).toEqual({ key: '+', command: true });
    expect(combos.get('copy_adjustments')).toBe('ctrl+shift+KeyC');
    // Single keys show too (UXR4M-05): ratings, flags, labels, views, panels.
    expect(find(spec.menus, 'rate_3').shortcut).toEqual({ key: '3' });
    expect(find(spec.menus, 'flag_pick').shortcut).toEqual({ key: 'p' });
    expect(find(spec.menus, 'toggle_fullscreen').shortcut).toEqual({ key: 'f' });
    expect(find(spec.menus, 'rotate_left').shortcut).toEqual({ key: '[' });
    expect(find(spec.menus, 'delete_selected').shortcut).toEqual({ key: 'Backspace' });
    expect(find(spec.menus, 'toggle_bottom_panel').shortcut).toEqual({ key: 'F6' });
    expect(nativeShortcut(['shift', 'Digit1'])).toEqual({ key: '1', shift: true });
    expect(nativeShortcut(['ctrl', 'alt', 'KeyC'])).toEqual({ key: 'c', command: true, option: true });
    // Keys a native menu cannot name stay the webview's alone.
    for (const key of ['Insert', 'NumpadAdd', 'PrintScreen', 'CapsLock', 'F25']) {
      expect(nativeShortcut(['ctrl', key]), key).toBeNull();
    }
    expect(nativeShortcut(['ctrl', 'F12'])).toEqual({ key: 'F12', command: true });
    const custom = buildMenuSpec(tr('en'), { toggle_export: ['ctrl', 'Insert'] }).spec;
    expect(find(custom.menus, 'toggle_export').shortcut).toBeUndefined();
    // Windows and Linux name the same keys their way.
    const windows = buildMenuSpec(tr('en'), {}, undefined, 'Rawtonic', {}, 'windows').spec;
    expect(find(windows.menus, 'toggle_export').shortcut).toBeUndefined();
    expect(find(windows.menus, 'toggle_export').shortcutLabel).toBe('Ctrl+Shift+E');
    expect(find(windows.menus, 'rate_3').shortcutLabel).toBe('3');
  });

  it('names punctuation keys as the keyboard layout types them (UXR4M-06)', () => {
    // A Swiss German Mac: the US ' [ ] = - keys type à è ¨ ^ ' there, and + is ⇧1.
    setLayoutChars(
      new Map([
        ['Quote', 'à'],
        ['BracketLeft', 'è'],
        ['BracketRight', '¨'],
        ['Equal', '^'],
        ['Minus', "'"],
        ['Slash', '-'],
        ['Digit1', '1']
      ])
    );
    try {
      const { spec } = buildMenuSpec(tr('en'), {});
      expect(find(spec.menus, 'virtual_copy').shortcut).toEqual({ key: 'à', command: true });
      expect(find(spec.menus, 'virtual_copy').shortcutLabel).toBe('⌘À');
      expect(find(spec.menus, 'rotate_left').shortcut).toEqual({ key: 'è' });
      // ⌘+ (⌘⇧1 here) and ⌘- zoom by the character on every layout.
      expect(find(spec.menus, 'zoom_in').shortcut).toEqual({ key: '+', command: true });
      expect(find(spec.menus, 'zoom_out').shortcut).toEqual({ key: '-', command: true });
      // German: + and - have keys of their own.
      setLayoutChars(
        new Map([
          ['Equal', '´'],
          ['BracketRight', '+'],
          ['Minus', 'ß'],
          ['Slash', '-']
        ])
      );
      const german = buildMenuSpec(tr('en'), {}).spec;
      expect(find(german.menus, 'zoom_in').shortcut).toEqual({ key: '+', command: true });
      expect(find(german.menus, 'zoom_out').shortcut).toEqual({ key: '-', command: true });
      expect(find(german.menus, 'rotate_right').shortcut).toEqual({ key: '+' });
    } finally {
      setLayoutChars(new Map());
    }
  });

  it("follows the user's own keys and leaves AppKit's keys to AppKit", () => {
    const { spec } = buildMenuSpec(tr('en'), {
      toggle_export: ['ctrl', 'alt', 'KeyE'],
      undo: [],
      // ⌘C stays Copy for text fields; the action keeps its key in the webview.
      copy_adjustments: ['ctrl', 'KeyC'],
      // Two actions on one key: the first item shows it.
      zoom_in: ['ctrl', 'KeyK']
    });
    expect(find(spec.menus, 'toggle_export').shortcut).toEqual({ key: 'e', command: true, option: true });
    expect(find(spec.menus, 'undo').shortcut).toBeUndefined();
    expect(find(spec.menus, 'copy_adjustments').shortcut).toBeUndefined();
    const keys = items(spec.menus).flatMap((node) => (node.shortcut ? [JSON.stringify(node.shortcut)] : []));
    expect(new Set(keys).size).toBe(keys.length);
    expect(keys.filter((key) => key === JSON.stringify({ key: 'k', command: true }))).toHaveLength(1);
  });

  it('describes every new shortcut of the table in the Preferences list', () => {
    for (const action of ['new_session', 'open_session', 'import_photos', 'select_none', 'show_in_finder']) {
      const def = KEYBIND_DEFINITIONS.find((d) => d.action === action)!;
      for (const lng of ['en', 'de']) expect(tr(lng)(def.description)).not.toBe(def.description);
    }
  });

  it("reads enabled and checked state from the commands' own rules", () => {
    let develop = false;
    unregister = registerAppCommands({
      view_develop: { run: () => {}, checked: () => develop },
      rate_3: { run: () => {}, enabled: () => false }
    });
    const menu = buildMenuSpec(tr('en'), {});
    expect(find(menu.spec.menus, 'view_develop')).toMatchObject({ enabled: true, checked: false });
    // Not registered (yet): greyed out, not hidden.
    expect(find(menu.spec.menus, 'about').enabled).toBe(false);
    develop = true;
    const states = new Map(menuItemStates(menu).map((s) => [s.id, s]));
    expect(states.get('cmd:view_develop')).toEqual({ id: 'cmd:view_develop', enabled: true, checked: true });
    expect(states.get('cmd:rate_3')).toEqual({ id: 'cmd:rate_3', enabled: false });
  });
});

describe('every menu command has a handler', () => {
  it('is registered by the code that owns it', () => {
    // useKeyboardShortcuts registers the shortcut table's actions (tested in
    // useAppMenu.test.tsx); the others register by name in their owner.
    const handled = new Set<string>();
    for (const text of Object.values(sources)) {
      if (!text.includes('registerAppCommands(')) continue;
      for (const match of text.matchAll(/^\s+([a-z_0-9]+): (?:\{|flag\()/gm)) handled.add(match[1]);
    }
    for (const match of keyboardShortcutsSource.matchAll(/^ {6}([a-z_0-9]+): (?:\{$|zoomAction\()/gm))
      handled.add(match[1]);
    const missing = menuCommands().filter((command) => !handled.has(command));
    expect(missing).toEqual([]);
  });
});
