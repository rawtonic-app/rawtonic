import { INITIAL_MASK_ADJUSTMENTS, type MaskContainer } from './adjustments';
import { joinedLayers } from './settingsClipboard';

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
  amount: number
): Record<string, unknown> {
  const a = Math.min(1, Math.max(0, amount));
  const blend = (from: unknown, to: unknown): unknown => {
    if (typeof to === 'number' && typeof from === 'number') return from + a * (to - from);
    if (
      to &&
      typeof to === 'object' &&
      !Array.isArray(to) &&
      from &&
      typeof from === 'object' &&
      !Array.isArray(from)
    ) {
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
      key.endsWith('Version') || key === 'curves' ? (a >= 0.5 ? value : base[key]) : blend(base[key], value)
    ])
  );
}

/** A preset's layers (`preset`) at `amount` (0 … 1) on a photo: the
 * photo's layers before it (`base`) with the preset's joined
 * (`joinedLayers`), each preset layer as the photo shows it now (`current`:
 * its detection kept) and its settings blended from neutral to the
 * preset's, as Lightroom's Amount scales an adaptive preset's masks. */
export function amountLayers(
  base: MaskContainer[] | undefined,
  current: MaskContainer[] | undefined,
  preset: MaskContainer[],
  amount: number
): MaskContainer[] {
  const shown = new Map((current ?? []).map((layer) => [layer.id, layer]));
  return joinedLayers(base, preset).map((layer) => {
    const own = preset.find((p) => p.id === layer.id);
    if (!own) return layer;
    const now = shown.get(layer.id) ?? layer;
    const scaled = blendPreset(
      INITIAL_MASK_ADJUSTMENTS as unknown as Record<string, unknown>,
      (own.adjustments ?? {}) as unknown as Record<string, unknown>,
      amount
    );
    return { ...now, adjustments: { ...now.adjustments, ...scaled } as MaskContainer['adjustments'] };
  });
}
