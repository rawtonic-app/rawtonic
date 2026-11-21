import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from 'react-i18next';
import { useEditorStore } from '../../store/useEditorStore';
import { useLibraryStore } from '../../store/useLibraryStore';
import { useActivityStore } from '../../store/useActivityStore';
import { modelDownloadId } from '../../activity/bridges';
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
  /** The eye and teeth parser was asked for but is not installed. */
  eyesTeethMissing?: boolean;
}

/** Analyses of photos (path + geometry), so a photo's faces are found
 * once per session even before a slider moves; `parses` the same with the
 * eye and teeth parser's outlines. */
const analyses = new Map<string, Promise<Analysis>>();
const parses = new Map<string, Promise<Analysis>>();

/** `download`: the user asked for the face models (PRIVACY.md: nothing is
 * downloaded just because the tab opened). */
function analyse(path: string, geometry: string, download = false): Promise<Analysis> {
  const key = `${path}\n${geometry}`;
  let pending = analyses.get(key);
  if (!pending) {
    pending = invoke<Analysis>(Invokes.AnalyzeFaceRetouch, { jsAdjustments: JSON.parse(geometry), path, download });
    pending.catch(() => analyses.delete(key));
    analyses.set(key, pending);
  }
  return pending;
}

/** The faces with the eye and teeth parser's outlines, the parser run when
 * it is installed or `download` (the user's click) may fetch it;
 * `downloadFaces` the same for the face models. A result without the
 * parser is not kept, so the next request after a download finds it; a
 * request in progress is shared. */
