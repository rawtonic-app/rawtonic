import { useCallback, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { noteBackgroundAiError, toastAiError } from '../utils/aiErrors';
import { useEditorStore } from '../store/useEditorStore';
import { useProcessStore } from '../store/useProcessStore';
import { useUIStore } from '../store/useUIStore';
import { useEditorActions } from './useEditorActions';
import { Adjustments, MaskContainer, Coord } from '../utils/adjustments';
import { SubMask } from '../components/panel/right/Masks';
import { Invokes, Panel } from '../components/ui/AppProperties';

/** The adjustments that warp the photo before its orientation (the engine's
 * `GEOMETRY_KEYS`): AI masks are computed in, and record, this frame. Upright
 * and guided perspective belong to it as much as the lens: without them the
 * masks were computed on the unrectified photo and sat off it. */
export const getTransformAdjustments = (adj: Adjustments) => ({
  guidedPerspective: adj.guidedPerspective,
  lensCorrectionMode: adj.lensCorrectionMode,
  autoChromaticAberration: adj.autoChromaticAberration,
  autoCaVersion: adj.autoCaVersion,
  transformDistortion: adj.transformDistortion,
  transformVertical: adj.transformVertical,
  transformHorizontal: adj.transformHorizontal,
  transformRotate: adj.transformRotate,
  transformAspect: adj.transformAspect,
  transformScale: adj.transformScale,
  transformXOffset: adj.transformXOffset,
  transformYOffset: adj.transformYOffset,
  lensDistortionAmount: adj.lensDistortionAmount,
  lensVignetteAmount: adj.lensVignetteAmount,
  lensTcaAmount: adj.lensTcaAmount,
  lensDistortionParams: adj.lensDistortionParams,
  lensMaker: adj.lensMaker,
  lensModel: adj.lensModel,
  lensDistortionEnabled: adj.lensDistortionEnabled,
  lensTcaEnabled: adj.lensTcaEnabled,
  lensVignetteEnabled: adj.lensVignetteEnabled,
});

/** The Subject and Sky masks computed ahead (`precompute_ai_foreground_mask`,
 * then `precompute_ai_sky_mask`): the photo and geometry asked for, the task
 * that runs now, and whether the run is still going. */
let maskPrecompute: { key: string; taskId: string; running: boolean; cancelled: boolean } | null = null;

/** Set once the masking tools were used; read by later launches. */
const MASKS_USED_KEY = 'rawtonic.masks.used';
/** After recent masking use, a photo that stays open this long in Develop gets
 * Subject and Sky computed ahead (quick stepping skips it). The first photo of
 * a Develop visit does not wait. */
export const AI_MASKS_AHEAD_DELAY_MS = 500;
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
  const reason: AiModelsUnloadReason = 'library';
  invoke(Invokes.UnloadAiModels, { reason }).catch((err) => console.error('Failed to unload the AI models:', err));
}

/** Loads the Subject and Sky models without computing anything (installed
 * models only); the first photo's masks, computed at once, are their first run. */
export function preloadAiMaskModels() {
  invoke(Invokes.PreloadAiMaskModels).catch((err) => console.error('Failed to load the mask models:', err));
}

