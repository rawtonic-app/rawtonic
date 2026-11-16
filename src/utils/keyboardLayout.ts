/**
 * The characters the active keyboard layout types, per key (`KeyboardEvent
 * .code`), so every place that names a key (menus, tooltips, Preferences)
 * names the key that works (UXR4M-06: on a Swiss keyboard the menu showed
 * "⌘^" for a command that ⌘^ did not run). Shortcuts match letters by the
 * character they type and punctuation by its place; a punctuation key is
 * named by what it types on this layout ("ä" on a German keyboard where a US
 * one has "'"). ⌘+ and ⌘- zoom by character on every layout
 * (`normalizeCombo`).
 *
 * Sources: macOS asks the system (src-tauri keyboard_layout.rs, which also
 * says when the layout changes); Windows reads the web view's keyboard map;
 * Linux (WebKitGTK has no keyboard map) keeps the US names.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

/** Code → the character the key types without modifiers ("Quote" → "ä"). */
export type LayoutChars = ReadonlyMap<string, string>;

let chars: LayoutChars = new Map();
const listeners = new Set<() => void>();

export function layoutChars(): LayoutChars {
  return chars;
}

/** Sets the active layout's characters (tests, and `watchKeyboardLayout`). */
export function setLayoutChars(next: LayoutChars): void {
  const same = next.size === chars.size && [...next].every(([code, char]) => chars.get(code) === char);
  chars = next;
  if (!same) listeners.forEach((listener) => listener());
}

/** Calls `listener` whenever the layout's characters change. */
export function onLayoutChange(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

interface KeyboardMapApi {
  getLayoutMap?: () => Promise<Map<string, string>>;
  addEventListener?: (type: 'layoutchange', listener: () => void) => void;
  removeEventListener?: (type: 'layoutchange', listener: () => void) => void;
}

async function readLayout(platform: string): Promise<LayoutChars | null> {
  if (platform === 'macos') {
    // Invokes.KeyboardLayout; not imported, as AppProperties pulls in the UI.
    const map = await invoke<Record<string, string>>('keyboard_layout');
    return new Map(Object.entries(map));
  }
  const keyboard = (navigator as Navigator & { keyboard?: KeyboardMapApi }).keyboard;
  if (keyboard?.getLayoutMap) return new Map(await keyboard.getLayoutMap());
  return null;
}

/** Reads the layout now and again whenever it may have changed. */
export function watchKeyboardLayout(platform: string): () => void {
  let disposed = false;
  const refresh = () => {
    readLayout(platform)
      .then((next) => {
        if (!disposed && next) setLayoutChars(next);
      })
      .catch(() => {});
  };
  refresh();
  const keyboard = (navigator as Navigator & { keyboard?: KeyboardMapApi }).keyboard;
  keyboard?.addEventListener?.('layoutchange', refresh);
  // Windows switches layouts per window: look again when the window comes back.
  window.addEventListener('focus', refresh);
  const unlisten = platform === 'macos' ? listen('keyboard-layout-changed', refresh) : null;
  return () => {
    disposed = true;
    keyboard?.removeEventListener?.('layoutchange', refresh);
    window.removeEventListener('focus', refresh);
    void unlisten?.then((stop) => stop()).catch(() => {});
  };
}
