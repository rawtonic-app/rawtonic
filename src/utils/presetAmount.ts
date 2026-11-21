import { INITIAL_MASK_ADJUSTMENTS, type MaskContainer } from './adjustments';

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

/** The photo's layers (`current`) with a preset's layers (`preset`, by id,
 * as applying it joined them) at `amount` (0 … 1): each one's settings
 * blended from neutral to the preset's, as Lightroom's Amount scales an
 * adaptive preset's masks. At 0 a shown one is hidden (nothing to render or
 * detect for it) and named in `hidden`; above 0 only those it hid are shown
 * again (a layer the photographer hid stays hidden). Its detection and the
 * photo's other layers stay. */
export function amountLayers(
  current: MaskContainer[] | undefined,
  preset: MaskContainer[],
  amount: number,
  hiddenBefore: ReadonlySet<string> = new Set()
): { layers: MaskContainer[]; hidden: Set<string> } {
  const hidden = new Set<string>();
  const layers = (current ?? []).map((layer) => {
    const own = preset.find((p) => p.id === layer.id);
    if (!own) return layer;
    const scaled = blendPreset(
      INITIAL_MASK_ADJUSTMENTS as unknown as Record<string, unknown>,
      (own.adjustments ?? {}) as unknown as Record<string, unknown>,
      amount
    );
    let visible = layer.visible;
    if (amount <= 0 && (visible || hiddenBefore.has(layer.id))) {
      visible = false;
      hidden.add(layer.id);
    } else if (amount > 0 && hiddenBefore.has(layer.id)) visible = true;
    return { ...layer, visible, adjustments: { ...layer.adjustments, ...scaled } as MaskContainer['adjustments'] };
  });
  return { layers, hidden };
}
