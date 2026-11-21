import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { useEditorStore } from '../../store/useEditorStore';
import { useLibraryStore } from '../../store/useLibraryStore';
import { recipeWrite, useEditorActions } from '../../hooks/useEditorActions';
import { Invokes } from '../ui/AppProperties';
import type {
  Adjustments,
  FaceProtectStroke,
  FaceRetouch,
  FaceRetouchSettings,
  RetouchFace
} from '../../utils/adjustments';
import { aiErrorMessage } from '../../utils/aiErrors';
import { errorCode } from '../../shared/ipc/errors';
import AdjustmentControl from './AdjustmentControl';
import { GEOMETRY_KEYS, ORIENTATION_KEYS } from '../../utils/geometryKeys';
import { frameGeometry as shownGeometry, reframePoint } from '../../utils/maskGeometry';
import { useSettingsStore } from '../../store/useSettingsStore';
import { bracketKeyLabels } from '../../utils/bracketKeys';

/** Geometry keys that correct colour, not where a pixel is (the engine's
 * `face_retouch::COLOUR_ONLY_KEYS`): faces stay valid when they change. */
const COLOUR_ONLY_KEYS: ReadonlySet<string> = new Set([
  'lensVignetteAmount',
  'lensManualVignetteAmount',
  'lensManualVignetteMidpoint',
  'lensVignetteEnabled',
  'lensTcaAmount',
  'lensTcaEnabled',
  'autoChromaticAberration',
  'autoCaVersion',
  'fringeVersion'
]);

/** What decides the mask frame the faces are found in: orientation,
 * flips, straightening and the geometry keys of
 * crates/engine/src/cache_utils.rs (`GEOMETRY_KEYS`) that move pixels; the
 * same set as the engine's `face_retouch::frame_key`, so both agree when a
 * geometry change leaves the stored faces in their own frame (the engine
 * still places them there; the panel finds them again in the new one). */
export const FACE_FRAME_KEYS = [
  ...ORIENTATION_KEYS,
  ...GEOMETRY_KEYS.filter((key) => !COLOUR_ONLY_KEYS.has(key))
] as const;

/** The keys of a frame without the engine's `2:` prefix (faces found
 * before 2026-10-20): every geometry key, colour corrections included. */
const FIRST_FACE_FRAME_KEYS = [...ORIENTATION_KEYS, ...GEOMETRY_KEYS] as const;

/** The geometry the faces are found in (the mask frame, without the crop). */
export function faceRetouchGeometry(
  adjustments: Adjustments,
  keys: ReadonlyArray<(typeof FIRST_FACE_FRAME_KEYS)[number]> = FACE_FRAME_KEYS
) {
  return Object.fromEntries(
    keys
      .filter((key) => adjustments[key] !== undefined && adjustments[key] !== null)
      .map((key) => [key, adjustments[key]])
  );
}

/** Whether `retouch`'s faces belong to the geometry of `adjustments`, by the
 * rule of its frame (as the engine's `face_retouch::frame_matches`). */
export function faceRetouchIsCurrent(retouch: FaceRetouch | null | undefined, adjustments: Adjustments) {
  return Boolean(retouch) && retouch!.geometry === frameGeometry(adjustments, retouch!.frame);
}

/** The panel's `geometry` for faces of `frame`, by that frame's rule. */
function frameGeometry(adjustments: Adjustments, frame: string | undefined) {
  const keys = frame?.startsWith('2:') ? FACE_FRAME_KEYS : FIRST_FACE_FRAME_KEYS;
  return JSON.stringify(faceRetouchGeometry(adjustments, keys));
}

interface Analysis {
  frame: string;
  faces: RetouchFace[];
}

/** Analyses of photos (path + geometry), so a photo's faces are found
 * once per session even before a slider moves. */
