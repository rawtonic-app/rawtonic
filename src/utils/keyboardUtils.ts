export interface KeybindDefinition {
  action: string;
  description: string;
  defaultCombo: string[];
  /** Further default combos (another editor's key for the same action);
   * dropped once the user assigns their own combo. */
  aliasCombos?: string[][];
  section: 'library' | 'tools' | 'view' | 'rating' | 'panels' | 'editing';
}

export interface KeybindSection {
  id: KeybindDefinition['section'];
  label: string;
}

export const KEYBIND_SECTIONS: KeybindSection[] = [
  { id: 'library', label: 'settings.keybinds.sections.library' },
  { id: 'tools', label: 'settings.keybinds.sections.tools' },
  { id: 'editing', label: 'settings.keybinds.sections.editing' },
  { id: 'view', label: 'settings.keybinds.sections.view' },
  { id: 'rating', label: 'settings.keybinds.sections.rating' },
  { id: 'panels', label: 'settings.keybinds.sections.panels' },
];

export const KEYBIND_DEFINITIONS: KeybindDefinition[] = [
  {
    action: 'open_image',
    description: 'settings.keybinds.actions.open_image',
    defaultCombo: ['Enter'],
    section: 'library',
  },
  {
    action: 'copy_files',
    description: 'settings.keybinds.actions.copy_files',
    // ⇧⌘C/⇧⌘V copy adjustments in Capture One and Lightroom.
    defaultCombo: [],
    section: 'library',
  },
  {
    action: 'paste_files',
    description: 'settings.keybinds.actions.paste_files',
    defaultCombo: [],
    section: 'library',
  },
  {
    action: 'copy_image_path',
    description: 'settings.keybinds.actions.copy_image_path',
    // Finder's "Copy as Pathname"; ⌘L is Capture One's Auto adjust.
    defaultCombo: ['ctrl', 'alt', 'KeyC'],
    section: 'library',
  },
  {
    action: 'select_all',
    description: 'settings.keybinds.actions.select_all',
    defaultCombo: ['ctrl', 'KeyA'],
    section: 'library',
  },
  {
    // Capture One's Move to Selects Folder (sessions).
    action: 'move_to_selects',
    description: 'settings.keybinds.actions.move_to_selects',
    defaultCombo: ['ctrl', 'KeyJ'],
    section: 'library',
  },
  {
    action: 'delete_selected',
    description: 'settings.keybinds.actions.delete_selected',
    defaultCombo: ['Delete'],
    section: 'library',
  },
  {
    action: 'preview_prev',
    description: 'settings.keybinds.actions.preview_prev',
    defaultCombo: ['ArrowLeft'],
    section: 'library',
  },
  {
    action: 'preview_next',
    description: 'settings.keybinds.actions.preview_next',
    defaultCombo: ['ArrowRight'],
    section: 'library',
  },
  {
    action: 'zoom_in_step',
    description: 'settings.keybinds.actions.zoom_in_step',
    defaultCombo: ['ArrowUp'],
    section: 'view',
  },
  {
    action: 'zoom_out_step',
    description: 'settings.keybinds.actions.zoom_out_step',
    defaultCombo: ['ArrowDown'],
    section: 'view',
  },
  {
    action: 'cycle_zoom',
    description: 'settings.keybinds.actions.cycle_zoom',
    defaultCombo: ['Space'],
    section: 'view',
  },
  {
    action: 'zoom_in',
    description: 'settings.keybinds.actions.zoom_in',
    defaultCombo: ['ctrl', 'Equal'],
    section: 'view',
  },
  {
    action: 'zoom_out',
    description: 'settings.keybinds.actions.zoom_out',
    defaultCombo: ['ctrl', 'Minus'],
    section: 'view',
  },
  {
    action: 'zoom_fit',
    description: 'settings.keybinds.actions.zoom_fit',
    defaultCombo: ['ctrl', 'Digit0'],
    aliasCombos: [['Comma']],
    section: 'view',
  },
  {
    action: 'zoom_100',
    description: 'settings.keybinds.actions.zoom_100',
    defaultCombo: ['ctrl', 'Digit1'],
    aliasCombos: [['Period']],
    section: 'view',
  },
  {
    action: 'toggle_fullscreen',
    description: 'settings.keybinds.actions.toggle_fullscreen',
    defaultCombo: ['KeyF'],
    section: 'view',
  },
  {
    action: 'show_original',
    description: 'settings.keybinds.actions.show_original',
    // Y in Capture One, backslash in Lightroom; B is the brush.
    defaultCombo: ['KeyY'],
    aliasCombos: [['Backslash']],
    section: 'view',
  },
  {
    // Before/after split with a movable divider (Capture One's split;
    // Lightroom's split is Shift+Y, Y here is the quick original toggle).
    action: 'toggle_split_view',
    description: 'settings.keybinds.actions.toggle_split_view',
    defaultCombo: ['shift', 'KeyY'],
    section: 'view',
  },
  {
    // Lightroom's J: highlight and shadow clipping warnings in the viewer.
    action: 'toggle_clipping',
    description: 'settings.keybinds.actions.toggle_clipping',
    defaultCombo: ['KeyJ'],
    section: 'view',
  },
  {
    // Lightroom's soft proofing (S there; S is the crop tool here).
    action: 'toggle_soft_proof',
    description: 'settings.keybinds.actions.toggle_soft_proof',
    defaultCombo: ['shift', 'KeyS'],
    section: 'view',
  },
  {
    action: 'toggle_mask_overlay',
    description: 'settings.keybinds.actions.toggle_mask_overlay',
    defaultCombo: ['KeyM'],
    section: 'view',
  },
  {
    action: 'toggle_mask_grayscale',
    description: 'settings.keybinds.actions.toggle_mask_grayscale',
    defaultCombo: ['alt', 'KeyM'],
    section: 'view',
  },
  { action: 'rate_0', description: 'settings.keybinds.actions.rate_0', defaultCombo: ['Digit0'], section: 'rating' },
  { action: 'rate_1', description: 'settings.keybinds.actions.rate_1', defaultCombo: ['Digit1'], section: 'rating' },
  { action: 'rate_2', description: 'settings.keybinds.actions.rate_2', defaultCombo: ['Digit2'], section: 'rating' },
  { action: 'rate_3', description: 'settings.keybinds.actions.rate_3', defaultCombo: ['Digit3'], section: 'rating' },
  { action: 'rate_4', description: 'settings.keybinds.actions.rate_4', defaultCombo: ['Digit4'], section: 'rating' },
  { action: 'rate_5', description: 'settings.keybinds.actions.rate_5', defaultCombo: ['Digit5'], section: 'rating' },
  {
    action: 'color_label_none',
    description: 'settings.keybinds.actions.color_label_none',
    defaultCombo: ['shift', 'Digit0'],
    section: 'rating',
  },
  {
    action: 'color_label_red',
    description: 'settings.keybinds.actions.color_label_red',
    defaultCombo: ['shift', 'Digit1'],
    section: 'rating',
  },
  {
    action: 'color_label_yellow',
    description: 'settings.keybinds.actions.color_label_yellow',
    defaultCombo: ['shift', 'Digit2'],
    section: 'rating',
  },
  {
    action: 'color_label_green',
    description: 'settings.keybinds.actions.color_label_green',
    defaultCombo: ['shift', 'Digit3'],
    section: 'rating',
  },
  {
    action: 'color_label_blue',
    description: 'settings.keybinds.actions.color_label_blue',
    defaultCombo: ['shift', 'Digit4'],
    section: 'rating',
  },
  {
    action: 'color_label_purple',
    description: 'settings.keybinds.actions.color_label_purple',
    defaultCombo: ['shift', 'Digit5'],
    section: 'rating',
  },
  {
    action: 'toggle_adjustments',
    description: 'settings.keybinds.actions.toggle_adjustments',
    defaultCombo: ['KeyD'],
    section: 'panels',
  },
  {
    action: 'toggle_masks',
    description: 'settings.keybinds.actions.toggle_masks',
    // M shows or hides the mask, as in Capture One.
    defaultCombo: ['shift', 'KeyM'],
    section: 'panels',
  },
  {
    action: 'toggle_metadata',
    description: 'settings.keybinds.actions.toggle_metadata',
    defaultCombo: ['KeyI'],
    section: 'panels',
  },
  {
    action: 'toggle_folder_tree',
    description: 'settings.keybinds.actions.toggle_folder_tree',
    defaultCombo: [],
    section: 'panels',
  },
  {
    action: 'toggle_analytics',
    description: 'settings.keybinds.actions.toggle_analytics',
    defaultCombo: ['KeyA'],
    section: 'panels',
  },
  {
    action: 'toggle_export',
    description: 'settings.keybinds.actions.toggle_export',
    // Lightroom's export; E is the eraser.
    defaultCombo: ['ctrl', 'shift', 'KeyE'],
    section: 'panels',
  },
  // Lightroom's panel keys; Tab and Shift+Tab act only while no field or
  // control has the focus (useKeyboardShortcuts). The earlier ⇧⌘B and ⌘B stay
  // as aliases.
  {
    action: 'toggle_side_panels',
    description: 'settings.keybinds.actions.toggle_side_panels',
    defaultCombo: ['Tab'],
    section: 'panels',
  },
  {
    action: 'toggle_all_panels',
    description: 'settings.keybinds.actions.toggle_all_panels',
    defaultCombo: ['shift', 'Tab'],
    section: 'panels',
  },
  {
    action: 'toggle_left_panel',
    description: 'settings.keybinds.actions.toggle_left_panel',
    defaultCombo: ['F7'],
    aliasCombos: [['ctrl', 'shift', 'KeyB']],
    section: 'panels',
  },
  {
    action: 'toggle_right_panel',
    description: 'settings.keybinds.actions.toggle_right_panel',
    defaultCombo: ['F8'],
    aliasCombos: [['ctrl', 'KeyB']],
    section: 'panels',
  },
  {
    // Lightroom's F6; ⌘J is Capture One's Move to Selects.
    action: 'toggle_bottom_panel',
    description: 'settings.keybinds.actions.toggle_bottom_panel',
    defaultCombo: ['F6'],
    section: 'panels',
  },
  {
    action: 'open_settings',
    description: 'settings.keybinds.actions.open_settings',
    defaultCombo: ['ctrl', 'Comma'],
    section: 'library',
  },
  {
    action: 'focus_search',
    description: 'settings.keybinds.actions.focus_search',
    defaultCombo: ['ctrl', 'KeyF'],
    section: 'library',
  },
  // Cursor tools take Capture One's keys (user decision, 2026-10-03), in
  // Develop only; Lightroom's K (brush) stays as an alias. R is Capture
  // One's straighten tool (2026-10-04; it was Lightroom's crop alias).
  {
    action: 'tool_select',
    description: 'settings.keybinds.actions.tool_select',
    defaultCombo: ['KeyV'],
    section: 'tools',
  },
  {
    action: 'tool_crop',
    description: 'settings.keybinds.actions.tool_crop',
    defaultCombo: ['KeyC'],
    section: 'tools',
  },
  {
    action: 'tool_straighten',
    description: 'settings.keybinds.actions.tool_straighten',
    defaultCombo: ['KeyR'],
    section: 'tools',
  },
  {
    action: 'tool_brush',
    description: 'settings.keybinds.actions.tool_brush',
    defaultCombo: ['KeyB'],
    aliasCombos: [['KeyK']],
    section: 'tools',
  },
  {
    action: 'tool_erase',
    description: 'settings.keybinds.actions.tool_erase',
    defaultCombo: ['KeyE'],
    section: 'tools',
  },
  {
    action: 'tool_linear',
    description: 'settings.keybinds.actions.tool_linear',
    defaultCombo: ['KeyL'],
    section: 'tools',
  },
  {
    action: 'tool_radial',
    description: 'settings.keybinds.actions.tool_radial',
    defaultCombo: ['KeyT'],
    section: 'tools',
  },
  {
    action: 'tool_spot',
    description: 'settings.keybinds.actions.tool_spot',
    // Lightroom's spot removal and Capture One's healing mask; Capture One's
    // spot removal key O as an alias.
    defaultCombo: ['KeyQ'],
    aliasCombos: [['KeyO']],
    section: 'tools',
  },
  {
    action: 'remove_objects',
    description: 'settings.keybinds.actions.remove_objects',
    // The spot removal tool's Remove with Detect objects: a click or box
    // selects an object and removes it (Shift with the tool's key).
    defaultCombo: ['shift', 'KeyQ'],
    section: 'tools',
  },
  // A new layer with the subject or the background selected, from the
  // keyboard (Capture One's keys; Lightroom has none for them).
  {
    action: 'mask_subject',
    description: 'settings.keybinds.actions.mask_subject',
    defaultCombo: ['shift', 'Comma'],
    section: 'tools',
  },
  {
    action: 'mask_background',
    description: 'settings.keybinds.actions.mask_background',
    defaultCombo: ['shift', 'Period'],
    section: 'tools',
  },
  {
    action: 'tool_wb',
    description: 'settings.keybinds.actions.tool_wb',
    defaultCombo: ['KeyW'],
    section: 'tools',
  },
  { action: 'undo', description: 'settings.keybinds.actions.undo', defaultCombo: ['ctrl', 'KeyZ'], section: 'editing' },
  {
    action: 'redo',
    description: 'settings.keybinds.actions.redo',
    defaultCombo: ['ctrl', 'KeyY'],
    aliasCombos: [['ctrl', 'shift', 'KeyZ']],
    section: 'editing',
  },
  {
    action: 'copy_adjustments',
    description: 'settings.keybinds.actions.copy_adjustments',
    defaultCombo: ['ctrl', 'shift', 'KeyC'],
    aliasCombos: [['ctrl', 'KeyC']],
    section: 'editing',
  },
  {
    action: 'paste_adjustments',
    description: 'settings.keybinds.actions.paste_adjustments',
    defaultCombo: ['ctrl', 'shift', 'KeyV'],
    aliasCombos: [['ctrl', 'KeyV']],
    section: 'editing',
  },
  {
    action: 'auto_adjust',
    description: 'settings.keybinds.actions.auto_adjust',
    // Capture One; Lightroom uses ⌘U.
    defaultCombo: ['ctrl', 'KeyL'],
    aliasCombos: [['ctrl', 'KeyU']],
    section: 'editing',
  },
  {
    action: 'rotate_left',
    description: 'settings.keybinds.actions.rotate_left',
    defaultCombo: ['BracketLeft'],
    section: 'editing',
  },
  {
    action: 'rotate_right',
    description: 'settings.keybinds.actions.rotate_right',
    defaultCombo: ['BracketRight'],
    section: 'editing',
  },
  {
    action: 'toggle_crop',
    description: 'settings.keybinds.actions.toggle_crop',
    defaultCombo: ['KeyS'],
    section: 'editing',
  },
  {
    action: 'brush_size_up',
    description: 'settings.keybinds.actions.brush_size_up',
    defaultCombo: ['ctrl', 'ArrowUp'],
    section: 'editing',
  },
  {
    action: 'brush_size_down',
    description: 'settings.keybinds.actions.brush_size_down',
    defaultCombo: ['ctrl', 'ArrowDown'],
    section: 'editing',
  },
];

