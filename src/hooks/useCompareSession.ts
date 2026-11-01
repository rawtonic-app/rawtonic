import { useEffect } from 'react';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useMultiViewStore } from '../store/useMultiViewStore';
import { clampSplitPosition, DEFAULT_SPLIT_POSITION, parseSplitOrientation, splitViewAvailable } from '../utils/beforeAfter';

/** View preferences belong to the opened session, never its photographs or recipes. */
export function useCompareSession() {
  const directory = useLibraryStore((s) => s.sessionDirectory);
  useEffect(() => {
    if (!directory) return;
    const key = `rawtonic.compare:${directory}`;
    let saved: Record<string, unknown> = {};
    try {
      const value: unknown = JSON.parse(localStorage.getItem(key) ?? '{}');
      if (value && typeof value === 'object') saved = value as Record<string, unknown>;
    } catch { /* Storage unavailable or invalid: use the defaults. */ }
    // Restore before a photo is selected, without overriding an active multi
    // view or enabling a split the display cannot present.
    const { appSettings, osPlatform } = useSettingsStore.getState();
    const enabled = saved.enabled === true &&
      splitViewAvailable(appSettings?.useWgpuRenderer, osPlatform) &&
      !useMultiViewStore.getState().active;
    useEditorStore.getState().setEditor({
      splitView: enabled,
      splitOrientation: parseSplitOrientation(saved.orientation),
      splitPosition: typeof saved.position === 'number' ? clampSplitPosition(saved.position) : DEFAULT_SPLIT_POSITION,
    });
    return useEditorStore.subscribe((state, previous) => {
      if (state.splitView === previous.splitView && state.splitOrientation === previous.splitOrientation && state.splitPosition === previous.splitPosition) return;
      try {
        localStorage.setItem(key, JSON.stringify({ enabled: state.splitView, orientation: state.splitOrientation, position: state.splitPosition }));
      } catch { /* The controls still work without persistence. */ }
    });
  }, [directory]);
}
