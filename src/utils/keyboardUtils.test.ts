import { describe, expect, it } from 'vitest';
import { KEYBIND_DEFINITIONS, buildComboMap, conflictingActions, shortcutHint } from './keyboardUtils';

describe('keybinds', () => {
  it('bind Capture One keys with Lightroom aliases', () => {
    const map = buildComboMap(null);
    expect(map.get('KeyB')).toEqual(['tool_brush']);
    expect(map.get('KeyK')).toEqual(['tool_brush']);
    expect(map.get('KeyR')).toEqual(['tool_straighten']);
    // One before/after tool: Y toggles the split, a held \ peeks at the before.
    expect(map.get('KeyY')).toEqual(['toggle_split_view']);
    expect(map.get('Backslash')).toEqual(['show_original']);
    expect(map.has('shift+KeyY')).toBe(false);
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
    const map = buildComboMap({ tool_brush: ['KeyG'], show_original: [] });
    expect(map.get('KeyG')).toEqual(['tool_brush']);
    expect(map.has('KeyB')).toBe(false);
    expect(map.has('KeyK')).toBe(false);
    expect(map.has('Backslash')).toBe(false);
  });

  it('keep every default key unique within a view', () => {
    const map = buildComboMap(null);
    const clashes = [...map].filter(([, actions]) => {
      const sections = actions.map((a) => KEYBIND_DEFINITIONS.find((d) => d.action === a)!.section);
      // Library-only and Develop-only actions may share a key.
      return actions.length > 1 && !(sections.includes('tools') && actions.length === 2);
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
