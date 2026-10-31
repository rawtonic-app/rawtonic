export const MAX_RELIGHT_LIGHTS = 4;
/** Built-in patterns, in GPU order (crates/engine/src/relight.rs `PATTERNS`). */
export const RELIGHT_PATTERNS = [
  "none",
  "blinds",
  "window",
  "strip",
  "foliage",
  "grid",
  "palm",
  "branches",
  "checkerboard",
  "dots",
  "horizontal",
  "diagonal",
  "weave",
  "noise",
  "crosshatch",
] as const;
/** Every pattern id the renderer knows: the built-ins and an imported image. */
export const RELIGHT_PATTERN_IDS = [...RELIGHT_PATTERNS, "custom"] as const;
export type RelightPattern = (typeof RELIGHT_PATTERN_IDS)[number];

/** Scene values when the recipe has none (the renderer's defaults). */
export const RELIGHT_SCENE_DEFAULTS = { originalLight: 100, depthAmount: 60 };
/** Scene values the first light starts with, unless already set. */
export const RELIGHT_FIRST_LIGHT_SCENE = { originalLight: 60, depthAmount: 100 };

export interface RelightCustomPattern {
  /** Names the immutable bitmap; the renderer caches it by this id. */
  id: string;
  /** Grayscale PNG data URL; white transmits light. Absent once sent. */
  data: string | null;
  name?: string;
}

export interface RelightLight {
  id: string;
  /** Stable display number ("Light 2" stays Light 2 when Light 1 goes). */
  number?: number;
  enabled: boolean;
  type: "sun" | "spot";
  color: string;
  intensity: number;
  elevation: number;
  x: number;
  y: number;
  size: number;
  rotation: number;
  /** Independent source and gobo controls; absent in legacy recipes. */
  sourceX?: number;
  sourceY?: number;
  patternRotation?: number;
  distance?: number;
  scale: number;
  softness: number;
  diffusion: number;
  shadows: number;
  pattern: RelightPattern;
  invert: boolean;
  /** Set when the color came from the Kelvin control. */
  temperature?: number;
  customPattern?: RelightCustomPattern;
  /** Mask layer the light is limited to; absent = the whole photo. */
  maskId?: string;
  /**
   * The slider ranges the light's values mean (crates/engine/src/relight.rs
   * `intensity_gain`); absent = 1, the ranges before 2026-10-31.
   */
  rangeVersion?: number;
}

/** Ranges new lights get: Intensity 100 % is a clearly visible key light. */
export const RELIGHT_RANGE_VERSION = 2;
// Intensity's gain in range 2 (crates/engine/src/relight.rs RANGE_2_*).
const RANGE_2_MAX_GAIN = 4;
const RANGE_2_EXPONENT = 1.5;

/**
 * A light of the first ranges moved to the current ones, as bright as it
 * was (same gain, within range 2's top). Softness, Diffusion and Face
 * reflections then act as range 2 defines.
 */
export function relightWithCurrentRanges(light: RelightLight): RelightLight {
  if ((light.rangeVersion ?? 1) >= RELIGHT_RANGE_VERSION) return light;
  const gain = (light.intensity / 100) * 3;
  const intensity = 100 * Math.min(1, gain / RANGE_2_MAX_GAIN) ** (1 / RANGE_2_EXPONENT);
  return { ...light, intensity: Math.round(intensity * 10) / 10, rangeVersion: RELIGHT_RANGE_VERSION };
}
// Absent fields render with these values (crates/engine/src/relight.rs
// `DEFAULT_*`; parity tested in relight.test.ts).
export const createRelightLight = (): RelightLight => ({
  id: crypto.randomUUID(),
  enabled: true,
  type: "sun",
  color: "#fff1db",
  intensity: 25,
  elevation: 45,
  x: 50,
  y: 50,
  size: 60,
  rotation: 135,
  sourceX: 20,
  sourceY: 20,
  patternRotation: 0,
  distance: 60,
  scale: 50,
  softness: 30,
  diffusion: 25,
  shadows: 100,
  pattern: "none",
  invert: false,
  rangeVersion: RELIGHT_RANGE_VERSION,
});

