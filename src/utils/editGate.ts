// One gate for every edit while the photograph is only shown (the Library's
// loupe and any full screen, utils/viewerMode). Edits are refused where they
// are applied, not on each way of asking for one (a key, a wheel, a menu,
// a tool): the open photograph's recipe and history only change in the
// editor store (`gateEditorUpdate`, applied to every store update), other
// photographs' recipes only through the commands in `RECIPE_WRITES`
// (`invokeRecipeWrite`). Saves of edits made before stay allowed, so
// nothing made in Develop is lost on the way into the full screen.
import { invoke, type InvokeArgs } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import type { EditorState } from '../store/useEditorStore';

// Set by the UI store (it knows the viewer mode); the editor store cannot
// import it (useUIStore imports useEditorStore).
let refused: () => boolean = () => false;

export function refuseEditsWhen(predicate: () => boolean) {
  refused = predicate;
}

/** Whether edits are refused now (the photograph is only shown). */
export function editsRefused(): boolean {
  return refused();
}

/** The commands that write recipes of photographs by path. */
export const RECIPE_WRITES = [
  Invokes.ApplyAdjustmentsToPaths,
  Invokes.ApplyAutoAdjustmentsToPaths,
  Invokes.ApplyAutoLensCorrectionToPaths,
  Invokes.ApplyFaceRetouchToPaths,
  Invokes.ResetAdjustmentsForPaths,
] as const;
export type RecipeWrite = (typeof RECIPE_WRITES)[number];

/** A recipe write refused because the photograph is only shown. */
export class ViewOnlyRefusal extends Error {
  constructor(command: string) {
    super(`${command}: the photograph is only shown`);
    this.name = 'ViewOnlyRefusal';
  }
}

/** A command of `RECIPE_WRITES`; rejects with `ViewOnlyRefusal` while edits
 * are refused (callers then change nothing and show no error). */
export function invokeRecipeWrite<T = unknown>(command: RecipeWrite, args: InvokeArgs): Promise<T> {
  if (editsRefused()) return Promise.reject(new ViewOnlyRefusal(command));
  return invoke<T>(command, args);
}

/** Editor states that make a click or drag on the photograph edit it (or
 * draw a tool's handles over it): only turning them off passes the gate. */
export const EDIT_TOOL_STATES = [
  'activeMaskContainerId',
  'activeMaskId',
  'colorPickerTarget',
  'interactivePatch',
  'isGuidedPerspectiveActive',
  'isRotationActive',
  'isSpotToolActive',
  'isStraightenActive',
  'isWbPickerActive',
  'liveRotation',
  'lumaPickerLayer',
  'shapeDrawTarget',
  'showFaceBlemishes',
  'tatTarget',
] as const satisfies ReadonlyArray<keyof EditorState>;

/**
 * What of the update `next` of the editor store may apply while edits are
 * refused. A change of the ready open photograph's recipe (an edit, undo,
 * redo, a paste or reset) is refused whole; a new photograph's recipe
 * (loading, which publishes `selectedImage` with it or comes before the
 * photograph is ready) passes. Tools may be turned off, not on.
 */
export function gateEditorUpdate(state: EditorState, next: Partial<EditorState>): Partial<EditorState> {
  const image = state.selectedImage;
  const loading = !image?.isReady || 'selectedImage' in next;
  if (!loading && 'adjustments' in next && next.adjustments !== state.adjustments) return {};
  let kept: Partial<EditorState> | null = null;
  for (const key of EDIT_TOOL_STATES) {
    if (!(key in next) || !next[key] || next[key] === state[key]) continue;
    kept ??= { ...next };
    delete kept[key];
  }
  return kept ?? next;
}
