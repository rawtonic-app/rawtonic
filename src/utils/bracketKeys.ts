// SPDX-License-Identifier: AGPL-3.0-or-later
import { useSettingsStore } from '../store/useSettingsStore';
import { buildComboMap, normalizeCombo } from './keyboardUtils';

const TYPED: Record<string, -1 | 1> = { '[': -1, ']': 1 };

/**
 * The step a key gives a brush or retouch size while that tool sizes with
 * [ and ]: -1 smaller, 1 larger, undefined for other keys. Besides the typed
 * brackets (⌥5/⌥6 on German and Swiss Macs), the keys bound to Rotate left
 * and Rotate right count: on US layouts they are the same keys, elsewhere
 * the physical [ ] key types ü or ¨ and would otherwise turn the photograph
 * and add a History step while painting (MSMOKE1 2026-11-16, UXF5). Shift is
 * left to the caller (the brush's hardness).
 */
export function bracketStep(e: KeyboardEvent): -1 | 1 | undefined {
  const typed = TYPED[e.key];
  if (typed) return typed;
  if (e.metaKey || e.ctrlKey) return undefined;
  const { appSettings, osPlatform } = useSettingsStore.getState();
  const plain = normalizeCombo(e, osPlatform)
    .filter((part) => part !== 'shift')
    .join('+');
  const commands = buildComboMap(appSettings?.keybinds).get(plain) ?? [];
  if (commands.includes('rotate_left')) return -1;
  if (commands.includes('rotate_right')) return 1;
  return undefined;
}