const analyses = new Map<string, Promise<Analysis>>();

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
function carryOver(
  previous: RetouchFace[],
  next: RetouchFace[],
  sides?: Sides
): { faces: RetouchFace[]; swapped: boolean } {
  const centre = (f: RetouchFace) => [f.box[0] + f.box[2] / 2, f.box[1] + f.box[3] / 2];
  // The old faces' centres where they are now (a flip or turn moves them).
  const moved = (f: RetouchFace) => {
    const [x, y] = centre(f);
    if (!sides) return [x, y];
    const p = reframePoint({ x, y }, sides.from, sides.to, sides.width, sides.height);
    return [p.x, p.y];
  };
  let swapped = false;
  const faces = next.map((face) => {
    const [x, y] = centre(face);
    let best: RetouchFace | undefined;
    let bestDistance = face.box[2] / 2;
    for (const old of previous) {
      const [ox, oy] = moved(old);
      const d = Math.hypot(ox - x, oy - y);
      if (d < bestDistance) {
        best = old;
        bestDistance = d;
      }
    }
    if (!best) return face;
    // Left and right of the photo: after a flip or a turn the face's other
    // eye may be on the left; its own values follow the eye.
    const turned = sides !== undefined && eyesSwapped(best, face, sides);
    swapped ||= turned;
    return { ...face, settings: turned ? swapSides(best.settings) : { ...best.settings }, enabled: best.enabled };
  });
  return { faces, swapped };
}

/** The geometry change faces are carried through, for the eyes' sides. */
interface Sides {
  from: ReturnType<typeof shownGeometry>;
  to: ReturnType<typeof shownGeometry>;
  width: number;
  height: number;
}

/** `settings` with the left and right eye's own values exchanged. */
export function swapSides(settings: FaceRetouchSettings): FaceRetouchSettings {
  const out: FaceRetouchSettings = { ...settings };
  for (const [left, right] of [
    ['irisLeft', 'irisRight'],
    ['scleraLeft', 'scleraRight']
  ] as const) {
    delete out[left];
    delete out[right];
    if (settings[right] !== undefined) out[left] = settings[right];
    if (settings[left] !== undefined) out[right] = settings[left];
  }
  return out;
}

/** Whether the eye on the left of `old` (its faces found in `sides.from`)
 * is the eye on the right of `next` (found in `sides.to`): the photo was
 * flipped or turned. Faces without both eyes keep their sides. */
export function eyesSwapped(old: RetouchFace, next: RetouchFace, sides: Sides): boolean {
  const side = old.eyes?.[0] ? 0 : old.eyes?.[1] ? 1 : -1;
  const [a, b] = [next.eyes?.[0], next.eyes?.[1]];
  if (side < 0 || (!a && !b)) return false;
  const iris = old.eyes![side]!.iris;
  const p = reframePoint({ x: iris[0], y: iris[1] }, sides.from, sides.to, sides.width, sides.height);
  const distance = (eye: { iris: number[] }) => Math.hypot(p.x - eye.iris[0], p.y - eye.iris[1]);
  let slot: number;
  if (a && b) slot = distance(a) <= distance(b) ? 0 : 1;
  else {
    // One eye seen now: the old eye is that one when it lands within a
    // quarter of the face's width of it, else the hidden one.
    const [seen, index] = a ? [a, 0] : [b!, 1];
    slot = distance(seen) <= next.box[2] / 4 ? index : 1 - index;
  }
  return slot !== side;
}

/** The geometry change from a retouch's panel `geometry` to `to`. */
function sidesOf(from: string, to: Adjustments, width?: number, height?: number): Sides | undefined {
  if (!width || !height) return undefined;
  try {
    return {
      from: shownGeometry(JSON.parse(from) as Record<string, unknown>),
      to: shownGeometry(to),
      width,
      height
    };
  } catch {
    return undefined;
  }
}

/** Protection strokes painted on the photo shown with the panel geometry
 * `from` (a `FaceRetouch.geometry`), where that spot of the photo is shown
 * with `to`'s orientation, flips and straightening; `width` × `height` is
 * the photo's own size. Lens and perspective corrections that changed
 * meanwhile are not followed (the strokes are a brush radius wide). */
export function reframeProtection(
  strokes: FaceProtectStroke[] | undefined,
  from: string,
  to: Adjustments,
  width: number | undefined,
  height: number | undefined
): FaceProtectStroke[] | undefined {
  if (!strokes?.length || !width || !height) return strokes;
  let before: Record<string, unknown>;
  try {
    before = JSON.parse(from) as Record<string, unknown>;
  } catch {
    return strokes;
  }
  const [a, b] = [shownGeometry(before), shownGeometry(to)];
  return strokes.map((stroke) => ({
    ...stroke,
    points: stroke.points.map((p) => reframePoint(p, a, b, width, height))
  }));
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
      .filter((b) => (b.score >= threshold || added.includes(b.id)) && !face.kept.includes(b.id) && (neck || !b.neck))
      .map((b) => b.id)
  );
}

/** How many blemishes of `face` the given values heal. */
export function removedBlemishes(face: RetouchFace, all: FaceRetouchSettings) {
  return healedBlemishes(face, all).size;
}

