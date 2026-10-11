import i18n from 'i18next';
import type { Option } from '../../ui/AppProperties';
import { useSettingsStore } from '../../../store/useSettingsStore';
import { resolveViewerBackground, VIEWER_BACKGROUNDS, type ViewerBackground } from '../../../utils/viewerDisplay';

const LABELS: Record<ViewerBackground, () => string> = {
  black: () => i18n.t('studio.workspace.viewer.background.black'),
  veryDark: () => i18n.t('studio.workspace.viewer.background.veryDark'),
  dark: () => i18n.t('studio.workspace.viewer.background.dark'),
  medium: () => i18n.t('studio.workspace.viewer.background.medium'),
  light: () => i18n.t('studio.workspace.viewer.background.light'),
  white: () => i18n.t('studio.workspace.viewer.background.white'),
};

export const viewerBackgroundLabel = (background: ViewerBackground) => LABELS[background]();

/** Saves the viewer background in the app settings. */
export function setViewerBackground(background: ViewerBackground) {
  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
  if (!appSettings) return;
  void handleSettingsChange({ ...appSettings, viewerBackground: background });
}

/** The viewer's background menu (right-click on the empty area around the photograph). */
export function viewerBackgroundMenu(): Option[] {
  const current = resolveViewerBackground(useSettingsStore.getState().appSettings);
  return VIEWER_BACKGROUNDS.map((background) => ({
    label: viewerBackgroundLabel(background),
    checked: background === current,
    onClick: () => setViewerBackground(background),
  }));
}
