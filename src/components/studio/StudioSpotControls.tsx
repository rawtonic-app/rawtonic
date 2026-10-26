import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "react-toastify";
import { hiddenByFrame, regenerateHiddenPatches } from "../../utils/regeneratePatches";
import AdjustmentControl from "./AdjustmentControl";
import { useEditorStore } from "../../store/useEditorStore";
import { updateAdjustments } from "../../hooks/useEditorActions";
import { useSettingsStore } from "../../store/useSettingsStore";
import { shortcutHint } from "../../utils/keyboardUtils";
import type { Spot } from "../../utils/adjustments";
import StudioRedEyeControls from "./StudioRedEyeControls";

type Settings = ReturnType<typeof useEditorStore.getState>["spotSettings"];

/** Spot removal settings (Lightroom's Remove tool): Remove (the area filled
 * by the local inpainting model, an AI patch), Heal or Clone, size, feather
 * and opacity. They apply to the selected spot and are what new spots get.
 * Remove's Detect objects (Lightroom's object-aware Remove) selects an
 * object by a click, a loose brush stroke over it or a box around it;
 * Shift adds to the selected removal and Alt subtracts from it. Red eye
 * places red eye and pet eye corrections (`StudioRedEyeControls`). */
/** AI patches a frame update hid (they do not fit the new frame): a note
 * and their regeneration on the new frame. */
function HiddenPatchesNote() {
  const { t } = useTranslation();
  const count = useEditorStore((s) => hiddenByFrame(s.adjustments.aiPatches).length);
  const [busy, setBusy] = useState(false);
  if (count === 0) return null;
  return (
    <div className="studio-metadata-note studio-frame-note" role="status">
      <span>{t("studio.sourceFrame.hiddenNote", { count })}</span>
      <button
        className="studio-text-button"
        disabled={busy}
        onClick={() => {
          setBusy(true);
          regenerateHiddenPatches()
            .then((failed) => {
              if (failed > 0) toast.error(t("studio.sourceFrame.regenerateFailed", { count: failed }));
            })
            .finally(() => setBusy(false));
        }}
      >
        {t("studio.sourceFrame.regenerate")}
      </button>
    </div>
  );
}

