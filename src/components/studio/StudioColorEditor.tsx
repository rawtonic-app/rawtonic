import { Eye, Pipette, Plus, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useShallow } from "zustand/react/shallow";
import { useEditorStore } from "../../store/useEditorStore";
import {
  MAX_COLOR_RANGES,
  MAX_LAYER_COLOR_RANGES,
  type Adjustments,
  type ColorRange,
} from "../../utils/adjustments";
import { colorRangesOf } from "../../utils/colorRanges";
import { oklchCss } from "../../utils/oklab";
import AdjustmentControl from "./AdjustmentControl";
import { ToolGroup } from "./StudioDevelop";
import type { ToolParams } from "./StudioToolCopy";

interface Props {
  values: Partial<Adjustments>;
  /** The selected mask layer, when the tool edits a layer. */
  layerId?: string;
  disabled: boolean;
  change(patch: Partial<Adjustments>): void;
  onInteraction(active: boolean): void;
  toggle?: { enabled: boolean; onToggle(): void };
  copy?: ToolParams;
}

/** The range's sliders (their accessible names name the group). */
type SliderKey =
  | "hueShift"
  | "saturation"
  | "lightnessShift"
  | "hueRange"
  | "chromaRange"
  | "lightnessRange"
  | "smoothness"
  | "hueUniformity"
  | "saturationUniformity"
  | "lightnessUniformity";

/** Swatch of a range after its own hue, saturation and lightness changes. */

export function rangeSwatch(range: ColorRange, shifted = false) {
  if (!shifted) return oklchCss(range.lightness, range.chroma, range.hue);
  return oklchCss(
    Math.min(
      1,
      Math.max(0, range.lightness * (1 + range.lightnessShift * 0.004)),
    ),
    Math.max(0, range.chroma * (1 + range.saturation / 100)),
    (range.hue + range.hueShift * 0.5 + 360) % 360,
  );
}

/**
 * Capture One's Advanced Color Editor / Lightroom's Point Color: pick a
 * colour in the photo, then shift the hue, saturation and lightness of
 * that colour range, shape the range, and even it out (uniformity, e.g.
 * for skin). Up to eight ranges; Show range greys out everything else.
 */
export default function StudioColorEditor({
  values,
  layerId,
  disabled,
  change,
  onInteraction,
  toggle,
  copy,
}: Props) {
  const { t } = useTranslation();
  const { pickerTarget, activeId, showRange, setEditor } = useEditorStore(
    useShallow((s) => ({
      pickerTarget: s.colorPickerTarget,
      activeId: s.activeColorRangeId,
      showRange: s.colorRangeShow,
      setEditor: s.setEditor,
    })),
  );
  const ranges = colorRangesOf(values);
  const active = ranges.find((r) => r.id === activeId) ?? ranges[0];
  const setRange = (patch: Partial<ColorRange>) =>
    active &&
    change({
      colorRanges: ranges.map((r) =>
        r.id === active.id ? { ...r, ...patch } : r,
      ),
    });
  const limit = layerId ? MAX_LAYER_COLOR_RANGES : MAX_COLOR_RANGES;
  const hue = t("adjustments.color.hue");
  const saturation = t("adjustments.color.saturation");
  const lightness = t("studio.color.editor.lightness");
  const pick = (target: string) =>
    setEditor({
      colorPickerTarget: pickerTarget === target ? null : target,
      colorPickerLayer: layerId ?? null,
      isWbPickerActive: false,
    });
  const slider = (
    label: string,
    key: SliderKey,
    min: number,
    max: number,
    fallback: number,
  ) =>
    active && (
      <AdjustmentControl
        key={key}
        label={label}
        ariaLabel={t(`studio.color.editor.aria.${key}`)}
        value={(active[key] as number) ?? fallback}
        min={min}
        max={max}
        defaultValue={fallback}
        disabled={disabled}
        onInteraction={onInteraction}
        onChange={(v) => setRange({ [key]: v })}
      />
    );
  return (
    <ToolGroup
      title="Color editor"
      label={t("studio.color.editor.title")}
      toggle={toggle}
      copy={copy}
      open={ranges.length > 0}
      onReset={() => {
        setEditor({
          activeColorRangeId: null,
          colorPickerTarget: null,
          colorRangeShow: null,
        });
        change({ colorRanges: [] });
      }}
    >
      <div className="studio-color-ranges">
        {ranges.map((range) => (
          <button
            key={range.id}
            type="button"
            className="studio-color-range"
            aria-pressed={active?.id === range.id}
            aria-label={t("studio.color.editor.rangeAria", {
              hue: Math.round(range.hue),
            })}
            title={t("studio.color.editor.selectRange")}
            style={{
              background: `linear-gradient(90deg, ${rangeSwatch(range)} 50%, ${rangeSwatch(range, true)} 50%)`,
            }}
            onClick={() => setEditor({ activeColorRangeId: range.id })}
          />
        ))}
        <button
          type="button"
          className="studio-color-range-add"
          aria-pressed={pickerTarget === "new"}
          aria-label={t("studio.color.editor.pickAria")}
          title={t("studio.color.editor.pickTitle")}
          disabled={disabled || ranges.length >= limit}
          onClick={() => pick("new")}
        >
          <Plus size={13} />
        </button>
      </div>
      {!active && (
        <p className="studio-hint">
          {pickerTarget === "new"
            ? t("studio.color.editor.hintPicking")
            : t("studio.color.editor.hintEmpty")}
        </p>
      )}
      {active && (
        <>
          <div className="studio-inline studio-color-range-actions">
            <button
              type="button"
              aria-pressed={pickerTarget === active.id}
              title={t("studio.color.editor.pickAgainTitle")}
              aria-label={t("studio.color.editor.pickAgainAria")}
              disabled={disabled}
              onClick={() => pick(active.id)}
            >
              <Pipette size={14} />
            </button>
            {!layerId && (
              <button
                type="button"
                aria-pressed={showRange === active.id}
                title={t("studio.color.editor.showTitle")}
                aria-label={t("studio.color.editor.showAria")}
                disabled={disabled}
                onClick={() =>
                  setEditor({
                    colorRangeShow: showRange === active.id ? null : active.id,
                  })
                }
              >
                <Eye size={14} />
              </button>
            )}
            <button
              type="button"
              title={t("studio.color.editor.removeTitle")}
              aria-label={t("studio.color.editor.removeAria")}
              disabled={disabled}
              onClick={() => {
                setEditor({
                  activeColorRangeId: null,
                  ...(showRange === active.id ? { colorRangeShow: null } : {}),
                });
                change({
                  colorRanges: ranges.filter((r) => r.id !== active.id),
                });
              }}
            >
              <Trash2 size={14} />
            </button>
          </div>
          {slider(hue, "hueShift", -100, 100, 0)}
          {slider(saturation, "saturation", -100, 100, 0)}
          {slider(lightness, "lightnessShift", -100, 100, 0)}
          <div className="studio-subheading">
            {t("studio.color.editor.range")}
          </div>
          {slider(hue, "hueRange", 5, 90, 25)}
          {slider(saturation, "chromaRange", 0, 100, 70)}
          {slider(lightness, "lightnessRange", 0, 100, 100)}
          {slider(t("studio.color.editor.smoothness"), "smoothness", 0, 100, 50)}
          <div className="studio-subheading">
            {t("studio.color.editor.uniformity")}
          </div>
          {slider(hue, "hueUniformity", 0, 100, 0)}
          {slider(saturation, "saturationUniformity", 0, 100, 0)}
          {slider(lightness, "lightnessUniformity", 0, 100, 0)}
        </>
      )}
    </ToolGroup>
  );
}
