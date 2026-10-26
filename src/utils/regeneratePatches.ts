import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { useProcessStore } from '../store/useProcessStore';
import { updateAdjustments } from '../hooks/useEditorActions';
import type { AiPatch, AiPatchData } from './adjustments';

/** The open photograph's AI patches a frame update hid (`frameHidden`). */
export const hiddenByFrame = (patches: readonly AiPatch[] | undefined): AiPatch[] =>
  (patches ?? []).filter((patch) => patch.frameHidden);

/**
 * Generates the AI patches a frame update hid again, on the new frame: each
 * from its own selection (moved onto the new frame with the recipe) by the
 * model that made it, one undo step each, in place of the hidden one (under
 * a new id: the preview sends a patch's pixels once per id). Resolves with
 * how many failed.
 */
export async function regenerateHiddenPatches(): Promise<number> {
  const { selectedImage } = useEditorStore.getState();
  if (!selectedImage?.isReady) return 0;
  const path = selectedImage.path;
  let failed = 0;
  for (const hidden of hiddenByFrame(useEditorStore.getState().adjustments.aiPatches)) {
    const { registerAiTask, unregisterAiTask } = useProcessStore.getState();
    const id = crypto.randomUUID();
    registerAiTask(id, 'inpaint');
    try {
      const current = useEditorStore.getState().adjustments;
      if (useEditorStore.getState().selectedImage?.path !== path) break;
      const definition: AiPatch = { ...hidden, visible: true, frameHidden: undefined, patchData: null };
      const json = await invoke<string>(Invokes.InvokeGenerativeReplaseWithMaskDef, {
        currentAdjustments: { ...current, aiPatches: current.aiPatches.filter((p) => p.id !== hidden.id) },
        patchDefinition: definition,
        path,
        // Remove's patches come from the local inpainting model.
        useFastInpaint: !hidden.prompt,
        taskId: id,
      });
      if (useEditorStore.getState().selectedImage?.path !== path) break;
      const done: AiPatch = { ...definition, id, patchData: JSON.parse(json) as AiPatchData, isLoading: false };
      delete done.frameHidden;
      updateAdjustments((prev) => ({
        ...prev,
        aiPatches: (prev.aiPatches ?? []).map((p) => (p.id === hidden.id ? done : p)),
      }));
    } catch (error) {
      console.error('Generating a hidden AI patch again failed:', error);
      failed += 1;
    } finally {
      unregisterAiTask(id);
    }
  }
  return failed;
}
