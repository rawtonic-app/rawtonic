// SPDX-License-Identifier: AGPL-3.0-or-later
/**
 * AI selections migrated from a camera TIFF's embedded thumbnail
 * (`DETECT_AGAIN`, set by `recipe_frame::scale_geometry` in
 * crates/engine/src/recipe_frame.rs): edits of these files made before
 * 0.1.0-alpha.17.1 hold Sky, Subject, People and depth range bitmaps at the
 * thumbnail's few hundred pixels. They express what to select, so the
 * editor computes them again on the full frame when the photo opens, all
 * in one History step: one undo brings back the stretched thumbnail
 * bitmaps, which then stay (the mark goes from every step). Their
 * refinements (Grow, Feather, Invert, a depth range's limits) stay; brush
 * strokes are components of their own and were moved with the photo.
 * Exports compute them the same way (`ai_selections::undetected`).
 */
import type { Adjustments, MaskContainer } from './adjustments';
import type { SubMask } from '../components/panel/right/Masks';
import { DETECT_PENDING } from './settingsClipboard';

export const DETECT_AGAIN = 'detectAgain';
/** The frame a migrated bitmap was made in (`mask_generation::BITMAP_FRAME`). */
export const BITMAP_FRAME = 'bitmapFrame';

/** What a waiting selection's request depends on: its kind and settings. */
export type MigratedKind = 'ai-sky' | 'ai-foreground' | 'ai-people' | 'ai-depth';

export interface MigratedSelection {
  layerId: string;
  sub: SubMask;
}

const KINDS: readonly string[] = ['ai-sky', 'ai-foreground', 'ai-people', 'ai-depth'];

const waits = (sub: SubMask) =>
  KINDS.includes(sub.type) && (sub.parameters as Record<string, unknown> | undefined)?.[DETECT_AGAIN] === true;

/** The components of `adjustments` that wait to be computed again. */
export function selectionsToDetectAgain(adjustments: Pick<Adjustments, 'masks'>): MigratedSelection[] {
  return (adjustments.masks ?? []).flatMap((layer: MaskContainer) =>
    layer.subMasks.filter(waits).map((sub: SubMask) => ({ layerId: layer.id, sub }))
  );
}

const mapComponents = (adjustments: Adjustments, change: (sub: SubMask) => SubMask): Adjustments => {
  if (!adjustments.masks?.some((layer: MaskContainer) => layer.subMasks.some(waits))) return adjustments;
  return {
    ...adjustments,
    masks: adjustments.masks.map((layer: MaskContainer) => ({
      ...layer,
      subMasks: layer.subMasks.map((sub: SubMask) => (waits(sub) ? change(sub) : sub))
    }))
  };
};

/** `adjustments` with the components `ids` no longer waiting: they keep
 * their stretched thumbnail bitmap (the state one undo back). */
export function settleMigrated(adjustments: Adjustments, ids: ReadonlySet<string>): Adjustments {
  return mapComponents(adjustments, (sub) => {
    if (!ids.has(sub.id)) return sub;
    const { [DETECT_AGAIN]: _again, ...parameters } = sub.parameters as Record<string, unknown>;
    return { ...sub, parameters } as SubMask;
  });
}

const PEOPLE_CHOICE = (parameters: Record<string, unknown> | undefined) =>
  JSON.stringify([parameters?.parts ?? [], parameters?.person ?? null]);

/**
 * `adjustments` with the selections computed on the full frame
 * (`results`, the detection's parameters by component id, each with the
 * component's parameters it was asked with) put into the components that
 * still wait for them: a People selection whose parts or person changed
 * since is left (its panel asks again). Each computed component gets a new
 * id. `depthLimits` names the result
 * fields a depth range's own values win over. Returns the recipe and how
 * many layers changed.
 */
export function withDetectedAgain(
  adjustments: Adjustments,
  results: ReadonlyMap<string, { asked: Record<string, unknown>; result: Record<string, unknown> }>,
  depthLimits: readonly string[] = []
): { adjustments: Adjustments; layers: number } {
  const changed = new Set<string>();
  const next = {
    ...adjustments,
    masks: (adjustments.masks ?? []).map((layer: MaskContainer) => ({
      ...layer,
      subMasks: layer.subMasks.map((sub: SubMask) => {
        const found = results.get(sub.id);
        const parameters = (sub.parameters ?? {}) as Record<string, unknown>;
        if (!found || !waits(sub)) return sub;
        if (sub.type === 'ai-people' && PEOPLE_CHOICE(parameters) !== PEOPLE_CHOICE(found.asked)) return sub;
        const keep = sub.type === 'ai-depth' ? depthLimits : [];
        const own = Object.fromEntries(
          keep.filter((key) => parameters[key] !== undefined).map((key) => [key, parameters[key]])
        );
        const {
          [DETECT_AGAIN]: _again,
          [BITMAP_FRAME]: _frame,
          [DETECT_PENDING]: _pending,
          ...merged
        } = { ...parameters, ...found.result, ...own };
        changed.add(layer.id);
        // An id of its own: the native side keeps bitmaps by component id,
        // so the stretched one (one undo back) and this one both show.
        return { ...sub, id: crypto.randomUUID(), parameters: merged } as SubMask;
      })
    }))
  };
  return changed.size ? { adjustments: next, layers: changed.size } : { adjustments, layers: 0 };
}
