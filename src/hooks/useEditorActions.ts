import { useCallback, type SyntheticEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import debounce from 'lodash.debounce';
import i18next from 'i18next';
import { toast } from 'react-toastify';
import { useEditorStore, debouncedSetHistory } from '../store/useEditorStore';
export { debouncedSetHistory } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { useProcessStore } from '../store/useProcessStore';
import { useMultiViewStore } from '../store/useMultiViewStore';
import {
  Adjustments,
  AdjustmentsUpdate,
  Coord,
  INITIAL_ADJUSTMENTS,
  initialAdjustmentsFor,
  COPYABLE_ADJUSTMENT_KEYS,
  PasteMode,
  LensAdjustment,
  normalizeLoadedAdjustments,
  sameAdjustmentValue,
  withGrainStructure,
} from '../utils/adjustments';
import { calculateCenteredCrop } from '../utils/cropUtils';
import { autoWhiteBalance } from '../utils/whiteBalance';
import { originalPreviewAdjustments, splitViewAvailable } from '../utils/beforeAfter';
import { frameDrawnMasks, keepDrawnMasksOnPhoto } from '../utils/maskGeometry';
import { ImageMetadata, Invokes } from '../components/ui/AppProperties';
import { globalImageCache } from '../utils/ImageLRUCache';
import { registerQuitFlush } from '../utils/quit';
import { saveFailure } from './useLibraryActions';
import { clampViewerZoom } from '../utils/viewerDisplay';

const pendingMetadataSaves = new Map<string, Promise<Adjustments | undefined>>();
const unsavedRecipes = new Map<string, Adjustments>();

export function waitForPendingMetadataSave(path: string): Promise<Adjustments | undefined> | undefined {
  return pendingMetadataSaves.get(path)
    ?? (unsavedRecipes.has(path) ? Promise.resolve(unsavedRecipes.get(path)) : undefined);
}

const isIdentityCurve = (points?: Array<Coord>) =>
  !points ||
  (points.length === 2 && points[0].x === 0 && points[0].y === 0 && points[1].x === 255 && points[1].y === 255);

export const debouncedSave = debounce((path: string, adjustmentsToSave: Adjustments) => {
  const save = () => invoke<void>(Invokes.SaveMetadataAndUpdateThumbnail, { path, adjustments: adjustmentsToSave });
  // A returned-to photograph must read its recipe after its flushed save, and
  // multiple writes to that photograph must retain the user's edit order.
  const previous = pendingMetadataSaves.get(path);
  const pending = (previous ? previous.then(save) : save())
    .then(() => {
      unsavedRecipes.delete(path);
      return undefined;
    })
    .catch((err) => {
      // A persistence error must not turn reopening into a silent undo. Keep
      // the unsaved recipe until a later successful write supersedes it.
      unsavedRecipes.set(path, adjustmentsToSave);
      console.error('Auto-save failed:', err);
      toast.error(`Failed to save changes: ${err}`);
      return adjustmentsToSave;
    });
  pendingMetadataSaves.set(path, pending);
  void pending.then(() => {
    if (pendingMetadataSaves.get(path) === pending) pendingMetadataSaves.delete(path);
  });
  return pending;
}, 300);

type ScheduledRecipe = { path: string; adjustments: Adjustments };
let readScheduledRecipe: (() => ScheduledRecipe | null) | null = null;

/** Tells the quit flush which recipe of the open photograph was last handed
 * to `debouncedSave` (useImageProcessing's `prevAdjustmentsRef`); returns
 * its unregistration. */
export function registerScheduledRecipe(read: () => ScheduledRecipe | null): () => void {
  readScheduledRecipe = read;
  return () => {
    if (readScheduledRecipe === read) readScheduledRecipe = null;
  };
}

/** Schedules the open photograph's recipe when it changed after the last
 * one scheduled: a slider still being dragged is saved only when the drag
 * ends. Only a fully loaded photograph whose recipe was taken before. */
function scheduleUnsavedEditorRecipe() {
  const scheduled = readScheduledRecipe?.();
  const { selectedImage, adjustments } = useEditorStore.getState();
  if (!scheduled || !selectedImage?.isReady || scheduled.path !== selectedImage.path) return;
  if (scheduled.adjustments !== adjustments) debouncedSave(selectedImage.path, adjustments);
}

/**
 * Writes the debounced recipe save now (with the open photograph's edit in
 * progress) and waits for every save in flight; recipes whose save failed
 * earlier are tried once more. Resolves with the photographs whose recipe
 * is still not saved (quitting, utils/quit.ts).
 */
export async function flushRecipeSaves(): Promise<string[]> {
  scheduleUnsavedEditorRecipe();
  debouncedSave.flush();
  while (pendingMetadataSaves.size > 0) {
    await Promise.all(pendingMetadataSaves.values());
  }
  const failed: string[] = [];
  for (const [path, adjustments] of [...unsavedRecipes]) {
    try {
      await invoke(Invokes.SaveMetadataAndUpdateThumbnail, { path, adjustments });
      if (unsavedRecipes.get(path) === adjustments) unsavedRecipes.delete(path);
    } catch (err) {
      console.error('Saving before quitting failed:', err);
      failed.push(path);
    }
  }
  return failed;
}

registerQuitFlush(async () =>
  (await flushRecipeSaves()).map((path) => `Edits of ${path.split(/[\\/]/).pop() ?? path}`),
);

/** A batch recipe command (a `SaveFailure` on error): tells the user what
 * was not saved and resolves with those paths, so the saved ones still
 * update. */
function savedBatch(command: Promise<unknown>, paths: string[], what: string): Promise<string[]> {
  return command.then(
    () => [],
    (err) => {
      console.error(`Failed to ${what}:`, err);
      const { message, failed } = saveFailure(err, paths);
      toast.error(`Failed to ${what}: ${message}`);
      return failed;
    },
  );
}

function isCurrentEditorImage(path: string): boolean {
  const image = useEditorStore.getState().selectedImage;
  return image?.path === path && image.isReady;
}

function afterRecipeSaves<T>(paths: string[], mutation: () => Promise<T>, preserveUnsaved = true): Promise<T> {
  if (!paths.some((path) => pendingMetadataSaves.has(path) || unsavedRecipes.has(path))) return mutation();
  return Promise.all(paths.map(async (path) => {
    await pendingMetadataSaves.get(path);
    const unsaved = unsavedRecipes.get(path);
    if (preserveUnsaved && unsaved) {
      // A partial batch edit must not overwrite unrelated edits from a failed
      // save. Retry that complete recipe first, or leave it intact on failure.
      await invoke(Invokes.SaveMetadataAndUpdateThumbnail, { path, adjustments: unsaved });
      if (unsavedRecipes.get(path) === unsaved) unsavedRecipes.delete(path);
    }
  })).then(mutation);
}

/** The editor's adjustment update outside React (history included). */
export function updateAdjustments(value: AdjustmentsUpdate) {
  useEditorStore.getState().setEditor((state) => {
    if (!state.selectedImage?.isReady) return state;
    const prev = state.adjustments;
    // Drawn masks stay on the photo when this edit turns, flips or straightens it.
    const newAdjustments = keepDrawnMasksOnPhoto(
      prev,
      typeof value === 'function' ? value(prev) : { ...prev, ...value },
      state.selectedImage.width,
      state.selectedImage.height,
    );
    debouncedSetHistory(newAdjustments);
    return {
      adjustments: newAdjustments,
      ...(state.showOriginal ? { showOriginal: false, previewOverride: null } : {}),
    };
  });
}

/**
 * Starts the held before peek (Lightroom's \ held down): the whole
 * photograph unedited, framed like the edit, until endBeforePeek. With the
 * split on it covers both sides; the split comes back on release.
 */
export function startBeforePeek(): boolean {
  let started = false;
  useEditorStore.getState().setEditor((state) => {
    if (!state.selectedImage?.isReady || state.showOriginal) return state;
    started = true;
    return { showOriginal: true, previewOverride: originalPreviewAdjustments(state.adjustments) };
  });
  return started;
}

/** Ends the held before peek; nothing happens when it is not showing. */
export function endBeforePeek() {
  useEditorStore.getState().setEditor((state) =>
    state.showOriginal ? { showOriginal: false, previewOverride: null } : state,
  );
}

export function useEditorActions() {
  const setEditor = useEditorStore((s) => s.setEditor);

  const setAdjustments = useCallback(
    (value: AdjustmentsUpdate) => updateAdjustments(value),
    [setEditor],
  );

  const handleRotate = useCallback(
    (degrees: number) => {
      const { selectedImage, adjustments } = useEditorStore.getState();
      const increment = degrees > 0 ? 1 : 3;
      const newAspectRatio =
        adjustments.aspectRatio && adjustments.aspectRatio !== 0 ? 1 / adjustments.aspectRatio : null;
      const newOrientationSteps = ((adjustments.orientationSteps || 0) + increment) % 4;
      const newCrop =
        selectedImage?.width && selectedImage?.height
          ? calculateCenteredCrop(selectedImage.width, selectedImage.height, newOrientationSteps, newAspectRatio)
          : null;

      setAdjustments((prev) => ({
        ...prev,
        aspectRatio: newAspectRatio,
        orientationSteps: newOrientationSteps,
        rotation: 0,
        crop: newCrop,
      }));
    },
    [setAdjustments],
  );

  const handleAutoAdjustments = useCallback(async () => {
    const selectedImage = useEditorStore.getState().selectedImage;
    if (!selectedImage?.isReady) return;
    try {
      // The backend picks the auto method for the edit's base look.
      const current = useEditorStore.getState().adjustments;
      const toneMapper = current.toneMapper ?? null;
      // The curve choice (Base characteristics) is part of the rendering Auto predicts.
      const baseCurve = current.baseCurve ?? null;
      const { autoBlackPoint, ...autoAdjustments }: Adjustments & { autoBlackPoint?: number } = await invoke(
        Invokes.CalculateAutoAdjustments,
        { toneMapper, baseCurve },
      );
      if (!isCurrentEditorImage(selectedImage.path)) return;
      setAdjustments((prev: Adjustments) => ({
        ...prev,
        ...autoAdjustments,
        sectionVisibility: { ...prev.sectionVisibility, ...autoAdjustments.sectionVisibility },
        // Auto's Levels black point goes into the luma curve only while that
        // curve is untouched (same rule as merge_auto_adjustments in Rust).
        curves:
          autoBlackPoint !== undefined && isIdentityCurve(prev.curves?.luma)
            ? { ...prev.curves, luma: [{ x: autoBlackPoint, y: 0 }, { x: 255, y: 255 }] }
            : prev.curves,
      }));
    } catch (err) {
      toast.error(`Failed to apply auto adjustments: ${err}`);
    }
  }, [setAdjustments]);

  // Background only: a layer's white balance is a relative local correction.
  const handleAutoWhiteBalance = useCallback(async () => {
    const selectedImage = useEditorStore.getState().selectedImage;
    if (!selectedImage?.isReady) return;
    try {
      const balance = await autoWhiteBalance(selectedImage);
      if (!isCurrentEditorImage(selectedImage.path)) return;
      if (!balance) {
        toast.warn(i18next.t('studio.whiteBalance.toasts.needsPreview'));
        return;
      }
      setAdjustments((prev: Adjustments) => ({ ...prev, ...balance }));
    } catch (error) {
      toast.error(i18next.t('studio.whiteBalance.toasts.autoFailed', { error: String(error) }));
    }
  }, [setAdjustments]);

  // Before/After (Y): the split with Capture One's movable divider. A
  // held \ shows the whole photograph before on top of it (startBeforePeek).
  // The split is single-view only: asking for it in the multi view turns the
  // multi view off (N brings it back and turns the split off, see
  // MultiViewStage). The held peek works in the multi view too: it shows
  // the primary cell (the live editor) before.
  const toggleSplitView = useCallback(() => {
    setEditor((state) => {
      if (state.splitView) return { splitView: false };
      const { appSettings, osPlatform } = useSettingsStore.getState();
      if (!state.selectedImage?.isReady || !splitViewAvailable(appSettings?.useWgpuRenderer, osPlatform)) return state;
      const multiView = useMultiViewStore.getState();
      if (multiView.active) multiView.setEnabled(false);
      return {
        splitView: true,
        ...(state.showOriginal ? { showOriginal: false, previewOverride: null } : {}),
      };
    });
  }, [setEditor]);

  const handleLutSelect = useCallback(
    async (path: string, isBuiltIn: boolean = false) => {
      const image = useEditorStore.getState().selectedImage;
      if (!image?.isReady) return;
      try {
        const result: { size: number } = await invoke('load_and_parse_lut', { path });
        const name = path.split(/[\\/]/).pop() || 'LUT';
        if (!isCurrentEditorImage(image.path)) return;
        setAdjustments((prev: Adjustments) => ({
          ...prev,
          lutPath: path,
          lutName: name,
          lutSize: result.size,
          lutIntensity: 100,
          lutIsSceneReferred: isBuiltIn,
          sectionVisibility: {
            ...(prev.sectionVisibility || INITIAL_ADJUSTMENTS.sectionVisibility),
            effects: true,
          },
        }));
      } catch (err) {
        toast.error(`Failed to load LUT: ${err}`);
      }
    },
    [setAdjustments],
  );

  const setLutPreviewOverride = useCallback(
    (path: string | null, isBuiltIn: boolean = false) => {
      setEditor((state) => {
        if (!path) return { previewOverride: null };
        if (!state.selectedImage?.isReady) return state;
        const name = path.split(/[\\/]/).pop() || 'LUT';
        return {
          previewOverride: {
            ...state.adjustments,
            lutPath: path,
            lutName: name,
            lutIntensity: state.adjustments.lutIntensity,
            lutIsSceneReferred: isBuiltIn,
          },
        };
      });
    },
    [setEditor],
  );

  const handleResetAdjustments = useCallback(
    (paths?: string[]) => {
      const { multiSelectedPaths, libraryActivePath, setLibrary } = useLibraryStore.getState();
      const { selectedImage, pushHistory } = useEditorStore.getState();
      const pathsToReset = paths || multiSelectedPaths;
      if (pathsToReset.length === 0) return;
      if (selectedImage && !selectedImage.isReady && pathsToReset.includes(selectedImage.path)) return;

      pathsToReset.forEach((p) => globalImageCache.delete(p));
      debouncedSetHistory.cancel();

      afterRecipeSaves(
        pathsToReset,
        () => savedBatch(invoke(Invokes.ResetAdjustmentsForPaths, { paths: pathsToReset }), pathsToReset, 'reset adjustments'),
        false,
      )
        .then((failed) => {
          const reset = pathsToReset.filter((path) => !failed.includes(path));
          reset.forEach((path) => unsavedRecipes.delete(path));
          if (libraryActivePath && reset.includes(libraryActivePath)
            && useLibraryStore.getState().libraryActivePath === libraryActivePath)
            setLibrary({ libraryActiveAdjustments: { ...INITIAL_ADJUSTMENTS } });
          if (selectedImage && reset.includes(selectedImage.path)
            && isCurrentEditorImage(selectedImage.path)) {
            const aspect =
              selectedImage.width && selectedImage.height ? selectedImage.width / selectedImage.height : null;
            const resetData = { ...initialAdjustmentsFor(!!selectedImage.isRaw), aspectRatio: aspect, aiPatches: [] };
            // One undoable step, as in Capture One and Lightroom.
            pushHistory(resetData);
            setEditor({ adjustments: resetData });
          }
        })
        .catch((err) => toast.error(`Failed to reset adjustments: ${err}`));
    },
    [setEditor],
  );

  const handleAutoLensCorrection = useCallback(
    (paths?: string[]) => {
      const { multiSelectedPaths, libraryActivePath, setLibrary } = useLibraryStore.getState();
      const { selectedImage, resetHistory } = useEditorStore.getState();

      const pathsToUpdate =
        paths && paths.length > 0
          ? paths
          : multiSelectedPaths.length > 0
            ? multiSelectedPaths
            : selectedImage
              ? [selectedImage.path]
              : [];

      if (pathsToUpdate.length === 0) return;
      if (selectedImage && !selectedImage.isReady && pathsToUpdate.includes(selectedImage.path)) return;

      pathsToUpdate.forEach((p) => globalImageCache.delete(p));

      afterRecipeSaves(pathsToUpdate, () =>
        savedBatch(
          invoke('apply_auto_lens_correction_to_paths', { paths: pathsToUpdate }),
          pathsToUpdate,
          'apply auto lens correction',
        ),
      )
        .then(async (failed) => {
          const updated = pathsToUpdate.filter((path) => !failed.includes(path));
          updated.forEach((path) => unsavedRecipes.delete(path));
          if (selectedImage && updated.includes(selectedImage.path)
            && isCurrentEditorImage(selectedImage.path)) {
            const meta = await invoke<ImageMetadata>(Invokes.LoadMetadata, { path: selectedImage.path });
            if (isCurrentEditorImage(selectedImage.path) && meta.adjustments && !meta.adjustments.is_null) {
              const normalized = normalizeLoadedAdjustments(meta.adjustments);
              setEditor({ adjustments: normalized });
              resetHistory(normalized);
            }
          }
          if (libraryActivePath && updated.includes(libraryActivePath)
            && useLibraryStore.getState().libraryActivePath === libraryActivePath) {
            const meta = await invoke<ImageMetadata>(Invokes.LoadMetadata, { path: libraryActivePath });
            if (useLibraryStore.getState().libraryActivePath === libraryActivePath
              && meta.adjustments && !meta.adjustments.is_null) {
              setLibrary({ libraryActiveAdjustments: normalizeLoadedAdjustments(meta.adjustments) });
            }
          }
        })
        .catch((err) => toast.error(`Failed to apply auto lens correction: ${err}`));
    },
    [setEditor],
  );

  const handleCopyAdjustments = useCallback(async (pathOrEvent?: string | SyntheticEvent) => {
    const pathOverride = typeof pathOrEvent === 'string' ? pathOrEvent : undefined;
    const { selectedImage, adjustments } = useEditorStore.getState();
    const { libraryActivePath, multiSelectedPaths } = useLibraryStore.getState();
    let sourceAdjustments: Adjustments | null = null;

    const pathToCopyFrom =
      pathOverride || (selectedImage ? selectedImage.path : libraryActivePath || multiSelectedPaths[0]);

    if (selectedImage?.isReady && pathToCopyFrom === selectedImage.path) {
      sourceAdjustments = adjustments;
    } else if (pathToCopyFrom) {
      try {
        const meta = await invoke<ImageMetadata>(Invokes.LoadMetadata, { path: pathToCopyFrom });
        if (meta?.adjustments && !meta.adjustments.is_null) {
          sourceAdjustments = normalizeLoadedAdjustments(meta.adjustments);
        } else {
          sourceAdjustments = INITIAL_ADJUSTMENTS;
        }
      } catch (err) {
        toast.error(`Failed to load metadata for copying: ${err}`);
        return;
      }
    }

    if (!sourceAdjustments) return;

    const adjustmentsToCopy: Partial<Adjustments> = {};

    for (const key of COPYABLE_ADJUSTMENT_KEYS) {
      if (Object.prototype.hasOwnProperty.call(sourceAdjustments, key)) {
        adjustmentsToCopy[key] = structuredClone(sourceAdjustments[key]);
      }
    }
    // Drawn masks keep the frame of the photo they were drawn on (its size
    // when it is the open one), so the photos they are pasted on place them
    // on the same part of themselves, whatever their turns and size.
    if (adjustmentsToCopy.masks) {
      const open = selectedImage?.isReady && pathToCopyFrom === selectedImage.path ? selectedImage : null;
      adjustmentsToCopy.masks = frameDrawnMasks(adjustmentsToCopy.masks, sourceAdjustments, open?.width, open?.height);
    }
    // The clipboard holds only the copyable keys; paste reads it key by key.
    useEditorStore.getState().setEditor({ copiedAdjustments: adjustmentsToCopy as Adjustments });
    useProcessStore.getState().setProcess({ isCopied: true });
  }, []);

  /** Apply some adjustments to `paths` (default: the selection, else the open
   * photo), saving each recipe after its pending save; resolves once saved. */
  const applyAdjustmentsToPaths = useCallback(
    (partial: Partial<Adjustments>, paths?: string[]) => {
      // Grain from before the structure choice (old presets, other photos)
      // keeps rendering Even on the photos it is applied to.
      const adjustmentsToApply = withGrainStructure(partial);
      const { selectedImage, adjustments } = useEditorStore.getState();
      const { multiSelectedPaths } = useLibraryStore.getState();
      const pathsToUpdate =
        paths || (multiSelectedPaths.length > 0 ? multiSelectedPaths : selectedImage ? [selectedImage.path] : []);
      if (pathsToUpdate.length === 0) return;
      if (selectedImage && !selectedImage.isReady && pathsToUpdate.includes(selectedImage.path)) return;

      pathsToUpdate.forEach((p) => globalImageCache.delete(p));

      if (selectedImage && pathsToUpdate.includes(selectedImage.path)) {
        setAdjustments({ ...adjustments, ...adjustmentsToApply });
      }

      return afterRecipeSaves(pathsToUpdate, () =>
        savedBatch(
          invoke(Invokes.ApplyAdjustmentsToPaths, { paths: pathsToUpdate, adjustments: adjustmentsToApply }),
          pathsToUpdate,
          'paste adjustments',
        ),
      )
        .then((failed) => {
          const updated = pathsToUpdate.filter((path) => !failed.includes(path));
          updated.forEach((path) => unsavedRecipes.delete(path));
          if (selectedImage && updated.includes(selectedImage.path)
            && isCurrentEditorImage(selectedImage.path)) {
            invoke<ImageMetadata>('load_metadata', { path: selectedImage.path }).then((meta) => {
              if (isCurrentEditorImage(selectedImage.path) && meta.adjustments) {
                setAdjustments((prev: Adjustments) => ({
                  ...prev,
                  lensMaker: meta.adjustments!.lensMaker,
                  lensModel: meta.adjustments!.lensModel,
                  lensDistortionParams: meta.adjustments!.lensDistortionParams,
                }));
              }
            });
          }
        })
        .catch((err) => toast.error(`Failed to paste adjustments: ${err}`));
    },
    [setAdjustments],
  );

  const handlePasteAdjustments = useCallback(
    (paths?: string[]) => {
      const { copiedAdjustments } = useEditorStore.getState();
      const { appSettings } = useSettingsStore.getState();
      const { setProcess } = useProcessStore.getState();

      if (!copiedAdjustments || !appSettings?.copyPasteSettings) return;

      const { mode, includedAdjustments } = appSettings.copyPasteSettings;
      const adjustmentsToApply: Partial<Adjustments> = {};

      for (const key of includedAdjustments) {
        if (Object.prototype.hasOwnProperty.call(copiedAdjustments, key)) {
          const value = copiedAdjustments[key as keyof Adjustments];
          if (mode === PasteMode.Merge) {
            const defaultValue = INITIAL_ADJUSTMENTS[key as keyof Adjustments];
            if (!sameAdjustmentValue(value, defaultValue))
              adjustmentsToApply[key as keyof Adjustments] = value;
          } else {
            adjustmentsToApply[key as keyof Adjustments] = value;
          }
        }
      }

      if (includedAdjustments.includes(LensAdjustment.LensMaker)) {
        if (!adjustmentsToApply.lensMaker) {
          adjustmentsToApply.lensDistortionParams = null;
        }
      }

      if (Object.keys(adjustmentsToApply).length === 0) {
        setProcess({ isPasted: true });
        return;
      }
      applyAdjustmentsToPaths(adjustmentsToApply, paths);
      setProcess({ isPasted: true });
    },
    [applyAdjustmentsToPaths],
  );

  const handleZoomChange = useCallback((zoomValue: number, fitToWindow: boolean = false) => {
    const { originalSize, baseRenderSize, adjustments } = useEditorStore.getState();
    const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
    let targetZoomPercent: number;

    const orientationSteps = adjustments.orientationSteps || 0;
    const isSwapped = orientationSteps === 1 || orientationSteps === 3;
    const effectiveOriginalWidth = isSwapped ? originalSize.height : originalSize.width;
    const effectiveOriginalHeight = isSwapped ? originalSize.width : originalSize.height;

    if (fitToWindow) {
      if (
        effectiveOriginalWidth > 0 &&
        effectiveOriginalHeight > 0 &&
        baseRenderSize.width > 0 &&
        baseRenderSize.height > 0
      ) {
        const originalAspect = effectiveOriginalWidth / effectiveOriginalHeight;
        const baseAspect = baseRenderSize.width / baseRenderSize.height;
        targetZoomPercent =
          originalAspect > baseAspect
            ? baseRenderSize.width / effectiveOriginalWidth
            : baseRenderSize.height / effectiveOriginalHeight;
      } else {
        targetZoomPercent = 1.0;
      }
    } else {
      targetZoomPercent = zoomValue / dpr;
    }

    // No lower bound here but fit (below): a large photograph in a small
    // view (a multi view cell) fits at well under 10 %, and a floor there
    // zoomed it in past its cell.
    targetZoomPercent = Math.min(2.0, targetZoomPercent);

    let transformZoom = 1.0;
    if (
      effectiveOriginalWidth > 0 &&
      effectiveOriginalHeight > 0 &&
      baseRenderSize.width > 0 &&
      baseRenderSize.height > 0
    ) {
      const originalAspect = effectiveOriginalWidth / effectiveOriginalHeight;
      const baseAspect = baseRenderSize.width / baseRenderSize.height;
      if (originalAspect > baseAspect) {
        transformZoom = (targetZoomPercent * effectiveOriginalWidth) / baseRenderSize.width;
      } else {
        transformZoom = (targetZoomPercent * effectiveOriginalHeight) / baseRenderSize.height;
      }
    }
    // Never below fit (zoom 1, with the proof margin when it is on).
    useEditorStore.getState().setEditor({ zoom: clampViewerZoom(transformZoom) });
  }, []);

  return {
    setAdjustments,
    handleRotate,
    handleAutoAdjustments,
    handleAutoWhiteBalance,
    handleLutSelect,
    setLutPreviewOverride,
    handleResetAdjustments,
    handleAutoLensCorrection,
    handleCopyAdjustments,
    handlePasteAdjustments,
    applyAdjustmentsToPaths,
    handleZoomChange,
    toggleSplitView,
  };
}
