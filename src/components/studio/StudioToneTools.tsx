import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import { useEditorStore } from "../../store/useEditorStore";
import {
  DEFRINGE_VERSION,
  INITIAL_LEVELS,
  type Adjustments,
  type Level,
  type Levels,
  type RegionCurve,
} from "../../utils/adjustments";
import AdjustmentControl from "./AdjustmentControl";
import { ToolGroup } from "./StudioDevelop";
import { histogramBins } from "./StudioHistogram";
import type { ToolParams } from "./StudioToolCopy";

interface ToolProps {
  values: Partial<Adjustments>;
  disabled: boolean;
  change(patch: Partial<Adjustments>): void;
  onInteraction(active: boolean): void;
  toggle?: { enabled: boolean; onToggle(): void };
  copy?: ToolParams;
}

const CHANNELS = [
  ["rgb", "RGB"],
  ["red", "R"],
  ["green", "G"],
  ["blue", "B"],
] as const;
type Channel = (typeof CHANNELS)[number][0];

/** Midtone slider position (−100…100) for a gamma and back: gamma 2^(m/50). */
export const midtoneOf = (gamma: number) =>
  Math.round(50 * Math.log2(Math.max(0.1, gamma)));
export const gammaOf = (midtone: number) =>
  Number((2 ** (midtone / 50)).toFixed(3));

/**
 * Input levels that clip `fraction` of the pixels at each end, from a
 * 256-bin histogram of the current rendering. The histogram already shows
 * the current input levels, so its positions are mapped back through them.
 */
export function autoLevels(
  bins: number[][],
  current: Level,
  fraction = 0.0005,
): Pick<Level, "inBlack" | "inWhite"> | null {
  const size = Math.min(...bins.map((b) => b.length));
  if (!Number.isFinite(size) || size < 2) return null;
  const totals = bins.map((b) => b.reduce((sum, v) => sum + v, 0));
  if (totals.some((t) => t <= 0)) return null;
  const edge = (fromTop: boolean) => {
    // The channel that reaches furthest out decides (no channel clips more
    // than `fraction`).
    let best = fromTop ? 0 : size - 1;
    bins.forEach((b, c) => {
      let acc = 0;
      for (let k = 0; k < size; k++) {
        const i = fromTop ? size - 1 - k : k;
        acc += b[i];
        if (acc > totals[c] * fraction) {
          best = fromTop ? Math.max(best, i) : Math.min(best, i);
          break;
        }
      }
    });
    return (best / (size - 1)) * 255;
  };
  const back = (shown: number) => {
    const out =
      (shown - current.outBlack) /
      Math.max(1, current.outWhite - current.outBlack);
    return (
      current.inBlack +
      Math.min(1, Math.max(0, out)) * (current.inWhite - current.inBlack)
    );
  };
  const inBlack = Math.round(back(edge(false)));
  const inWhite = Math.round(back(edge(true)));
  if (inWhite - inBlack < 16) return null;
  return { inBlack, inWhite };
}

/**
 * Capture One's Levels: input black and white point, midtone (gamma) and
 * output range for the RGB composite and each channel, applied to display
 * values before the curve. Auto sets the input points from the histogram.
 */
export function StudioLevels({
  values,
  disabled,
  change,
  onInteraction,
  toggle,
  copy,
}: ToolProps) {
  const { t } = useTranslation();
  const [channel, setChannel] = useState<Channel>("rgb");
  const levels: Levels = { ...INITIAL_LEVELS, ...values.levels };
  const current: Level = { ...INITIAL_LEVELS[channel], ...levels[channel] };
  const set = (patch: Partial<Level>) =>
    change({ levels: { ...levels, [channel]: { ...current, ...patch } } });
  const histogram = useEditorStore((s) => s.histogram);
  const auto = () => {
    if (!histogram) return;
    const bins =
      channel === "rgb"
        ? (["red", "green", "blue"] as const).map((c) =>
            histogramBins(histogram[c]),
          )
        : [histogramBins(histogram[channel])];
    const next = autoLevels(bins, current);
    if (next) set(next);
  };
  return (
    <ToolGroup
      title="Levels"
      label={t("studio.tone.levels.title")}
      toggle={toggle}
      copy={copy}
      open={false}
      actions={
        <button
          type="button"
          className="studio-tool-auto"
          aria-label={t("studio.tone.levels.autoAria")}
          disabled={disabled || !histogram}
          title={t("studio.tone.levels.autoTitle")}
          onClick={(e) => {
            e.preventDefault();
            auto();
          }}
        >
          <Sparkles size={12} />
          {t("studio.tone.levels.auto")}
        </button>
      }
      onReset={() => change({ levels: structuredClone(INITIAL_LEVELS) })}
    >
      <div className="studio-segments" aria-label={t("studio.tone.levels.channelAria")}>
        {CHANNELS.map(([key, label]) => (
          <button
            key={key}
            type="button"
            aria-pressed={channel === key}
            onClick={() => setChannel(key)}
          >
            {label}
          </button>
        ))}
      </div>
      <AdjustmentControl
        label={t("studio.tone.levels.shadow")}
        value={current.inBlack}
        min={0}
        max={254}
        disabled={disabled}
        onInteraction={onInteraction}
        onChange={(inBlack) =>
          set({ inBlack, inWhite: Math.max(current.inWhite, inBlack + 1) })
        }
      />
      <AdjustmentControl
        label={t("studio.tone.levels.midtone")}
        value={midtoneOf(current.gamma)}
        disabled={disabled}
        onInteraction={onInteraction}
        onChange={(m) => set({ gamma: gammaOf(m) })}
      />
      <AdjustmentControl
        label={t("studio.tone.levels.highlight")}
        value={current.inWhite}
        min={1}
        max={255}
        defaultValue={255}
        disabled={disabled}
        onInteraction={onInteraction}
        onChange={(inWhite) =>
          set({ inWhite, inBlack: Math.min(current.inBlack, inWhite - 1) })
        }
      />
      <div className="studio-subheading">{t("studio.tone.levels.output")}</div>
      <AdjustmentControl
        label={t("studio.tone.levels.black")}
        value={current.outBlack}
        min={0}
        max={255}
        disabled={disabled}
        onInteraction={onInteraction}
        onChange={(outBlack) => set({ outBlack })}
      />
      <AdjustmentControl
        label={t("studio.tone.levels.white")}
        value={current.outWhite}
        min={0}
        max={255}
        defaultValue={255}
        disabled={disabled}
        onInteraction={onInteraction}
        onChange={(outWhite) => set({ outWhite })}
      />
    </ToolGroup>
  );
}

