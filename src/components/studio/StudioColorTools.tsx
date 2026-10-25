import { Sparkles } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "react-toastify";
import { useEditorStore } from "../../store/useEditorStore";
import { Invokes } from "../ui/AppProperties";
import {
  GRAY_MIXER_CHANNELS,
  INITIAL_ADJUSTMENTS,
  INITIAL_GRAY_MIXER,
  type Adjustments,
  type GrayMixer,
  type HueSatLum,
} from "../../utils/adjustments";
import AdjustmentControl from "./AdjustmentControl";
import StudioColorWheel from "./StudioColorWheel";
import StudioLuminanceArc, {
  THREE_WAY_ASPECT,
  THREE_WAY_DISC,
} from "./StudioLuminanceArc";
import { ToolGroup } from "./StudioDevelop";
import type { ToolParams } from "./StudioToolCopy";
import { invokeEditStep } from "../../utils/editGate";

type Values = Pick<Adjustments, "colorGrading" | "sectionVisibility"> &
  Partial<Pick<Adjustments, "blackAndWhite" | "grayMixer">>;

interface ToolProps {
  values: Values;
  disabled: boolean;
  change(patch: Partial<Adjustments>): void;
  onInteraction(active: boolean): void;
  toggle?: { enabled: boolean; onToggle(): void };
  copy?: ToolParams;
}

const REGIONS = [
  ["threeWay", "studio.color.grading.threeWay"],
  ["shadows", "adjustments.color.grading.shadows"],
  ["midtones", "adjustments.color.grading.midtones"],
  ["highlights", "adjustments.color.grading.highlights"],
  ["global", "adjustments.color.grading.global"],
] as const;
type View = (typeof REGIONS)[number][0];
type Region = Exclude<View, "threeWay">;
/** Capture One's 3-way layout: midtones on top, shadows and highlights below. */
const THREE_WAY: Region[] = ["midtones", "shadows", "highlights"];

export const tintColor = ({ hue, saturation }: HueSatLum) =>
  saturation > 0
    ? `hsl(${hue} ${Math.round(35 + saturation * 0.65)}% 55%)`
    : undefined;

/**
 * The range switcher's icons (16 px, stroke 1.5, like the app's lucide
 * icons): the 3-way view as three circles laid out like the view, each
 * filled with its range's tint; Shadows dark, Midtones half, Highlights
 * light, Global a ring around a dot. A range with saturation shows its tint
 * as a dot at the icon's corner.
 */
function GradingRangeIcon({
  view,
  tints,
}: {
  view: View;
  tints: (string | undefined)[];
}) {
  const outline = {
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.5,
  } as const;
  if (view === "threeWay")
    return (
      <svg
        className="studio-grading-icon"
        width="16"
        height="16"
        viewBox="0 0 16 16"
        aria-hidden
      >
        {THREE_WAY_ICON.map(([cx, cy], i) => (
          <circle
            key={i}
            cx={cx}
            cy={cy}
            r={2.9}
            {...outline}
            fill={tints[i] ?? "none"}
          />
        ))}
      </svg>
    );
  const tint = tints[0];
  return (
    <span className="studio-grading-icon-wrap" aria-hidden>
      <svg
        className="studio-grading-icon"
        width="16"
        height="16"
        viewBox="0 0 16 16"
      >
        {view === "midtones" && (
          <path d="M8 2.25a5.75 5.75 0 0 0 0 11.5z" fill="currentColor" />
        )}
        {view === "global" && (
          <circle cx={8} cy={8} r={2.25} fill="currentColor" />
        )}
        <circle
          cx={8}
          cy={8}
          r={5.75}
          {...outline}
          fill={
            view === "highlights"
              ? "currentColor"
              : view === "shadows"
                ? "#141516"
                : "none"
          }
        />
      </svg>
      {tint && (
        <span className="studio-grading-tint" style={{ background: tint }} />
      )}
    </span>
  );
}
/** The 3-way icon's circles, laid out like the view (midtones on top). */
const THREE_WAY_ICON = [
  [8, 4.25],
  [4.25, 11.5],
  [11.75, 11.5],
] as const;

