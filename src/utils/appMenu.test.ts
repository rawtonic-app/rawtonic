import i18next from 'i18next';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { buildMenuSpec, menuAccelerator, menuCommands, menuItemStates, type MenuNode, type Translate } from './appMenu';
import { registerAppCommands } from './appCommands';
import { KEYBIND_DEFINITIONS } from './keyboardUtils';
import fixture from '../../src-tauri/tests/fixtures/app-menu-en.json?raw';
import keyboardShortcutsSource from '../hooks/useKeyboardShortcuts.ts?raw';

const locales = Object.fromEntries(
  Object.entries(import.meta.glob<Record<string, unknown>>('../i18n/locales/*.json', { eager: true, import: 'default' })).map(
    ([file, translation]) => [file.replace(/^.*\/|\.json$/g, ''), translation],
  ),
);
/** Every source file, for the commands registered by name. */
const sources = import.meta.glob<string>(['../**/*.{ts,tsx}', '!../**/*.test.{ts,tsx}'], {
  eager: true,
  query: '?raw',
  import: 'default',
});
/** The tree app_menu.rs's Rust tests read (`the_webview_tree_is_valid…`). */
const FIXTURE = 'src-tauri/tests/fixtures/app-menu-en.json';

const i18n = i18next.createInstance();
beforeAll(async () => {
  await i18n.init({
    lng: 'en',
    fallbackLng: false,
    resources: Object.fromEntries(Object.entries(locales).map(([lng, translation]) => [lng, { translation }])),
    interpolation: { escapeValue: false },
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
    if (import.meta.env.UPDATE_MENU_FIXTURE || (globalThis as { process?: { env: Record<string, string> } }).process?.env.UPDATE_MENU_FIXTURE) {
      const fs = await import(/* @vite-ignore */ 'node:fs' as string);
      fs.writeFileSync(FIXTURE, json);
      return;
    }
    expect(json).toBe(fixture);
  });

  it('places live recent libraries and recipes, with their handlers and state', () => {
    unregister = registerAppCommands({
      'recent:one': { run: () => {}, menu: { group: 'recent', label: 'One' } },
      'recipe:one': { run: () => {}, enabled: () => false, menu: { group: 'recipes', label: 'JPEG' } },
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
      'Hilfe',
    ]);
    expect(find(de, 'undo').label).toBe('Widerrufen');
  });

  it('turns ⌘ shortcuts into key equivalents and never a single key, ⇧ or ⌥ alone', () => {
    const { spec, combos } = buildMenuSpec(tr('en'), {});
    expect(find(spec.menus, 'undo').accelerator).toBe('CmdOrCtrl+KeyZ');
    // Redo shows ⇧⌘Z (the Mac's key); ⌘Y keeps working as the webview's.
    expect(find(spec.menus, 'redo').accelerator).toBe('CmdOrCtrl+Shift+KeyZ');
    expect(find(spec.menus, 'toggle_export').accelerator).toBe('CmdOrCtrl+Shift+KeyE');
    expect(find(spec.menus, 'paste_from_previous').accelerator).toBe('CmdOrCtrl+Alt+KeyV');
    expect(find(spec.menus, 'zoom_in').accelerator).toBe('CmdOrCtrl+Equal');
    expect(combos.get('copy_adjustments')).toBe('ctrl+shift+KeyC');
    for (const command of ['rate_3', 'toggle_fullscreen', 'toggle_split_view', 'color_label_red', 'rotate_left', 'delete_selected', 'toggle_bottom_panel']) {
      expect(find(spec.menus, command).accelerator, command).toBeUndefined();
    }
    expect(menuAccelerator(['shift', 'Digit1'])).toBeNull();
    expect(menuAccelerator(['alt', 'KeyM'])).toBeNull();
    expect(menuAccelerator(['KeyF'])).toBeNull();
    expect(menuAccelerator(['ctrl', 'alt', 'KeyC'])).toBe('CmdOrCtrl+Alt+KeyC');
    // Keys the recorder takes but a native key equivalent cannot name stay
    // the webview's (app_menu.rs would refuse the whole tree).
    for (const key of ['Insert', 'NumpadAdd', 'PrintScreen', 'CapsLock', 'Delete', 'F25']) {
      expect(menuAccelerator(['ctrl', key]), key).toBeNull();
    }
    expect(menuAccelerator(['ctrl', 'F12'])).toBe('CmdOrCtrl+F12');
    const custom = buildMenuSpec(tr('en'), { toggle_export: ['ctrl', 'Insert'] }).spec;
    expect(find(custom.menus, 'toggle_export').accelerator).toBeUndefined();
  });

  it("follows the user's own keys and leaves AppKit's keys to AppKit", () => {
    const { spec } = buildMenuSpec(tr('en'), {
      toggle_export: ['ctrl', 'alt', 'KeyE'],
      undo: [],
      // ⌘C stays Copy for text fields; the action keeps its key in the webview.
      copy_adjustments: ['ctrl', 'KeyC'],
      // Two actions on one key: the first item shows it.
      zoom_in: ['ctrl', 'KeyK'],
    });
    expect(find(spec.menus, 'toggle_export').accelerator).toBe('CmdOrCtrl+Alt+KeyE');
    expect(find(spec.menus, 'undo').accelerator).toBeUndefined();
    expect(find(spec.menus, 'copy_adjustments').accelerator).toBeUndefined();
    const accelerators = items(spec.menus).flatMap((node) => (node.accelerator ? [node.accelerator] : []));
    expect(new Set(accelerators).size).toBe(accelerators.length);
    expect(accelerators.filter((a) => a === 'CmdOrCtrl+KeyK')).toHaveLength(1);
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
      rate_3: { run: () => {}, enabled: () => false },
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
    for (const match of keyboardShortcutsSource.matchAll(/^ {6}([a-z_0-9]+): (?:\{$|zoomAction\()/gm)) handled.add(match[1]);
    const missing = menuCommands().filter((command) => !handled.has(command));
    expect(missing).toEqual([]);
  });
});