/** Combos that trigger an action: the user's own, else the default and its
 * aliases. An empty user combo disables the action. */
export function effectiveCombos(def: KeybindDefinition, keybinds?: Record<string, string[]> | null): string[][] {
  const userCombo = keybinds?.[def.action];
  if (userCombo !== undefined) return userCombo.length > 0 ? [userCombo] : [];
  return [def.defaultCombo, ...(def.aliasCombos ?? [])].filter((combo) => combo.length > 0);
}

/** Combo string ("ctrl+shift+KeyC") → actions bound to it, in definition
 * order; actions scoped to different views may share a key. */
export function buildComboMap(keybinds?: Record<string, string[]> | null): Map<string, string[]> {
  const map = new Map<string, string[]>();
  for (const def of KEYBIND_DEFINITIONS) {
    for (const combo of effectiveCombos(def, keybinds)) {
      const key = combo.join('+');
      map.set(key, [...(map.get(key) ?? []), def.action]);
    }
  }
  return map;
}

/** Actions that share a key with another action; the shortcut editor marks them. */
export function conflictingActions(keybinds?: Record<string, string[]> | null): Set<string> {
  const actions = new Set<string>();
  for (const bound of buildComboMap(keybinds).values()) {
    if (bound.length > 1) bound.forEach((action) => actions.add(action));
  }
  return actions;
}

