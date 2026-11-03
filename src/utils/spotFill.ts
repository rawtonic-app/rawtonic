import { invoke } from '@tauri-apps/api/core';
import { updateAdjustments } from '../hooks/useEditorActions';
import { useEditorStore } from '../store/useEditorStore';
import { useProcessStore } from '../store/useProcessStore';
import { Invokes } from '../components/ui/AppProperties';
import { Mask, SubMaskMode, ToolType } from '../components/panel/right/Masks';
import type { AiPatch, AiPatchData, Spot } from './adjustments';
import { isAiTaskCancelled, toastAiError } from './aiErrors';
import { errorMessage } from '../shared/ipc/errors';
import { shapeAsShown, shapeFrame, type SizedFrame } from './maskGeometry';

type Point = { x: number; y: number };

/** What `find_spot_source` decided for a new spot: Remove's fill where a
 * heal spot crosses strong edges (`fill`, only when asked to), else the
 * heal's automatic source. */
export interface SpotPlan {
  source: [number, number] | null;
  fill: boolean;
}

/** A heal spot's look carried over to its fill: opacity 0–100 and feather
 * 0–100 % of the radius. */
export interface HealLook {
  opacity: number;
  feather: number;
}

/** Remove's own soft edge (a fraction of the radius): a fill's edge is never
 * harder, since a hard-edged fill shows its seam. */
const REMOVE_FEATHER = 0.2;

/** A removal of the area a brush of `radius` painted along `points` (one
 * point: a circle): an AI patch named Remove, not yet filled. `fromHeal`:
 * the heal tool chose the fill because the area crosses strong edges (the
 * spot list shows it as Fill and turns it back into a heal); it keeps the
 * heal's opacity and, softer than Remove's edge, its feather. */
export function removalPatch(points: Point[], radius: number, fromHeal?: HealLook): AiPatch {
  const { selectedImage, adjustments } = useEditorStore.getState();
  // The geometry the area is painted in, so it stays on its spot.
  const frame = shapeFrame(adjustments, selectedImage?.width, selectedImage?.height);
  return {
    id: crypto.randomUUID(),
    name: 'Remove',
    isLoading: false,
    invert: false,
    patchData: null,
    prompt: '',
    visible: true,
    ...(fromHeal && { fromHeal: true }),
    subMasks: [
      {
        id: crypto.randomUUID(),
        type: Mask.Brush,
        mode: SubMaskMode.Additive,
        invert: false,
        opacity: fromHeal?.opacity ?? 100,
        visible: true,
        parameters: {
          lines: [
            {
              points,
              brushSize: radius * 2,
              feather: Math.max(REMOVE_FEATHER, (fromHeal?.feather ?? 0) / 100),
              tool: ToolType.Brush,
              ...(frame && { frame })
            }
          ]
        }
      }
    ]
  };
}

/** A painted removal's area where the photo shows it `now` (a stroke
 * painted before a turn stays on its spot); null for an object removal. */
export function removalArea(patch: AiPatch, now: SizedFrame | undefined): { points: Point[]; radius: number } | null {
  const sub = patch.subMasks[0];
  if (!sub || sub.type === Mask.QuickEraser || patch.subMasks.length !== 1) return null;
  const shown = sub.parameters && shapeAsShown(sub.type, sub.parameters, now);
  const line = (shown?.lines as { points: Point[]; brushSize: number }[] | undefined)?.[0];
  return line?.points?.length ? { points: line.points, radius: line.brushSize / 2 } : null;
}

/** The heal look a fill the heal tool made carries (`removalPatch`). */
function healLookOf(patch: AiPatch): HealLook {
  const sub = patch.subMasks[0];
  const line = (sub?.parameters?.lines as { feather?: number }[] | undefined)?.[0];
  return { opacity: sub?.opacity ?? 100, feather: Math.round((line?.feather ?? REMOVE_FEATHER) * 100) };
}

/** Fills `patch` (a removal) from the open photo as `adjustments` render
 * it, under the AI task `taskId` (cancel, progress). Resolves to the filled
 * patch, or null when the photo changed meanwhile; throws when the fill
 * failed or was cancelled. */
