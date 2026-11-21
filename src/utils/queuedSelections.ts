// SPDX-License-Identifier: AGPL-3.0-or-later
// The open photograph and the background selection queue
// (src-tauri/src/selection_queue.rs): in the Library the active photo is
// written by its editor, so the batch that queues the other photos' AI
// selections leaves it out. It is queued after its editor's save, and when
// the queue has computed its selections the editor takes them into the
// recipe it holds (`adopt_selections`), which may be ahead of the saved one:
// otherwise its next save would write them back as still waiting.
import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import type { Adjustments, MaskContainer, SubMask } from './adjustments';
import { DETECTED_TYPES } from './settingsClipboard';

/** Event of the queue with the path of a photo it computed. */
export const AI_SELECTIONS_DETECTED = 'ai-selections-detected';

/** Component id → its parameters with the computed selection. */
type Filled = Record<string, Record<string, unknown>>;

const waiting = (sub: SubMask) =>
  DETECTED_TYPES.has(sub.type) && !(sub.parameters as { maskDataBase64?: unknown } | undefined)?.maskDataBase64;

const hasWaiting = (recipe: Adjustments) =>
  Boolean(recipe.masks?.some((layer: MaskContainer) => layer.subMasks?.some(waiting)));

/** Bitmaps a recipe holds already take nothing (`keep_detected_in`): sent
 * as a short stand-in, not as their data. */
const KEPT_BITMAP = 'data:kept';
function withoutBitmaps(recipe: Adjustments): Adjustments {
  return {
    ...recipe,
    masks: recipe.masks.map((layer: MaskContainer) => ({
      ...layer,
      subMasks: layer.subMasks.map((sub: SubMask) =>
        (sub.parameters as { maskDataBase64?: unknown } | undefined)?.maskDataBase64
          ? { ...sub, parameters: { ...sub.parameters, maskDataBase64: KEPT_BITMAP } }
          : sub
      )
    }))
  };
}

/** `recipe` with the components of `filled` that still wait in it filled. */
function fill(recipe: Adjustments, filled: Filled | null | undefined): Adjustments {
  if (!filled || !recipe.masks?.some((layer: MaskContainer) => layer.subMasks?.some((sub) => filled[sub.id]))) {
    return recipe;
  }
  return {
    ...recipe,
    masks: recipe.masks.map((layer: MaskContainer) =>
      layer.subMasks.some((sub) => filled[sub.id] && waiting(sub))
        ? {
            ...layer,
            subMasks: layer.subMasks.map((sub: SubMask) =>
              filled[sub.id] && waiting(sub) ? { ...sub, parameters: filled[sub.id] } : sub
            )
          }
        : layer
    )
  };
}

/** Resolves once no slider of the photograph `path` is being dragged (its
 * draft and undo step stay its own). */
function sliderReleased(path: string): Promise<void> {
  if (!useEditorStore.getState().isSliderDragging) return Promise.resolve();
  return new Promise((resolve) => {
    const unsubscribe = useEditorStore.subscribe((state) => {
      if (state.isSliderDragging && state.selectedImage?.path === path) return;
      unsubscribe();
      resolve();
    });
  });
}

/** Asked again when the recipe changed while the backend matched it. */
const ATTEMPTS = 5;

/** The queue computed the selections of `path`: if it is the open
 * photograph, its recipe and every History entry take them into the
 * components still waiting for them (same layer and component, no bitmap,
 * the same geometry; the backend's rule). Edits made meanwhile stay; no
 * History step of its own (the selection belongs to the step that asked
 * for it). The changed recipe is saved as any edit. */
export async function adoptQueuedSelections(path: string): Promise<boolean> {
  for (let attempt = 0; attempt < ATTEMPTS; attempt += 1) {
    await sliderReleased(path);
    debouncedSetHistory.flush();
    const { selectedImage, adjustments, history } = useEditorStore.getState();
    // Loading reads the saved recipe, which holds them (written before the event).
    if (selectedImage?.path !== path || !selectedImage.isReady) return false;
    // Each recipe once (the shown one is also its History entry).
    const asked = [...new Set([adjustments, ...history])].filter(hasWaiting);
    if (asked.length === 0) return false;
    const answers = await invoke<(Filled | null)[]>(Invokes.AdoptSelections, {
      path,
      recipes: asked.map(withoutBitmaps)
    });
    if (!answers.some(Boolean)) return false;
    const now = useEditorStore.getState();
    if (now.selectedImage?.path !== path) return false;
    // Changed while it was matched (an edit, an undo): matched again.
    if (now.adjustments !== adjustments || now.history !== history || now.isSliderDragging) continue;
    for (const filled of answers) {
      for (const id of Object.keys(filled ?? {})) now.patchesSentToBackend.delete(id);
    }
    const filledOf = new Map(asked.map((recipe, i) => [recipe, fill(recipe, answers[i])]));
    const adopted = (recipe: Adjustments) => filledOf.get(recipe) ?? recipe;
    now.setEditor({ adjustments: adopted(adjustments), history: history.map(adopted) });
    return true;
  }
  return false;
}
