import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { Invokes } from "../ui/AppProperties";
import type { Adjustments, FaceRetouch, FaceRetouchSettings, RetouchFace } from "../../utils/adjustments";
import { aiErrorMessage } from "../../utils/aiErrors";
import AdjustmentControl from "./AdjustmentControl";

/** What decides the mask frame the faces are found in: orientation,
 * flips, straightening and the geometry keys of
 * crates/engine/src/cache_utils.rs (`GEOMETRY_KEYS`); the same set as the
 * engine's `face_retouch::frame_key`, so both agree when a geometry change
 * makes the stored faces stale. */
export const FACE_FRAME_KEYS = [
  'orientationSteps',
  'flipHorizontal',
  'flipVertical',
  'rotation',
  'transformDistortion',
  'transformVertical',
  'transformHorizontal',
  'transformRotate',
  'transformAspect',
  'transformScale',
  'transformXOffset',
  'transformYOffset',
  'lensDistortionAmount',
  'lensVignetteAmount',
  'lensTcaAmount',
  'lensDistortionParams',
  'lensCorrectionMode',
  'lensMaker',
  'lensModel',
  'lensDistortionEnabled',
  'lensTcaEnabled',
  'autoChromaticAberration',
  'fringeVersion',
  'lensVignetteEnabled',
  'guidedPerspective',
] as const;

/** The geometry the faces are found in (the mask frame, without the crop). */
export function faceRetouchGeometry(adjustments: Adjustments) {
  return Object.fromEntries(
    FACE_FRAME_KEYS.filter((key) => adjustments[key] !== undefined && adjustments[key] !== null).map((key) => [
      key,
      adjustments[key],
    ]),
  );
}

interface Analysis {
  frame: string;
  faces: RetouchFace[];
}

/** Analyses of photos (path + geometry), so a photo's faces are found
 * once per session even before a slider moves. */
const analyses = new Map<string, Promise<Analysis>>();

/** The backend's answer when the face models are not installed and the
 * analysis was not allowed to download them (ai_faces.rs). */
const MODELS_MISSING = 'face-models-missing';

/** `download`: the user asked for the face models (PRIVACY.md: nothing is
 * downloaded just because the tab opened). */
function analyse(path: string, geometry: string, download = false): Promise<Analysis> {
  const key = `${path}\n${geometry}`;
  let pending = analyses.get(key);
  if (!pending) {
    pending = invoke<Analysis>(Invokes.AnalyzeFaceRetouch, { jsAdjustments: JSON.parse(geometry), download });
    pending.catch(() => analyses.delete(key));
    analyses.set(key, pending);
  }
  return pending;
}

/** Faces found again after a geometry change keep the settings of the
 * face nearest to them (within half a face). */
function carryOver(previous: RetouchFace[], next: RetouchFace[]): RetouchFace[] {
  const centre = (f: RetouchFace) => [f.box[0] + f.box[2] / 2, f.box[1] + f.box[3] / 2];
  return next.map((face) => {
    const [x, y] = centre(face);
    let best: RetouchFace | undefined;
    let bestDistance = face.box[2] / 2;
    for (const old of previous) {
      const [ox, oy] = centre(old);
      const d = Math.hypot(ox - x, oy - y);
      if (d < bestDistance) {
        best = old;
        bestDistance = d;
      }
    }
    return best ? { ...face, settings: { ...best.settings }, enabled: best.enabled } : face;
  });
}

/** Least prominence of the blemishes removed at `amount` (0–1), as
 * crates/engine/src/face_retouch.rs `blemish_threshold`. */
export function blemishThreshold(amount: number) {
  return amount <= 0 ? Infinity : 1.0 * Math.pow(0.015 / 1.0, Math.pow(Math.min(amount, 1), 0.25));
}

/** The blemishes of `face` the given values heal (ids), as the engine's
 * `selected_blemishes`: those the slider reaches and those added, without
 * the kept ones. */
export function healedBlemishes(face: RetouchFace, all: FaceRetouchSettings): Set<number> {
  const value = (key: SliderKey, initial: number) => face.settings[key] ?? all[key] ?? initial;
  const amount = (value('blemishes', 0) / 100) * (value('impact', 100) / 100);
  const threshold = blemishThreshold(amount);
  const neck = face.settings.includeNeck ?? all.includeNeck ?? false;
  const added = face.added ?? [];
  return new Set(
    face.blemishes
      .filter(
        (b) =>
          (b.score >= threshold || added.includes(b.id)) && !face.kept.includes(b.id) && (neck || !b.neck),
      )
      .map((b) => b.id),
  );
}

/** How many blemishes of `face` the given values heal. */
export function removedBlemishes(face: RetouchFace, all: FaceRetouchSettings) {
  return healedBlemishes(face, all).size;
}

