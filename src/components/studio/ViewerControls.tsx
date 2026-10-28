import { SquareSquare } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { shortcutHint } from "../../utils/keyboardUtils";
import { captureInfoParts } from "../../utils/captureInfo";
import { fitZoomValue, zoomPresetsAboveFit } from "../../utils/viewerDisplay";

/** Capture One's proof margin: fit leaves a margin around the photograph (Alt+X). */
export function ProofMarginButton() {
  const { t } = useTranslation();
  const appSettings = useSettingsStore((s) => s.appSettings);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const on = appSettings?.viewerProofMargin ?? false;
  const label = t("studio.workspace.viewer.proofMargin");
  const key = shortcutHint("toggle_proof_margin", appSettings?.keybinds, osPlatform);
  return (
    <button
      aria-pressed={on}
      aria-label={label}
      title={t("studio.workspace.withShortcut", {
        label: t("studio.workspace.viewer.proofMarginTitle"),
        shortcut: key ?? t("studio.workspace.noKey"),
      })}
      disabled={!appSettings}
      onClick={() => {
        const { appSettings: current, handleSettingsChange } = useSettingsStore.getState();
        if (current) void handleSettingsChange({ ...current, viewerProofMargin: !on });
      }}
    >
      <SquareSquare size={14} />
    </button>
  );
}

/** The zoom menu's entries; those below fit are left out (the viewer never goes below fit). */
export function ZoomPresetOptions() {
  const fit = useEditorStore((s) => {
    const { originalSize, baseRenderSize, adjustments } = s;
    const swapped = (adjustments.orientationSteps || 0) % 2 === 1;
    const width = swapped ? originalSize.height : originalSize.width;
    const height = swapped ? originalSize.width : originalSize.height;
    const dpr = typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1;
    return fitZoomValue(width, height, baseRenderSize?.width ?? 0, baseRenderSize?.height ?? 0, dpr);
  });
  return (
    <>
      {zoomPresetsAboveFit(fit).map((preset) => (
        <option key={preset} value={String(preset)}>
          {`${Math.round(preset * 100)}%`}
        </option>
      ))}
    </>
  );
}

/** ISO, shutter speed, aperture and focal length under the photograph. */
export function ViewerCaptureInfo() {
  const exif = useEditorStore((s) => s.selectedImage?.exif);
  const parts = captureInfoParts(exif);
  if (parts.length === 0) return null;
  return (
    <span className="studio-capture-info" title={parts.join("  ")}>
      {parts.map((part) => (
        <span key={part}>{part}</span>
      ))}
    </span>
  );
}
