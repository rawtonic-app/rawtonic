import { useTranslation } from "react-i18next";
import {
  DEFRINGE_VERSION,
  type Adjustments,
  type RegionCurve,
} from "../../utils/adjustments";
import AdjustmentControl from "./AdjustmentControl";
import { ToolGroup } from "./StudioDevelop";
import type { ToolParams } from "./StudioToolCopy";

interface ToolProps {
  values: Partial<Adjustments>;
  disabled: boolean;
  change(patch: Partial<Adjustments>): void;
  onInteraction(active: boolean): void;
  toggle?: { enabled: boolean; onToggle(): void };
  copy?: ToolParams;
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
      {/* The split points are handles under the curve (PointCurve). */}

    </>
  );
}