type SliderKey = Exclude<keyof FaceRetouchSettings, 'includeNeck'>;

/** Which eye the Eyes sliders show: both, or the one on the left or right
 * of the photo. */
export type EyeSide = 'both' | 'left' | 'right';

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** The value an Eyes slider shows for `side`: the value for both eyes, or
 * for one eye the value for both plus its own offset (as the engine's
 * `Settings::amounts`). `get` reads a key with the face's own value over
 * the one for all faces. */
export function eyeValue(get: (key: SliderKey) => number | undefined, part: 'iris' | 'sclera', side: EyeSide) {
  const both = clamp(get(part) ?? 0, 0, 100);
  if (side === 'both') return both;
  const offset = get(side === 'left' ? `${part}Left` : `${part}Right`) ?? 0;
  return clamp(both + clamp(offset, -100, 100), 0, 100);
}

/** The settings change that gives one eye (`side` left or right) the value
 * `value`: its offset against the value for both eyes. */
export function eyeChange(
  get: (key: SliderKey) => number | undefined,
  part: 'iris' | 'sclera',
  side: 'left' | 'right',
  value: number
) {
  const key: SliderKey = side === 'left' ? `${part}Left` : `${part}Right`;
  return { key, value: clamp(value - clamp(get(part) ?? 0, 0, 100), -100, 100) };
}

/** `faces` with the eyes and mouth of the analysed faces filled in where
 * they are missing (faces found before Eyes and Teeth), each from the
 * analysed face at the same place (within half a face). */
export function withEyesAndMouth(faces: RetouchFace[], analysed: RetouchFace[]): RetouchFace[] {
  const centre = (f: RetouchFace) => [f.box[0] + f.box[2] / 2, f.box[1] + f.box[3] / 2];
  return faces.map((face) => {
    if (face.eyes !== undefined) return face;
    const [x, y] = centre(face);
    let best: RetouchFace | undefined;
    let bestDistance = face.box[2] / 2;
    for (const other of analysed) {
      const [ox, oy] = centre(other);
      const d = Math.hypot(ox - x, oy - y);
      if (d < bestDistance) {
        best = other;
        bestDistance = d;
      }
    }
    return best ? { ...face, eyes: best.eyes ?? [], mouth: best.mouth ?? null } : face;
  });
}

const SLIDERS = [
  { key: 'impact', min: 0, max: 100, initial: 100 },
  { key: 'blemishes', min: 0, max: 100, initial: 0 },
  { key: 'evenSkin', min: 0, max: 100, initial: 0 },
  { key: 'texture', min: -100, max: 100, initial: 0 },
  { key: 'darkCircles', min: 0, max: 100, initial: 0 },
  { key: 'shine', min: 0, max: 100, initial: 0 },
  { key: 'contouring', min: 0, max: 100, initial: 0 }
] as const satisfies ReadonlyArray<{ key: SliderKey; min: number; max: number; initial: number }>;

/**
 * Face retouch (Capture One's Retouch Faces Skin tool): all faces or one,
 * Impact, Blemishes (the most prominent first, healed with the skin's own
 * texture), Even skin, Texture, Dark circles and Shine. Faces are found
 * when the tool shows; the recipe keeps them only once a slider moves.
 */