export async function fillRemoval(patch: AiPatch, adjustments: object, taskId: string): Promise<AiPatch | null> {
  const path = useEditorStore.getState().selectedImage?.path;
  const json = await invoke<string>(Invokes.InvokeGenerativeReplaseWithMaskDef, {
    currentAdjustments: adjustments,
    patchDefinition: patch,
    taskId
  });
  if (useEditorStore.getState().selectedImage?.path !== path) return null;
  return { ...patch, patchData: JSON.parse(json) as AiPatchData, isLoading: false };
}

/** Turns a heal spot into Remove's fill of the same area, or a fill the
 * heal tool chose back into a heal spot with an automatic source (one undo
 * step, once the new one is ready; the item shows as busy meanwhile, under
 * its own id). Nothing changes when the photo or the item went away. */
export async function flipSpotItem(id: string): Promise<void> {
  const { selectedImage, adjustments, spotSettings: settings } = useEditorStore.getState();
  if (!selectedImage?.isReady) return;
  const path = selectedImage.path;
  const spot = adjustments.spots?.find((s) => s.id === id && s.mode === 'heal');
  const fill = adjustments.aiPatches?.find((p) => p.id === id && p.name === 'Remove');
  if (!spot && !fill) return;
  const { registerAiTask, unregisterAiTask } = useProcessStore.getState();
  registerAiTask(id, 'inpaint');
  try {
    if (spot) {
      const points = (spot.points?.length ?? 0) >= 2 ? spot.points! : [{ x: spot.x, y: spot.y }];
      // The fill starts from the photo without the heal it replaces.
      const without = { ...adjustments, spots: (adjustments.spots ?? []).filter((s) => s.id !== id) };
      const look = { opacity: spot.opacity, feather: spot.feather };
      const patch = { ...removalPatch(points, spot.radius, look), visible: spot.visible !== false };
      const done = await fillRemoval(patch, without, id);
      if (!done) return;
      updateAdjustments((prev) =>
        prev.spots?.some((s) => s.id === id)
          ? { ...prev, spots: prev.spots.filter((s) => s.id !== id), aiPatches: [...(prev.aiPatches ?? []), done] }
          : prev
      );
      if (useEditorStore.getState().activeSpotId === id) useEditorStore.getState().setEditor({ activeSpotId: done.id });
      return;
    }
    const now = shapeFrame(adjustments, selectedImage.width, selectedImage.height);
    const area = fill && removalArea(fill, now);
    if (!area) return;
    const without = { ...adjustments, aiPatches: (adjustments.aiPatches ?? []).filter((p) => p.id !== id) };
    const [first] = area.points;
    const plan = await invoke<SpotPlan>(Invokes.FindSpotSource, {
      x: first.x,
      y: first.y,
      radius: area.radius,
      points: area.points.length >= 2 ? area.points : null,
      jsAdjustments: without,
      fillAcrossEdges: false
    });
    if (useEditorStore.getState().selectedImage?.path !== path || !plan.source) return;
    const healed: Spot = {
      id: crypto.randomUUID(),
      mode: 'heal',
      x: first.x,
      y: first.y,
      radius: area.radius,
      sourceX: plan.source[0],
      sourceY: plan.source[1],
      ...(fill!.fromHeal ? healLookOf(fill!) : { feather: settings.feather, opacity: settings.opacity }),
      visible: fill!.visible !== false,
      ...(area.points.length >= 2 && { points: area.points })
    };
    updateAdjustments((prev) =>
      prev.aiPatches?.some((p) => p.id === id)
        ? { ...prev, aiPatches: prev.aiPatches.filter((p) => p.id !== id), spots: [...(prev.spots ?? []), healed] }
        : prev
    );
    if (useEditorStore.getState().activeSpotId === id) useEditorStore.getState().setEditor({ activeSpotId: healed.id });
  } catch (error) {
    if (isAiTaskCancelled(error)) return;
    console.error('Spot: switching heal and fill failed', error);
    toastAiError(error, `Switching heal and fill failed: ${errorMessage(error)}`);
  } finally {
    unregisterAiTask(id);
  }
}
