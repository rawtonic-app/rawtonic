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