/**
 * Lightroom's Defringe (Capture One's Purple Fringing): removes purple and
 * green colour fringes along high-contrast edges; the hue ranges choose
 * which colours count as fringe.
 */
export function StudioDefringe({
  values,
  disabled,
  change,
  onInteraction,
  copy,
}: ToolProps) {
  const { t } = useTranslation();
  const v = (key: string, fallback: number) =>
    (values[key] as number | undefined) ?? fallback;
  const slider = (
    label: string,
    key: string,
    fallback: number,
    min: number,
    max: number,
  ) => (
    <AdjustmentControl
      label={label}
      value={v(key, fallback)}
      min={min}
      max={max}
      defaultValue={fallback}
      disabled={disabled}
      onInteraction={onInteraction}
      onChange={(next) => change({ [key]: next })}
    />
  );
  return (
    <ToolGroup
      title="Defringe"
      label={t("studio.tone.defringe.title")}
      copy={copy}
      open={false}
      onReset={() =>
        change({
          defringePurpleAmount: 0,
          defringePurpleHueLo: 30,
          defringePurpleHueHi: 70,
          defringeGreenAmount: 0,
          defringeGreenHueLo: 40,
          defringeGreenHueHi: 60,
          // The fringe detector goes back to a new edit's.
          defringeVersion: DEFRINGE_VERSION,
        })
      }
    >
      {slider(t("studio.tone.defringe.purple"), "defringePurpleAmount", 0, 0, 20)}
      {slider(t("studio.tone.defringe.purpleFrom"), "defringePurpleHueLo", 30, 0, 100)}
      {slider(t("studio.tone.defringe.purpleTo"), "defringePurpleHueHi", 70, 0, 100)}
      {slider(t("studio.tone.defringe.green"), "defringeGreenAmount", 0, 0, 20)}
      {slider(t("studio.tone.defringe.greenFrom"), "defringeGreenHueLo", 40, 0, 100)}
      {slider(t("studio.tone.defringe.greenTo"), "defringeGreenHueHi", 60, 0, 100)}
    </ToolGroup>
  );
}

/**
 * Lightroom's region curve (parametric tone curve): Highlights, Lights,
 * Darks and Shadows, with the three split points between the regions;
 * applied before the point curve, with Camera Raw's measured response.
 */
export function RegionCurveControls({
  value,
  disabled,
  onChange,
  onInteraction,
  targeted,
}: {
  /** The Targeted Adjustment toggle for the regions. */
  targeted?: React.ReactNode;
  value: RegionCurve;
  disabled: boolean;
  onChange(value: RegionCurve): void;
  onInteraction(active: boolean): void;
}) {
  const { t } = useTranslation();
  const slider = (
    key: keyof RegionCurve,
    min = -100,
    max = 100,
    fallback = 0,
  ) => (
    <AdjustmentControl
      key={key}
      label={t(`studio.tone.regionCurve.labels.${key}`)}
      ariaLabel={t(`studio.tone.regionCurve.aria.${key}`)}
      value={value[key]}
      min={min}
      max={max}
      defaultValue={fallback}
      disabled={disabled}
      onInteraction={onInteraction}
      onChange={(next) => onChange({ ...value, [key]: next })}
    />
  );
  return (
    <>
      <div className="studio-subheading">
        {t("studio.tone.regionCurve.region")}
        {targeted}
      </div>
      {slider("highlights")}
      {slider("lights")}
      {slider("darks")}
      {slider("shadows")}
      <div className="studio-subheading">{t("studio.tone.regionCurve.splits")}</div>
      {slider("shadowSplit", 5, 45, 25)}
      {slider("midtoneSplit", 25, 75, 50)}
      {slider("highlightSplit", 55, 95, 75)}

    </>
  );
}
