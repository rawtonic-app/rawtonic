import { describe, expect, it } from 'vitest';
import { KEYBIND_DEFINITIONS, buildComboMap, conflictingActions, shortcutHint } from './keyboardUtils';

describe('keybinds', () => {
  it('bind Capture One keys with Lightroom aliases', () => {
    const map = buildComboMap(null);
    expect(map.get('KeyB')).toEqual(['tool_brush']);
    expect(map.get('KeyK')).toEqual(['tool_brush']);
    expect(map.get('KeyR')).toEqual(['tool_straighten']);
    expect(map.get('KeyY')).toEqual(['show_original']);
    expect(map.get('Backslash')).toEqual(['show_original']);
    // Lightroom's split before/after key; Y stays the quick original.
    expect(map.get('shift+KeyY')).toEqual(['toggle_split_view']);
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
    const map = buildComboMap({ tool_brush: ['KeyN'], show_original: [] });
    expect(map.get('KeyN')).toEqual(['tool_brush']);
    expect(map.has('KeyB')).toBe(false);
    expect(map.has('KeyK')).toBe(false);
    expect(map.has('KeyY')).toBe(false);
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

  it('format hints for tooltips', () => {
    expect(shortcutHint('tool_brush', null, 'macos')).toBe('B');
    expect(shortcutHint('toggle_export', null, 'macos')).toBe('⌘ + Shift + E');
    expect(shortcutHint('copy_files', null, 'macos')).toBeNull();
  });
});