export interface RelightDepth {
  /** 16-bit PNG data URL. Null only in a preview request after it was sent. */
  data: string | null;
  geometry: Record<string, unknown>;
  version?: number;
  faces?: number[][][] | null;
  camera?: {
    fx: number; fy: number; cx: number; cy: number;
    inverseMin: number; inverseMax: number; reference: number;
  };
  /** Names this analysis so the renderer receives its bitmap once. */
  cacheKey?: string;
  /**
   * Version 5: the subject's people normals and matte (16-bit RGBA PNG
   * data URL; crates/engine/src/relight_source.rs). Null only in a preview
   * request after it was sent.
   */
  subject?: string | null;
  /** Version 5: the photo's own lighting on its people (nine coefficients). */
  sourceLight?: number[];
  /** The subject matte's centre, x/y normalized (analyses since 2026-10-31). */
  subjectCenter?: [number, number];
}

/** The pattern's own rotation; legacy lights turn it with the light. */
export const relightPatternRotation = (light: RelightLight) =>
  light.patternRotation ?? light.rotation;

/** The lowest display number no other light uses. */
export function nextRelightNumber(lights: RelightLight[]): number {
  const used = new Set(lights.map((l, i) => l.number ?? i + 1));
  let n = 1;
  while (used.has(n)) n++;
  return n;
}

const hashString = (text: string) => {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 0x01000193);
  return (h >>> 0).toString(16);
};

/**
 * A loaded recipe in the current form, rendering identically: legacy source
 * position, pattern rotation and distance become explicit, lights get stable
 * numbers, and depth gets the name the editor sends it by once.
 */
export function normalizeRelight(
  lights: RelightLight[] | undefined,
  depth: RelightDepth | undefined,
): { relightLights?: RelightLight[]; relightDepth?: RelightDepth } {
  const migrated = lights?.map((light, i) => {
    const source = relightSource(light, 1, 1);
    return {
      ...light,
      number: light.number ?? i + 1,
      sourceX: light.sourceX ?? source.x,
      sourceY: light.sourceY ?? source.y,
      patternRotation: relightPatternRotation(light),
      distance: light.distance ?? 60,
    };
  });
  return {
    relightLights: migrated,
    relightDepth:
      depth && !depth.cacheKey && depth.data
        ? { ...depth, cacheKey: `${depth.data.length}-${hashString(depth.data)}` }
        : depth,
  };
}

/**
 * Drops Relight bitmaps the renderer already holds from a preview request
 * (crates/engine/src/adjustment_utils.rs restores them by name). `sent`
 * holds the names already sent; returns the names this request sends.
 */
export function stripSentRelightData(
  payload: { relightLights?: RelightLight[]; relightDepth?: RelightDepth },
  sent: Set<string>,
): string[] {
  const sending: string[] = [];
  const depth = payload.relightDepth;
  if (depth?.cacheKey && depth.data) {
    const name = `relight-depth:${depth.cacheKey}`;
    if (sent.has(name)) {
      payload.relightDepth = {
        ...depth,
        data: null,
        faces: null,
        ...(depth.subject ? { subject: null } : {}),
      };
    } else sending.push(name);
  }
  payload.relightLights = payload.relightLights?.map((light) => {
    const pattern = light.customPattern;
    if (!pattern?.data) return light;
    const name = `relight-pattern:${pattern.id}`;
    if (!sent.has(name)) {
      sending.push(name);
      return light;
    }
    return { ...light, customPattern: { ...pattern, data: null } };
  });
  return sending;
}

/** sRGB hex of a blackbody-like light at `kelvin` (1000–40000 K), from a
 * smooth fit of the Planckian locus normalised to its brightest channel. */
export function kelvinToHex(kelvin: number): string {
  const t = Math.min(40000, Math.max(1000, kelvin)) / 100;
  const r = t <= 66 ? 255 : 329.699 * (t - 60) ** -0.1332;
  const g =
    t <= 66 ? 99.4708 * Math.log(t) - 161.1196 : 288.122 * (t - 60) ** -0.0755;
  const b = t >= 66 ? 255 : t <= 19 ? 0 : 138.5177 * Math.log(t - 10) - 305.0448;
  return `#${[r, g, b]
    .map((c) => Math.round(Math.min(255, Math.max(0, c))).toString(16).padStart(2, "0"))
    .join("")}`;
}

