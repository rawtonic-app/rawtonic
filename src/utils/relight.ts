export const MAX_RELIGHT_LIGHTS = 4;
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
export interface RelightLight {
  id: string;
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
  pattern: (typeof RELIGHT_PATTERNS)[number];
  invert: boolean;
}
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
  data: string;
  geometry: Record<string, unknown>;
  version?: number;
  faces?: number[][][];
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

export type RelightHandle = "source" | "move" | "rotate" | "scale" | "radius";
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
        (light.patternRotation ?? light.rotation) +
          ((Math.atan2(b.y, b.x) - Math.atan2(a.y, a.x)) * 180) / Math.PI,
      ),
    };
  const ratio = Math.hypot(b.x, b.y) / Math.max(1, Math.hypot(a.x, a.y));
  return handle === "radius"
    ? { size: clampRelight(light.size * ratio, 5, 200) }
    : { scale: clampRelight(light.scale * ratio, 5, 300) };
}
