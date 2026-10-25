// Where the native viewer shows the open photograph. Develop always shows
// it; the Library shows it in its loupe (one large photograph beside the
// Library tools) and in the full screen (the photograph alone, F). The
// Library's loupe and full screen use the same native display as Develop, so
// they show the developed photograph at screen resolution, never a
// thumbnail scaled up.

export interface ViewerModeState {
  activeView: string;
  libraryLoupe: boolean;
  isFullScreen: boolean;
}

/** Whether the centre shows the photograph in the native viewer. */
export function viewerShown(ui: ViewerModeState): boolean {
  return ui.activeView === 'editor' || ui.libraryLoupe || ui.isFullScreen;
}

/** The Library's loupe or its full screen: the viewer without Develop's tools. */
export function libraryViewer(ui: ViewerModeState): boolean {
  return ui.activeView !== 'editor' && (ui.libraryLoupe || ui.isFullScreen);
}

/** A view of the photograph without edit tools: the Library's loupe or any
 * full screen (Develop's tools are put aside meanwhile). */
export function viewerOnly(ui: ViewerModeState): boolean {
  return ui.isFullScreen || libraryViewer(ui);
}

/** What P, U and ⇧X flag: the photograph shown in Develop, the loupe or the
 * full screen, else the Library's selection. */
export function flagTargets(ui: ViewerModeState, shownPath: string | null | undefined, selection: string[]): string[] {
  return viewerShown(ui) && shownPath ? [shownPath] : [...selection];
}