export const RELIGHT_TEMPERATURES = [1900, 2700, 3500, 4500, 5500, 6500, 8000];

/** Classic portrait and scene lighting setups, by what they do. */
export const RELIGHT_PRESETS = [
  "side",
  "loop",
  "split",
  "butterfly",
  "edge",
  "window",
  "evening",
  "fill",
  "blinds",
] as const;
export type RelightPreset = (typeof RELIGHT_PRESETS)[number];

/** Where a preset is placed: the largest detected face, else the subject. */
export interface RelightAnchor {
  /** Centre, percent of the developed frame. */
  x: number;
  y: number;
  /** -1 the face turns to the frame's left, 1 to its right, 0 frontal or unknown. */
  turn: number;
}

/**
 * The largest face of an analysis (face mesh landmarks, x/y normalized to
 * the displayed frame): its centre and which way it turns, from the nose
 * tip (landmark 1) against the middle of the face.
 */
export function relightFaceAnchor(depth: RelightDepth | undefined): RelightAnchor | null {
  let best: RelightAnchor | null = null;
  let bestArea = 0;
  for (const face of depth?.faces ?? []) {
    if (face.length < 2) continue;
    const xs = face.map((p) => p[0]);
    const ys = face.map((p) => p[1]);
    const [x0, x1, y0, y1] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
    const area = (x1 - x0) * (y1 - y0);
    if (!(area > bestArea)) continue;
    bestArea = area;
    const offset = (face[1][0] - (x0 + x1) / 2) / Math.max(1e-6, x1 - x0);
    best = {
      x: Math.round((x0 + x1) * 500) / 10,
      y: Math.round((y0 + y1) * 500) / 10,
      turn: Math.abs(offset) < 0.06 ? 0 : Math.sign(offset),
    };
  }
  return best;
}

/** Where presets go: the largest face, else the subject, else null (the
 * frame's centre). */
export function relightPresetAnchor(depth: RelightDepth | undefined): RelightAnchor | null {
  const face = relightFaceAnchor(depth);
  if (face) return face;
  const center = depth?.subjectCenter;
  return center && center.every(Number.isFinite)
    ? { x: Math.round(center[0] * 1000) / 10, y: Math.round(center[1] * 1000) / 10, turn: 0 }
    : null;
}

type PresetSettings = Partial<RelightLight> & {
  /** Rotation for a key from the frame's left; mirrored for the right. */
  rotation: number;
  originalLight: number;
};
// Rotation 90 lights from the frame's left, 180 from above (relightSource).
// Elevation 90 lights from the camera, 5 grazes the surfaces. Intensities
// are in range version 2 (benchmark-results/rl-relight-ux-2026-10-31).
const PRESETS: Record<RelightPreset, PresetSettings> = {
  side: { rotation: 95, elevation: 30, intensity: 55, temperature: 5000, softness: 35, diffusion: 15, originalLight: 50 },
  loop: { rotation: 140, elevation: 50, intensity: 50, temperature: 5000, softness: 45, diffusion: 20, originalLight: 55 },
  split: { rotation: 90, elevation: 8, intensity: 60, temperature: 5000, softness: 20, diffusion: 5, originalLight: 35 },
  butterfly: { rotation: 180, elevation: 65, intensity: 45, temperature: 5500, softness: 45, diffusion: 20, originalLight: 60 },
  edge: { rotation: 100, elevation: 5, intensity: 70, temperature: 6500, softness: 15, diffusion: 0, originalLight: 70 },
  window: {
    rotation: 120, elevation: 30, intensity: 55, temperature: 6000, softness: 45, diffusion: 20,
    pattern: "window", scale: 120, originalLight: 50,
  },
  evening: { rotation: 105, elevation: 12, intensity: 60, temperature: 2700, softness: 35, diffusion: 15, originalLight: 50 },
  fill: { rotation: 180, elevation: 80, intensity: 35, temperature: 5500, softness: 80, diffusion: 60, originalLight: 100 },
  blinds: {
    rotation: 110, elevation: 15, intensity: 60, temperature: 2700, softness: 20, diffusion: 15,
    pattern: "blinds", scale: 18, originalLight: 50,
  },
};

/**
 * A starting lighting setup: one key light and the share of the photo's own
 * light that stays. It is placed on `anchor` (pattern on the face, source on
 * the side the light comes from); the key comes from the side a turned face
 * looks to, else from the frame's left.
 */
