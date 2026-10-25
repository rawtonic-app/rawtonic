import { useRef, useState } from "react";
import { Pipette, Wand2 } from "lucide-react";
import { toast } from "react-toastify";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import type { NegativeKind, NegativeSettings } from "../../utils/adjustments";
import { autoNegative, negativeAnalysisGeneration } from "../../utils/negative";
import AdjustmentControl from "./AdjustmentControl";

const CHANNELS = ["red", "green", "blue"] as const;

/** The film base as a swatch colour: its linear RGB scaled to its largest
 * channel, sRGB-encoded. */
export function baseSwatch([r, g, b]: [number, number, number]) {
  const top = Math.max(r, g, b, 1e-6);
  const encode = (v: number) => {
    const c = Math.max(0, Math.min(1, v / top));
    const s = c <= 0.0031308 ? 12.92 * c : 1.055 * Math.pow(c, 1 / 2.4) - 0.055;
    return Math.round(255 * s);
  };
  return `rgb(${encode(r)}, ${encode(g)}, ${encode(b)})`;
}

/**
 * The Negative tool: turns a scanned film negative into a positive at the
 * start of the pipeline (engine `negative.rs`), so white balance, tone and
 * curves work on the positive. Turning it on finds the film base on the
 * frame's border and the frame's highlight; the picker takes the base from
 * the unexposed film instead.
 */
export default function StudioNegative({
  disabled,
  onInteraction,
}: {
  disabled: boolean;
  onInteraction(active: boolean): void;
}) {
  const { t } = useTranslation();
  const negative = useEditorStore((s) => s.adjustments.negative ?? null);
  const path = useEditorStore((s) => s.selectedImage?.path);
  const picking = useEditorStore((s) => s.filmBasePicking);
  const setEditor = useEditorStore((s) => s.setEditor);
  const { setAdjustments } = useEditorActions();
  const [analysing, setAnalysing] = useState(false);

  const update = (patch: Partial<NegativeSettings>) =>
    setAdjustments((prev) =>
      prev.negative ? { ...prev, negative: { ...prev.negative, ...patch } } : prev,
    );

  // Only the latest analysis lands, and only its film base and highlight:
  // controls moved meanwhile keep their values, a reset tool stays reset.
  const autoRequest = useRef(0);
  const runAuto = async (type?: NegativeKind) => {
    if (!path) return;
    const request = ++autoRequest.current;
    const generation = negativeAnalysisGeneration();
    const started = useEditorStore.getState().adjustments;
    const hadTool = !!started.negative;
    setAnalysing(true);
    try {
      const next = await autoNegative(path, useEditorStore.getState().adjustments, type);
      if (!next || request !== autoRequest.current || generation !== negativeAnalysisGeneration()) return;
      setAdjustments((prev) => {
        if (prev.negative) {
          return { ...prev, negative: { ...prev.negative, base: next.base, highlight: next.highlight } };
        }
        // A first conversion lands only on the recipe it was asked for: a
        // reset or any other change meanwhile keeps the recipe as it is.
        return hadTool || prev !== started ? prev : { ...prev, negative: next };
      });
    } catch (error) {
      toast.error(t("studio.develop.negative.analyzeFailed", { error: String(error) }));
    } finally {
      if (request === autoRequest.current) setAnalysing(false);
    }
  };

  if (!negative) {
    return (
      <div className="studio-inline">
        <span>{t("studio.develop.negative.off")}</span>
        <button disabled={disabled || analysing || !path} onClick={() => void runAuto("color")}>
          {t("studio.develop.negative.convertColor")}
        </button>
        <button disabled={disabled || analysing || !path} onClick={() => void runAuto("bw")}>
          {t("studio.develop.negative.convertBw")}
        </button>
      </div>
    );
  }

  const balanceLabel = {
    red: t("studio.develop.negative.balanceRed"),
    green: t("studio.develop.negative.balanceGreen"),
    blue: t("studio.develop.negative.balanceBlue"),
  };
  return (
    <>
      <label className="studio-inline">
        <span>{t("studio.develop.negative.type")}</span>
        <select
          aria-label={t("studio.develop.negative.type")}
          value={negative.type}
          disabled={disabled}
          onChange={(e) => update({ type: e.target.value as NegativeKind })}
        >
          <option value="color">{t("studio.develop.negative.color")}</option>
          <option value="bw">{t("studio.develop.negative.bw")}</option>
        </select>
      </label>
      <div className="studio-inline">
        <span>{t("studio.develop.negative.filmBase")}</span>
        <span
          className="studio-swatch"
          role="img"
          aria-label={t("studio.develop.negative.filmBase")}
          style={{
            background: baseSwatch(negative.base),
            display: "inline-block",
            width: 18,
            height: 14,
            borderRadius: 2,
          }}
        />
        <button
          aria-label={t("studio.develop.negative.pickBase")}
          title={t("studio.develop.negative.pickBase")}
          aria-pressed={picking}
          disabled={disabled}
          onClick={() =>
            setEditor({
              filmBasePicking: !picking,
              isWbPickerActive: false,
              colorPickerTarget: null,
            })
          }
        >
          <Pipette size={15} />
        </button>
        <button
          aria-label={t("studio.develop.negative.auto")}
          title={t("studio.develop.negative.autoHint")}
          disabled={disabled || analysing}
          onClick={() => void runAuto()}
        >
          <Wand2 size={15} />
        </button>
      </div>
      <AdjustmentControl
        label={t("studio.develop.negative.printExposure")}
        value={negative.exposure}
        min={-3}
        max={3}
        step={0.01}
        defaultValue={0}
        disabled={disabled}
        onChange={(exposure) => update({ exposure })}
        onInteraction={onInteraction}
      />
      <AdjustmentControl
        label={t("studio.develop.negative.printGrade")}
        value={negative.grade}
        min={-100}
        max={100}
        step={1}
        defaultValue={0}
        disabled={disabled}
        onChange={(grade) => update({ grade })}
        onInteraction={onInteraction}
      />
      <div className="studio-subheading">{t("studio.develop.negative.densityBalance")}</div>
      {CHANNELS.map((channel, index) => (
        <AdjustmentControl
          key={channel}
          label={balanceLabel[channel]}
          value={negative.balance[index]}
          min={-100}
          max={100}
          step={1}
          defaultValue={0}
          disabled={disabled}
          onChange={(value) =>
            setAdjustments((prev) => {
              if (!prev.negative) return prev;
              const balance = [...prev.negative.balance] as [number, number, number];
              balance[index] = value;
              return { ...prev, negative: { ...prev.negative, balance } };
            })
          }
          onInteraction={onInteraction}
        />
      ))}
    </>
  );
}
