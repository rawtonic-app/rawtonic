// SPDX-License-Identifier: AGPL-3.0-or-later
import { useSettingsStore } from '../store/useSettingsStore';
import { buildComboMap, normalizeCombo } from './keyboardUtils';

const TYPED: Record<string, -1 | 1> = { '[': -1, ']': 1 };

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
 * 2026-11-16, UXF5). A Rotate key that holds Shift itself is a plain [ ].
 */
export function bracketKey(e: KeyboardEvent): BracketKey | undefined {
  const typed = TYPED[e.key];
  if (typed) return { step: typed, shifted: e.shiftKey };
  if (e.metaKey || e.ctrlKey) return undefined;
  const { appSettings, osPlatform } = useSettingsStore.getState();
  const combos = buildComboMap(appSettings?.keybinds);
  const combo = normalizeCombo(e, osPlatform);
  const exact = rotateStep(combos.get(combo.join('+')) ?? []);
  if (exact) return { step: exact, shifted: false };
  if (!e.shiftKey) return undefined;
  const plain = combo.filter((part) => part !== 'shift').join('+');
  const step = rotateStep(combos.get(plain) ?? []);
  return step ? { step, shifted: true } : undefined;
}

/** The size step of `bracketKey`: -1 smaller, 1 larger, undefined for other keys. */
export function bracketStep(e: KeyboardEvent): -1 | 1 | undefined {
  return bracketKey(e)?.step;
}
