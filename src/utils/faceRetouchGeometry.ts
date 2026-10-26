import type { Adjustments, FaceRetouch } from "./adjustments";
import { GEOMETRY_KEYS, ORIENTATION_KEYS } from "./geometryKeys";

/** Geometry keys that correct colour, not where a pixel is (the engine's
 * `face_retouch::COLOUR_ONLY_KEYS`): faces stay valid when they change. */
const COLOUR_ONLY_KEYS: ReadonlySet<string> = new Set([
  'lensVignetteAmount',
  'lensVignetteEnabled',
  'lensTcaAmount',
  'lensTcaEnabled',
  'autoChromaticAberration',
  'autoCaVersion',
  'fringeVersion',
]);

/** The source frame the faces are found in (ADR 0020); stamps from before
 * the key have none. */
const SOURCE_FRAME_KEY = ['sourceFrameVersion'] as const satisfies ReadonlyArray<keyof Adjustments>;

/** What decides the mask frame the faces are found in: the source frame,
 * orientation, flips, straightening and the geometry keys of
 * crates/engine/src/cache_utils.rs (`GEOMETRY_KEYS`) that move pixels; the
 * same set as the engine's `face_retouch::frame_key`, so both agree when a
 * geometry change makes the stored faces stale. */
export const FACE_FRAME_KEYS = [
  ...SOURCE_FRAME_KEY,
  ...ORIENTATION_KEYS,
  ...GEOMETRY_KEYS.filter((key) => !COLOUR_ONLY_KEYS.has(key)),
] as const;

/** The keys of a frame without the engine's `2:` prefix (faces found
 * before 2026-10-20): every geometry key, colour corrections included. */
const FIRST_FACE_FRAME_KEYS = [...SOURCE_FRAME_KEY, ...ORIENTATION_KEYS, ...GEOMETRY_KEYS] as const;

/** The geometry the faces are found in (the mask frame, without the crop). */
export function faceRetouchGeometry(
  adjustments: Adjustments,
  keys: ReadonlyArray<(typeof FIRST_FACE_FRAME_KEYS)[number]> = FACE_FRAME_KEYS,
) {
  return Object.fromEntries(
    keys.filter((key) => adjustments[key] !== undefined && adjustments[key] !== null).map((key) => [
      key,
      adjustments[key],
    ]),
  );
}

/** Whether `retouch`'s faces belong to the geometry of `adjustments`, by the
 * rule of its frame (as the engine's `face_retouch::frame_matches`). */
export function faceRetouchIsCurrent(retouch: FaceRetouch | null | undefined, adjustments: Adjustments) {
  if (!retouch) return false;
  if (retouch.geometry === frameGeometry(adjustments, retouch.frame)) return true;
  // The same keys and values in another key order (a frame update writes
  // the geometry in the engine's order, `source_frame::migrate`).
  try {
    return canonical(JSON.parse(retouch.geometry)) === canonical(JSON.parse(frameGeometry(adjustments, retouch.frame)));
  } catch {
    return false;
  }
}

/** JSON with every object's keys sorted. */
function canonical(value: unknown): string {
  const sorted = (v: unknown): unknown =>
    Array.isArray(v)
      ? v.map(sorted)
      : v !== null && typeof v === "object"
        ? Object.fromEntries(Object.keys(v as object).sort().map((k) => [k, sorted((v as Record<string, unknown>)[k])]))
        : v;
  return JSON.stringify(sorted(value));
}

/** The panel's `geometry` for faces of `frame`, by that frame's rule. */
export function frameGeometry(adjustments: Adjustments, frame: string | undefined) {
  const keys = frame?.startsWith("2:") ? FACE_FRAME_KEYS : FIRST_FACE_FRAME_KEYS;
  return JSON.stringify(faceRetouchGeometry(adjustments, keys));
}

/** `moved` (a recipe `before` moved to another frame of the RAW,
 * `source_frame::migrate`) with its face retouch's panel geometry following
 * the stamp the backend moved, where the faces were current before. */
export function withFaceRetouchFollowing<T extends Adjustments>(before: Adjustments, moved: T): T {
  const retouch = moved.faceRetouch;
  if (!retouch || !faceRetouchIsCurrent(before.faceRetouch, before)) return moved;
  return { ...moved, faceRetouch: { ...retouch, geometry: frameGeometry(moved, retouch.frame) } };
}
