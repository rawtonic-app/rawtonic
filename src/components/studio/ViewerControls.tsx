import { SquareSquare } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { shortcutHint } from "../../utils/keyboardUtils";
import { captureInfoParts } from "../../utils/captureInfo";
import { fitZoomValue, zoomPresetsAboveFit } from "../../utils/viewerDisplay";
import { readoutColor, readoutValues, usePointerSampleStore } from "../../utils/pointerSample";

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

/** Capture One's pointer readout: R, G, B of the displayed photograph under
 * the pointer (the colour pickers' 11 × 11 sample, sRGB), when the View
 * menu's Pointer RGB Readout is on. A click switches 0–255 and percent. */
export function PointerReadout() {
  const { t } = useTranslation();
  const on = useSettingsStore((s) => s.appSettings?.pointerReadout ?? false);
  const unit = useSettingsStore((s) => (s.appSettings?.pointerReadoutUnit === "percent" ? "percent" : "rgb255"));
  const patch = usePointerSampleStore((s) => s.sample?.patch ?? null);
  if (!on) return null;
  const color = patch && readoutColor(patch);
  const values = color ? readoutValues(color, unit) : ["–", "–", "–"];
  return (
    <button
      className="studio-pointer-readout"
      data-testid="pointer-readout"
      data-unit={unit}
      aria-label={t("studio.workspace.viewer.pointerReadout")}
      title={t("studio.workspace.viewer.pointerReadoutTitle")}
      onClick={() => {
        const { appSettings: current, handleSettingsChange } = useSettingsStore.getState();
        if (current)
          void handleSettingsChange({ ...current, pointerReadoutUnit: unit === "percent" ? "rgb255" : "percent" });
      }}
    >
      {(["R", "G", "B"] as const).map((channel, index) => (
        <span key={channel}>
          <span className="studio-muted">{channel}</span>
          <span className="studio-pointer-readout-value">{values[index]}</span>
        </span>
      ))}
      {unit === "percent" && <span className="studio-muted">%</span>}
    </button>
  );
}