export default function StudioSpotControls() {
  const { t } = useTranslation();
  const settings = useEditorStore((s) => s.spotSettings);
  const visualize = useEditorStore((s) => s.spotVisualize);
  const keybinds = useSettingsStore((s) => s.appSettings?.keybinds);
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const setVisualize = (patch: Partial<typeof visualize>) =>
    useEditorStore.getState().setEditor({ spotVisualize: { ...visualize, ...patch } });
  const activeSpot = useEditorStore((s) =>
    s.adjustments.spots?.find((spot) => spot.id === s.activeSpotId),
  );
  const redEye = settings.mode === "redEye";
  const shown: Settings = activeSpot && !redEye
    ? {
        mode: activeSpot.mode,
        size: Math.round(activeSpot.radius),
        feather: activeSpot.feather,
        opacity: activeSpot.opacity,
      }
    : settings;
  const detectObjects = shown.mode === "remove" && Boolean(settings.detectObjects);
  const objectSelect = settings.objectSelect ?? "brush";
  const objectsKey = shortcutHint("remove_objects", keybinds, osPlatform);

  const set = (patch: Partial<Settings>) => {
    const { setEditor, spotSettings } = useEditorStore.getState();
    // Into or out of Red eye: the selection belongs to the other mode.
    const switching =
      patch.mode !== undefined && (patch.mode === "redEye") !== (spotSettings.mode === "redEye");
    setEditor({
      spotSettings: { ...spotSettings, ...patch },
      ...(switching && { activeSpotId: null }),
    });
    if (!activeSpot || switching) return;
    // A spot stays heal or clone; Remove makes new removals only.
    const spotPatch: Partial<Spot> = {
      ...(patch.mode &&
        patch.mode !== "remove" &&
        patch.mode !== "redEye" && { mode: patch.mode }),
      ...(patch.size !== undefined && { radius: patch.size }),
      ...(patch.feather !== undefined && { feather: patch.feather }),
      ...(patch.opacity !== undefined && { opacity: patch.opacity }),
    };
    updateAdjustments((prev) => ({
      ...prev,
      spots: (prev.spots ?? []).map((s) =>
        s.id === activeSpot.id ? { ...s, ...spotPatch } : s,
      ),
    }));
  };

  return (
    <div className="studio-spot-controls">
      <HiddenPatchesNote />
      <div className="studio-segments" role="radiogroup" aria-label={t("spotRemoval.mode")}>
        {(["remove", "heal", "clone", "redEye"] as const).map((mode) => (
          <button
            key={mode}
            type="button"
            role="radio"
            aria-checked={shown.mode === mode}
            onClick={() => set({ mode })}
          >
            {t(`spotRemoval.modes.${mode}`)}
          </button>
        ))}
      </div>
      {redEye && <StudioRedEyeControls />}
      {shown.mode === "remove" && (
        <label
          className="studio-inline studio-check"
          title={
            objectsKey
              ? t("studio.spot.withShortcut", {
                  text: t("spotRemoval.detectObjectsHint"),
                  key: objectsKey,
                })
              : t("spotRemoval.detectObjectsHint")
          }
        >
          <input
            type="checkbox"
            checked={detectObjects}
            onChange={(e) => set({ detectObjects: e.target.checked })}
          />
          {t("spotRemoval.detectObjects")}
        </label>
      )}
      {redEye ? null : detectObjects ? (
        <>
          <div
            className="studio-segments"
            role="radiogroup"
            aria-label={t("spotRemoval.objectSelect")}
          >
            {(["brush", "box"] as const).map((way) => (
              <button
                key={way}
                type="button"
                role="radio"
                aria-checked={objectSelect === way}
                onClick={() => set({ objectSelect: way })}
              >
                {t(`spotRemoval.objectSelectModes.${way}`)}
              </button>
            ))}
          </div>
          {objectSelect === "brush" && (
            <AdjustmentControl
              label={t("spotRemoval.size")}
              ariaLabel={t("spotRemoval.sizeAria")}
              value={shown.size}
              min={2}
              max={1000}
              step={1}
              defaultValue={60}
              unit=" px"
              onChange={(size) => set({ size })}
            />
          )}
          <p className="studio-hint">
            {t(
              objectSelect === "brush"
                ? "spotRemoval.detectObjectsHint"
                : "spotRemoval.detectObjectsBoxHint",
            )}{" "}
            {t("spotRemoval.detectObjectsRefineHint")}
          </p>
        </>
      ) : (
        <>
          <AdjustmentControl
            label={t("spotRemoval.size")}
            ariaLabel={t("spotRemoval.sizeAria")}
            value={shown.size}
            min={2}
            max={1000}
            step={1}
            defaultValue={60}
            unit=" px"
            onChange={(size) => set({ size })}
          />
          {shown.mode !== "remove" && (
            <>
              <AdjustmentControl
                label={t("spotRemoval.feather")}
                ariaLabel={t("spotRemoval.featherAria")}
                value={shown.feather}
                min={0}
                max={100}
                defaultValue={0}
                onChange={(feather) => set({ feather })}
              />
              <AdjustmentControl
                label={t("spotRemoval.opacity")}
                ariaLabel={t("spotRemoval.opacityAria")}
                value={shown.opacity}
                min={0}
                max={100}
                defaultValue={100}
                onChange={(opacity) => set({ opacity })}
              />
            </>
          )}
        </>
      )}
      {!redEye && (
        <label className="studio-inline studio-check">
          <input
            type="checkbox"
            checked={visualize.on}
            onChange={(e) => setVisualize({ on: e.target.checked })}
          />
          {t("spotRemoval.visualize")}
        </label>
      )}
      {!redEye && visualize.on && (
        <AdjustmentControl
          label={t("spotRemoval.sensitivity")}
          ariaLabel={t("spotRemoval.sensitivityAria")}
          value={visualize.sensitivity}
          min={0}
          max={100}
          defaultValue={50}
          onChange={(sensitivity) => setVisualize({ sensitivity })}
        />
      )}
    </div>
  );
}