export function relightPresetLight(
  preset: RelightPreset,
  anchor: RelightAnchor | null = null,
): { light: RelightLight; originalLight: number } {
  const { originalLight, ...settings } = PRESETS[preset];
  const light: RelightLight = { ...createRelightLight(), ...settings };
  if (anchor?.turn === 1) light.rotation = -light.rotation;
  light.color = kelvinToHex(light.temperature!);
  light.patternRotation = light.rotation;
  light.x = anchor?.x ?? 50;
  light.y = anchor?.y ?? 50;
  // Same placement rule as dragging a sun's source (35% of the short side).
  delete light.sourceX;
  delete light.sourceY;
  const source = relightSource(light, 1, 1);
  return { light: { ...light, sourceX: source.x, sourceY: source.y }, originalLight };
}

/**
 * The direction ball: a sun's direction as a point in the unit disc, as seen
 * from the camera. The centre is light from the camera (elevation 90), the
 * rim light grazing the photo (elevation 0), the point lies on the side the
 * light comes from (y down, like the frame).
 */
export function relightBallPoint(light: RelightLight): RelightPoint {
  const a = (light.rotation * Math.PI) / 180;
  const r = Math.cos((light.elevation * Math.PI) / 180);
  return { x: -Math.sin(a) * r, y: Math.cos(a) * r };
}

/** Rotation and elevation for a point of the direction ball (clamped to it). */
export function relightBallPatch(
  light: RelightLight,
  point: RelightPoint,
): Pick<RelightLight, "rotation" | "elevation"> {
  const r = Math.min(1, Math.hypot(point.x, point.y));
  const patch = {
    // At the centre the direction is undefined: keep the light's.
    rotation:
      r > 1e-3
        ? // Light from straight above reads 180, as in the presets.
          Math.round(relightAngle((Math.atan2(-point.x, point.y) * 180) / Math.PI) * 10) / 10 + 0
        : light.rotation,
    elevation: Math.round(clampRelight((Math.acos(r) * 180) / Math.PI, 5, 90) * 10) / 10,
  };
  if (patch.rotation === -180) patch.rotation = 180;
  return patch;
}

/** Why a control cannot act on this light now (an i18n key), or null. */
export type RelightReason =
  | "needsAnalysis"
  | "needsFace"
  | "frontalLight"
  | "spotDirection"
  | "needsPatternOrSubject";
export function relightControlReason(
  control: "direction" | "elevation" | "diffusion" | "skinReflections" | "depthAmount" | "shadows",
  light: RelightLight | undefined,
  depth: { valid: boolean; faces: number; subject?: boolean },
): RelightReason | null {
  if (!light) return null;
  const patterned = light.pattern !== "none";
  switch (control) {
    case "direction":
      if (light.type === "spot") return "spotDirection";
      // Without surfaces a sun only steers its pattern's projection.
      return depth.valid || patterned ? null : "needsAnalysis";
    case "elevation":
    case "depthAmount":
      return depth.valid ? null : "needsAnalysis";
    case "skinReflections":
      return !depth.valid ? "needsAnalysis" : depth.faces ? null : "needsFace";
    case "diffusion":
      // In the first ranges Diffusion widens the terminator and lifts a
      // pattern's shadows: a frontal light without a pattern already
      // reaches every surface facing the camera. From range 2 it mixes the
      // response towards an even share at any direction.
      return (light.rangeVersion ?? 1) < 2 &&
        depth.valid &&
        !patterned &&
        light.type === "sun" &&
        light.elevation >= 70
        ? "frontalLight"
        : null;
    case "shadows":
      // A pattern's shadows, a face's own, or the subject's on its background.
      return patterned || (depth.valid && (depth.faces || depth.subject))
        ? null
        : "needsPatternOrSubject";
  }
}

