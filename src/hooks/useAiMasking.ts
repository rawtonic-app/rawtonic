import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { isPhotoChanged, noteBackgroundAiError, toastAiError } from '../utils/aiErrors';
import { DETECT_PENDING } from '../utils/settingsClipboard';
import { debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import { useProcessStore } from '../store/useProcessStore';
import { useUIStore } from '../store/useUIStore';
import { useEditorActions } from './useEditorActions';
import { Adjustments, MaskContainer, Coord } from '../utils/adjustments';
import { SubMask } from '../components/panel/right/Masks';
import { Invokes, Panel } from '../components/ui/AppProperties';
import { GEOMETRY_KEYS, ORIENTATION_KEYS } from '../utils/geometryKeys';
import type { ObjectPrompt } from '../utils/objectPrompts';

/** The adjustments that warp the photo before its orientation (the engine's
 * `GEOMETRY_KEYS`): AI masks are computed in, and record, this frame. Upright
 * and guided perspective belong to it as much as the lens: without them the
 * masks were computed on the unrectified photo and sat off it. */
export const getTransformAdjustments = (adj: Adjustments) =>
  Object.fromEntries(GEOMETRY_KEYS.map((key) => [key, adj[key]])) as Pick<Adjustments, (typeof GEOMETRY_KEYS)[number]>;

/** An imported Lightroom spot or red eye correction not yet placed (`lrNormalized`). */
const isUnplacedRetouch = (item: unknown) => !!(item as { lrNormalized?: unknown } | undefined)?.lrNormalized;

/** What placing a photo's pending Lightroom spots and red eyes depends on:
 * its geometry and those items; empty without any. */
const retouchBasis = (a: Adjustments) => {
  const spots = (a.spots ?? []).filter(isUnplacedRetouch);
  const redEyes = (a.redEyes ?? []).filter(isUnplacedRetouch);
  if (spots.length === 0 && redEyes.length === 0) return '';
  return JSON.stringify([getTransformAdjustments(a), ORIENTATION_KEYS.map((key) => a[key]), spots, redEyes]);
};

/** The Subject and Sky masks computed ahead (`precompute_ai_foreground_mask`,
 * then `precompute_ai_sky_mask`): the photo and geometry asked for, the task
 * that runs now, and whether the run is still going. */
let maskPrecompute: { key: string; taskId: string; running: boolean; cancelled: boolean } | null = null;

/** What an Object selection request checks it still stands on: whether
 * the component is there, its mask, its prompts and a recipe's one box. */
interface ObjectSelection {
  exists: boolean;
  mask: unknown;
  prompts: string;
  bounds: string;
}

function objectSelection(subMaskId: string): ObjectSelection {
  const params = useEditorStore
    .getState()
    .adjustments.masks?.flatMap((c: MaskContainer) => c.subMasks)
    .find((sm: SubMask) => sm.id === subMaskId)?.parameters as Record<string, unknown> | undefined;
  return {
    exists: Boolean(params),
    mask: params?.maskDataBase64 ?? null,
    prompts: JSON.stringify(params?.prompts ?? null),
    bounds: JSON.stringify([params?.startX, params?.startY, params?.endX, params?.endY]),
  };
}

const sameSelection = (a: ObjectSelection, b: ObjectSelection) =>
  a.exists === b.exists && a.mask === b.mask && a.prompts === b.prompts && a.bounds === b.bounds;

/** Object selections being computed, by photo and component: a press on
 * one of them waits for its mask (`objectSelectionBusy`), so prompts never
 * build on a result not in yet. */
const objectSelectionsComputing = new Set<string>();
const computingKey = (path: string, subMaskId: string) => JSON.stringify([path, subMaskId]);

/** Whether this photo's Object selection is still computing its mask. */
export const objectSelectionBusy = (path: string, subMaskId: string) =>
  objectSelectionsComputing.has(computingKey(path, subMaskId));

/** Set once the masking tools were used; read by later launches. */
const MASKS_USED_KEY = 'rawtonic.masks.used';
/** After recent masking use, a photo that stays open this long in Develop gets
 * Subject and Sky computed ahead (quick stepping skips it). The first photo of
 * a Develop visit does not wait. */
export const AI_MASKS_AHEAD_DELAY_MS = 500;
/** For someone who has not masked yet, the Subject and Sky models' GPU
 * kernels are built in the background once a decoded photo has stayed open
 * this long in Develop (`warmUpAiMaskModels`). */
export const AI_WARM_UP_DELAY_MS = 1000;
/** The backend drops the masking models after this long without masking use
 * (`ai_lifecycle::IDLE_UNLOAD`). */
export const AI_MODELS_IDLE_UNLOAD_MS = 5 * 60_000;
/** Masking use this recent computes ahead on every photo opened in Develop.
 * A minute shorter than the backend's idle time, so nothing is loaded again
 * just before it would be dropped. */
export const AI_MASKING_RECENT_MS = AI_MODELS_IDLE_UNLOAD_MS - 60_000;
/** Masking use is reported to the backend at most this often. */
const USE_REPORT_INTERVAL_MS = 30_000;
/** Event of `ai_lifecycle::UNLOADED_EVENT`. */
export const AI_MODELS_UNLOADED_EVENT = 'ai-models-unloaded';

/** Why the backend dropped the models (`ai_lifecycle::UnloadReason`). */
export type AiModelsUnloadReason = 'idle' | 'library' | 'memoryPressure';

/** Last masking use in this session (`Date.now()`), or null. */
let lastMaskingUse: number | null = null;
let lastUseReported = 0;
/** The first photo of a Develop visit loads the models and gets its masks
 * ahead as soon as it is ready, for someone who used the masking tools before.
 * Cleared once such a run has finished (not cancelled by stepping on), by
 * masking use (then recent use decides) and by an idle or memory-pressure
 * unload; set again when the Library is shown. */
let firstPhotoOfVisit = true;
/** This Develop visit asked for the warm-up (`warmUpAiMaskModels`). */
let warmUpAsked = false;
/** The visit's first photo started its run at once; if the photo was left
 * before the run ended, later photos wait `AI_MASKS_AHEAD_DELAY_MS` as after
 * recent use (quick stepping skips them). */
let firstPhotoStarted = false;

/** Notes that the masking tools were used: for the next minutes every photo
 * opened in Develop gets Subject and Sky ahead and the backend keeps the
 * models; later launches load them on the first photo. */
export function noteMaskingToolsUsed() {
  const now = Date.now();
  lastMaskingUse = now;
  firstPhotoOfVisit = false;
  try {
    localStorage.setItem(MASKS_USED_KEY, '1');
  } catch {
    // Storage unavailable: only this session knows.
  }
  if (now - lastUseReported >= USE_REPORT_INTERVAL_MS) {
    lastUseReported = now;
    invoke(Invokes.NoteAiMaskUse).catch((err) => console.error('Failed to report masking use:', err));
  }
}

function maskingToolsUsedBefore() {
  try {
    return localStorage.getItem(MASKS_USED_KEY) === '1';
  } catch {
    return false;
  }
}

function maskingUsedRecently() {
  return lastMaskingUse !== null && Date.now() - lastMaskingUse < AI_MASKING_RECENT_MS;
}

/** Whether the photo that just opened in Develop gets its masks computed
 * ahead (loading the models if needed): after recent masking use, or as the
 * first photo of a Develop visit when the masking tools were used before. */
function computeAheadOnOpen() {
  return maskingUsedRecently() || (firstPhotoOfVisit && maskingToolsUsedBefore());
}

/** The backend dropped the models (`ai-models-unloaded`). After an idle or
 * memory-pressure unload, opening photos no longer loads them again until
 * the masking tools are used (or the next Develop visit after the Library);
 * under memory pressure recent use no longer counts either. */
export function handleAiModelsUnloaded(reason: AiModelsUnloadReason) {
  maskPrecompute = null;
  if (reason === 'idle' || reason === 'memoryPressure') firstPhotoOfVisit = false;
  if (reason === 'memoryPressure') lastMaskingUse = null;
}

/** The Library is shown: background runs stop, the backend drops the masking
 * models, and the next photo opened in Develop loads them again. */
export function leaveDevelop() {
  cancelAiMaskPrecompute();
  firstPhotoOfVisit = true;
  firstPhotoStarted = false;
  warmUpAsked = false;
  const reason: AiModelsUnloadReason = 'library';
  invoke(Invokes.UnloadAiModels, { reason }).catch((err) => console.error('Failed to unload the AI models:', err));
}

/** Loads the Subject and Sky models without computing anything (installed
 * models only); the first photo's masks, computed at once, are their first run. */
export function preloadAiMaskModels() {
  invoke(Invokes.PreloadAiMaskModels).catch((err) => console.error('Failed to load the mask models:', err));
}

/** Builds the Subject and Sky models' GPU kernels in the background, once
 * per Develop visit, for someone who has not masked yet: on a fresh install
 * Core ML compiles them on a model's first run (Subject 11-14 s), which the
 * first click would wait for. Installed models only; the backend skips
 * models that have run in this app before, and keeps none loaded. Showing
 * the Library cancels it (`unload_ai_models`). */
export function warmUpAiMaskModels() {
  if (warmUpAsked) return;
  warmUpAsked = true;
  const taskId = `mask-warm-up-${crypto.randomUUID()}`;
  invoke(Invokes.WarmUpAiMaskModels, { taskId }).catch((err) =>
    console.error('Failed to build the mask models ahead:', err),
  );
}

/** Forgets the session state (tests). */
export function resetAiMaskSession() {
  cancelAiMaskPrecompute();
  lastMaskingUse = null;
  lastUseReported = 0;
  firstPhotoOfVisit = true;
  firstPhotoStarted = false;
  warmUpAsked = false;
}

/**
 * Computes the open photo's Subject mask (BiRefNet), then its Sky mask
 * (UperNet), then SAM's encoding for object masks in the background, so
 * Subject, Background, Sky and object clicks answer at once (each run costs
 * about a second of GPU and CPU, and on Core ML a few GB of memory for a
 * moment). Loads the models if they are not loaded; only
 * with the models already installed. The backend caches the masks per photo
 * and geometry, and a click during a run waits for it. Not masking use by
 * itself (see `openMaskingTools`).
 */
export function precomputeAiMasks() {
  const { selectedImage, adjustments } = useEditorStore.getState();
  if (!selectedImage?.path || !selectedImage.isReady) return;
  const jsAdjustments = getTransformAdjustments(adjustments);
  const path = selectedImage.path;
  const key = `${path}\n${JSON.stringify(jsAdjustments)}`;
  if (maskPrecompute?.key === key) return;
  cancelAiMaskPrecompute();
  const run = { key, taskId: `subject-precompute-${crypto.randomUUID()}`, running: true, cancelled: false };
  maskPrecompute = run;
  let failed = false;
  invoke(Invokes.PrecomputeAiForegroundMask, { jsAdjustments, path, taskId: run.taskId })
    .catch((err) => {
      console.error('Failed to precompute the subject mask:', err);
      failed = true;
    })
    .then(() => {
      if (run.cancelled) return;
      // Subject ran for a photo that stayed open: the models are warm.
      firstPhotoOfVisit = false;
      run.taskId = `sky-precompute-${crypto.randomUUID()}`;
      return invoke(Invokes.PrecomputeAiSkyMask, { jsAdjustments, path, taskId: run.taskId });
    })
    .catch((err) => {
      console.error('Failed to precompute the sky mask:', err);
      failed = true;
    })
    // SAM's encoding for object masks last: run beside Subject, the three
    // models' working memory added up (an app peak of about 10 GB on Core ML).
    .then(() => {
      if (run.cancelled) return;
      run.taskId = `sam-precompute-${crypto.randomUUID()}`;
      return invoke(Invokes.PrecomputeAiSubjectMask, { jsAdjustments, path, background: true, taskId: run.taskId });
    })
    .catch((err) => console.error('Failed to precompute the object mask encoding:', err))
    .finally(() => {
      run.running = false;
      // A failed run may be asked for again.
      if (failed && maskPrecompute === run) maskPrecompute = null;
    });
}

/** The masking tools are opened or pointed at (the Layers tab, the layer
 * panel's selection buttons, a component menu): masking use, and the masks
 * computed ahead. */
export function openMaskingTools() {
  noteMaskingToolsUsed();
  precomputeAiMasks();
}

/** Stops the background Subject/Sky run (the photo changed). */
export function cancelAiMaskPrecompute() {
  const run = maskPrecompute;
  maskPrecompute = null;
  if (run?.running) {
    run.cancelled = true;
    invoke(Invokes.CancelAiTask, { taskId: run.taskId }).catch((err) =>
      console.error('Failed to cancel the mask precompute:', err),
    );
  }
}

/** The models' lifecycle in the workspace: showing the Library unloads them,
 * and the backend's own unloads (idle, memory pressure) stop loading them
 * ahead. */
export function useAiModelLifecycle(develop: boolean) {
  const wasDevelop = useRef(develop);
  useEffect(() => {
    if (wasDevelop.current && !develop) leaveDevelop();
    // Entering Develop (or starting in it): the models start loading at once,
    // beside the photo's decoding, when its first photo will compute ahead.
    if (develop && computeAheadOnOpen()) preloadAiMaskModels();
    wasDevelop.current = develop;
  }, [develop]);
  useEffect(() => {
    let stop: (() => void) | undefined;
    let unmounted = false;
    listen<AiModelsUnloadReason>(AI_MODELS_UNLOADED_EVENT, (event) => handleAiModelsUnloaded(event.payload))
      .then((unlisten) => {
        if (unmounted) unlisten();
        else stop = unlisten;
      })
      .catch((err) => console.error('Failed to listen for AI model unloads:', err));
    return () => {
      unmounted = true;
      stop?.();
    };
  }, []);
}

/** A depth range's limits: applied when it renders, so values changed while
 * its depth map computes are kept over the ones the request echoes back. */
const DEPTH_LIMITS = ['minDepth', 'maxDepth', 'minFade', 'maxFade', 'feather'] as const;

/** Depth range of an AI depth sub-mask (`generate_ai_depth_mask` arguments); unset values use the defaults below. */
interface AiDepthMaskSettings {
  minDepth?: number;
  maxDepth?: number;
  minFade?: number;
  maxFade?: number;
  feather?: number;
}

export function useAiMasking() {
  const { setAdjustments } = useEditorActions();
  const registerAiTask = useProcessStore((state) => state.registerAiTask);
  const unregisterAiTask = useProcessStore((state) => state.unregisterAiTask);

  const updateSubMask = useCallback(
    (subMaskId: string, updatedData: Partial<SubMask>) => {
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        masks: prev.masks.map((c: MaskContainer) => ({
          ...c,
          subMasks: c.subMasks.map((sm: SubMask) => (sm.id === subMaskId ? { ...sm, ...updatedData } : sm)),
        })),
      }));
    },
    [setAdjustments],
  );

  /** Puts a finished AI mask into its component, over the settings it holds
   * now: Grow, Feather or depth limits changed while it ran stay (`keep`:
   * result fields the component's own values win over). Nothing changes when
   * another photo is open by then (components keep their ids when settings
   * are copied) or the component is gone. False when the result was dropped
   * because another photo is open or its photo is still loading. */
  const applyAiResult = (
    path: string,
    subMaskId: string,
    result: Record<string, unknown>,
    keep: readonly string[] = [],
  ): boolean => {
    const { selectedImage, patchesSentToBackend } = useEditorStore.getState();
    // A photo opened again but still loading takes no edits yet.
    if (selectedImage?.path !== path || !selectedImage.isReady) return false;
    patchesSentToBackend.delete(subMaskId);
    // Merged inside the update: during a slider drag `prev` is the drag's
    // newest draft, not the committed recipe.
    setAdjustments((prev: Adjustments) => {
      if (!prev.masks?.some((c: MaskContainer) => c.subMasks.some((sm: SubMask) => sm.id === subMaskId))) return prev;
      return {
        ...prev,
        masks: prev.masks.map((c: MaskContainer) => ({
          ...c,
          subMasks: c.subMasks.map((sm: SubMask) => {
            if (sm.id !== subMaskId) return sm;
            const parameters = sm.parameters || {};
            const own = Object.fromEntries(
              keep.filter((key) => parameters[key] !== undefined).map((key) => [key, parameters[key]]),
            );
            // Detected on this photograph: no longer waiting (pasted ones;
            // an Object selection when the object is pointed at again).
            const { [DETECT_PENDING]: _pending, ...rest } = { ...parameters, ...result, ...own } as Record<string, unknown>;
            return { ...sm, parameters: rest };
          }),
        })),
      };
    });
    return true;
  };

  /** An Object selection by its prompts (or, without them, one box or
   * click): the prompts are written with their mask as one history step
   * when the mask arrives, unless the selection changed meanwhile (an undo,
   * another change of the component). Resolves to whether the result was
   * dropped because the photo changed (here, or in the backend). */
  const handleGenerateAiMask = async (
    subMaskId: string,
    startPoint: Coord,
    endPoint: Coord,
    prompts?: ObjectPrompt[],
  ): Promise<boolean> => {
    const { selectedImage, adjustments } = useEditorStore.getState();
    if (!selectedImage?.path) return false;
    const path = selectedImage.path;
    const computing = computingKey(path, subMaskId);
    const pressedOn = objectSelection(subMaskId);
    objectSelectionsComputing.add(computing);
    registerAiTask(subMaskId, 'subject');

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke<Record<string, unknown>>(Invokes.GenerateAiSubjectMask, {
        jsAdjustments: transformAdjustments,
        endPoint: [endPoint.x, endPoint.y],
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        path,
        prompts: prompts ?? null,
        rotation: adjustments.rotation,
        startPoint: [startPoint.x, startPoint.y],
        taskId: subMaskId,
      });
      if (useEditorStore.getState().selectedImage?.path !== path) return true;
      // A slider owns its draft until release. Applying here would put the
      // answer in that draft and its undo step; wait, then validate again.
      if (useEditorStore.getState().isSliderDragging) {
        await new Promise<void>((resolve) => {
          const unsubscribe = useEditorStore.subscribe((state) => {
            if (state.isSliderDragging && state.selectedImage?.path === path) return;
            unsubscribe();
            resolve();
          });
        });
      }
      if (useEditorStore.getState().selectedImage?.path !== path) return true;
      if (prompts && !sameSelection(pressedOn, objectSelection(subMaskId))) return false;

      // One step per prompt: an edit pending before stays its own, and the
      // next prompt does not merge into this one.
      debouncedSetHistory.flush();
      const applied = applyAiResult(path, subMaskId, prompts ? { ...newParameters, prompts } : newParameters);
      debouncedSetHistory.flush();
      return !applied;
    } catch (error) {
      if (isPhotoChanged(error)) return true;
      toastAiError(error, `AI Mask Failed: ${error}`);
      return false;
    } finally {
      objectSelectionsComputing.delete(computing);
      unregisterAiTask(subMaskId);
    }
  };

  // Depth, Foreground and Sky resolve to whether their result was dropped
  // because the photo changed meanwhile (it may then be asked for again):
  // here, or in the backend, which had another photo loaded by then
  // (`PHOTO_CHANGED`: an A→B→A switch must not give A the mask of B).
  const handleGenerateAiDepthMask = async (subMaskId: string, parameters: AiDepthMaskSettings): Promise<boolean> => {
    const { selectedImage, adjustments } = useEditorStore.getState();
    if (!selectedImage?.path) return false;
    registerAiTask(subMaskId, 'depth');

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke<Record<string, unknown>>(Invokes.GenerateAiDepthMask, {
        jsAdjustments: transformAdjustments,
        path: selectedImage.path,
        minDepth: parameters.minDepth ?? 20,
        maxDepth: parameters.maxDepth ?? 100,
        minFade: parameters.minFade ?? 15,
        maxFade: parameters.maxFade ?? 15,
        feather: parameters.feather ?? 10,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        rotation: adjustments.rotation,
        taskId: subMaskId,
      });

      return !applyAiResult(selectedImage.path, subMaskId, newParameters, DEPTH_LIMITS);
    } catch (error) {
      if (isPhotoChanged(error)) return true;
      toastAiError(error, `AI Depth Mask Failed: ${error}`);
      return false;
    } finally {
      unregisterAiTask(subMaskId);
    }
  };

  const handleGenerateAiForegroundMask = async (subMaskId: string): Promise<boolean> => {
    const { selectedImage, adjustments } = useEditorStore.getState();
    if (!selectedImage?.path) return false;
    registerAiTask(subMaskId, 'foreground');
    noteMaskingToolsUsed();

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke<Record<string, unknown>>(Invokes.GenerateAiForegroundMask, {
        jsAdjustments: transformAdjustments,
        path: selectedImage.path,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        rotation: adjustments.rotation,
        taskId: subMaskId,
      });

      return !applyAiResult(selectedImage.path, subMaskId, newParameters);
    } catch (error) {
      if (isPhotoChanged(error)) return true;
      toastAiError(error, `AI Foreground Mask Failed: ${error}`);
      return false;
    } finally {
      unregisterAiTask(subMaskId);
    }
  };

  const handleGenerateAiSkyMask = async (subMaskId: string): Promise<boolean> => {
    const { selectedImage, adjustments } = useEditorStore.getState();
    if (!selectedImage?.path) return false;
    registerAiTask(subMaskId, 'sky');
    noteMaskingToolsUsed();

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke<Record<string, unknown>>(Invokes.GenerateAiSkyMask, {
        jsAdjustments: transformAdjustments,
        path: selectedImage.path,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        rotation: adjustments.rotation,
        taskId: subMaskId,
      });

      return !applyAiResult(selectedImage.path, subMaskId, newParameters);
    } catch (error) {
      if (isPhotoChanged(error)) return true;
      toastAiError(error, `AI Sky Mask Failed: ${error}`);
      return false;
    } finally {
      unregisterAiTask(subMaskId);
    }
  };

  /** A People mask of its stored parts and person (`generate_ai_people_mask`). */
  const handleGenerateAiPeopleMask = async (
    subMaskId: string,
    parts: string[],
    person: number | null,
  ): Promise<boolean> => {
    const { selectedImage, adjustments } = useEditorStore.getState();
    if (!selectedImage?.path) return false;
    registerAiTask(subMaskId, 'people');

    try {
      const newParameters = await invoke<Record<string, unknown>>(Invokes.GenerateAiPeopleMask, {
        jsAdjustments: getTransformAdjustments(adjustments),
        path: selectedImage.path,
        parts,
        person,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        rotation: adjustments.rotation,
        taskId: subMaskId,
      });
      // The parts or the person may have changed meanwhile: keep the newer
      // request's choice, which computes again.
      const current = useEditorStore
        .getState()
        .adjustments.masks?.flatMap((m: MaskContainer) => m.subMasks)
        .find((sm: SubMask) => sm.id === subMaskId);
      const now = current?.parameters as { parts?: string[]; person?: number | null } | undefined;
      if (now && (JSON.stringify(now.parts ?? []) !== JSON.stringify(parts) || (now.person ?? null) !== person)) {
        return true;
      }
      applyAiResult(selectedImage.path, subMaskId, newParameters);
      return true;
    } catch (error) {
      toastAiError(error, `People mask failed: ${error}`);
      return false;
    } finally {
      unregisterAiTask(subMaskId);
    }
  };

  // Imported Lightroom masks: automatic AI masks without data (Select Sky,
  // Select Subject) are computed for the open photo, once each; geometry
  // kept in normalised raw coordinates (gradients, brushes, colour ranges)
  // is placed in its pixels by the backend, which knows the photo's EXIF
  // orientation. Renders that know the full decode's size place it the same
  // way before (`rawtonic_engine::lightroom_masks`), so placement waits for
  // the full decode and uses its size: opening the photo changes no pixel.
  const selectedImageReady = useEditorStore((s) => !!s.selectedImage?.isReady);
  const selectedImageDecoded = useEditorStore((s) => !!s.selectedImage?.isDecoded);
  const masksKey = useEditorStore((s) => s.adjustments.masks);
  /** Whether the open photograph's component `id` holds its bitmap now. */
  const componentHasData = (id: string) => {
    const sub = (useEditorStore.getState().adjustments.masks ?? [])
      .flatMap((m: MaskContainer) => m.subMasks)
      .find((sm: SubMask) => sm.id === id);
    return Boolean((sub?.parameters as { maskDataBase64?: string | null } | undefined)?.maskDataBase64);
  };
  const requestedAutoMasks = useRef(new Set<string>());
  const [askAgain, setAskAgain] = useState(0);
  const requestedPlacements = useRef(new Set<string>());
  const [placeMasksAgain, setPlaceMasksAgain] = useState(0);
  const sliderDragging = useEditorStore((s) => s.isSliderDragging);
  useEffect(() => {
    if (!selectedImageDecoded || sliderDragging) return;
    const { selectedImage } = useEditorStore.getState();
    const masks = useEditorStore.getState().adjustments.masks ?? [];
    if (!selectedImage) return;
    const path = selectedImage.path;
    const unplaced = masks
      .flatMap((m: MaskContainer) => m.subMasks)
      .filter(
        (sub: SubMask) =>
          (sub.parameters as { lrNormalized?: unknown } | undefined)?.lrNormalized &&
          !requestedPlacements.current.has(`${path}:${sub.id}`),
      );
    if (unplaced.length === 0) return;
    // Per photo: copied settings keep component ids.
    const keys = unplaced.map((sub: SubMask) => `${path}:${sub.id}`);
    const asked = new Map(unplaced.map((sub: SubMask) => [sub.id, JSON.stringify(sub.parameters)]));
    keys.forEach((key) => requestedPlacements.current.add(key));
    /** Asked again: at once when the photo is open and decoded, else when it is. */
    const askAgainLater = () => {
      keys.forEach((key) => requestedPlacements.current.delete(key));
      const now = useEditorStore.getState().selectedImage;
      if (now?.path === path && now.isDecoded) setPlaceMasksAgain((n) => n + 1);
    };
    void invoke<{ masks?: MaskContainer[] }>(Invokes.PlaceLightroomMasks, {
      path,
      adjustments: { masks: [{ subMasks: unplaced }] },
      width: selectedImage.width,
      height: selectedImage.height,
    })
      .then((placed) => {
        const now = useEditorStore.getState();
        // A drag holds its own draft of the recipe: placed when it ends.
        if (now.selectedImage?.path !== path || now.isSliderDragging) {
          askAgainLater();
          return;
        }
        const placedParameters = new Map(
          (placed.masks?.[0]?.subMasks ?? [])
            .filter((sub) => !(sub.parameters as { lrNormalized?: unknown } | undefined)?.lrNormalized)
            .map((sub) => [sub.id, sub.parameters]),
        );
        if (placedParameters.size === 0) return;
        /** `adjustments` with the asked components, where still as asked, placed. */
        const placeIn = (adjustments: Adjustments): Adjustments => {
          let changed = false;
          const masks = (adjustments.masks ?? []).map((container: MaskContainer) => {
            let own = false;
            const subMasks = container.subMasks.map((sub: SubMask) => {
              const parameters = placedParameters.get(sub.id);
              if (!parameters || JSON.stringify(sub.parameters) !== asked.get(sub.id)) return sub;
              own = true;
              return { ...sub, parameters };
            });
            if (!own) return container;
            changed = true;
            return { ...container, subMasks };
          });
          return changed ? { ...adjustments, masks } : adjustments;
        };
        // No history step of its own: the placed form replaces the imported
        // one in the recipe and in every step that holds it, so an undo never
        // brings back the imported form (which the editing tools cannot use)
        // and redo stays. An edit still waiting to become a step is settled
        // first, so it is replaced too.
        debouncedSetHistory.flush();
        now.setEditor((state) =>
          state.selectedImage?.path !== path
            ? state
            : { adjustments: placeIn(state.adjustments), history: state.history.map(placeIn) },
        );
      })
      .catch((error) => console.error('Placing Lightroom masks failed:', error));
  }, [selectedImageDecoded, sliderDragging, masksKey, placeMasksAgain]);
  useEffect(() => {
    if (!selectedImageReady) return;
    const { selectedImage } = useEditorStore.getState();
    const masks = useEditorStore.getState().adjustments.masks ?? [];
    for (const sub of masks.flatMap((m: MaskContainer) => m.subMasks)) {
      if ((sub.parameters as { lrNormalized?: unknown } | undefined)?.lrNormalized) continue;
      // A People mask computes again whenever its parts or person change
      // (the panel clears its data then).
      if (sub.type === 'ai-people') {
        const p = (sub.parameters ?? {}) as { parts?: string[]; person?: number | null; maskDataBase64?: string | null };
        const parts = p.parts ?? [];
        const key = `${selectedImage?.path}:${sub.id}:${JSON.stringify(parts)}:${p.person ?? ''}`;
        if (p.maskDataBase64 || parts.length === 0 || requestedAutoMasks.current.has(key)) continue;
        requestedAutoMasks.current.add(key);
        // Done or superseded: the same choice may be asked for again later
        // (a failure is not retried until the photo opens again).
        void handleGenerateAiPeopleMask(sub.id, parts, p.person ?? null).then((ok) => {
          if (ok) requestedAutoMasks.current.delete(key);
        });
        continue;
      }
      // Automatic selections (and a new depth range) compute their data
      // once; a depth range's limits apply at render time.
      const pending =
        (sub.type === 'ai-sky' || sub.type === 'ai-foreground' || sub.type === 'ai-depth') &&
        !(sub.parameters as { maskDataBase64?: string | null } | undefined)?.maskDataBase64;
      // Per photo: copied settings keep component ids.
      const key = `${selectedImage?.path}:${sub.id}`;
      if (!pending || requestedAutoMasks.current.has(key)) continue;
      requestedAutoMasks.current.add(key);
      if (sub.type !== 'ai-depth' && (!sub.parameters || Object.keys(sub.parameters).length === 0)) {
        updateSubMask(sub.id, { parameters: { maskDataBase64: null, grow: 0, feather: 0 } });
      }
      const run =
        sub.type === 'ai-depth'
          ? handleGenerateAiDepthMask(sub.id, sub.parameters as AiDepthMaskSettings)
          : sub.type === 'ai-sky'
            ? handleGenerateAiSkyMask(sub.id)
            : handleGenerateAiForegroundMask(sub.id);
      // A result dropped because the photo changed is asked for again when
      // the photo opens again, or now when it is open again already.
      const asked = selectedImage?.path;
      void run.then((dropped) => {
        if (!dropped) {
          // Computed: asked again only when its data is cleared again (a
          // paste of the same layer). A failure stays asked until the photo
          // opens again.
          if (componentHasData(sub.id)) requestedAutoMasks.current.delete(key);
          return;
        }
        requestedAutoMasks.current.delete(key);
        const now = useEditorStore.getState().selectedImage;
        if (now?.path === asked && now?.isReady) setAskAgain((n) => n + 1);
      });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedImageReady, masksKey, askAgain]);

  // Lightroom Heal/Clone spots and Red Eye corrections imported from a
  // catalog keep Lightroom's geometry (renders place them on the photo
  // themselves) until their photo is open: the backend then places them on
  // its decode for editing, each where it stands in its list. Each request
  // is for the photo, its geometry and its pending items (`retouchBasis`): a
  // placement that comes back for another one is asked for again against the
  // current recipe; items the geometry cannot show are asked for again when
  // it changes, and items brought back (an undo) as well. Placement waits for
  // the full decode (it places on it), not the preview proxy.
  const retouchBasisKey = useEditorStore((s) => retouchBasis(s.adjustments));
  const requestedRetouchPlacements = useRef(new Set<string>());
  const [placeRetouchAgain, setPlaceRetouchAgain] = useState(0);
  useEffect(() => {
    const { selectedImage, adjustments } = useEditorStore.getState();
    if (!selectedImageDecoded || !selectedImage || !retouchBasisKey) return;
    const sent = { spots: adjustments.spots ?? [], redEyes: adjustments.redEyes ?? [] };
    const sentBasis = retouchBasis(adjustments);
    const path = selectedImage.path;
    const key = `${path}\n${sentBasis}`;
    if (requestedRetouchPlacements.current.has(key)) return;
    requestedRetouchPlacements.current.add(key);
    const done = () => requestedRetouchPlacements.current.delete(key);
    /** Asked again: at once when the photo is open and decoded, else when it is. */
    const askAgain = () => {
      done();
      const now = useEditorStore.getState().selectedImage;
      if (now?.path === path && now.isDecoded) setPlaceRetouchAgain((n) => n + 1);
    };
    /** `current` with its pending items replaced by the placed ones `got` holds for `asked`. */
    const merged = <T>(current: T[], asked: T[], got: T[] | undefined): T[] => {
      const placed = (got ?? []).filter((_, i) => isUnplacedRetouch(asked[i]));
      let next = 0;
      return current.map((item) => (isUnplacedRetouch(item) ? placed[next++] : item));
    };
    void invoke<{ spots?: Adjustments['spots']; redEyes?: Adjustments['redEyes'] } | null>(
      Invokes.PlaceLightroomRetouch,
      { path, jsAdjustments: adjustments },
    )
      .then((placed) => {
        const now = useEditorStore.getState();
        if (
          !placed ||
          now.selectedImage?.path !== path ||
          retouchBasis(now.adjustments) !== sentBasis ||
          placed.spots?.length !== sent.spots.length ||
          placed.redEyes?.length !== sent.redEyes.length
        ) {
          askAgain();
          return;
        }
        const pending = (items: unknown[] | undefined) => (items ?? []).filter(isUnplacedRetouch).length;
        // Nothing the geometry could show: no edit to record, and not asked
        // again until the geometry or the items change.
        if (pending(placed.spots) + pending(placed.redEyes) === pending(sent.spots) + pending(sent.redEyes)) return;
        done();
        setAdjustments((prev: Adjustments) =>
          retouchBasis(prev) !== sentBasis
            ? prev
            : {
                ...prev,
                spots: merged(prev.spots ?? [], sent.spots, placed.spots),
                redEyes: merged(prev.redEyes ?? [], sent.redEyes, placed.redEyes),
              },
        );
      })
      .catch((error) => {
        done();
        console.error('Placing Lightroom spots and red eye corrections failed:', error);
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedImageDecoded, retouchBasisKey, placeRetouchAgain]);

  // SAM encodes the open photo in the background, so a click or box only
  // runs the decoder: after Subject and Sky where those are computed ahead
  // (`precomputeAiMasks`; only with the model already installed), and at
  // once when an object mask or Remove's Detect objects becomes active
  // (which may download the model).
  const activeObjectMaskKey = useEditorStore((s) => {
    const sub = s.adjustments?.masks
      ?.flatMap((m: MaskContainer) => m.subMasks)
      .find((sm: SubMask) => sm.id === s.activeMaskId);
    return sub?.type === 'ai-subject' ? sub.id : null;
  });
  // The spot removal tool's Remove with Detect objects selects with SAM too.
  const removeObjectsActive = useEditorStore(
    (s) => s.isSpotToolActive && s.spotSettings.mode === 'remove' && Boolean(s.spotSettings.detectObjects),
  );
  const selectedImagePathKey = useEditorStore((s) => s.selectedImage?.path);

  useEffect(() => {
    if (!selectedImageReady || !selectedImagePathKey) return;
    const precompute = (background: boolean) => {
      const { adjustments, selectedImage } = useEditorStore.getState();
      if (!selectedImage?.path) return;
      invoke(Invokes.PrecomputeAiSubjectMask, {
        jsAdjustments: getTransformAdjustments(adjustments),
        path: selectedImage.path,
        background,
      }).catch((err) => {
        console.error('Failed to precompute AI subject mask:', err);
        noteBackgroundAiError(err);
      });
    };
    if (activeObjectMaskKey || removeObjectsActive) {
      noteMaskingToolsUsed();
      precompute(false);
    }
  }, [activeObjectMaskKey, removeObjectsActive, selectedImagePathKey, selectedImageReady]);

  // Subject/Background and Sky run in the background once the masking
  // tools are open (the Layers tab or a selected layer; the layer panel's
  // selection buttons and component menus ask too), on a photo that stays
  // open briefly after recent masking use, and on the first photo of a
  // Develop visit for someone who used the masking tools before (this loads
  // the models; the backend drops them after minutes without use, in the
  // Library and under memory pressure). They stop when the photo changes.
  const masksPanelOpen = useUIStore((s) => s.activePanel === Panel.Masks);
  useEffect(() => {
    if (!selectedImageReady || !selectedImagePathKey) return;
    if (masksPanelOpen) {
      openMaskingTools();
      return;
    }
    if (maskingUsedRecently()) {
      const timer = window.setTimeout(() => precomputeAiMasks(), AI_MASKS_AHEAD_DELAY_MS);
      return () => window.clearTimeout(timer);
    }
    // The first photo of a Develop visit at once: its run is the models' first
    // after loading, which Core ML makes slower (0.5-0.9 s instead of
    // 0.2-0.3 s), so it starts as soon as the photo is ready and a click soon
    // after opening finds it done or nearly (a run on a constant input right
    // after loading did not speed up the photo's first run; notes:
    // reference/birefnet-fp16.md).
    if (firstPhotoOfVisit && maskingToolsUsedBefore()) {
      if (!firstPhotoStarted) {
        firstPhotoStarted = true;
        precomputeAiMasks();
        return;
      }
      // Left before its run ended: as after recent use.
      const timer = window.setTimeout(() => precomputeAiMasks(), AI_MASKS_AHEAD_DELAY_MS);
      return () => window.clearTimeout(timer);
    }
  }, [masksPanelOpen, selectedImagePathKey, selectedImageReady]);
  // Someone who has not masked yet: nothing is loaded ahead, but the models'
  // GPU kernels are built once in the background, after the photo's full
  // decode (its frames first) and only on a photo that stays open.
  useEffect(() => {
    if (!selectedImageDecoded || !selectedImagePathKey) return;
    if (masksPanelOpen || maskingUsedRecently() || maskingToolsUsedBefore()) return;
    const timer = window.setTimeout(warmUpAiMaskModels, AI_WARM_UP_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [masksPanelOpen, selectedImageDecoded, selectedImagePathKey]);
  useEffect(() => () => cancelAiMaskPrecompute(), [selectedImagePathKey]);

  return { handleGenerateAiMask };
}
