import { useSettingsStore } from '../store/useSettingsStore';
import { rememberedZoomLevel } from './viewerDisplay';

/**
 * Remembers the zoom level chosen last (the zoom menu, the 100 % button or
 * key) for Z, across launches; the viewer itself still opens at fit.
 */
export function rememberZoomLevel(level: number): void {
  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
  if (!appSettings) return;
  const remembered = rememberedZoomLevel(level);
  if (Math.abs(remembered - level) > 1e-3 || appSettings.viewerZoomLevel === remembered) return;
  void handleSettingsChange({ ...appSettings, viewerZoomLevel: remembered });
}