/**
 * Lightroom's Color Grading / Capture One's Color Balance: one wheel per
 * tonal range (hue and saturation) with its luminance, plus Blending (how
 * far the ranges overlap) and Balance (where shadows end and highlights
 * begin). The renderer follows Camera Raw's response
 * (notes: reference/color-grading.md).
 *
 * The 3-way view (default, as in both editors) shows the three range wheels
 * at once, each with its luminance as an arc beside it (Capture One's
 * compact form); the range tabs show one large wheel with numeric sliders.
 * Both are views of the same recipe values and history entries.
 */
export function StudioColorGrading({
  values,
  disabled,
  change,
  onInteraction,
  toggle,
  copy,
}: ToolProps) {
  const { t } = useTranslation();
  const [view, setView] = useState<View>("threeWay");
  const region: Region = view === "threeWay" ? "midtones" : view;
  const grading = {
    ...INITIAL_ADJUSTMENTS.colorGrading,
    ...values.colorGrading,
  };
  const wheelOf = (key: Region): HueSatLum => ({
    ...(INITIAL_ADJUSTMENTS.colorGrading[key] as HueSatLum),
    ...(grading[key] as HueSatLum),
  });
  const setWheelOf = (key: Region, next: HueSatLum) =>
    change({ colorGrading: { ...grading, [key]: next } });
  const nameOf = (key: View) => t(REGIONS.find(([k]) => k === key)![1]);
  const wheel = wheelOf(region);
  const setWheel = (next: HueSatLum) => setWheelOf(region, next);
  const label = nameOf(region);
  return (
    <ToolGroup
      title="Color grading"
      label={t("studio.color.grading.title")}
      toggle={toggle}
      copy={copy}
      open={false}
      onReset={() =>
        change({
          colorGrading: structuredClone(INITIAL_ADJUSTMENTS.colorGrading),
        })
      }
    >
      <div
        className="studio-segments studio-grading-regions"
        role="group"
        aria-label={t("studio.color.grading.tonalRange")}
      >
        {REGIONS.map(([key, nameKey]) => {
          const name = t(nameKey);
          return (
            // Icon only (Lightroom's view switcher): five equal segments fit
            // the narrowest panel in every language; the name is the
            // tooltip, the accessible name and the caption under the wheel.
            <button
              key={key}
              type="button"
              aria-pressed={view === key}
              aria-label={name}
              title={
                key === "threeWay"
                  ? t("studio.color.grading.threeWayTitle")
                  : name
              }
              onClick={() => setView(key)}
            >
              <GradingRangeIcon
                view={key}
                tints={
                  key === "threeWay"
                    ? THREE_WAY.map((r) => tintColor(wheelOf(r)))
                    : [tintColor(wheelOf(key))]
                }
              />
            </button>
          );
        })}
      </div>
      {view === "threeWay" ? (
        <div
          className="studio-grading-3way"
          role="group"
          aria-label={t("studio.color.grading.threeWayTitle")}
        >
          {THREE_WAY.map((key) => {
            const value = wheelOf(key);
            const name = nameOf(key);
            return (
              <div key={key} className={`studio-grading-3way-cell is-${key}`}>
                <div
                  className="studio-grading-3way-wheel"
                  style={{ aspectRatio: THREE_WAY_ASPECT }}
                >
                  <StudioColorWheel
                    label={name}
                    value={value}
                    size="fill"
                    className="is-fill"
                    style={THREE_WAY_DISC}
                    disabled={disabled}
                    onChange={(next) => setWheelOf(key, next)}
                    onInteraction={onInteraction}
                  />
                  <StudioLuminanceArc
                    label={name}
                    value={value.luminance}
                    tint={tintColor(value)}
                    disabled={disabled}
                    onChange={(luminance) =>
                      setWheelOf(key, { ...wheelOf(key), luminance })
                    }
                    onInteraction={onInteraction}
                  />
                </div>
                <span className="studio-grading-3way-name">{name}</span>
              </div>
            );
          })}
        </div>
      ) : (
        <>
          <div className="studio-wheel-row is-named">
            <StudioColorWheel
              label={label}
              value={wheel}
              disabled={disabled}
              onChange={setWheel}
              onInteraction={onInteraction}
            />
            <span className="studio-grading-3way-name">{label}</span>
          </div>
          <AdjustmentControl
            label={t("adjustments.color.hue")}
            ariaLabel={t("studio.color.grading.hueAria", { range: label })}
            value={wheel.hue}
            min={0}
            max={359}
            track="hue"
            disabled={disabled}
            onInteraction={onInteraction}
            onChange={(hue) => setWheel({ ...wheel, hue })}
          />
          <AdjustmentControl
            label={t("adjustments.color.saturation")}
            ariaLabel={t("studio.color.grading.saturationAria", {
              range: label,
            })}
            value={wheel.saturation}
            min={0}
            max={100}
            disabled={disabled}
            onInteraction={onInteraction}
            onChange={(saturation) => setWheel({ ...wheel, saturation })}
          />
          <AdjustmentControl
            label={t("adjustments.color.luminance")}
            ariaLabel={t("studio.color.grading.luminanceAria", {
              range: label,
            })}
            value={wheel.luminance}
            disabled={disabled}
            onInteraction={onInteraction}
            onChange={(luminance) => setWheel({ ...wheel, luminance })}
          />
        </>
      )}
      <div className="studio-subheading">
        {t("studio.color.grading.ranges")}
      </div>
      <AdjustmentControl
        label={t("adjustments.color.grading.blending")}
        value={grading.blending}
        min={0}
        max={100}
        defaultValue={50}
        disabled={disabled}
        onInteraction={onInteraction}
        onChange={(blending) =>
          change({ colorGrading: { ...grading, blending } })
        }
      />
      <AdjustmentControl
        label={t("adjustments.color.grading.balance")}
        value={grading.balance}
        disabled={disabled}
        onInteraction={onInteraction}
        onChange={(balance) =>
          change({ colorGrading: { ...grading, balance } })
        }
      />
    </ToolGroup>
  );
}

