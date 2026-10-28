import type { Adjustments } from './adjustments';

/** What a spot tool item does: a heal or clone spot, a removal (an AI patch
 * Remove made) or a red or pet eye correction. */
export type SpotItemKind = 'heal' | 'clone' | 'remove' | 'red' | 'pet';

export interface SpotItem {
  id: string;
  kind: SpotItemKind;
  visible: boolean;
}

/** The spot tool's items in the recipe: the red eye corrections in Red eye
 * mode, else the spots, then the removals, each in the order made. */
export function spotItems(
  adjustments: Pick<Adjustments, 'spots' | 'aiPatches' | 'redEyes'>,
  redEye: boolean,
): SpotItem[] {
  if (redEye)
    return (adjustments.redEyes ?? []).map((eye) => ({ id: eye.id, kind: eye.kind, visible: eye.visible !== false }));
  return [
    ...(adjustments.spots ?? []).map((spot) => ({ id: spot.id, kind: spot.mode, visible: spot.visible !== false })),
    ...(adjustments.aiPatches ?? [])
      .filter((patch) => patch.name === 'Remove')
      .map((patch) => ({ id: patch.id, kind: 'remove' as const, visible: patch.visible !== false })),
  ];
}

/** The recipe without the spot tool items `ids` (other AI patches stay). */
export function withoutSpotItems(prev: Adjustments, ids: ReadonlySet<string>): Adjustments {
  return {
    ...prev,
    spots: prev.spots?.filter((spot) => !ids.has(spot.id)),
    aiPatches: prev.aiPatches?.filter((patch) => patch.name !== 'Remove' || !ids.has(patch.id)),
    ...(prev.redEyes && { redEyes: prev.redEyes.filter((eye) => !ids.has(eye.id)) }),
  };
}

/** The recipe with the spot tool items `ids` shown or hidden (in the photo
 * and its exports; hidden items stay in the recipe). */
export function withSpotItemsVisible(prev: Adjustments, ids: ReadonlySet<string>, visible: boolean): Adjustments {
  return {
    ...prev,
    spots: prev.spots?.map((spot) => (ids.has(spot.id) ? { ...spot, visible } : spot)),
    aiPatches: prev.aiPatches?.map((patch) =>
      patch.name === 'Remove' && ids.has(patch.id) ? { ...patch, visible } : patch,
    ),
    ...(prev.redEyes && { redEyes: prev.redEyes.map((eye) => (ids.has(eye.id) ? { ...eye, visible } : eye)) }),
  };
}