export default function StudioFaceRetouch({ onInteraction }: { onInteraction(dragging: boolean): void }) {
  const { t } = useTranslation();
  const osPlatform = useSettingsStore((s) => s.osPlatform);
  const sizeKeys = bracketKeyLabels(osPlatform);
  const path = useEditorStore((s) => s.selectedImage?.path);
  const ready = useEditorStore((s) => Boolean(s.selectedImage?.isReady));
  const adjustments = useEditorStore((s) => s.adjustments);
  const { setAdjustments } = useEditorActions();
  const geometry = JSON.stringify(faceRetouchGeometry(adjustments));
  const stored = adjustments.faceRetouch ?? null;
  const current = faceRetouchIsCurrent(stored, adjustments) ? stored : null;
  // `modelsMissing`: the face models are not installed and the analysis was
  // not allowed to download them (`ai_faces::FACE_MODELS_MISSING`).
  const [found, setFound] = useState<{
    key: string;
    analysis?: Analysis;
    error?: string;
    modelsMissing?: boolean;
  } | null>(null);
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
    setApplying(t('studio.faceRetouch.applying', { count: others.length }));
    try {
      // A long command (faces found in each photo): it reports each photo's
      // write, which quitting counts.
      const done = await recipeWrite(
        others,
        (batch) =>
          invoke<number>(Invokes.ApplyFaceRetouchToPaths, {
            paths: others,
            settings: current.settings,
            faceSettings,
            ...(batch ? { batch } : {})
          }),
        /** It answers how many it wrote; short of all it failed with the
         * photos not written (a single one: any may have failed). */
        { kind: 'faceRetouch', failedOf: (written) => (written < others.length ? others : []), batch: true }
      );
      setApplying(t('studio.faceRetouch.applied', { done, total: others.length }));
    } catch (e) {
      setApplying(aiErrorMessage(e));
    }
  };
  const setEditor = useEditorStore((s) => s.setEditor);
  const protectBrush = useEditorStore((s) => s.faceProtect);
  const showProtection = useEditorStore((s) => s.showFaceProtection);
  // The overlays and the brush belong to this tool.
  useEffect(
    () => () =>
      setEditor({
        showFaceBlemishes: false,
        showFaceProtection: false,
        faceProtect: { ...useEditorStore.getState().faceProtect, painting: false }
      }),
    [setEditor]
  );
  const key = `${path}\n${geometry}`;

  const [download, setDownload] = useState(0);
  // Faces stored before Eyes and Teeth have no eyes or mouth. Their Eyes
  // and Teeth wait until the user asks to find them (the face models run
  // again, no download); the eyes and mouth are then stored with the faces
  // in one change, so no value is ever saved that an export cannot apply.
  const needsEyes = Boolean(current?.faces.some((f) => f.eyes === undefined));
  const [eyesAsked, setEyesAsked] = useState<string | null>(null);
  const findEyes = needsEyes && eyesAsked === key;
  useEffect(() => {
    if (!path || !ready || (current && !findEyes)) return;
    let live = true;
    analyse(path, geometry, download > 0)
      .then((analysis) => {
        if (!live) return;
        setFound({ key: `${path}\n${geometry}`, analysis });
        // Retouch made in another geometry follows the faces, and the
        // protected areas the photo.
        setAdjustments((prev: Adjustments) => {
          const old = prev.faceRetouch;
          if (old && faceRetouchIsCurrent(old, prev) && old.faces.some((f) => f.eyes === undefined)) {
            return { ...prev, faceRetouch: { ...old, faces: withEyesAndMouth(old.faces, analysis.faces) } };
          }
          if (!old || faceRetouchIsCurrent(old, prev)) return prev;
          const { width, height } = useEditorStore.getState().selectedImage ?? {};
          const protect = reframeProtection(old.protect, old.geometry, prev, width, height);
          const carried = carryOver(old.faces, analysis.faces, sidesOf(old.geometry, prev, width, height));
          return {
            ...prev,
            faceRetouch: {
              ...old,
              frame: analysis.frame,
              geometry: frameGeometry(prev, analysis.frame),
              faces: carried.faces,
              settings: carried.swapped ? swapSides(old.settings) : old.settings,
              ...(protect && { protect })
            }
          };
        });
      })
      .catch((error) => {
        if (!live) return;
        // A failed search for eyes and teeth can be asked again.
        setEyesAsked(null);
        setFound({
          key: `${path}\n${geometry}`,
          error: aiErrorMessage(error),
          modelsMissing: errorCode(error) === 'models_missing'
        });
      });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path, ready, geometry, Boolean(current), findEyes, download]);

  const analysis = found?.key === key ? found.analysis : undefined;
  const faces = current?.faces ?? analysis?.faces ?? [];
  const settings = current?.settings ?? {};
  const face = faces.find((f) => f.id === target) ?? null;

  const update = (change: (retouch: FaceRetouch) => FaceRetouch) =>
    setAdjustments((prev: Adjustments) => {
      const base: FaceRetouch | null =
        prev.faceRetouch && faceRetouchIsCurrent(prev.faceRetouch, prev)
          ? prev.faceRetouch
          : analysis
            ? {
                settings: {},
                faces: analysis.faces,
                frame: analysis.frame,
                geometry: frameGeometry(prev, analysis.frame)
              }
            : null;
      return base ? { ...prev, faceRetouch: change(base) } : prev;
    });
  const set = <K extends keyof FaceRetouchSettings>(key: K, value: FaceRetouchSettings[K]) =>
    update((r) =>
      face
        ? {
            ...r,
            faces: r.faces.map((f) => (f.id === face.id ? { ...f, settings: { ...f.settings, [key]: value } } : f))
          }
        : { ...r, settings: { ...r.settings, [key]: value } }
    );
  const valueOf = (key: SliderKey, initial: number) => face?.settings[key] ?? settings[key] ?? initial;
  const get = (key: SliderKey) => face?.settings[key] ?? settings[key];
  const [eyeSide, setEyeSide] = useState<EyeSide>('both');
  // Faces found before Eyes and Teeth: their sliders wait for the eyes and
  // mouth (all faces when none is chosen).
  const eyesMissing = Boolean(current) && (face ? face.eyes === undefined : needsEyes);
  const busy = !current && !analysis && !(found?.key === key && found.error);
  const failure = found?.key === key ? found.error : undefined;
  const modelsMissing = found?.key === key && Boolean(found.modelsMissing);
  const error = modelsMissing ? undefined : failure;

  return (
    <div className="studio-face-retouch">
      {faces.length > 0 && (
        <div className="studio-people-list" role="radiogroup" aria-label={t('studio.faceRetouch.facesAria')}>
          <button
            role="radio"
            aria-checked={face === null}
            className="studio-people-everyone"
            onClick={() => setTarget(null)}
          >
            {t('studio.faceRetouch.allFaces')}
          </button>
          {faces.map((f, i) => (
            <button
              key={f.id}
              role="radio"
              aria-checked={face?.id === f.id}
              title={
                Object.keys(f.settings).length
                  ? t('studio.faceRetouch.faceOwn', { n: i + 1 })
                  : t('studio.faceRetouch.face', { n: i + 1 })
              }
              className={Object.keys(f.settings).length ? 'has-own' : undefined}
              onClick={() => setTarget(f.id)}
            >
              {f.thumbnail ? (
                <img src={f.thumbnail} alt={t('studio.faceRetouch.face', { n: i + 1 })} />
              ) : (
                t('studio.faceRetouch.face', { n: i + 1 })
              )}
            </button>
          ))}
        </div>
      )}
      {busy && <p className="studio-hint">{t('studio.faceRetouch.finding')}</p>}
      {modelsMissing && (
        <div className="studio-hint" role="note">
          <p>{t('studio.faceRetouch.modelsMissing')}</p>
          <button
            type="button"
            className="studio-text-button"
            onClick={() => {
              setFound(null);
              setDownload((n) => n + 1);
            }}
          >
            {t('studio.faceRetouch.downloadModels')}
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
          {t('studio.faceRetouch.noFaces')}
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
            aria-label={t('studio.faceRetouch.includeNeck')}
            checked={face?.settings.includeNeck ?? settings.includeNeck ?? false}
            onChange={(e) => set('includeNeck', e.target.checked)}
          />
          {t('studio.faceRetouch.includeNeck')}
        </label>
      )}
      {faces.length > 0 && (
        <>
          <div className="studio-subheading">{t('studio.faceRetouch.eyes.title')}</div>
          {eyesMissing && (
            <div className="studio-hint" role="note">
              <p>{t('studio.faceRetouch.eyes.missing')}</p>
              {findEyes ? (
                <p role="status">{t('studio.faceRetouch.eyes.finding')}</p>
              ) : (
                <button type="button" className="studio-text-button" onClick={() => setEyesAsked(key)}>
                  {t('studio.faceRetouch.eyes.find')}
                </button>
              )}
            </div>
          )}
          <div className="studio-segments" role="radiogroup" aria-label={t('studio.faceRetouch.eyes.sideAria')}>
            {(['both', 'left', 'right'] as const).map((side) => (
              <button
                key={side}
                type="button"
                role="radio"
                aria-checked={eyeSide === side}
                title={t(`studio.faceRetouch.eyes.sideHint.${side}`)}
                onClick={() => setEyeSide(side)}
              >
                {t(`studio.faceRetouch.eyes.sides.${side}`)}
              </button>
            ))}
          </div>
          {(['iris', 'sclera'] as const).map((part) => (
            <AdjustmentControl
              key={`${part}-${eyeSide}`}
              label={t(`studio.faceRetouch.sliders.${part}`)}
              ariaLabel={t(`studio.faceRetouch.aria.${part}`, {
                side: t(`studio.faceRetouch.eyes.sides.${eyeSide}`)
              })}
              min={0}
              max={100}
              value={eyeValue(get, part, eyeSide)}
              defaultValue={eyeSide === 'both' ? 0 : eyeValue(get, part, 'both')}
              disabled={eyesMissing}
              onInteraction={onInteraction}
              onChange={(value) => {
                if (eyeSide === 'both') set(part, value);
                else {
                  const change = eyeChange(get, part, eyeSide, value);
                  set(change.key, change.value);
                }
              }}
            />
          ))}
          <div className="studio-subheading">{t('studio.faceRetouch.teeth.title')}</div>
          {(['teethBrightness', 'teethWhiten'] as const).map((key) => (
            <AdjustmentControl
              key={key}
              label={t(`studio.faceRetouch.sliders.${key}`)}
              ariaLabel={t(`studio.faceRetouch.aria.${key}`)}
              min={0}
              max={100}
              value={valueOf(key, 0)}
              defaultValue={0}
              disabled={eyesMissing}
              onInteraction={onInteraction}
              onChange={(value) => set(key, value)}
            />
          ))}
          {face?.eyes?.length === 0 && !face.mouth && (
            <p className="studio-hint" role="note">
              {t('studio.faceRetouch.eyes.none')}
            </p>
          )}
        </>
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
          {t('studio.faceRetouch.showBlemishes')}
        </label>
      )}
      {faces.length > 0 && (
        <div className="studio-face-protect">
          <div
            className="studio-segments"
            role="radiogroup"
            aria-label={t('studio.faceRetouch.protect.brush')}
            title={t('studio.faceRetouch.protect.hint', sizeKeys)}
          >
            {(['off', 'paint', 'erase'] as const).map((mode) => {
              const on = protectBrush.painting && (mode === 'erase') === protectBrush.erase;
              return (
                <button
                  key={mode}
                  type="button"
                  role="radio"
                  aria-checked={mode === 'off' ? !protectBrush.painting : on}
                  onClick={() => {
                    // The brush paints on the recipe's faces: keep them there.
                    if (mode !== 'off' && !current) update((r) => r);
                    setEditor({
                      faceProtect: {
                        ...protectBrush,
                        painting: mode !== 'off',
                        erase: mode === 'erase'
                      }
                    });
                  }}
                >
                  {t(`studio.faceRetouch.protect.modes.${mode}`)}
                </button>
              );
            })}
          </div>
          {protectBrush.painting && (
            <>
              <AdjustmentControl
                label={t('studio.faceRetouch.protect.size')}
                ariaLabel={t('studio.faceRetouch.protect.sizeAria')}
                min={2}
                max={500}
                step={1}
                unit=" px"
                value={protectBrush.size}
                defaultValue={30}
                onChange={(size) => setEditor({ faceProtect: { ...useEditorStore.getState().faceProtect, size } })}
              />
              <AdjustmentControl
                label={t('studio.faceRetouch.protect.feather')}
                ariaLabel={t('studio.faceRetouch.protect.featherAria')}
                min={0}
                max={100}
                value={protectBrush.feather}
                defaultValue={50}
                onChange={(feather) =>
                  setEditor({ faceProtect: { ...useEditorStore.getState().faceProtect, feather } })
                }
              />
              <p className="studio-hint">{t('studio.faceRetouch.protect.hint', sizeKeys)}</p>
            </>
          )}
          <label className="studio-check">
            <input
              type="checkbox"
              checked={showProtection || protectBrush.painting}
              disabled={protectBrush.painting}
              onChange={(e) => setEditor({ showFaceProtection: e.target.checked })}
            />
            {t('studio.faceRetouch.protect.show')}
          </label>
          {(current?.protect?.length ?? 0) > 0 && (
            <button
              type="button"
              className="studio-text-button"
              onClick={() =>
                update((r) => {
                  const { protect: _cleared, ...rest } = r;
                  return rest;
                })
              }
            >
              {t('studio.faceRetouch.protect.clear')}
            </button>
          )}
        </div>
      )}
      {(() => {
        const n = (face ? [face] : faces).reduce((sum, f) => sum + removedBlemishes(f, settings), 0);
        return n > 0 ? (
          <p className="studio-hint" role="status">
            {t('studio.faceRetouch.healed', { count: n })}
          </p>
        ) : null;
      })()}
      {current && others.length > 0 && (
        <button type="button" className="studio-text-button" onClick={() => void applyToOthers()}>
          {t('studio.faceRetouch.applyTo', { count: others.length })}
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
          {t('studio.faceRetouch.useAll')}
        </button>
      )}
    </div>
  );
}