const MIXER_SWATCH: Record<(typeof GRAY_MIXER_CHANNELS)[number], string> = {
  red: "#dd6767",
  orange: "#e6a461",
  yellow: "#dac878",
  green: "#86bb77",
  aqua: "#72b9b9",
  blue: "#6d9cc9",
  purple: "#a18ac5",
  magenta: "#ca83b0",
};

/**
 * Black & white conversion with Lightroom's eight-channel B&W mix: each
 * slider lightens or darkens the grey of one colour range. The tool's eye
 * switches the conversion; Color grading tones the result.
 */
export function StudioBlackAndWhite({
  values,
  disabled,
  change,
  onInteraction,
  copy,
  targeted,
}: ToolProps & { targeted?: React.ReactNode }) {
  const { t } = useTranslation();
  const enabled = !!values.blackAndWhite;
  const mixer: GrayMixer = { ...INITIAL_GRAY_MIXER, ...values.grayMixer };
  const [busy, setBusy] = useState(false);
  // Lightroom's Auto: the mix that widens the tonal range of this photo as
  // it is developed (bw_auto.rs).
  const auto = async () => {
    const { selectedImage, adjustments } = useEditorStore.getState();
    if (!selectedImage?.isReady) return;
    setBusy(true);
    try {
      const mix = await invokeEditStep<number[] | null>(Invokes.AutoGrayMix, {
        path: selectedImage.path,
        jsAdjustments: adjustments,
      });
      if (
        !Array.isArray(mix) ||
        useEditorStore.getState().selectedImage?.path !== selectedImage.path
      )
        return;
      change({
        blackAndWhite: true,
        grayMixer: Object.fromEntries(
          GRAY_MIXER_CHANNELS.map((channel, i) => [channel, mix[i] ?? 0]),
        ) as unknown as GrayMixer,
      });
    } catch (error) {
      toast.error(t("studio.color.bw.autoFailed", { error: String(error) }));
    } finally {
      setBusy(false);
    }
  };
  return (
    <ToolGroup
      title="Black & white"
      label={t("studio.color.bw.title")}
      open={enabled}
      copy={copy}
      toggle={{
        enabled,
        onToggle: () => change({ blackAndWhite: !enabled }),
      }}
      actions={
        <>
          {targeted}
          <button
            type="button"
            className="studio-tool-auto"
            disabled={disabled || busy}
            aria-label={t("studio.color.bw.autoAria")}
            title={t("studio.color.bw.autoTitle")}
            onClick={(e) => {
              e.preventDefault();
              void auto();
            }}
          >
            <Sparkles size={12} />
            {t("studio.color.bw.auto")}
          </button>
        </>
      }
      onReset={() => change({ grayMixer: { ...INITIAL_GRAY_MIXER } })}
    >
      {!enabled && (
        <button
          type="button"
          className="studio-text-button"
          disabled={disabled}
          onClick={() => change({ blackAndWhite: true })}
        >
          {t("studio.color.bw.convert")}
        </button>
      )}
      {GRAY_MIXER_CHANNELS.map((channel) => (
        <div
          key={channel}
          className="studio-mixer-row"
          style={{ "--swatch": MIXER_SWATCH[channel] } as React.CSSProperties}
        >
          <AdjustmentControl
            label={t(`studio.color.bw.channels.${channel}`)}
            value={mixer[channel]}
            disabled={disabled || !enabled}
            onInteraction={onInteraction}
            onChange={(v) => change({ grayMixer: { ...mixer, [channel]: v } })}
          />
        </div>
      ))}
    </ToolGroup>
  );
}

