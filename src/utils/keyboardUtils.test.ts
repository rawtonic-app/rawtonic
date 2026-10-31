import { describe, expect, it } from 'vitest';
import { KEYBIND_DEFINITIONS, buildComboMap, conflictingActions, shortcutHint, splitByView } from './keyboardUtils';

describe('keybinds', () => {
  it.each([
    ['view_grid', ['KeyG']], ['view_loupe', ['KeyE']], ['view_develop', ['KeyD']],
    ['flag_pick', ['KeyP']], ['flag_unflag', ['KeyU']], ['flag_reject', ['KeyX']],
  ])('binds and rebinds %s through the shared table', (action, combo) => {
    expect(buildComboMap(null).get(combo.join('+'))).toContain(action);
    const rebound = buildComboMap({ [action as string]: ['ctrl', 'KeyF12'] });
    expect(rebound.get(combo.join('+')) ?? []).not.toContain(action);
    expect(rebound.get('ctrl+KeyF12')).toContain(action);
    expect(buildComboMap({ [action as string]: [] }).get(combo.join('+')) ?? []).not.toContain(action);
  });
  it('binds the culling keys and keeps earlier aliases', () => {
    const map = buildComboMap(null);
    for (const [key, action] of Object.entries({ KeyP: 'flag_pick', KeyX: 'flag_reject', 'shift+KeyX': 'flag_reject', KeyU: 'flag_unflag', Digit6: 'color_label_red', Digit7: 'color_label_yellow', Digit8: 'color_label_green', Digit9: 'color_label_blue', 'shift+Digit5': 'color_label_purple', CapsLock: 'toggle_auto_advance', 'alt+KeyX': 'toggle_proof_margin' })) expect(map.get(key)).toEqual([action]);
    for (let n = 0; n <= 5; n++) expect(map.get(`Digit${n}`)).toEqual([`rate_${n}`]);
  });
  it('bind Capture One keys with Lightroom aliases', () => {
    const map = buildComboMap(null);
    expect(map.get('KeyB')).toEqual(['tool_brush']);
    expect(map.get('KeyK')).toEqual(['tool_brush']);
    expect(map.get('KeyR')).toEqual(['tool_straighten']);
    // One before/after tool: Y toggles the split, a held \ peeks at the before.
    expect(map.get('KeyY')).toEqual(['toggle_split_view']);
    expect(map.get('Backslash')).toEqual(['show_original']);
    expect(map.get('shift+KeyY')).toEqual(['cycle_compare_mode']);
    expect(map.get('ctrl+shift+KeyC')).toEqual(['copy_adjustments']);
    expect(map.get('ctrl+shift+KeyE')).toEqual(['toggle_export']);
    expect(map.get('ctrl+KeyL')).toEqual(['auto_adjust']);
    expect(map.get('Period')).toEqual(['zoom_100']);
  });

  it('mark no default shortcut as conflicting, unassigned ones included', () => {
    expect([...conflictingActions(null)]).toEqual([]);
    expect([...conflictingActions({ tool_brush: ['KeyV'] })].sort()).toEqual(['tool_brush', 'tool_select']);
  });

  it('give T to the radial tool only (the Library EXIF overlay no longer exists)', () => {
    expect(buildComboMap(null).get('KeyT')).toEqual(['tool_radial']);
  });

  it('drop the defaults once the user assigns a key, and an empty key disables', () => {
    const map = buildComboMap({ tool_brush: ['KeyG'], view_grid: [], show_original: [] });
    expect(map.get('KeyG')).toEqual(['tool_brush']);
    expect(map.has('KeyB')).toBe(false);
    expect(map.has('KeyK')).toBe(false);
    expect(map.has('Backslash')).toBe(false);
  });

  it('keep every default key unique within a view', () => {
    const map = buildComboMap(null);
    const clashes = [...map].filter(([, actions]) => {
      // Library-only and Develop-only actions may share a key.
      return actions.length > 1 && !splitByView(actions);
    });
    expect(clashes).toEqual([]);
  });

  it('bind every default and alias combo of an action once', () => {
    const combos = KEYBIND_DEFINITIONS.flatMap((d) =>
      [d.defaultCombo, ...(d.aliasCombos ?? [])].filter((c) => c.length > 0).map((c) => `${d.action}:${c.join('+')}`),
    );
    expect(combos.length).toBe(new Set(combos).size);
    const actions = KEYBIND_DEFINITIONS.map((d) => d.action);
    expect(actions.length).toBe(new Set(actions).size);
  });

  it('format hints for tooltips', () => {
    expect(shortcutHint('toggle_split_view', null, 'macos')).toBe('Y');
    expect(shortcutHint('show_original', null, 'macos')).toBe('\\');
    expect(shortcutHint('tool_brush', null, 'macos')).toBe('B');
    expect(shortcutHint('toggle_export', null, 'macos')).toBe('⌘ + Shift + E');
    expect(shortcutHint('copy_files', null, 'macos')).toBeNull();
  });
});