/** Forgets the session state (tests). */
export function resetAiMaskSession() {
  cancelAiMaskPrecompute();
  lastMaskingUse = null;
  lastUseReported = 0;
  firstPhotoOfVisit = true;
  firstPhotoStarted = false;
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
      return invoke(Invokes.PrecomputeAiSubjectMask, { jsAdjustments, path, background: true });
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

  const handleGenerateAiMask = async (subMaskId: string, startPoint: Coord, endPoint: Coord) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    registerAiTask(subMaskId, 'subject');

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke<Record<string, unknown>>(Invokes.GenerateAiSubjectMask, {
        jsAdjustments: transformAdjustments,
        endPoint: [endPoint.x, endPoint.y],
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        path: selectedImage.path,
        rotation: adjustments.rotation,
        startPoint: [startPoint.x, startPoint.y],
        taskId: subMaskId,
      });

      const subMask = adjustments.masks
        ?.flatMap((m: MaskContainer) => m.subMasks)
        .find((sm: SubMask) => sm.id === subMaskId);

      const mergedParameters = { ...(subMask?.parameters || {}), ...newParameters };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toastAiError(error, `AI Mask Failed: ${error}`);
    } finally {
      unregisterAiTask(subMaskId);
    }
  };

  const handleGenerateAiDepthMask = async (subMaskId: string, parameters: AiDepthMaskSettings) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    registerAiTask(subMaskId, 'depth');

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke<Record<string, unknown>>('generate_ai_depth_mask', {
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

      const subMask = adjustments.masks
        ?.flatMap((m: MaskContainer) => m.subMasks)
        .find((sm: SubMask) => sm.id === subMaskId);

      const mergedParameters = { ...(subMask?.parameters || {}), ...newParameters };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toastAiError(error, `AI Depth Mask Failed: ${error}`);
    } finally {
      unregisterAiTask(subMaskId);
    }
  };

  const handleGenerateAiForegroundMask = async (subMaskId: string) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    registerAiTask(subMaskId, 'foreground');
    noteMaskingToolsUsed();

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke<Record<string, unknown>>(Invokes.GenerateAiForegroundMask, {
        jsAdjustments: transformAdjustments,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        rotation: adjustments.rotation,
        taskId: subMaskId,
      });

      const subMask = adjustments.masks
        ?.flatMap((m: MaskContainer) => m.subMasks)
        .find((sm: SubMask) => sm.id === subMaskId);

      const mergedParameters = { ...(subMask?.parameters || {}), ...newParameters };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toastAiError(error, `AI Foreground Mask Failed: ${error}`);
    } finally {
      unregisterAiTask(subMaskId);
    }
  };

  const handleGenerateAiSkyMask = async (subMaskId: string) => {
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
    if (!selectedImage?.path) return;
    registerAiTask(subMaskId, 'sky');
    noteMaskingToolsUsed();

    try {
      const transformAdjustments = getTransformAdjustments(adjustments);
      const newParameters = await invoke<Record<string, unknown>>(Invokes.GenerateAiSkyMask, {
        jsAdjustments: transformAdjustments,
        flipHorizontal: adjustments.flipHorizontal,
        flipVertical: adjustments.flipVertical,
        orientationSteps: adjustments.orientationSteps,
        rotation: adjustments.rotation,
        taskId: subMaskId,
      });

      const subMask = adjustments.masks
        ?.flatMap((m: MaskContainer) => m.subMasks)
        .find((sm: SubMask) => sm.id === subMaskId);

      const mergedParameters = { ...(subMask?.parameters || {}), ...newParameters };
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: mergedParameters });
    } catch (error) {
      toastAiError(error, `AI Sky Mask Failed: ${error}`);
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
    const { selectedImage, adjustments, patchesSentToBackend } = useEditorStore.getState();
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
      patchesSentToBackend.delete(subMaskId);
      updateSubMask(subMaskId, { parameters: { ...(current?.parameters || {}), ...newParameters } });
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
  // kept in normalised raw coordinates (gradients, brushes) is placed in its
  // pixels by the backend, which knows the photo's EXIF orientation.
  const selectedImageReady = useEditorStore((s) => !!s.selectedImage?.isReady);
  const masksKey = useEditorStore((s) => s.adjustments.masks);
  const requestedAutoMasks = useRef(new Set<string>());
  const requestedPlacements = useRef(new Set<string>());
  useEffect(() => {
    if (!selectedImageReady) return;
    const { selectedImage } = useEditorStore.getState();
    const masks = useEditorStore.getState().adjustments.masks ?? [];
    const unplaced = masks
      .flatMap((m: MaskContainer) => m.subMasks)
      .filter(
        (sub: SubMask) =>
          (sub.parameters as { lrNormalized?: unknown } | undefined)?.lrNormalized &&
          !requestedPlacements.current.has(sub.id),
      );
    if (unplaced.length > 0 && selectedImage) {
      unplaced.forEach((sub: SubMask) => requestedPlacements.current.add(sub.id));
      const path = selectedImage.path;
      void invoke<{ masks?: MaskContainer[] }>(Invokes.PlaceLightroomMasks, {
        path,
        adjustments: { masks: [{ subMasks: unplaced }] },
        width: selectedImage.width,
        height: selectedImage.height,
      })
        .then((placed) => {
          if (useEditorStore.getState().selectedImage?.path !== path) return;
          for (const sub of placed.masks?.[0]?.subMasks ?? []) {
            if (!(sub.parameters as { lrNormalized?: unknown } | undefined)?.lrNormalized) {
              updateSubMask(sub.id, { parameters: sub.parameters });
            }
          }
        })
        .catch((error) => console.error('Placing Lightroom masks failed:', error));
    }
    for (const sub of masks.flatMap((m: MaskContainer) => m.subMasks)) {
      if ((sub.parameters as { lrNormalized?: unknown } | undefined)?.lrNormalized) continue;
      // A People mask computes again whenever its parts or person change
      // (the panel clears its data then).
      if (sub.type === 'ai-people') {
        const p = (sub.parameters ?? {}) as { parts?: string[]; person?: number | null; maskDataBase64?: string | null };
        const parts = p.parts ?? [];
        const key = `${sub.id}:${JSON.stringify(parts)}:${p.person ?? ''}`;
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
      if (!pending || requestedAutoMasks.current.has(sub.id)) continue;
      requestedAutoMasks.current.add(sub.id);
      if (sub.type === 'ai-depth') {
        void handleGenerateAiDepthMask(sub.id, sub.parameters as AiDepthMaskSettings);
        continue;
      }
      if (!sub.parameters || Object.keys(sub.parameters).length === 0) {
        updateSubMask(sub.id, { parameters: { maskDataBase64: null, grow: 0, feather: 0 } });
      }
      void (sub.type === 'ai-sky' ? handleGenerateAiSkyMask(sub.id) : handleGenerateAiForegroundMask(sub.id));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedImageReady, masksKey]);

  // Lightroom Heal/Clone spots imported from a sidecar or catalog stay
  // normalised until the photo they belong to is shown.
  const spotsKey = useEditorStore((s) => s.adjustments.spots);
  const requestedSpotPlacements = useRef(new Set<string>());
  useEffect(() => {
    const { selectedImage, adjustments } = useEditorStore.getState();
    if (!selectedImageReady || !selectedImage) return;
    const isUnplaced = (spot: unknown) => !!(spot as { lrNormalized?: unknown }).lrNormalized;
    const unplaced = (adjustments.spots ?? []).filter(isUnplaced);
    const path = selectedImage.path;
    if (unplaced.length === 0 || requestedSpotPlacements.current.has(path)) return;
    requestedSpotPlacements.current.add(path);
    void invoke<{ spots?: Adjustments['spots'] }>(Invokes.PlaceLightroomMasks, {
      path,
      adjustments: { spots: unplaced },
      width: selectedImage.width,
      height: selectedImage.height,
    })
      .then((placed) => {
        if (useEditorStore.getState().selectedImage?.path !== path) return;
        setAdjustments((prev: Adjustments) => ({
          ...prev,
          spots: [...(prev.spots ?? []).filter((spot) => !isUnplaced(spot)), ...(placed.spots ?? [])],
        }));
      })
      .catch((error) => console.error('Placing Lightroom spots failed:', error));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedImageReady, spotsKey]);

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
  useEffect(() => () => cancelAiMaskPrecompute(), [selectedImagePathKey]);

  return { handleGenerateAiMask };
}