const PRIMARIES = [
  ["red", "#dd6767"],
  ["green", "#86bb77"],
  ["blue", "#6d9cc9"],
] as const;

/**
 * Lightroom's Calibration: hue and saturation of the red, green and blue
 * primaries (every colour that contains them shifts too) and the shadows
 * tint, rendered as Camera Raw does (notes: reference/calibration.md).
 */
export function StudioCalibration({
  values,
  disabled,
  change,
  onInteraction,
  toggle,
  copy,
}: ToolProps & { values: Pick<Adjustments, "colorCalibration"> }) {
  const { t } = useTranslation();
  const calibration = {
    ...INITIAL_ADJUSTMENTS.colorCalibration,
    ...values.colorCalibration,
  };
  const set = (key: string, value: number) =>
    change({ colorCalibration: { ...calibration, [key]: value } });
  const slider = (label: string, key: string, ariaLabel: string) => (
    <AdjustmentControl
      key={key}
      label={label}
      ariaLabel={ariaLabel}
      value={(calibration as unknown as Record<string, number>)[key] ?? 0}
      disabled={disabled}
      onInteraction={onInteraction}
      onChange={(v) => set(key, v)}
    />
  );
  return (
    <ToolGroup
      title="Calibration"
      label={t("studio.color.calibration.title")}
      toggle={toggle}
      copy={copy}
      open={false}
      onReset={() =>
        change({
          colorCalibration: structuredClone(
            INITIAL_ADJUSTMENTS.colorCalibration,
          ),
        })
      }
    >
      {slider(
        t("adjustments.color.calibration.shadows"),
        "shadowsTint",
        t("studio.color.calibration.shadowsTintAria"),
      )}
      {PRIMARIES.map(([key, swatch]) => (
        <div
          key={key}
          className="studio-mixer-row"
          style={{ "--swatch": swatch } as React.CSSProperties}
        >
          <div className="studio-subheading">
            {t(`studio.color.calibration.primaries.${key}`)}
          </div>
          {slider(
            t("adjustments.color.hue"),
            `${key}Hue`,
            t(`studio.color.calibration.hueAria.${key}`),
          )}
          {slider(
            t("adjustments.color.saturation"),
            `${key}Saturation`,
            t(`studio.color.calibration.saturationAria.${key}`),
          )}
        </div>
      ))}
    </ToolGroup>
  );
}
