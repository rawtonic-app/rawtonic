/**
 * Lightroom's preset Amount: a preset applied at less than full strength
 * blends each of its settings with the photo's settings before it was
 * applied. Numbers move part of the way; nested settings (HSL, grading)
 * blend per value; other settings (curves, masks, profiles, switches, and
 * the `…Version` keys that say how a recipe reads its values) take the
 * preset's value from half strength on. Curves go whole, with the version
 * inside them that says how they render (`Curves.version`).
 */
export function blendPreset(
  base: Record<string, unknown>,
  preset: Record<string, unknown>,
  amount: number,
): Record<string, unknown> {
  const a = Math.min(1, Math.max(0, amount));
  const blend = (from: unknown, to: unknown): unknown => {
    if (typeof to === 'number' && typeof from === 'number') return from + a * (to - from);
    if (to && typeof to === 'object' && !Array.isArray(to) && from && typeof from === 'object' && !Array.isArray(from)) {
      const out: Record<string, unknown> = { ...(from as Record<string, unknown>) };
      for (const [key, value] of Object.entries(to as Record<string, unknown>)) {
        out[key] = blend((from as Record<string, unknown>)[key], value);
      }
      return out;
    }
    return a >= 0.5 ? to : from;
  };
  return Object.fromEntries(
    Object.entries(preset).map(([key, value]) => [
      key,
      key.endsWith('Version') || key === 'curves' ? (a >= 0.5 ? value : base[key]) : blend(base[key], value),
    ]),
  );
}