// Rust serializes object keys in sorted order. Geometry equality must not
// depend on JavaScript insertion order (notably guidedPerspective).
export function matchesRelightGeometry(
  adjustments: Record<string, unknown>,
  geometry: Record<string, unknown>,
): boolean {
  if (adjustments.crop != null && !("crop" in geometry)) return false;
  if (Number(adjustments.rotation ?? 0) !== 0 && !("rotation" in geometry))
    return false;
  const canonical = (value: unknown): unknown => {
    if (Array.isArray(value)) return value.map(canonical);
    if (value !== null && typeof value === "object")
      return Object.fromEntries(
        Object.entries(value)
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([key, child]) => [key, canonical(child)]),
      );
    return value ?? null;
  };
  return Object.entries(geometry).every(
    ([key, value]) =>
      JSON.stringify(canonical(adjustments[key])) ===
      JSON.stringify(canonical(value)),
  );
}

/** `elevation`: the sun's source handle dragged with Alt (up = higher);
 * `direction`: the direction ball on the photo. */
export type RelightHandle =
  | "direction"
  | "source"
  | "move"
  | "rotate"
  | "scale"
  | "radius"
  | "elevation";
export type RelightPoint = { x: number; y: number };
const clampRelight = (n: number, lo: number, hi: number) =>
  Math.max(lo, Math.min(hi, n));
export const relightAngle = (angle: number) =>
  ((((angle + 180) % 360) + 360) % 360) - 180;

/** Percent coordinates in the developed frame; isotropic distances use its short side. */
export function relightSource(
  light: RelightLight,
  width: number,
  height: number,
): RelightPoint {
  const a = (light.rotation * Math.PI) / 180;
  const reach = light.type === "sun" ? Math.min(width, height) * 35 : 0;
  return {
    x: light.sourceX ?? light.x - (Math.sin(a) * reach) / width,
    y: light.sourceY ?? light.y + (Math.cos(a) * reach) / height,
  };
}

export function relightDragPatch(
  light: RelightLight,
  handle: RelightHandle,
  start: RelightPoint,
  now: RelightPoint,
  width: number,
  height: number,
  /** The direction ball's centre (percent) and radius (pixels). */
  ring?: { x: number; y: number; radius: number },
): Partial<RelightLight> {
  if (handle === "direction")
    return ring
      ? relightBallPatch(light, {
          x: ((now.x - ring.x) * width) / 100 / ring.radius,
          y: ((now.y - ring.y) * height) / 100 / ring.radius,
        })
      : {};
  const dx = now.x - start.x,
    dy = now.y - start.y;
  const source = relightSource(light, width, height);
  if (handle === "elevation")
    // One degree per percent of the frame's height.
    return {
      elevation: clampRelight(light.elevation - (dy * height) / Math.min(width, height), 5, 90),
    };
  if (handle === "source") {
    const sourceX = clampRelight(source.x + dx, -100, 200);
    const sourceY = clampRelight(source.y + dy, -100, 200);
    const angle = (x: number, y: number) => {
      const vx = (x - light.x) * width,
        vy = (y - light.y) * height;
      return Math.hypot(vx, vy) > 1 ? (Math.atan2(-vx, vy) * 180) / Math.PI : null;
    };
    // The sun turns by the angle its source moved around the centre: a
    // direction set with the ball stays until the source is moved.
    const from = angle(source.x, source.y),
      to = angle(sourceX, sourceY);
    return {
      sourceX,
      sourceY,
      ...(light.type === "sun" && from !== null && to !== null
        ? { rotation: relightAngle(light.rotation + to - from) }
        : {}),
    };
  }
  if (handle === "move") {
    const x = clampRelight(light.x + dx, -100, 200),
      y = clampRelight(light.y + dy, -100, 200);
    return { x, y, sourceX: source.x, sourceY: source.y };
  }
  const center = handle === "radius" ? source : light;
  const vector = (p: RelightPoint) => ({
    x: ((p.x - center.x) * width) / 100,
    y: ((p.y - center.y) * height) / 100,
  });
  const a = vector(start),
    b = vector(now);
  if (handle === "rotate")
    return {
      patternRotation: relightAngle(
        relightPatternRotation(light) +
          ((Math.atan2(b.y, b.x) - Math.atan2(a.y, a.x)) * 180) / Math.PI,
      ),
    };
  const ratio = Math.hypot(b.x, b.y) / Math.max(1, Math.hypot(a.x, a.y));
  return handle === "radius"
    ? { size: clampRelight(light.size * ratio, 5, 200) }
    : { scale: clampRelight(light.scale * ratio, 5, 300) };
}
