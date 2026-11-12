/** Translate legacy Mac key notation at the display boundary. Recipes and bindings are untouched. */
export function platformLabel(label: string, platform: string): string {
  if (platform !== 'windows' && platform !== 'linux') return label;
  return label
    .replace(/([⇧⌥⌃⌘]+)(,|[^\s)。,;]+)?/g, (_, modifiers: string, key = '') => {
      const parts = [];
      if (modifiers.includes('⌘') || modifiers.includes('⌃')) parts.push('Ctrl');
      if (modifiers.includes('⇧')) parts.push('Shift');
      if (modifiers.includes('⌥')) parts.push('Alt');
      return [...parts, key].filter(Boolean).join('+');
    })
    .replace(/\b(Ctrl(?:\+[A-Za-z0-9]+)+) \/ \1\b/g, '$1')
    .replace(/\bReturn\b/g, 'Enter')
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

/** Transform only bundled resources, before i18next inserts names and paths. */
export function platformResources<T>(resources: T, platform: string): T {
  if (typeof resources === 'string') return platformLabel(resources, platform) as T;
  if (Array.isArray(resources)) return resources.map((value) => platformResources(value, platform)) as T;
  if (resources && typeof resources === 'object') {
    return Object.fromEntries(
      Object.entries(resources).map(([key, value]) => [key, platformResources(value, platform)])
    ) as T;
  }
  return resources;
}

export function fileManagerLabelKey(platform: string) {
  if (platform === 'macos') return 'contextMenus.thumbnail.showFinder';
  if (platform === 'linux') return 'contextMenus.thumbnail.showFileManager';
  return 'contextMenus.thumbnail.showExplorer';
}
