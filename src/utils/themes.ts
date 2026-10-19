/**
 * Colours of the inherited UI (`bg-surface`, `text-accent`, …): the Develop canvas,
 * Preferences and the dialogs. The studio chrome uses its own fixed colours; there is
 * no theme choice, so a `theme` value in old settings files is ignored.
 */
export const APP_COLOR_VARIABLES: Record<string, string> = {
  '--app-bg-primary': 'rgb(24, 24, 24)',
  '--app-bg-secondary': 'rgb(35, 35, 35)',
  '--app-surface': 'rgb(28, 28, 28)',
  '--app-card-active': 'rgb(43, 43, 43)',
  '--app-button-text': 'rgb(0, 0, 0)',
  '--app-text-primary': 'rgb(232, 234, 237)',
  '--app-text-secondary': 'rgb(158, 158, 158)',
  '--app-accent': 'rgb(255, 255, 255)',
  '--app-border-color': 'rgb(45, 45, 45)',
  '--app-hover-color': 'rgb(255, 255, 255)',
};
