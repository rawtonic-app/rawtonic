/** Which parts of the workspace a module shows: the panels at the window's
 * left and right edge, Develop's filmstrip, and the bars (the top bar and the
 * status bar under the photograph or grid). Each module keeps its own layout,
 * as in Lightroom, persisted with the app settings (`uiVisibility.modules`). */
export type WorkspaceModule = 'library' | 'develop';
export type PanelGroup = 'left' | 'right' | 'filmstrip' | 'bars';
export type ModuleLayout = Record<PanelGroup, boolean>;
/** Tab (`sides`), Shift+Tab (`all`), F7, F8 and F6. */
export type PanelRequest = 'sides' | 'all' | 'left' | 'right' | 'filmstrip';

export const PANEL_GROUPS: PanelGroup[] = ['left', 'right', 'filmstrip', 'bars'];
export const SHOWN_LAYOUT: ModuleLayout = { left: true, right: true, filmstrip: true, bars: true };

/** Tools on the left (`tools-left`), or Develop's tools on the right with the
 * Library tools on the left (`split-tools`). */
export type ToolLayout = 'tools-left' | 'split-tools';

export function moduleOf(activeView: string): WorkspaceModule {
  return activeView === 'editor' ? 'develop' : 'library';
}

/** A module's stored layout; missing or unreadable values show the part. */
export function readModuleLayout(stored: unknown, module: WorkspaceModule): ModuleLayout {
  const entry = (stored as Record<string, unknown> | null | undefined)?.[module];
  const layout = { ...SHOWN_LAYOUT };
  if (entry && typeof entry === 'object') {
    for (const group of PANEL_GROUPS) {
      const value = (entry as Record<string, unknown>)[group];
      if (typeof value === 'boolean') layout[group] = value;
    }
  }
  return layout;
}

/** The parts a module has on screen in a tool layout. The Library tools beside
 * Develop's right-hand tools are dropped below 1200 px (`narrow`). */
export function presentGroups(module: WorkspaceModule, toolLayout: ToolLayout, narrow = false): PanelGroup[] {
  if (module === 'library') return ['left', 'bars'];
  if (toolLayout === 'split-tools') return narrow ? ['right', 'filmstrip', 'bars'] : ['left', 'right', 'filmstrip', 'bars'];
  return ['left', 'filmstrip', 'bars'];
}

export function requestGroups(request: PanelRequest, present: PanelGroup[]): PanelGroup[] {
  const wanted: PanelGroup[] =
    request === 'sides' ? ['left', 'right'] : request === 'all' ? PANEL_GROUPS : [request];
  return wanted.filter((group) => present.includes(group));
}

/** A group of parts hides while any of it shows and comes back once all of it
 * is hidden (Lightroom's Tab and Shift+Tab). */
export function togglePanelGroups(layout: ModuleLayout, groups: PanelGroup[]): ModuleLayout {
  if (!groups.length) return layout;
  const show = groups.every((group) => !layout[group]);
  return setPanelGroups(layout, groups, show);
}

export function setPanelGroups(layout: ModuleLayout, groups: PanelGroup[], shown: boolean): ModuleLayout {
  const next = { ...layout };
  for (const group of groups) next[group] = shown;
  return next;
}
