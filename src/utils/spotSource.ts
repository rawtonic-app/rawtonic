import { invoke } from '@tauri-apps/api/core';
import { updateAdjustments } from '../hooks/useEditorActions';
import { useEditorStore } from '../store/useEditorStore';
import { Invokes } from '../components/ui/AppProperties';
import type { Spot } from './adjustments';
import { errorMessage } from '../shared/ipc/errors';

/** What `find_another_spot_source` found: the spot's next source centre in
 * the mask frame; null when no other source fits (the spot keeps its own). */
export interface SpotSource {
  x: number;
  y: number;
}

/** Another automatic source each spot's "Find another source" turned down,
 * by spot id: the search never offers those again (Lightroom's `/` cycles
 * through new ones). Kept per photograph and reset with it. Rule: a press
 * whose answer the next press must not repeat is remembered here; an Undo
 * leaves the spot on an earlier source, so pressing again offers the
 * source that was current before that Undo, not a fresh one. */
const turnedDown = new Map<string, Array<[number, number]>>();
let turnedDownPhoto: string | null = null;

/** The source centres earlier presses on `spotId` turned down. */
export function sourcesTurnedDown(spotId: string): Array<[number, number]> {
  return (turnedDown.get(spotId) ?? []).map(([x, y]) => [x, y]);
}

/** Opens the list for the photograph `photo` (`findAnotherSource` also does
 * this): the sources turned down belong to the photograph they were found
 * on. */
export function resetTurnedDownForPhoto(photo: string): void {
  if (photo !== turnedDownPhoto) {
    turnedDown.clear();
    turnedDownPhoto = photo;
  }
}

/** Forgets the source that was turned down on the spot `spotId` (it is
 * deleted). */
export function resetTurnedDown(spotId: string): void {
  turnedDown.delete(spotId);
}

/** "Find another source" for the spot `spotId` (Lightroom's `/` in the spot
 * tool, the spot list's button, the spot settings' item): asks the engine,
 * on the photo with the spot's own healed pixels taken out, for a source
 * that is not the one the spot copies from now nor any an earlier press on
 * it turned down, and commits it as one undo step (`updateAdjustments`
 * pushes one history step, labelled as the change of the spots). Resolves to whether the spot got a new
 * source; false leaves it as it was (no other source fits, the spot went
 * away or is not a heal or clone spot, the photo changed meanwhile). */
export async function findAnotherSource(spotId: string): Promise<boolean> {
  const { selectedImage, adjustments } = useEditorStore.getState();
  const spot = adjustments.spots?.find((s) => s.id === spotId);
  // Fill removals have no source; red eye corrections neither.
  if (!selectedImage?.isReady || !spot) return false;
  const photo = selectedImage.path;
  resetTurnedDownForPhoto(photo);
  let source: SpotSource | null;
  try {
    source = await invoke<SpotSource | null>(Invokes.FindAnotherSpotSource, {
      spotId,
      excluded: sourcesTurnedDown(spotId).map(([x, y]) => ({ x, y })),
      jsAdjustments: adjustments
    });
  } catch (error) {
    // No other source: the spot keeps its own, as before (not a failure).
    console.error('Spot: no other source fits', errorMessage(error));
    return false;
  }
  // Same check as the backend: another photograph, or the spot went away.
  const now = useEditorStore.getState();
  if (now.selectedImage?.path !== photo) return false;
  const current = now.adjustments.spots?.find((s) => s.id === spotId);
  if (!current || !source) return false;
  const found = source;
  // The new source is one undo step (the label of a spot change, "Spot removal").
  updateAdjustments((prev) => ({
    ...prev,
    spots: (prev.spots ?? []).map((s): Spot => (s.id === spotId ? { ...s, sourceX: found.x, sourceY: found.y } : s))
  }));
  // The source the spot leaves is never offered again (also the one it
  // came back to after an Undo).
  const left = [current.sourceX, current.sourceY] as [number, number];
  turnedDown.set(spotId, [...(turnedDown.get(spotId) ?? []).filter(([x, y]) => x !== left[0] || y !== left[1]), left]);
  return true;
}
