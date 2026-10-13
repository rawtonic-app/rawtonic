export const MAX_RELIGHT_LIGHTS = 4;
export const RELIGHT_PATTERNS = [
  "none",
  "blinds",
  "window",
  "strip",
  "foliage",
  "grid",
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
  intensity: 50,
  elevation: 45,
  x: 50,
  y: 50,
  size: 60,
  rotation: -30,
  scale: 50,
  softness: 30,
  diffusion: 25,
  shadows: 35,
  pattern: "blinds",
  invert: false,
});

export interface RelightDepth {
  data: string;
  geometry: Record<string, unknown>;
}

// Rust serializes object keys in sorted order. Geometry equality must not
// depend on JavaScript insertion order (notably guidedPerspective).
export function matchesRelightGeometry(
  adjustments: Record<string, unknown>,
  geometry: Record<string, unknown>,
): boolean {
  const canonical = (value: unknown): unknown => {
    if (Array.isArray(value)) return value.map(canonical);
    if (value !== null && typeof value === "object")
      return Object.fromEntries(
        Object.entries(value).sort(([a], [b]) => a.localeCompare(b))
          .map(([key, child]) => [key, canonical(child)]),
      );
    return value ?? null;
  };
  return Object.entries(geometry).every(([key, value]) =>
    JSON.stringify(canonical(adjustments[key])) === JSON.stringify(canonical(value)),
  );
}
