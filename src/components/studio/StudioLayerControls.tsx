import { Eraser, FlipHorizontal2, PenLine, Pipette, Sun, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { featherOf, type RadialParams } from "../../utils/radialMask";
import AdjustmentControl from "./AdjustmentControl";
import { DEFAULT_BRUSH_SIZE } from "../../utils/brushSize";
import { createSubMask, DEFAULT_LUMA_RANGE } from "../../utils/maskUtils";
import { MASK_VERSION } from "../../utils/adjustments";
import { Mask } from "../panel/right/Masks";
import { warnIfComponentStartsLayer } from "./layerLimitToast";
import { keepWithinLayerLimit } from "../../utils/layerLimit";
import { usePenAvailable } from "../../utils/penPressure";
import {
  BRUSH_SIZE_MAX,
  BRUSH_SIZE_MIN,
  useStudioTools,
} from "./useStudioTools";

/** Opacity, radial feather, brush settings and Draw/Erase/Invert of the
 * selected layer. Shared by the Layers & masks tab and the viewer's tool
 * settings popover, so both always show the same state. */
export default function StudioLayerControls({
  compact = false,
}: {
  /** The tool panel at the pointer: with the brush only its four settings
   * (Capture One's brush panel), without the layer's own opacity. */
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const {
    layer,
    brushActive,
    activeBrushDraws,
    radialMask,
    brushSettings,
    paintOnLayer,
    toggleInvert,
    setBrushSize,
  } = useStudioTools();
  const setEditor = useEditorStore((s) => s.setEditor);
  const lumaPickerLayer = useEditorStore((s) => s.lumaPickerLayer);
  const { setAdjustments } = useEditorActions();
  const appSettings = useSettingsStore((s) => s.appSettings);
  const handleSettingsChange = useSettingsStore((s) => s.handleSettingsChange);
  const penAvailable = usePenAvailable();
  if (!layer) return null;
  const onInteraction = (active: boolean) =>
    setEditor({ isSliderDragging: active });
  // Luma range sliders show the mask while dragged (Capture One).
  const onRangeInteraction = (active: boolean) =>
    setEditor({ isSliderDragging: active, isMaskRangeDragging: active });
  const setBrush = (patch: Partial<NonNullable<typeof brushSettings>>) =>
    setEditor({ brushSettings: { ...brushSettings!, ...patch } });
  const brushOnly = compact && brushActive;
  const lumaRange = layer.subMasks.find((m) => m.type === Mask.LumaRange);
  const range = { ...DEFAULT_LUMA_RANGE, ...(lumaRange?.parameters ?? {}) } as typeof DEFAULT_LUMA_RANGE;
  const layerId = layer.id;
  type SubMasks = typeof layer.subMasks;
  const updateLayer = (subMasks: (subs: SubMasks) => SubMasks) =>
    setAdjustments((prev) => keepWithinLayerLimit(prev, {
      ...prev,
      masks: prev.masks.map((m) => (m.id === layerId ? { ...m, subMasks: subMasks(m.subMasks) } : m)),
    }));
  // Editing the range replaces a Lightroom range imported with a preset
  // (`lrRange`, in the photo's luminance percentiles).
  const setRange = (patch: Partial<typeof DEFAULT_LUMA_RANGE>) =>
    updateLayer((subs) =>
      subs.map((sub) => {
        if (sub.type !== Mask.LumaRange) return sub;
        const { lrRange: _imported, ...parameters } = (sub.parameters ?? {}) as Record<string, unknown>;
        return { ...sub, parameters: { ...parameters, ...patch } };
      }),
    );
  // Where the range covers fully (accent) and fades out, over the tonal scale.
  const low = Math.min(range.rangeLow, range.rangeHigh);
  const high = Math.max(range.rangeLow, range.rangeHigh);
  const coverage = `linear-gradient(to right, transparent ${low - range.falloff}%, var(--studio-accent, #9cc2e0) ${low}%, var(--studio-accent, #9cc2e0) ${high}%, transparent ${high + range.falloff}%)`;
  return (
    <>
      {!brushOnly && (
        <AdjustmentControl
          label={t("editor.masks.settings.opacity")}
          min={0}
          value={layer.opacity}
          defaultValue={100}
          onInteraction={onInteraction}
          onChange={(v) =>
            setAdjustments((prev) => ({
              ...prev,
              masks: prev.masks.map((m) =>
                m.id === layer.id ? { ...m, opacity: v } : m,
              ),
            }))
          }
        />
      )}
      {!brushOnly && (
        // Capture One's Refine Mask: near its edge the layer follows the
        // photo's own edges (notes: reference/local-masks-baseline.md "WP5").
        // It needs the 16-bit mask, so it moves the layer to the current
        // mask version (MASK_VERSION).
        <div title={t("studio.layers.refineEdgeTitle")}>
          <AdjustmentControl
            label={t("studio.layers.refineEdge")}
            min={0}
            max={200}
            unit="px"
            value={layer.refineEdge ?? 0}
            defaultValue={0}
            onInteraction={onInteraction}
            onChange={(radius) =>
              setAdjustments((prev) => ({
                ...prev,
                masks: prev.masks.map((m) => {
                  if (m.id !== layer.id) return m;
                  const { refineEdge: _old, ...rest } = m;
                  return radius > 0
                    ? { ...rest, maskVersion: MASK_VERSION, refineEdge: radius }
                    : rest;
                }),
              }))
            }
          />
        </div>
      )}
      {radialMask && (
        // Capture One sets a radial gradient's feathering on the canvas; the
        // same value is also available here as a slider.
        <AdjustmentControl
          label={t("editor.masks.params.feather")}
          min={0}
          max={100}
          unit="%"
          value={Math.round(
            featherOf(radialMask.parameters as RadialParams) * 100,
          )}
          defaultValue={50}
          onChange={(feather) =>
            setAdjustments((prev) => ({
              ...prev,
              masks: prev.masks.map((m) =>
                m.id !== layer.id
                  ? m
                  : {
                      ...m,
                      subMasks: m.subMasks.map((sub) =>
                        sub.id === radialMask.id
                          ? {
                              ...sub,
                              parameters: {
                                ...sub.parameters,
                                feather: feather / 100,
                              },
                            }
                          : sub,
                      ),
                    },
              ),
            }))
          }
        />
      )}
      {radialMask && (
        // Capture One's radial panel: which side of the gradient the mask
        // covers. It sets this gradient and the default for new ones.
        <label className="studio-inline studio-check">
          <input
            type="checkbox"
            checked={!radialMask.invert}
            onChange={(e) => {
              const inside = e.target.checked;
              setAdjustments((prev) => ({
                ...prev,
                masks: prev.masks.map((m) =>
                  m.id !== layer.id
                    ? m
                    : {
                        ...m,
                        subMasks: m.subMasks.map((sub) =>
                          sub.id === radialMask.id ? { ...sub, invert: !inside } : sub,
                        ),
                      },
                ),
              }));
              if (appSettings) handleSettingsChange({ ...appSettings, radialDrawInside: inside });
            }}
          />
          {t("studio.layers.drawInside")}
        </label>
      )}
      {brushActive && (
        <>
          <AdjustmentControl
            label={brushOnly ? t("studio.layers.brush.size") : t("studio.layers.brush.brushSize")}
            min={BRUSH_SIZE_MIN}
            max={BRUSH_SIZE_MAX}
            value={brushSettings?.size ?? DEFAULT_BRUSH_SIZE}
            defaultValue={DEFAULT_BRUSH_SIZE}
            unit="px"
            onChange={setBrushSize}
          />
          {/* Capture One's brush: hardness shapes the tip (50% coverage at
              the brush size); opacity caps each stroke, strokes build up over
              each other; flow sets how fast a stroke builds up towards its
              opacity. */}
          <AdjustmentControl
            label={t("studio.layers.brush.hardness")}
            min={0}
            max={100}
            value={brushSettings?.hardness ?? 50}
            defaultValue={50}
            onChange={(hardness) => setBrush({ hardness })}
          />
          <AdjustmentControl
            label={brushOnly ? t("editor.masks.settings.opacity") : t("studio.layers.brush.brushOpacity")}
            min={1}
            max={100}
            value={brushSettings?.opacity ?? 100}
            defaultValue={100}
            onChange={(opacity) => setBrush({ opacity })}
          />
          <AdjustmentControl
            label={brushOnly ? t("editor.masks.brush.flow") : t("studio.layers.brush.brushFlow")}
            min={1}
            max={100}
            value={brushSettings?.flow ?? 100}
            defaultValue={100}
            onChange={(flow) => setBrush({ flow })}
          />
          {/* Lightroom's and Capture One's Auto Mask, measured in Camera Raw
              (notes: reference/auto-mask.md): new strokes keep to colours
              like the one under the brush centre. */}
          <label className="studio-check">
            <input
              type="checkbox"
              checked={!!brushSettings?.autoMask}
              onChange={(e) => setBrush({ autoMask: e.target.checked })}
            />
            {t("studio.layers.brush.autoMask")}
          </label>
          {/* Pen pressure (Capture One: size; flow as well): only strokes
              drawn with a pressure-sensitive pen carry it, so it shows once
              the webview has reported a pen. */}
          {penAvailable && (
            <div className="studio-inline" title={t("studio.layers.brush.penPressureTitle")}>
              <span>{t("studio.layers.brush.penPressure")}</span>
              <label className="studio-check">
                <input
                  type="checkbox"
                  checked={brushSettings?.pressureSize ?? true}
                  onChange={(e) => setBrush({ pressureSize: e.target.checked })}
                />
                {t("studio.layers.brush.pressureSize")}
              </label>
              <label className="studio-check">
                <input
                  type="checkbox"
                  checked={!!brushSettings?.pressureFlow}
                  onChange={(e) => setBrush({ pressureFlow: e.target.checked })}
                />
                {t("studio.layers.brush.pressureFlow")}
              </label>
            </div>
          )}
        </>
      )}
      {/* Draw and Erase paint on the layer's mask (one is active while
          painting); Invert flips the layer's coverage. */}
      <div className="studio-segments studio-layer-paint" role="group" aria-label={t("studio.layers.paintAria")}>
        <button
          title={t("studio.layers.drawTitle")}
          aria-pressed={brushActive && activeBrushDraws}
          onClick={() => paintOnLayer(layer, true)}
        >
          <PenLine size={12} />
          <span>{t("studio.layers.draw")}</span>
        </button>
        <button
          title={t("studio.layers.eraseTitle")}
          aria-pressed={brushActive && !activeBrushDraws}
          onClick={() => paintOnLayer(layer, false)}
        >
          <Eraser size={12} />
          <span>{t("studio.layers.erase")}</span>
        </button>
        {!brushOnly && (
          <button
            title={t("studio.layers.invertTitle")}
            aria-pressed={Boolean(layer.invert)}
            onClick={() => toggleInvert(layer)}
          >
            <FlipHorizontal2 size={12} />
            <span>{t("studio.layers.invert")}</span>
          </button>
        )}
      </div>
      {!compact &&
        (lumaRange ? (
          // Capture One's Luma Range: the layer only acts on these tones.
          <div className="studio-luma-range" aria-label={t("studio.layers.lumaRange.title")}>
            <div className="studio-inline">
              <span>{t("studio.layers.lumaRange.title")}</span>
              {/* Lightroom's Luminance Range picker: sets the range from
                  tones on the photo (notes: reference/layers-panel-c1-lr.md). */}
              <button
                title={t("studio.layers.lumaRange.sample")}
                aria-label={t("studio.layers.lumaRange.sample")}
                aria-pressed={lumaPickerLayer === layerId}
                onClick={() => setEditor({ lumaPickerLayer: lumaPickerLayer === layerId ? null : layerId })}
              >
                <Pipette size={12} />
              </button>
              <button
                title={t("studio.layers.lumaRange.removeTitle")}
                aria-label={t("studio.layers.lumaRange.removeAria")}
                onClick={() => updateLayer((subs) => subs.filter((sub) => sub.type !== Mask.LumaRange))}
              >
                <X size={12} />
              </button>
            </div>
            {lumaPickerLayer === layerId && (
              <small className="studio-hint" role="note">
                {t("studio.layers.lumaRange.sampleHint")}
              </small>
            )}
            <div className="studio-luma-scale" aria-hidden="true">
              <span style={{ background: coverage }} />
            </div>
            <AdjustmentControl
              label={t("studio.layers.lumaRange.shadows")}
              min={0}
              max={100}
              unit="%"
              value={range.rangeLow}
              defaultValue={0}
              onInteraction={onRangeInteraction}
              onChange={(rangeLow) => setRange({ rangeLow })}
            />
            <AdjustmentControl
              label={t("studio.layers.lumaRange.highlights")}
              min={0}
              max={100}
              unit="%"
              value={range.rangeHigh}
              defaultValue={100}
              onInteraction={onRangeInteraction}
              onChange={(rangeHigh) => setRange({ rangeHigh })}
            />
            <AdjustmentControl
              label={t("studio.layers.lumaRange.falloff")}
              min={0}
              max={100}
              unit="%"
              value={range.falloff}
              defaultValue={20}
              onInteraction={onRangeInteraction}
              onChange={(falloff) => setRange({ falloff })}
            />
          </div>
        ) : (
          <button
            className="studio-text-button"
            title={t("studio.layers.lumaRange.limitTitle")}
            onClick={() => {
              if (warnIfComponentStartsLayer(layer.id)) return;
              updateLayer((subs) => [...subs, createSubMask(Mask.LumaRange, { width: 0, height: 0 })]);
            }}
          >
            <Sun size={12} />
            {t("studio.layers.lumaRange.limit")}

          </button>
        ))}
    </>
  );
}