const symMap: Record<string, string> = {
  Space: 'Space',
  Backspace: '⌫',
  Enter: 'Enter',
  Delete: 'Delete',
  ArrowUp: '↑',
  ArrowDown: '↓',
  ArrowLeft: '←',
  ArrowRight: '→',
  BracketLeft: '[',
  BracketRight: ']',
  Minus: '-',
  Equal: '+',
  Comma: ',',
  Period: '.',
  Slash: '/',
  Semicolon: ';',
  Quote: "'",
  Backquote: '`',
  Backslash: '\\',
  Tab: 'Tab',
  Escape: 'Esc',
  PageUp: 'Page Up',
  PageDown: 'Page Down',
  Home: 'Home',
  End: 'End',
  Insert: 'Insert',
  NumpadAdd: 'Numpad +',
  NumpadMultiply: 'Numpad *',
  NumpadDivide: 'Numpad /',
  NumpadSubtract: 'Numpad -',
  NumpadDecimal: 'Numpad .',
  NumpadComma: 'Numpad ,',
  NumpadEnter: 'Numpad Enter',
  NumpadEqual: 'Numpad =',
  CapsLock: 'Caps Lock',
  PrintScreen: 'PrtSc',
};

export function normalizeCombo(event: KeyboardEvent, osPlatform?: string): string[] {
  const isMacDelete = osPlatform === 'macos' && event.code === 'Backspace' && (event.ctrlKey || event.metaKey);
  const parts: string[] = [];
  if ((event.ctrlKey || event.metaKey) && !isMacDelete) parts.push('ctrl');
  if (event.shiftKey) parts.push('shift');
  if (event.altKey) parts.push('alt');
  let code = isMacDelete ? 'Delete' : event.code;
  if (event.key && /^[a-zA-Z]$/.test(event.key)) {
    code = `Key${event.key.toUpperCase()}`;
  } else if (/^Numpad[0-9]$/.test(code)) {
    code = `Digit${code.slice(-1)}`;
  } else if (code === 'NumpadAdd') {
    code = 'Equal';
  } else if (code === 'NumpadSubtract') {
    code = 'Minus';
  }
  if (isValidShortcutKey(code)) {
    parts.push(code);
  }
  return parts;
}

