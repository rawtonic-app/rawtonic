// SPDX-License-Identifier: AGPL-3.0-or-later
import { useSettingsStore } from '../store/useSettingsStore';
import { buildComboMap, formatKeyCode, normalizeCombo } from './keyboardUtils';

const TYPED: Record<string, -1 | 1> = { '[': -1, ']': 1 };
/** The US bracket places (KeyboardEvent.code): they size the brush on every
 * layout while the key there has no other binding (ü/¨ on Swiss German,
 * ü/+ on German keyboards; WBRS1 2026-11-16). */
const PLACES: Record<string, -1 | 1> = { BracketLeft: -1, BracketRight: 1 };

/** A [ or ] press: the size step and whether Shift came on top of the key
 * (the brush's hardness, Capture One). */
export interface BracketKey {
  step: -1 | 1;
  shifted: boolean;
}

const rotateStep = (commands: string[]): -1 | 1 | undefined =>
  commands.includes('rotate_left') ? -1 : commands.includes('rotate_right') ? 1 : undefined;

/**
 * The [ or ] a key stands for while a brush or retouch tool sizes with them.
 * Besides the typed brackets (⌥5/⌥6 on German and Swiss Macs), the keys
 * bound to Rotate left and Rotate right count: on US layouts they are the
 * same keys, elsewhere the physical [ ] key types ü or ¨ and would otherwise
 * turn the photograph and add a History step while painting (MSMOKE1
 * 2026-11-16, UXF5); so do the bracket places themselves while nothing else
 * is bound there. A Rotate key that holds Shift itself is a plain [ ].
 */
export function bracketKey(e: KeyboardEvent): BracketKey | undefined {
  const typed = TYPED[e.key];
  if (typed) return { step: typed, shifted: e.shiftKey };
  if (e.metaKey || e.ctrlKey) return undefined;
  const { appSettings, osPlatform } = useSettingsStore.getState();
  const combos = buildComboMap(appSettings?.keybinds);
  const combo = normalizeCombo(e, osPlatform);
  const bound = combos.get(combo.join('+')) ?? [];
  const exact = rotateStep(bound);
  if (exact) return { step: exact, shifted: false };
  const place = bound.length ? undefined : PLACES[e.code];
  if (place) return { step: place, shifted: e.shiftKey };
  if (!e.shiftKey) return undefined;
  const plainCombo = combo.filter((part) => part !== 'shift').join('+');
  const plainBound = combos.get(plainCombo) ?? [];
  const step = rotateStep(plainBound) ?? (plainBound.length ? undefined : PLACES[e.code]);
  return step ? { step, shifted: true } : undefined;
}

/** The size keys as the active layout names them ("[" and "]" on US, "Ü"
 * and "¨" on Swiss German), for hints. */
export function bracketKeyLabels(osPlatform: string): { smaller: string; larger: string } {
  return { smaller: formatKeyCode('BracketLeft', osPlatform), larger: formatKeyCode('BracketRight', osPlatform) };
}

/** The size step of `bracketKey`: -1 smaller, 1 larger, undefined for other keys. */
export function bracketStep(e: KeyboardEvent): -1 | 1 | undefined {
  return bracketKey(e)?.step;
}
