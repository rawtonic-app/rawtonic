import { useTranslation } from "react-i18next";
import AdjustmentControl from "./AdjustmentControl";
import { useEditorStore } from "../../store/useEditorStore";
import { updateAdjustments } from "../../hooks/useEditorActions";
import type { RedEye } from "../../utils/adjustments";

type Settings = ReturnType<typeof useEditorStore.getState>["redEyeSettings"];

/** Red eye settings of the spot tool (Lightroom's Red Eye with Pet Eye,
 * Capture One's Remove Red Eye): the kind, Pupil Size, Darken and, for pet
 * eyes, a catchlight. They apply to the selected correction and are what
 * new corrections get. */
export default function StudioRedEyeControls() {
  const { t } = useTranslation();
  const settings = useEditorStore((s) => s.redEyeSettings);
  const active = useEditorStore((s) =>
    s.adjustments.redEyes?.find((eye) => eye.id === s.activeSpotId),
  );
  const shown: Settings = active
    ? {
        kind: active.kind,
        size: settings.size,
        pupilSize: active.pupilSize,
        darken: active.darken,
        catchlight: active.catchlight,
      }
    : settings;

  const set = (patch: Partial<Settings>) => {
    const { setEditor, redEyeSettings } = useEditorStore.getState();
    setEditor({ redEyeSettings: { ...redEyeSettings, ...patch } });
    if (!active) return;
    const eyePatch: Partial<RedEye> = {
      ...(patch.kind && { kind: patch.kind }),
      ...(patch.pupilSize !== undefined && { pupilSize: patch.pupilSize }),
      ...(patch.darken !== undefined && { darken: patch.darken }),
      ...(patch.catchlight !== undefined && { catchlight: patch.catchlight }),
    };
    updateAdjustments((prev) => ({
      ...prev,
      redEyes: (prev.redEyes ?? []).map((eye) =>
        eye.id === active.id ? { ...eye, ...eyePatch } : eye,
      ),
    }));
  };

  return (
    <>
      <div
        className="studio-segments"
        role="radiogroup"
        aria-label={t("spotRemoval.redEye.kind")}
      >
        {(["red", "pet"] as const).map((kind) => (
          <button
            key={kind}
            type="button"
            role="radio"
            aria-checked={shown.kind === kind}
            onClick={() => set({ kind })}
          >
            {t(`spotRemoval.redEye.kinds.${kind}`)}
          </button>
        ))}
      </div>
      <AdjustmentControl
        label={t("spotRemoval.redEye.pupilSize")}
        ariaLabel={t("spotRemoval.redEye.pupilSizeAria")}
        value={shown.pupilSize}
        min={0}
        max={100}
        defaultValue={50}
        onChange={(pupilSize) => set({ pupilSize })}
      />
      <AdjustmentControl
        label={t("spotRemoval.redEye.darken")}
        ariaLabel={t("spotRemoval.redEye.darkenAria")}
        value={shown.darken}
        min={0}
        max={100}
        defaultValue={50}
        onChange={(darken) => set({ darken })}
      />
      {shown.kind === "pet" && (
        <label className="studio-inline studio-check">
          <input
            type="checkbox"
            checked={shown.catchlight}
            onChange={(e) => set({ catchlight: e.target.checked })}
          />
          {t("spotRemoval.redEye.catchlight")}
        </label>
      )}
      <p className="studio-hint">{t("spotRemoval.redEye.hint")}</p>
    </>
  );
}
