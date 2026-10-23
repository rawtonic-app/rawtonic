import {
  FlipHorizontal2,
  FlipVertical2,
  RectangleHorizontal,
  RectangleVertical,
  RotateCcw,
  RotateCw,
  Ruler,
} from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { OverlayMode } from "../panel/editor/overlays/CompositionOverlays";
import { useShallow } from "zustand/react/shallow";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import {
  calculateAreaPreservingCrop,
  calculateCenteredCrop,
} from "../../utils/cropUtils";
import type { CropGrid } from "../ui/AppProperties";
import AdjustmentControl from "./AdjustmentControl";
import { useStudioTools } from "./useStudioTools";
import { shortcutHint } from "../../utils/keyboardUtils";
import { runAutoStraighten } from "../../utils/autoStraighten";
import { selectOnFocus } from "../../utils/selectOnFocus";

const withKey = (title: string, key: string | null) =>
  key ? `${title} (${key})` : title;

export const DEFAULT_CROP_GRID: CropGrid = "drag";
export const DEFAULT_CROP_SHADE = 50;

/** Ids of the named ratios; their labels are translated when shown. */
const A_PAPER = "A paper (√2 × 1)";
const ORIGINAL = "Original";

/** Lightroom's and Capture One's ratios together (landscape; portrait is
 * 1 / r): prints (5 × 7, 8.5 × 11, A sizes), screens and film formats. */
const RATIOS: [number, string][] = [
  [1, "1 × 1"],
  [1.5, "3 × 2"],
  [4 / 3, "4 × 3"],
  [5 / 4, "5 × 4"],
  [7 / 5, "7 × 5"],
  [11 / 8.5, "11 × 8.5"],
  [Math.SQRT2, A_PAPER],
  [16 / 10, "16 × 10"],
  [16 / 9, "16 × 9"],
  [21 / 9, "21 × 9"],
  [65 / 24, "65 × 24"],
];

/** Composition overlays of the crop (Lightroom's Tool Overlay, Capture One's
 * guides), drawn by the viewer. */
const OVERLAYS = [
  "thirds",
  "phiGrid",
  "goldenSpiral",
  "goldenTriangle",
  "diagonal",
  "armature",
  "center",
] as const satisfies readonly OverlayMode[];

const near = (a: number, b: number) => Math.abs(a - b) < 1e-3;

/** The ratio as chosen, landscape or portrait (1 / r). */
function ratioLabel(ratio: number, extra: [number, string][] = []) {
  const landscape = ratio >= 1 ? ratio : 1 / ratio;
  const match = [...RATIOS, ...extra].find(([r]) => near(r, landscape));
  if (!match) return null;
  const parts = match[1].split(" × ");
  if (parts.length !== 2 || ratio >= 1) return match[1];
  return `${parts[1]} × ${parts[0]}`;
}

/** Crop settings after Capture One's crop tool (ratio with portrait /
 * landscape, size, straighten, grid) with Lightroom's reset and crop
 * overlay darkening. Shared by the Crop & geometry tab and the crop tool's
 * panel at the pointer. */
