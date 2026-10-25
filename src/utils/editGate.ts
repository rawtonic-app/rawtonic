// One gate for every edit while the photograph is only shown (the Library's
// loupe and any full screen, utils/viewerMode). Edits are refused where they
// are applied, not on each way of asking for one (a key, a wheel, a menu, a
// tool): the open photograph's recipe and history only change in the editor
// store (`gateEditorUpdate`, applied to every store update), other
// photographs' recipes only through the commands in `RECIPE_WRITES`
// (`invokeRecipeWrite`). Nothing accepted before is refused late: the mode
// only begins once the edits under way have settled (`settleEdits`, called
// by the UI store before it shows the photograph alone) — drafts committed
// and Edit all written at once (`onSettle`), a held pointer released (its
// gesture commits on release), and answers and writes still on their way
// (`trackEdit`, `invokeEditStep`) come back first. Saves of edits made
// before stay allowed, and so does the editor completing a photograph's
// recipe by itself as it opens (`asLoaded`), like loading.
import { invoke, type InvokeArgs } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import type { EditorState } from '../store/useEditorStore';

// Set by the UI store (it knows the viewer mode); the editor store cannot
// import it (useUIStore imports useEditorStore).
let refused: () => boolean = () => false;

export function refuseEditsWhen(predicate: () => boolean) {
  refused = predicate;
}

// Steps of the editor completing a photograph's recipe by itself (`asLoaded`).
let completing = 0;

/** Whether edits are refused now (the photograph is only shown). */
export function editsRefused(): boolean {
  return completing === 0 && refused();
}

/** `step` completes the open photograph's recipe by itself, as part of
 * opening it (its AI masks detected, imported masks and retouch placed on
 * its decode, retouch following its faces): no edit, it passes like loading
 * also while the photograph is only shown. */
export function asLoaded<T>(step: () => T): T {
  completing++;
  try {
    return step();
  } finally {
    completing--;
  }
}

// Edits accepted and still under way: an answer or a write to come back.
const underWay = new Set<Promise<unknown>>();

/** `work` (an accepted edit finishing later, all its steps) is waited for
 * before the photograph is only shown. */
export function trackEdit<T>(work: Promise<T>): Promise<T> {
  underWay.add(work);
  const done = () => underWay.delete(work);
  work.then(done, done);
  return work;
}

/** Edits under way that settle at once when asked (drafts, Edit all). */
const settlers = new Set<() => void>();

/** `settle` commits an edit under way at once; returns its unregistration. */
export function onSettle(settle: () => void): () => void {
  settlers.add(settle);
  return () => settlers.delete(settle);
}

// A pointer held down: its gesture (a drag, a stroke) commits on release.
let pointerReleased: Promise<void> | null = null;
if (typeof window !== 'undefined') {
  let release: (() => void) | null = null;
  const up = () => {
    release?.();
    release = null;
    pointerReleased = null;
  };
  window.addEventListener(
    'pointerdown',
    () => {
      pointerReleased ??= new Promise<void>((resolve) => (release = resolve));
    },
    true,
  );
  window.addEventListener('pointerup', up, true);
  // A release the window did not see (outside it): no button is down now.
  window.addEventListener('pointermove', (event) => event.buttons === 0 && pointerReleased && up(), true);
  window.addEventListener('pointercancel', up, true);
  window.addEventListener('blur', up);
}

/**
 * Settles the edits under way: commits drafts and Edit all at once; null
 * when nothing else is under way, else resolves once the held pointer is
 * released and every answer and write has come back (with what their
 * continuations started), a task after the last.
 */
export function settleEdits(): Promise<void> | null {
  settlers.forEach((settle) => settle());
  if (underWay.size === 0 && !pointerReleased) return null;
  return new Promise<void>((resolve) => {
    const wait = (): void => {
      void Promise.allSettled([...underWay, pointerReleased]).then(() =>
        // The continuations of what came back run first (they may start more).
        setTimeout(() => {
          settlers.forEach((settle) => settle());
          if (underWay.size === 0 && !pointerReleased) resolve();
          else wait();
        }, 0),
      );
    };
    wait();
  });
}

/** An answer of the backend that becomes an edit (auto, a pick, a placement). */
export function invokeEditStep<T = unknown>(command: string, args?: InvokeArgs): Promise<T> {
  return trackEdit(invoke<T>(command, args));
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
  return trackEdit(invoke<T>(command, args));
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
 * photograph is ready) passes.
 * Tools may be turned off, not on.
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