function codeToDisplayLabel(code: string): string | null {
  if (/^Key[A-Z]$/.test(code) || /^Digit[0-9]$/.test(code)) {
    return code[code.length - 1].toUpperCase();
  }
  if (/^Numpad[0-9]$/.test(code)) {
    return `Numpad ${code.slice(-1)}`;
  }
  return symMap[code] ?? null;
}

function isValidShortcutKey(code: string): boolean {
  if (code.startsWith('Key') || code.startsWith('Digit')) return true;
  if (code.startsWith('F') && /^\d+$/.test(code.slice(1))) return true;
  if (/^Numpad[0-9]$/.test(code)) return true;
  return code in symMap;
}

export function formatKeyCode(key: string, osPlatform: string): string {
  if (key === 'ctrl') return osPlatform === 'macos' ? '⌘' : 'Ctrl';
  if (key === 'shift') return 'Shift';
  if (key === 'alt') return osPlatform === 'macos' ? '⌥' : 'Alt';
  if (key === 'Delete' && osPlatform === 'macos') return 'Delete / ⌘+⌫';
  const label = codeToDisplayLabel(key);
  return label || key;
}

/** The first key of an action for tooltips ("B", "Ctrl + Shift + E"), or
 * null when it has none. */
export function shortcutHint(
  action: string,
  keybinds: Record<string, string[]> | null | undefined,
  osPlatform: string,
): string | null {
  const def = KEYBIND_DEFINITIONS.find((d) => d.action === action);
  const combo = def && effectiveCombos(def, keybinds)[0];
  return combo ? combo.map((k) => formatKeyCode(k, osPlatform)).join(' + ') : null;
}
