/** Translate legacy Mac key notation at the display boundary. Recipes and bindings are untouched. */
export function platformLabel(label: string, platform: string): string {
  if (platform !== 'windows' && platform !== 'linux') return label;
  return label
    .replace(/⌘K \/ Ctrl\+K/g, 'Ctrl+K')
    .replace(/([⇧⌥⌃⌘]+)([^\s)。,;]+)?/g, (_, modifiers: string, key = '') => {
      const parts = [];
      if (modifiers.includes('⌘') || modifiers.includes('⌃')) parts.push('Ctrl');
      if (modifiers.includes('⇧')) parts.push('Shift');
      if (modifiers.includes('⌥')) parts.push('Alt');
      return [...parts, key].filter(Boolean).join('+');
    })
    .replace(/Alt\/Option/g, 'Alt')
    .replace(/Alt \(Alt\)/g, 'Alt')
    .replace(/Alt \(Option\)/g, 'Alt')
    .replace(/\bOption(?=[-:+)]|$)/g, 'Alt');
}

/** Plain copy is the fast remembered choice in Develop; the picker remains
 * on Shift+copy, in the Library, and on macOS. */
export function quickSettingsCopy(platform: string, view: string, shift: boolean): boolean {
  return (platform === 'windows' || platform === 'linux') && view === 'editor' && !shift;
}