export default function StudioCropControls({
  inPopover = false,
}: {
  /** In the tool panel at the pointer, whose title has the reset. */
  inPopover?: boolean;
} = {}) {
  const { t } = useTranslation();
  const ratioName = (name: string) =>
    name === A_PAPER
      ? t("studio.crop.ratios.aPaper")
      : name === ORIGINAL
        ? t("studio.crop.ratios.original")
        : name;
  // Only the values the tool shows: other edits (a slider drag's commits)
  // do not re-render it.
  const { selectedImage, setEditor, ...adjustments } = useEditorStore(
    useShallow((s) => ({
      selectedImage: s.selectedImage,
      setEditor: s.setEditor,
      aspectRatio: s.adjustments.aspectRatio,
      orientationSteps: s.adjustments.orientationSteps,
      crop: s.adjustments.crop,
      rotation: s.adjustments.rotation,
      flipHorizontal: s.adjustments.flipHorizontal,
      flipVertical: s.adjustments.flipVertical,
    })),
  );
  const { appSettings, handleSettingsChange } = useSettingsStore(
    useShallow((s) => ({
      appSettings: s.appSettings,
      handleSettingsChange: s.handleSettingsChange,
    })),
  );
  const { setAdjustments, handleRotate, handleAutoStraighten } = useEditorActions();
  const [straightening, setStraightening] = useState(false);
  const autoStraighten = async () => {
    setStraightening(true);
    try {
      await runAutoStraighten(handleAutoStraighten);
    } finally {
      setStraightening(false);
    }
  };
  const { activeTool, selectTool } = useStudioTools();
  const straightenKey = useSettingsStore((s) =>
    shortcutHint("tool_straighten", s.appSettings?.keybinds, s.osPlatform),
  );
  const autoStraightenKey = useSettingsStore((s) =>
    shortcutHint("auto_straighten", s.appSettings?.keybinds, s.osPlatform),
  );
  // In the tools panel it keeps its look while the next photograph loads
  // (the panel is inert meanwhile); the viewer's popover is not inert.
  const ready = useEditorStore((s) =>
    inPopover ? Boolean(s.selectedImage?.isReady) : panelsShowReady(s),
  );
  const ratio = adjustments.aspectRatio || null;
  const steps = adjustments.orientationSteps || 0;
  const [orientedW, orientedH] =
    steps % 2 === 1
      ? [selectedImage?.height ?? 0, selectedImage?.width ?? 0]
      : [selectedImage?.width ?? 0, selectedImage?.height ?? 0];
  const crop = adjustments.crop;
  const size =
    crop && crop.unit === "px"
      ? [Math.round(crop.width), Math.round(crop.height)]
      : crop && crop.unit === "%"
        ? [
            Math.round((crop.width / 100) * orientedW),
            Math.round((crop.height / 100) * orientedH),
          ]
        : [orientedW, orientedH];

  const setRatio = (aspectRatio: number | null) => {
    if (!selectedImage?.isReady) return;
    const next =
      aspectRatio === null
        ? adjustments.crop
        : (calculateAreaPreservingCrop(
            selectedImage.width,
            selectedImage.height,
            adjustments.orientationSteps,
            aspectRatio,
            adjustments.rotation,
            adjustments.crop,
          ) ??
          calculateCenteredCrop(
            selectedImage.width,
            selectedImage.height,
            adjustments.orientationSteps,
            aspectRatio,
            adjustments.rotation,
          ));
    setAdjustments((prev) => ({ ...prev, aspectRatio, crop: next }));
  };
  // The photo's own ratio (Lightroom's Original, Capture One's Original)
  // and the user's saved ratios.
  const original = orientedW && orientedH ? Math.max(orientedW, orientedH) / Math.min(orientedW, orientedH) : null;
  const custom: [number, string][] = (appSettings?.customAspectRatios ?? [])
    .filter((r) => r.width > 0 && r.height > 0)
    .map((r) => [Math.max(r.width, r.height) / Math.min(r.width, r.height), `${r.width} × ${r.height}`]);
  const extra: [number, string][] = [...(original ? [[original, ORIGINAL] as [number, string]] : []), ...custom];
  const label = ratio ? ratioLabel(ratio, extra) : null;
  const [newRatio, setNewRatio] = useState<{ w: string; h: string } | null>(null);
  const overlay = useEditorStore((s) => s.overlayMode);
  const overlayTurn = useEditorStore((s) => s.overlayRotation);
  const grid = appSettings?.cropGrid ?? DEFAULT_CROP_GRID;
  const shade = appSettings?.cropShade ?? DEFAULT_CROP_SHADE;
  const setSetting = (patch: Partial<NonNullable<typeof appSettings>>) =>
    appSettings && handleSettingsChange({ ...appSettings, ...patch });

  return (
    <>
      <div className="studio-inline">
        <label>
          {t("studio.crop.ratio")}
          <select
            aria-label={t("studio.crop.aria.ratio")}
            value={
              ratio
                ? label
                  ? String(ratio >= 1 ? ratio : 1 / ratio)
                  : String(ratio)
                : "0"
            }
            disabled={!ready}
            onChange={(e) => {
              const chosen = Number(e.target.value) || null;
              // Keep the current orientation when changing the ratio.
              setRatio(chosen && ratio && ratio < 1 ? 1 / chosen : chosen);
            }}
          >
            {ratio && !label && (
              <option value={String(ratio)}>
                {t("studio.crop.ratios.current", { ratio: ratio.toFixed(3) })}
              </option>
            )}
            <option value="0">{t("studio.crop.ratios.unconstrained")}</option>
            {[...extra.slice(0, original ? 1 : 0), ...RATIOS, ...custom]
              .filter(([value], i, all) => all.findIndex(([v]) => near(v, value)) === i)
              .map(([value, name]) => (
                <option key={name} value={String(value)}>
                  {ratioName(name)}
                </option>
              ))}
          </select>
        </label>
        <button
          title={t("studio.crop.addRatioTitle")}
          aria-label={t("studio.crop.aria.addRatio")}
          disabled={!appSettings}
          onClick={() => setNewRatio(newRatio ? null : { w: "", h: "" })}
        >
          +
        </button>
        <button
          aria-label={ratio && ratio < 1 ? t("studio.crop.landscape") : t("studio.crop.portrait")}
          title={t("studio.crop.swapOrientation")}
          disabled={!ready || !ratio || Math.abs(ratio - 1) < 1e-3}
          onClick={() => ratio && setRatio(1 / ratio)}
        >
          {ratio && ratio < 1 ? (
            <RectangleVertical size={14} />
          ) : (
            <RectangleHorizontal size={14} />
          )}
        </button>
      </div>
      {newRatio && (
        // Lightroom's Enter Custom, Capture One's custom ratio: kept for
        // every photo.
        <form
          className="studio-inline"
          onSubmit={(e) => {
            e.preventDefault();
            const w = Number(newRatio.w);
            const h = Number(newRatio.h);
            if (!(w > 0 && h > 0) || !appSettings) return;
            setSetting({ customAspectRatios: [...(appSettings.customAspectRatios ?? []), { width: w, height: h }] });
            setNewRatio(null);
            setRatio(Math.max(w, h) / Math.min(w, h));
          }}
        >
          <input
            {...selectOnFocus}
            aria-label={t("studio.crop.aria.ratioWidth")}
            inputMode="decimal"
            value={newRatio.w}
            onChange={(e) => setNewRatio({ ...newRatio, w: e.target.value })}
          />
          ×
          <input
            {...selectOnFocus}
            aria-label={t("studio.crop.aria.ratioHeight")}
            inputMode="decimal"
            value={newRatio.h}
            onChange={(e) => setNewRatio({ ...newRatio, h: e.target.value })}
          />
          <button type="submit">{t("studio.crop.add")}</button>
        </form>
      )}
      <div className="studio-inline">
        <span>{t("studio.crop.size")}</span>
        <span aria-label={t("studio.crop.aria.size")}>
          {size[0]} × {size[1]} px
        </span>
      </div>
      <AdjustmentControl
        label={t("studio.crop.straighten")}
        min={-45}
        max={45}
        step={0.1}
        value={adjustments.rotation ?? 0}
        defaultValue={0}
        disabled={!ready}
        onInteraction={(active) => setEditor({ isSliderDragging: active })}
        onChange={(rotation) =>
          setAdjustments((prev) => ({ ...prev, rotation }))
        }
      />
      <div className="studio-inline">
        <span>{t("studio.crop.levelByLine")}</span>
        <button
          aria-label={t("studio.crop.aria.straightenTool")}
          title={withKey(t("studio.crop.straightenToolTitle"), straightenKey)}
          aria-pressed={activeTool === "straighten"}
          disabled={!ready}
          onClick={() => selectTool("straighten")}
        >
          <Ruler size={14} />
        </button>
        <button
          title={withKey(t("studio.crop.autoStraightenTitle"), autoStraightenKey)}
          disabled={!ready || straightening}
          onClick={() => void autoStraighten()}
        >
          {t("studio.crop.autoStraighten")}
        </button>
      </div>
      <div className="studio-inline studio-inline-wrap">
        <button disabled={!ready} onClick={() => handleRotate(-90)}>
          {t("studio.crop.rotateLeft")}
        </button>
        <button disabled={!ready} onClick={() => handleRotate(90)}>
          {t("studio.crop.rotateRight")}
        </button>
        <button
          title={t("studio.crop.flipHorizontal")}
          aria-label={t("studio.crop.flipHorizontal")}
          aria-pressed={Boolean(adjustments.flipHorizontal)}
          disabled={!ready}
          onClick={() => setAdjustments((prev) => ({ ...prev, flipHorizontal: !prev.flipHorizontal }))}
        >
          <FlipHorizontal2 size={14} />
        </button>
        <button
          title={t("studio.crop.flipVertical")}
          aria-label={t("studio.crop.flipVertical")}
          aria-pressed={Boolean(adjustments.flipVertical)}
          disabled={!ready}
          onClick={() => setAdjustments((prev) => ({ ...prev, flipVertical: !prev.flipVertical }))}
        >
          <FlipVertical2 size={14} />
        </button>
        {!inPopover && (
          <button
            title={t("studio.crop.resetTitle")}
            disabled={!ready}
            onClick={() =>
              setAdjustments((prev) => ({
                ...prev,
                crop: null,
                aspectRatio: null,
                rotation: 0,
              }))
            }
          >
            <RotateCcw size={12} />
            {t("studio.crop.reset")}
          </button>
        )}
      </div>
      <div className="studio-inline studio-inline-wrap">
        <label>
          {t("studio.crop.grid.label")}
          <select
            aria-label={t("studio.crop.aria.grid")}
            value={grid}
            disabled={!appSettings}
            onChange={(e) =>
              setSetting({ cropGrid: e.target.value as CropGrid })
            }
          >
            <option value="drag">{t("studio.crop.grid.drag")}</option>
            <option value="always">{t("studio.crop.grid.always")}</option>
            <option value="off">{t("studio.crop.off")}</option>
          </select>
        </label>
        <label>
          {t("studio.crop.overlay")}
          <select
            aria-label={t("studio.crop.aria.overlay")}
            value={overlay === "none" ? "thirds" : overlay}
            onChange={(e) => setEditor({ overlayMode: e.target.value as OverlayMode })}
          >
            {OVERLAYS.map((mode) => (
              <option key={mode} value={mode}>
                {t(`studio.crop.overlays.${mode}`)}
              </option>
            ))}
          </select>
        </label>
        {(overlay === "goldenSpiral" || overlay === "goldenTriangle") && (
          <button
            title={t("studio.crop.turnOverlay")}
            aria-label={t("studio.crop.turnOverlay")}
            onClick={() => setEditor({ overlayRotation: ((overlayTurn ?? 0) + 1) % 4 })}
          >
            <RotateCw size={12} />
          </button>
        )}
        <label>
          {t("studio.crop.outside")}
          <select
            aria-label={t("studio.crop.aria.outside")}
            value={shade}
            disabled={!appSettings}
            onChange={(e) => setSetting({ cropShade: Number(e.target.value) })}
          >
            {[100, 80, 50, 20, 0].map((value) => (
              <option key={value} value={value}>
                {value === 0 ? t("studio.crop.off") : `${value} %`}
              </option>
            ))}
          </select>
        </label>
      </div>
    </>
  );
}