function analyseEyesTeeth(path: string, geometry: string, download = false, downloadFaces = false): Promise<Analysis> {
  const key = `${path}\n${geometry}`;
  let pending = parses.get(key);
  if (!pending || download || downloadFaces) {
    pending = invoke<Analysis>(Invokes.AnalyzeFaceRetouch, {
      jsAdjustments: JSON.parse(geometry),
      path,
      download: downloadFaces,
      eyesTeeth: true,
      downloadEyesTeeth: download
    });
    const mine = pending;
    const forget = () => parses.get(key) === mine && parses.delete(key);
    pending.then((result) => result.eyesTeethMissing && forget(), forget);
    parses.set(key, pending);
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

/** The analysed face at the place of `face` (within half a face). */
function nearestFace(face: RetouchFace, analysed: RetouchFace[]): RetouchFace | undefined {
  const centre = (f: RetouchFace) => [f.box[0] + f.box[2] / 2, f.box[1] + f.box[3] / 2];
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
  return best;
}

/** `faces` with the eyes and mouth of the analysed faces filled in where
 * they are missing (faces found before Eyes and Teeth), each from the
 * analysed face at the same place (within half a face), with the eye and
 * teeth parser's outlines when it ran. */
export function withEyesAndMouth(faces: RetouchFace[], analysed: RetouchFace[]): RetouchFace[] {
  return faces.map((face) => {
    if (face.eyes !== undefined) return face;
    const best = nearestFace(face, analysed);
    return best
      ? {
          ...face,
          eyes: best.eyes ?? [],
          mouth: best.mouth ?? null,
          ...(best.parsedTeeth !== undefined && { parsedTeeth: best.parsedTeeth })
        }
      : face;
  });
}

/** The Eyes and Teeth sliders' keys. */
const EYES_TEETH_KEYS = [
  'iris',
  'sclera',
  'irisLeft',
  'irisRight',
  'scleraLeft',
  'scleraRight',
  'teethBrightness',
  'teethWhiten'
] as const satisfies ReadonlyArray<SliderKey>;

/** Whether the eye and teeth parser looked at `faces`. */
export function eyesTeethParsed(faces: RetouchFace[]) {
  return faces.some((f) => f.parsedTeeth !== undefined);
}

/** Whether `retouch` has an Eyes or Teeth value (for all faces or one). */
export function eyesTeethSet(retouch: FaceRetouch) {
  return [retouch.settings, ...retouch.faces.map((f) => f.settings)].some((s) =>
    EYES_TEETH_KEYS.some((key) => (s[key] ?? 0) !== 0)
  );
}

/** `faces` with the eyes, mouth and eye and teeth parser's outlines of the
 * analysed face at the same place (within half a face). */
export function withParsedEyesTeeth(faces: RetouchFace[], analysed: RetouchFace[]): RetouchFace[] {
  return faces.map((face) => {
    const best = nearestFace(face, analysed);
    return best?.parsedTeeth !== undefined
      ? { ...face, eyes: best.eyes ?? [], mouth: best.mouth ?? null, parsedTeeth: best.parsedTeeth }
      : face;
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

/** The backend's names of the models this panel downloads (`ai_faces`). */
const FACE_MODELS = 'Face models';
const EYES_TEETH_MODEL = 'Eye and teeth model';

/** In place of a model's download button while the download the user
 * clicked runs: the percent of that model's Activity entry (all its files
 * together), else "Preparing" (the request starting, or the model loading
 * after the download). */
function ModelDownloadStatus({ model }: { model: string }) {
  const { t } = useTranslation();
  const percent = useActivityStore((state) => {
    const id = modelDownloadId(model);
    const progress = state.entries.find((e) => e.id === id && e.status === 'running')?.progress;
    return progress && progress.total > 0 ? Math.round((progress.done / progress.total) * 100) : null;
  });
  return (
    <p role="status">
      {percent === null
        ? t('studio.faceRetouch.download.preparing')
        : t('studio.faceRetouch.download.progress', { percent })}
    </p>
  );
}

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
    /** The face models' download failed (offline): the prompt stays, with
     * the reason, so it can be tried again in place. */
    downloadFailed?: boolean;
    /** A retouch with the parser's outlines was found again without the
     * parser installed: it stays as stored until the parser is there. */
    eyesTeethMissing?: boolean;
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

  // The face models' download: for the photo and geometry it was clicked
  // on, taken when the request goes out (PRIVACY.md).
  const [download, setDownload] = useState<string | null>(null);
  const downloadFaces = download === key;
  // Faces stored before Eyes and Teeth have no eyes or mouth. Their Eyes
  // and Teeth wait until the user asks to find them (the face models run
  // again, no download); the eyes and mouth are then stored with the faces
  // in one change, so no value is ever saved that an export cannot apply.
  const needsEyes = Boolean(current?.faces.some((f) => f.eyes === undefined));
  const [eyesAsked, setEyesAsked] = useState<string | null>(null);
  const findEyes = needsEyes && eyesAsked === key;
  // A retouch with the parser's outlines is found again with them (only a
  // parser removed from the model folder meanwhile leaves Face Mesh's).
  const storedParsed = Boolean(stored && eyesTeethParsed(stored.faces));
  // The parser's download is allowed for the photo and geometry it was
  // clicked on, once: taken when the request goes out (PRIVACY.md); a failed
  // one offers the button again.
  const [eyesDownload, setEyesDownload] = useState<string | null>(null);
  const downloadNow = eyesDownload === key;
  // A download the user clicked runs (for this photo and geometry): its
  // progress replaces the button until the request that carries it settles
  // (the faces' search for the face models or a stored parser retouch, the
  // parser's own run otherwise; the other one may answer from its cache).
  const [fetching, setFetching] = useState<string | null>(null);
  const isFetching = fetching === key;
  // Which download that is: a failure of the face models' download asks
  // for them again, one of the parser's for the parser.
  const fetchingFaces = useRef(false);
  useEffect(() => {
    if (!path || !ready || (current && !findEyes)) return;
    let live = true;
    const reparse = storedParsed && !current;
    if (reparse && downloadNow) setEyesDownload(null);
    if (downloadFaces) setDownload(null);
    (reparse ? analyseEyesTeeth(path, geometry, downloadNow, downloadFaces) : analyse(path, geometry, downloadFaces))
      .then((analysis) => {
        if (!live) return;
        if (reparse || fetchingFaces.current) setFetching(null);
        if (reparse && analysis.eyesTeethMissing) {
          setFound({ key: `${path}\n${geometry}`, eyesTeethMissing: true });
          return;
        }
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
        // A failed search for eyes and teeth can be asked again, a failed
        // parser download too.
        setEyesAsked(null);
        if (reparse || fetchingFaces.current) setFetching(null);
        const code = errorCode(error);
        // A failed face model download asks again in place; with the face
        // models there, a failed parser download is the parser's.
        const facesFailed = code === 'download_failed' && (!reparse || fetchingFaces.current);
        setFound({
          key: `${path}\n${geometry}`,
          error: aiErrorMessage(error),
          modelsMissing: code === 'models_missing' || facesFailed,
          downloadFailed: code === 'download_failed',
          ...(reparse && code !== 'models_missing' && !facesFailed && { eyesTeethMissing: true })
        });
      });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path, ready, geometry, Boolean(current), findEyes, downloadFaces, storedParsed, downloadNow]);

  const analysis = found?.key === key ? found.analysis : undefined;
  // New Eyes and Teeth use the eye and teeth parser: a retouch it has not
  // looked at and without Eyes or Teeth values waits for its outlines (an
  // edit with values keeps Face Mesh's and renders as before). They are
  // stored with the first Eyes or Teeth value, so no value is saved that
  // an export renders otherwise.
  const wantsParse = current
    ? !needsEyes && !eyesTeethParsed(current.faces) && !eyesTeethSet(current)
    : Boolean(analysis && analysis.faces.length > 0 && !eyesTeethParsed(analysis.faces));
  const [parse, setParse] = useState<{ key: string; analysis?: Analysis; error?: string } | null>(null);
  useEffect(() => {
    if (!path || !ready || !wantsParse) return;
    let live = true;
    const at = `${path}\n${geometry}`;
    if (downloadNow) setEyesDownload(null);
    analyseEyesTeeth(path, geometry, downloadNow)
      .then((analysis) => live && setParse({ key: at, analysis }))
      .catch((error) => live && setParse({ key: at, error: aiErrorMessage(error) }))
      .finally(() => live && !fetchingFaces.current && setFetching(null));
    return () => {
      live = false;
    };
  }, [path, ready, geometry, wantsParse, downloadNow]);
  const parsedFaces =
    parse?.key === key && parse.analysis && !parse.analysis.eyesTeethMissing ? parse.analysis.faces : undefined;
  const parseMissing = wantsParse && parse?.key === key && Boolean(parse.analysis?.eyesTeethMissing);
  const parseError = wantsParse && parse?.key === key ? parse.error : undefined;
  const parseWaiting = wantsParse && !parsedFaces;
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
    update((before) => {
      const r =
        parsedFaces &&
        (EYES_TEETH_KEYS as readonly string[]).includes(key) &&
        !eyesTeethParsed(before.faces) &&
        !eyesTeethSet(before)
          ? { ...before, faces: withParsedEyesTeeth(before.faces, parsedFaces) }
          : before;
      return face
        ? {
            ...r,
            faces: r.faces.map((f) => (f.id === face.id ? { ...f, settings: { ...f.settings, [key]: value } } : f))
          }
        : { ...r, settings: { ...r.settings, [key]: value } };
    });
  const valueOf = (key: SliderKey, initial: number) => face?.settings[key] ?? settings[key] ?? initial;
  const get = (key: SliderKey) => face?.settings[key] ?? settings[key];
  const [eyeSide, setEyeSide] = useState<EyeSide>('both');
  // Faces found before Eyes and Teeth: their sliders wait for the eyes and
  // mouth (all faces when none is chosen).
  const eyesMissing = Boolean(current) && (face ? face.eyes === undefined : needsEyes);
  const eyesTeethWait = eyesMissing || parseWaiting;
  const reparseMissing = found?.key === key && Boolean(found.eyesTeethMissing);
  const busy = !current && !analysis && !(found?.key === key && (found.error || found.eyesTeethMissing));
  const failure = found?.key === key ? found.error : undefined;
  const modelsMissing = found?.key === key && Boolean(found.modelsMissing);
  const downloadFailure = found?.key === key && found.downloadFailed ? found.error : undefined;
  const error = modelsMissing || reparseMissing ? undefined : failure;
  const downloadEyesTeeth = () => {
    fetchingFaces.current = false;
    setEyesDownload(key);
    setFetching(key);
  };

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
      {reparseMissing && (
        <div className="studio-hint" role="note">
          {failure ? <p role="alert">{failure}</p> : <p>{t('studio.faceRetouch.eyes.modelMissing')}</p>}
          {isFetching ? (
            <ModelDownloadStatus model={EYES_TEETH_MODEL} />
          ) : (
            <button type="button" className="studio-text-button" onClick={downloadEyesTeeth}>
              {t('studio.faceRetouch.eyes.downloadModel')}
            </button>
          )}
        </div>
      )}
      {modelsMissing && (
        <div className="studio-hint" role="note">
          {downloadFailure ? <p role="alert">{downloadFailure}</p> : <p>{t('studio.faceRetouch.modelsMissing')}</p>}
          {isFetching ? (
            <ModelDownloadStatus model={FACE_MODELS} />
          ) : (
            <button
              type="button"
              className="studio-text-button"
              onClick={() => {
                fetchingFaces.current = true;
                setDownload(key);
                setFetching(key);
              }}
            >
              {t('studio.faceRetouch.downloadModels')}
            </button>
          )}
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
          {!eyesMissing && (parseMissing || parseError) && (
            <div className="studio-hint" role="note">
              {parseError ? <p role="alert">{parseError}</p> : <p>{t('studio.faceRetouch.eyes.modelMissing')}</p>}
              {isFetching ? (
                <ModelDownloadStatus model={EYES_TEETH_MODEL} />
              ) : (
                <button type="button" className="studio-text-button" onClick={downloadEyesTeeth}>
                  {t('studio.faceRetouch.eyes.downloadModel')}
                </button>
              )}
            </div>
          )}
          {!eyesMissing && parseWaiting && !parseMissing && !parseError && (
            <p className="studio-hint" role="status">
              {t('studio.faceRetouch.eyes.finding')}
            </p>
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
              disabled={eyesTeethWait}
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
              disabled={eyesTeethWait}
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
