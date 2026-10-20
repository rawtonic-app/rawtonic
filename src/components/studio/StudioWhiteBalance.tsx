import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState, type ReactNode } from "react";
import type { WhiteBalanceMode } from "../../utils/adjustments";
import { KELVIN_SCALE, WHITE_BALANCE_PRESETS } from "../../utils/whiteBalance";
import { useTranslation } from "react-i18next";
import { Invokes } from "../ui/AppProperties";
import AdjustmentControl from "./AdjustmentControl";

export interface WhiteBalanceValues {
  whiteBalance?: WhiteBalanceMode;
  wbTemperature?: number;
  wbTint?: number;
}

interface Balance {
  temperature: number;
  tint: number;
}

/**
 * Absolute white balance of a RAW, as in Lightroom: As Shot, the light
 * presets or a custom Kelvin/tint, computed in camera space from the
 * camera's own calibration (camera_profile.rs). Files without one (JPEGs,
 * some 4-colour sensors) get `fallback`, the relative sliders.
 */
export default function StudioWhiteBalance({
  values,
  imagePath,
  disabled,
  onChange,
  onAuto,
  onInteraction,
  fallback,
}: {
  values: WhiteBalanceValues;
  imagePath: string | undefined;
  disabled: boolean;
  onChange(patch: WhiteBalanceValues): void;
  onAuto(): void;
  onInteraction?(active: boolean): void;
  fallback?: ReactNode;
}) {
  const { t } = useTranslation();
  const [asShot, setAsShot] = useState<Balance | null>(null);
  useEffect(() => {
    let current = true;
    setAsShot(null);
    if (!imagePath) return;
    invoke<Balance | null>(Invokes.WhiteBalanceInfo, { imagePath })
      .then((info) => current && setAsShot(info ?? null))
      .catch(() => current && setAsShot(null));
    return () => {
      current = false;
    };
  }, [imagePath]);
  if (!asShot) return <>{fallback}</>;

  const mode = values.whiteBalance ?? "asShot";
  const shown: Balance =
    (mode === "custom" || mode === "auto") && values.wbTemperature !== undefined
      ? { temperature: values.wbTemperature, tint: values.wbTint ?? 0 }
      : mode in WHITE_BALANCE_PRESETS
        ? WHITE_BALANCE_PRESETS[mode as keyof typeof WHITE_BALANCE_PRESETS]
        : asShot;
  const custom = (balance: Balance) =>
    onChange({
      whiteBalance: "custom",
      wbTemperature: balance.temperature,
      wbTint: balance.tint,
    });

  return (
    <>
      <label className="studio-inline">
        <span>{t("studio.whiteBalance.short")}</span>
        <select
          aria-label={t("studio.whiteBalance.aria.mode")}
          value={mode}
          disabled={disabled}
          onChange={(e) => {
            const next = e.target.value as WhiteBalanceMode;
            if (next === "auto") {
              // Lightroom's Auto: the balance found for this photo, kept.
              onAuto();
              return;
            }
            onChange(
              next === "asShot"
                ? {
                    whiteBalance: undefined,
                    wbTemperature: undefined,
                    wbTint: undefined,
                  }
                : next === "custom"
                  ? { whiteBalance: "custom" }
                  : {
                      whiteBalance: next,
                      wbTemperature: undefined,
                      wbTint: undefined,
                    },
            );
          }}
        >
          <option value="asShot">{t("studio.whiteBalance.modes.asShot")}</option>
          <option value="auto">{t("studio.whiteBalance.modes.auto")}</option>
          {(Object.keys(WHITE_BALANCE_PRESETS) as (keyof typeof WHITE_BALANCE_PRESETS)[]).map((key) => (
            <option key={key} value={key}>
              {t(`studio.whiteBalance.modes.${key}`)}
            </option>
          ))}
          {mode === "custom" && <option value="custom">{t("studio.whiteBalance.modes.custom")}</option>}
        </select>
      </label>
      <AdjustmentControl
        label={t("studio.whiteBalance.temp")}
        ariaLabel={t("studio.whiteBalance.aria.temperature")}
        unit="K"
        value={shown.temperature}
        min={2000}
        max={50000}
        step={50}
        defaultValue={asShot.temperature}
        scale={KELVIN_SCALE}
        track="temperature"
        disabled={disabled}
        onChange={(temperature) => custom({ temperature, tint: shown.tint })}
        onInteraction={onInteraction}
      />
      <AdjustmentControl
        label={t("studio.whiteBalance.tint")}
        ariaLabel={t("studio.whiteBalance.aria.tint")}
        value={shown.tint}
        min={-150}
        max={150}
        step={1}
        defaultValue={asShot.tint}
        track="tint"
        disabled={disabled}
        onChange={(tint) => custom({ temperature: shown.temperature, tint })}
        onInteraction={onInteraction}
      />
    </>
  );
}