type SliderKey = Exclude<keyof FaceRetouchSettings, "includeNeck">;
const SLIDERS = [
  { key: "impact", min: 0, max: 100, initial: 100 },
  { key: "blemishes", min: 0, max: 100, initial: 0 },
  { key: "evenSkin", min: 0, max: 100, initial: 0 },
  { key: "texture", min: -100, max: 100, initial: 0 },
  { key: "darkCircles", min: 0, max: 100, initial: 0 },
  { key: "shine", min: 0, max: 100, initial: 0 },
  { key: "contouring", min: 0, max: 100, initial: 0 },
] as const satisfies ReadonlyArray<{ key: SliderKey; min: number; max: number; initial: number }>;

/**
 * Face retouch (Capture One's Retouch Faces Skin tool): all faces or one,
 * Impact, Blemishes (the most prominent first, healed with the skin's own
 * texture), Even skin, Texture, Dark circles and Shine. Faces are found
 * when the tool shows; the recipe keeps them only once a slider moves.
 */
export default function StudioFaceRetouch({ onInteraction }: { onInteraction(dragging: boolean): void }) {
  const { t } = useTranslation();
  const path = useEditorStore((s) => s.selectedImage?.path);
  const ready = useEditorStore((s) => Boolean(s.selectedImage?.isReady));
  const adjustments = useEditorStore((s) => s.adjustments);
  const { setAdjustments } = useEditorActions();
  const geometry = JSON.stringify(faceRetouchGeometry(adjustments));
  const stored = adjustments.faceRetouch ?? null;
  const current = stored && stored.geometry === geometry ? stored : null;
  const [found, setFound] = useState<{ key: string; analysis?: Analysis; error?: string } | null>(null);
  const [target, setTarget] = useState<string | null>(null);
  const showBlemishes = useEditorStore((s) => s.showFaceBlemishes);
  const multiSelected = useLibraryStore((s) => s.multiSelectedPaths);
  const others = multiSelected.filter((p) => p !== path);
  const [applying, setApplying] = useState<string | null>(null);
  const selected = useEditorStore((s) => s.selectedImage);
  /** Capture One's Copy/Apply: the same retouch on the other selected
   * photos, their faces found there; per-face values go to the face at the
   * same place. */
  const applyToOthers = async () => {
    if (!current || !selected || others.length === 0) return;
    const turned = (adjustments.orientationSteps ?? 0) % 2 === 1;
    const [w, h] = turned ? [selected.height, selected.width] : [selected.width, selected.height];
    const faceSettings = current.faces
      .filter((f) => Object.keys(f.settings).length > 0)
      .map((f) => ({ center: [(f.box[0] + f.box[2] / 2) / w, (f.box[1] + f.box[3] / 2) / h], settings: f.settings }));
    setApplying(t("studio.faceRetouch.applying", { count: others.length }));
    try {
      const done = await invoke<number>(Invokes.ApplyFaceRetouchToPaths, {
        paths: others,
        settings: current.settings,
        faceSettings,
      });
      setApplying(t("studio.faceRetouch.applied", { done, total: others.length }));
    } catch (e) {
      setApplying(aiErrorMessage(e));
    }
  };
  const setEditor = useEditorStore((s) => s.setEditor);
  // The overlay belongs to this tool.
  useEffect(() => () => setEditor({ showFaceBlemishes: false }), [setEditor]);
  const key = `${path}\n${geometry}`;

  const [download, setDownload] = useState(0);
  useEffect(() => {
    if (!path || !ready || current) return;
    let live = true;
    analyse(path, geometry, download > 0)
      .then((analysis) => {
        if (!live) return;
        setFound({ key: `${path}\n${geometry}`, analysis });
        // Retouch made in another geometry follows the faces.
        setAdjustments((prev: Adjustments) => {
          const old = prev.faceRetouch;
          if (!old || old.geometry === geometry) return prev;
          return {
            ...prev,
            faceRetouch: { ...old, frame: analysis.frame, geometry, faces: carryOver(old.faces, analysis.faces) },
          };
        });
      })
      .catch((error) => live && setFound({ key: `${path}\n${geometry}`, error: aiErrorMessage(error) }));
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path, ready, geometry, Boolean(current), download]);

  const analysis = found?.key === key ? found.analysis : undefined;
  const faces = current?.faces ?? analysis?.faces ?? [];
  const settings = current?.settings ?? {};
  const face = faces.find((f) => f.id === target) ?? null;

  const update = (change: (retouch: FaceRetouch) => FaceRetouch) =>
    setAdjustments((prev: Adjustments) => {
      const base: FaceRetouch | null =
        prev.faceRetouch && prev.faceRetouch.geometry === geometry
          ? prev.faceRetouch
          : analysis
            ? { settings: {}, faces: analysis.faces, frame: analysis.frame, geometry }
            : null;
      return base ? { ...prev, faceRetouch: change(base) } : prev;
    });
  const set = <K extends keyof FaceRetouchSettings>(key: K, value: FaceRetouchSettings[K]) =>
    update((r) =>
      face
        ? {
            ...r,
            faces: r.faces.map((f) => (f.id === face.id ? { ...f, settings: { ...f.settings, [key]: value } } : f)),
          }
        : { ...r, settings: { ...r.settings, [key]: value } },
    );
  const valueOf = (key: SliderKey, initial: number) => face?.settings[key] ?? settings[key] ?? initial;
  const busy = !current && !analysis && !(found?.key === key && found.error);
  const failure = found?.key === key ? found.error : undefined;
  const modelsMissing = failure === MODELS_MISSING;
  const error = modelsMissing ? undefined : failure;

  return (
    <div className="studio-face-retouch">
      {faces.length > 0 && (
        <div className="studio-people-list" role="radiogroup" aria-label={t("studio.faceRetouch.facesAria")}>
          <button
            role="radio"
            aria-checked={face === null}
            className="studio-people-everyone"
            onClick={() => setTarget(null)}
          >
            {t("studio.faceRetouch.allFaces")}
          </button>
          {faces.map((f, i) => (
            <button
              key={f.id}
              role="radio"
              aria-checked={face?.id === f.id}
              title={
                Object.keys(f.settings).length
                  ? t("studio.faceRetouch.faceOwn", { n: i + 1 })
                  : t("studio.faceRetouch.face", { n: i + 1 })
              }
              className={Object.keys(f.settings).length ? "has-own" : undefined}
              onClick={() => setTarget(f.id)}
            >
              {f.thumbnail ? (
                <img src={f.thumbnail} alt={t("studio.faceRetouch.face", { n: i + 1 })} />
              ) : (
                t("studio.faceRetouch.face", { n: i + 1 })
              )}
            </button>
          ))}
        </div>
      )}
      {busy && <p className="studio-hint">{t("studio.faceRetouch.finding")}</p>}
      {modelsMissing && (
        <div className="studio-hint" role="note">
          <p>{t("studio.faceRetouch.modelsMissing")}</p>
          <button
            type="button"
            className="studio-text-button"
            onClick={() => {
              setFound(null);
              setDownload((n) => n + 1);
            }}
          >
            {t("studio.faceRetouch.downloadModels")}
          </button>
        </div>
      )}
      {error && (
        <p className="studio-hint" role="alert">
          {error}
        </p>
      )}
      {!busy && !failure && faces.length === 0 && (
        <p className="studio-hint" role="note">
          {t("studio.faceRetouch.noFaces")}
        </p>
      )}
      {faces.length > 0 &&
        SLIDERS.map(({ key, min, max, initial }) => (
          <AdjustmentControl
            key={key}
            label={t(`studio.faceRetouch.sliders.${key}`)}
            ariaLabel={t(`studio.faceRetouch.aria.${key}`)}
            min={min}
            max={max}
            value={valueOf(key, initial)}
            defaultValue={initial}
            onInteraction={onInteraction}
            onChange={(value) => set(key, value)}
          />
        ))}
      {faces.length > 0 && (
        <label className="studio-check">
          <input
            type="checkbox"
            aria-label={t("studio.faceRetouch.includeNeck")}
            checked={face?.settings.includeNeck ?? settings.includeNeck ?? false}
            onChange={(e) => set("includeNeck", e.target.checked)}
          />
          {t("studio.faceRetouch.includeNeck")}
        </label>
      )}
      {faces.length > 0 && (
        <label className="studio-check">
          <input
            type="checkbox"
            checked={showBlemishes}
            onChange={(e) => {
              // The overlay edits the recipe's faces: keep them there.
              if (e.target.checked && !current) update((r) => r);
              setEditor({ showFaceBlemishes: e.target.checked });
            }}
          />
          {t("studio.faceRetouch.showBlemishes")}
        </label>
      )}
      {(() => {
        const n = (face ? [face] : faces).reduce((sum, f) => sum + removedBlemishes(f, settings), 0);
        return n > 0 ? (
          <p className="studio-hint" role="status">
            {t("studio.faceRetouch.healed", { count: n })}
          </p>
        ) : null;
      })()}
      {current && others.length > 0 && (
        <button type="button" className="studio-text-button" onClick={() => void applyToOthers()}>
          {t("studio.faceRetouch.applyTo", { count: others.length })}
        </button>
      )}
      {applying && (
        <p className="studio-hint" role="status">
          {applying}
        </p>
      )}
      {face && Object.keys(face.settings).length > 0 && (
        <button
          type="button"
          className="studio-text-button"
          onClick={() =>
            update((r) => ({ ...r, faces: r.faces.map((f) => (f.id === face.id ? { ...f, settings: {} } : f)) }))
          }
        >
          {t("studio.faceRetouch.useAll")}

        </button>
      )}
    </div>
  );
}
