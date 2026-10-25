import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Trans, useTranslation } from "react-i18next";
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { isCurrentEditorImage, useEditorActions } from "../../hooks/useEditorActions";
import type { GuideLine, UprightMode } from "../../utils/adjustments";
import { GUIDE_COLORS } from "../../utils/guideLines";
import { Invokes, Panel } from "../ui/AppProperties";

/** `calculate_upright` (UprightResult in upright.rs). */
interface UprightResult {
  lines: GuideLine[];
  applied: "level" | "vertical" | "full";
  segments: number;
  agreeing: number;
}

/** Upright modes; labels and descriptions in `studio.upright.modes`. */
const MODES = ["off", "auto", "level", "vertical", "full", "guided"] as const satisfies readonly UprightMode[];

/** What each automatic mode did, shown under the buttons (Lightroom shows it
 * only by the selected button; the note says what Auto chose). */
const DONE = {
  level: "studio.upright.done.level",
  vertical: "studio.upright.done.vertical",
  full: "studio.upright.done.full",
} as const;
const AUTO_DONE = {
  level: "studio.upright.autoDone.level",
  vertical: "studio.upright.autoDone.vertical",
  full: "studio.upright.autoDone.full",
} as const;

/** Lightroom's Upright (Capture One's Keystone): automatic modes find straight
 * lines in the photo; Guided uses guides drawn on it. Both feed the guided
 * perspective correction; Constrain crop zooms to the corrected area. */
export default function StudioUpright() {
  const { t } = useTranslation();
  // Only its own value: other edits (a slider drag's commits) do not
  // re-render it.
  const guided = useEditorStore((s) => s.adjustments.guidedPerspective);
  const drawing = useEditorStore((s) => s.isGuidedPerspectiveActive);
  const setEditor = useEditorStore((s) => s.setEditor);
  const activePanel = useUIStore((s) => s.activePanel);
  const { setAdjustments } = useEditorActions();
  const [busy, setBusy] = useState<UprightMode | null>(null);
  const [note, setNote] = useState("");
  const lines = guided?.lines ?? [];
  const mode: UprightMode = guided?.upright ?? (guided?.enabled && lines.length >= 2 ? "guided" : "off");
  // Keeps its look while the next photograph loads (the tools are inert).
  const ready = useEditorStore(panelsShowReady);
  const drawingNow = drawing && activePanel === Panel.Crop;

  // The Crop & geometry tab is the crop panel itself: only drawing stops.
  const stopDrawing = () => setEditor({ isGuidedPerspectiveActive: false });
  // Guides are drawn on the uncropped photo, as in the crop tool.
  const startDrawing = () => {
    setEditor({ isGuidedPerspectiveActive: true });
    useUIStore.getState().setPanel(Panel.Crop);
  };
  const choose = async (next: UprightMode) => {
    setNote("");
    const autoCrop = guided?.autoCrop ?? true;
    if (next === "off") {
      stopDrawing();
      setAdjustments((prev) => ({
        ...prev,
        guidedPerspective: { enabled: false, lines: [], autoCrop, upright: "off" },
      }));
      return;
    }
    if (next === "guided") {
      if (mode !== "guided")
        setAdjustments((prev) => ({
          ...prev,
          guidedPerspective: { enabled: false, lines: [], autoCrop, upright: "guided" },
        }));
      startDrawing();
      return;
    }
    stopDrawing();
    // The result belongs to the photograph it was asked for.
    const path = useEditorStore.getState().selectedImage?.path;
    if (!path) return;
    setBusy(next);
    try {
      const result = await invoke<UprightResult | null>(Invokes.CalculateUpright, {
        mode: next,
        adjustments: useEditorStore.getState().adjustments,
      });
      if (!isCurrentEditorImage(path)) return;
      if (!result) {
        setNote(
          next === "level"
            ? t("studio.upright.noLines")
            : t("studio.upright.notEnoughLines"),
        );
        return;
      }
      setAdjustments((prev) => ({
        ...prev,
        guidedPerspective: { enabled: true, lines: result.lines, autoCrop, upright: next },
      }));
      setNote(next === "auto" ? t(AUTO_DONE[result.applied]) : t(DONE[result.applied]));
    } catch (e) {
      if (isCurrentEditorImage(path)) setNote(t("studio.upright.failed", { error: String(e) }));
    } finally {
      setBusy(null);
    }
  };
  const clearGuides = () =>
    setAdjustments((prev) => ({
      ...prev,
      guidedPerspective: { ...prev.guidedPerspective, enabled: false, lines: [] },
    }));

  return (
    <div className="studio-upright">
      <div className="studio-segments" role="radiogroup" aria-label={t("studio.upright.title")}>
        {MODES.map((id) => (
          <button
            key={id}
            role="radio"
            aria-checked={mode === id}
            title={t(`studio.upright.modes.${id}.title`)}
            disabled={!ready || busy !== null}
            onClick={() => void choose(id)}
          >
            {t(`studio.upright.modes.${id}.label`)}
          </button>
        ))}
      </div>
      {busy && <p className="studio-metadata-note">{t("studio.upright.finding")}</p>}
      {mode === "guided" && (
        <div className="studio-upright-guided">
          {drawingNow ? (
            <p className="studio-metadata-note">
              <Trans
                i18nKey="studio.upright.drawHelp"
                components={{
                  v: <b style={{ color: GUIDE_COLORS.vertical }} />,
                  h: <b style={{ color: GUIDE_COLORS.horizontal }} />,
                }}
              />
            </p>
          ) : (
            <p className="studio-metadata-note">
              {lines.length < 2
                ? t("studio.upright.drawTwo")
                : t("studio.upright.corrected")}
            </p>
          )}
          <div className="studio-inline">
            <span>
              {t("studio.upright.guideCount", {
                vertical: lines.filter((l) => l.type === "vertical").length,
                horizontal: lines.filter((l) => l.type !== "vertical").length,
              })}
            </span>
            {lines.length > 0 && (
              <button className="studio-text-button" onClick={clearGuides}>
                {t("studio.upright.clear")}
              </button>
            )}
            {drawingNow ? (
              <button className="studio-text-button" onClick={stopDrawing} title={t("studio.upright.doneTitle")}>
                {t("studio.upright.doneButton")}
              </button>
            ) : (
              <button className="studio-text-button" onClick={startDrawing}>
                {t("studio.upright.editGuides")}
              </button>
            )}
          </div>
        </div>
      )}
      {note && !busy && <p className="studio-metadata-note">{note}</p>}
      {mode !== "off" && (
        <label className="studio-inline studio-check" title={t("studio.upright.constrainTitle")}>
          <input
            type="checkbox"
            checked={guided?.autoCrop !== false}
            disabled={!ready}
            onChange={(e) =>
              setAdjustments((prev) => ({
                ...prev,
                guidedPerspective: { ...prev.guidedPerspective, autoCrop: e.target.checked },
              }))
            }
          />
          {t("studio.upright.constrainCrop")}
        </label>
      )}
    </div>
  );
}
