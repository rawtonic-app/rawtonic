import { useEffect } from "react";
import { useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import type { AppSettings, BrushSettings } from "../ui/AppProperties";

const KEYS = ["size", "hardness", "opacity", "flow"] as const;
/** Saving waits for the end of a slider drag or a run of [ ] presses. */
const SAVE_DELAY_MS = 600;

const remembered = (brush: BrushSettings | null) =>
  Object.fromEntries(KEYS.filter((k) => brush?.[k] !== undefined).map((k) => [k, brush![k]]));

/** Keeps the brush's size, hardness, opacity and flow across photographs and
 * launches (Capture One), in the app settings' `brushDefaults`. */
export function useBrushMemory() {
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let restored = false;
    const restore = (settings: AppSettings | null) => {
      if (restored || !settings) return;
      restored = true;
      const saved = settings.brushDefaults;
      if (!saved) return;
      const { brushSettings, setEditor } = useEditorStore.getState();
      if (brushSettings) setEditor({ brushSettings: { ...brushSettings, ...saved } });
    };
    restore(useSettingsStore.getState().appSettings);
    const stopSettings = useSettingsStore.subscribe((s) => restore(s.appSettings));
    const stopBrush = useEditorStore.subscribe((state, previous) => {
      if (!restored || state.brushSettings === previous.brushSettings) return;
      clearTimeout(timer);
      timer = setTimeout(() => {
        const { appSettings, handleSettingsChange } = useSettingsStore.getState();
        if (!appSettings) return;
        const next = remembered(useEditorStore.getState().brushSettings);
        if (KEYS.every((k) => appSettings.brushDefaults?.[k] === next[k])) return;
        void handleSettingsChange({ ...appSettings, brushDefaults: next });
      }, SAVE_DELAY_MS);
    });
    return () => {
      clearTimeout(timer);
      stopSettings();
      stopBrush();
    };
  }, []);
}
