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
  sent: { has(name: string): boolean },
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

export const RELIGHT_PRESETS = [
  "rembrandt",
  "butterfly",
  "split",
  "window",
  "blinds",
] as const;
export type RelightPreset = (typeof RELIGHT_PRESETS)[number];

/** A starting lighting setup: one key light placed for a classic portrait or
 * window look. The source sits on the side the light comes from. */
export function relightPresetLight(preset: RelightPreset): RelightLight {
  const settings: Record<RelightPreset, Partial<RelightLight>> = {
    rembrandt: { rotation: 135, elevation: 40, intensity: 35, temperature: 4500, softness: 40 },
    butterfly: { rotation: 180, elevation: 60, intensity: 30, temperature: 5500, softness: 45 },
    split: { rotation: 90, elevation: 10, intensity: 40, temperature: 5000, diffusion: 10 },
    window: {
      rotation: 120,
      elevation: 30,
      intensity: 45,
      temperature: 5500,
      pattern: "window",
      scale: 120,
      softness: 45,
    },
    blinds: {
      rotation: 110,
      elevation: 15,
      intensity: 50,
      temperature: 2700,
      pattern: "blinds",
      scale: 18,
      softness: 20,
    },
  };
  const light = { ...createRelightLight(), ...settings[preset] };
  light.color = kelvinToHex(light.temperature!);
  light.patternRotation = light.rotation;
  // Same placement rule as dragging a sun's source (35% of the short side).
  delete light.sourceX;
  delete light.sourceY;
  const source = relightSource(light, 1, 1);
  return { ...light, sourceX: source.x, sourceY: source.y };
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

/** `elevation`: the sun's source handle dragged with Alt (up = higher). */
export type RelightHandle =
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
): Partial<RelightLight> {
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
    const vx = (sourceX - light.x) * width,
      vy = (sourceY - light.y) * height;
    return {
      sourceX,
      sourceY,
      ...(light.type === "sun" && Math.hypot(vx, vy) > 1
        ? { rotation: relightAngle((Math.atan2(-vx, vy) * 180) / Math.PI) }
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
