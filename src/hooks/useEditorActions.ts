import { useCallback, type SyntheticEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import debounce from 'lodash.debounce';
import i18next from 'i18next';
import { toast } from 'react-toastify';
import { useEditorStore, debouncedSetHistory, isRecipeHeld } from '../store/useEditorStore';
export { debouncedSetHistory } from '../store/useEditorStore';
import { isRawInLibrary, useLibraryStore } from '../store/useLibraryStore';
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
  pastedAdjustments,
  sameAdjustmentValue,
  withAiDenoiseVersion,
  withDiffractionVersion,
  withSharpenVersion,
  withGrainStructure,
} from '../utils/adjustments';
import { calculateCenteredCrop, reframingInputs } from '../utils/cropUtils';
import { autoWhiteBalance } from '../utils/whiteBalance';
import { originalPreviewAdjustments, splitViewAvailable } from '../utils/beforeAfter';
import { frameDrawnMasks, keepDrawnMasksOnPhoto } from '../utils/maskGeometry';
import { ImageMetadata, Invokes } from '../components/ui/AppProperties';
import { globalImageCache } from '../utils/ImageLRUCache';
import { registerQuitFlush } from '../utils/quit';
import { draftFor, flushSliderDraft, renderDraft } from '../utils/sliderDraft';
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

/** `apply_auto_straighten_to_paths` (StraightenReport in edits.rs). */
export interface StraightenReport {
  straightened: string[];
  noLines: string[];
  changed: string[];
  failed: [string, string][];
}

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

/** Whether `path` is still the open, fully loaded photograph (an async
 * result may only reach the editor then). */
export function isCurrentEditorImage(path: string): boolean {
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

// A drag's last moves commit as it ends, before the final render.
useEditorStore.subscribe((state, previous) => {
  if (previous.isSliderDragging && !state.isSliderDragging) flushSliderDraft();
});

/** Commits a slider drag's draft (sliderDraft.ts) for `path`. */
function commitDraft(path: string, adjustments: Adjustments) {
  useEditorStore.getState().setEditor((state) => {
    if (state.selectedImage?.path !== path || state.adjustments === adjustments) return state;
    debouncedSetHistory(adjustments);
    return { adjustments };
  });
}

/** The editor's adjustment update outside React (history included). During a
 * slider drag the moves render as a draft and commit every COMMIT_INTERVAL_MS.
 * `queued`: the update of a queued recipe operation (exclusiveRecipeOperation)
 * running before a batch that already holds the photograph. */
export function updateAdjustments(value: AdjustmentsUpdate, { queued = false }: { queued?: boolean } = {}) {
  const current = useEditorStore.getState();
  const image = current.selectedImage;
  // A batch edit is writing this photograph's recipe (heldRecipePaths); the
  // queued operations ahead of it still show their edits.
  if (image && isRecipeHeld(current) && !queued) return;
  if (image?.isReady && current.isSliderDragging && !current.showOriginal && !current.previewOverride) {
    const prev = draftFor(image.path) ?? current.adjustments;
    const next = keepDrawnMasksOnPhoto(
      prev,
      typeof value === 'function' ? value(prev) : { ...prev, ...value },
      image.width,
      image.height,
    );
    if (next === prev || renderDraft(image.path, next, commitDraft)) return;
  }
  flushSliderDraft();
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
    // An update that changes nothing (a crop released where it was) is no step.
    if (newAdjustments === prev) return state;
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

/** Edit all selected's pending write (useEditAllSelected): flushed and
 * awaited before a batch, whose results it must not overwrite. */
let editAllFlush: (() => Promise<unknown> | null) | null = null;

/** Registers Edit all selected's flush (null to unregister); returns its
 * unregistration. */
export function registerEditAllFlush(flush: () => Promise<unknown> | null): () => void {
  editAllFlush = flush;
  return () => {
    if (editAllFlush === flush) editAllFlush = null;
  };
}

/** Running recipe operations by photograph — those that rewrite recipes in
 * the backend and then reload the open one (batch Auto straighten, Auto lens
 * correction, selection Auto Adjust): the latest one's end per photograph. */
const recipeOperations = new Map<string, Promise<void>>();

/** When an operation is running on any of `paths`: a promise for its end
 * (paste, presets and reset wait for it before they write), else null. */
function afterRecipeOperations(paths: string[]): Promise<void> | null {
  const waits = [...new Set(paths.map((path) => recipeOperations.get(path)).filter((wait) => wait !== undefined))];
  return waits.length > 0 ? Promise.all(waits).then(() => undefined) : null;
}

/** Runs `operation` on the recipes of `paths` after the operations already
 * running on any of them, so one's write and reload never interleave with
 * another's; later ones wait for this one. */
export async function exclusiveRecipeOperation<T>(paths: string[], operation: () => Promise<T>): Promise<T> {
  const unique = [...new Set(paths)];
  const earlier = afterRecipeOperations(unique);
  let ended!: () => void;
  const end = new Promise<void>((resolve) => {
    ended = resolve;
  });
  unique.forEach((path) => recipeOperations.set(path, end));
  try {
    if (earlier) await earlier;
    return await operation();
  } finally {
    unique.forEach((path) => {
      if (recipeOperations.get(path) === end) recipeOperations.delete(path);
    });
    ended();
  }
}

/** The batch-written recipe of `path` into the editor as one undoable
 * step, once it is open and loaded (a photograph opened during the batch may
 * still be loading; at most a minute is waited). Nothing when another
 * photograph was opened meanwhile. */
async function reloadHeldRecipe(path: string): Promise<void> {
  const status = () => {
    const image = useEditorStore.getState().selectedImage;
    return image?.path !== path ? 'gone' : image.isReady ? 'ready' : 'loading';
  };
  if (status() === 'loading') {
    await new Promise<void>((resolve) => {
      const timer = setTimeout(done, 60_000);
      const unsubscribe = useEditorStore.subscribe(() => {
        if (status() !== 'loading') done();
      });
      function done() {
        clearTimeout(timer);
        unsubscribe();
        resolve();
      }
    });
  }
  if (status() !== 'ready') return;
  const meta = await invoke<ImageMetadata>(Invokes.LoadMetadata, { path });
  if (isCurrentEditorImage(path) && meta.adjustments && !meta.adjustments.is_null) {
    // The commit changed only these (apply_auto_straighten_to_paths: the
    // rotation, the crop and the frames of drawn masks and AI patches); the
    // rest stays as the editor shows it, with the defaults its loader gave a
    // never-edited photograph (which the saved recipe leaves implicit).
    const written = normalizeLoadedAdjustments(meta.adjustments, !!useEditorStore.getState().selectedImage?.isRaw);
    const shown = useEditorStore.getState().adjustments;
    const loaded = {
      ...shown,
      rotation: written.rotation,
      crop: written.crop,
      masks: written.masks,
      aiPatches: written.aiPatches,
    };
    useEditorStore.getState().pushHistory(loaded);
    useEditorStore.getState().setEditor({ adjustments: loaded });
  }
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

  /** Auto straighten (the crop tool's Auto): the backend levels the photo
   * from its horizon or verticals and fits the crop without empty corners.
   * The answer belongs to the photograph and the frame it was asked for:
   * it is dropped when either changed meanwhile. One history step. */
  const handleAutoStraighten = useCallback(async (): Promise<'done' | 'none' | 'stale'> => {
    const { selectedImage, adjustments } = useEditorStore.getState();
    if (!selectedImage?.isReady) return 'stale';
    const asked = reframingInputs(adjustments as unknown as Record<string, unknown>);
    let result: { rotation: number; crop: Adjustments['crop'] } | null;
    try {
      result = await invoke(Invokes.CalculateAutoStraighten, { path: selectedImage.path, adjustments });
    } catch (err) {
      // The decode was another photograph's (auto_straighten.rs ANOTHER_PHOTO).
      if (String(err).includes('another photograph is open')) return 'stale';
      throw err;
    }
    const now = useEditorStore.getState().adjustments as unknown as Record<string, unknown>;
    if (!isCurrentEditorImage(selectedImage.path) || reframingInputs(now) !== asked) return 'stale';
    if (!result) return 'none';
    setAdjustments((prev: Adjustments) => ({ ...prev, rotation: result.rotation, crop: result.crop }));
    return 'done';
  }, [setAdjustments]);

  /** Auto straighten for a selection (Library, filmstrip), one photo at a
   * time in the backend. The open photograph, when selected, is held: its
   * slider draft and edits are saved first, it takes no edits, undo or
   * autosave while the batch writes, and it is reloaded with the result as
   * one undoable step. Resolves with what was done, or null. */
  const handleAutoStraightenPaths = useCallback(
    async (paths?: string[]): Promise<StraightenReport | null> => {
      const { multiSelectedPaths, libraryActivePath, setLibrary } = useLibraryStore.getState();
      const { selectedImage } = useEditorStore.getState();
      const pathsToUpdate =
        paths && paths.length > 0
          ? paths
          : multiSelectedPaths.length > 0
            ? multiSelectedPaths
            : selectedImage
              ? [selectedImage.path]
              : [];
      if (pathsToUpdate.length === 0) return null;
      if (selectedImage && !selectedImage.isReady && pathsToUpdate.includes(selectedImage.path)) return null;
      const open = selectedImage && pathsToUpdate.includes(selectedImage.path) ? selectedImage.path : null;
      // The open photograph's slider draft is committed (Edit all selected
      // passes it on), then every photograph of the batch is held, also one
      // opened while it runs: no edits from here on.
      flushSliderDraft();
      debouncedSetHistory.flush();
      const held = [...new Set(pathsToUpdate)];
      useEditorStore.getState().setEditor((state) => ({ heldRecipePaths: [...state.heldRecipePaths, ...held] }));
      // Edit all selected's pending write takes its place in the queue before
      // the batch (behind it, it would overwrite the others' results).
      editAllFlush?.();
      const release = (paths: string[]) =>
        useEditorStore.getState().setEditor((state) => {
          const remaining = [...state.heldRecipePaths];
          for (const path of paths) {
            const at = remaining.indexOf(path);
            if (at >= 0) remaining.splice(at, 1);
          }
          return { heldRecipePaths: remaining };
        });
      let released = false;
      // After the operations already running on these photographs (another
      // batch, Auto lens correction, Auto Adjust).
      return exclusiveRecipeOperation(held, async () => {
        if (open) {
          flushSliderDraft();
          debouncedSetHistory.flush();
          await flushRecipeSaves();
        }
        try {
          pathsToUpdate.forEach((p) => globalImageCache.delete(p));
          const report = await afterRecipeSaves(pathsToUpdate, () =>
            invoke<StraightenReport>(Invokes.ApplyAutoStraightenToPaths, { paths: pathsToUpdate }),
          );
          // Whichever batch photograph is open now (the one open at the start
          // or one opened meanwhile, maybe still loading) shows the result.
          const now = useEditorStore.getState().selectedImage?.path;
          release(held.filter((path) => path !== now));
          if (now && held.includes(now)) {
            if (report.straightened.includes(now)) await reloadHeldRecipe(now);
            release([now]);
          }
          released = true;
          if (libraryActivePath && report.straightened.includes(libraryActivePath)
            && useLibraryStore.getState().libraryActivePath === libraryActivePath) {
            const meta = await invoke<ImageMetadata>(Invokes.LoadMetadata, { path: libraryActivePath });
            if (useLibraryStore.getState().libraryActivePath === libraryActivePath
              && meta.adjustments && !meta.adjustments.is_null) {
              setLibrary({
                libraryActiveAdjustments: normalizeLoadedAdjustments(meta.adjustments, isRawInLibrary(libraryActivePath)),
              });
            }
          }
          return report;
        } finally {
          if (!released) release(held);
        }
      });
    },
    [],
  );

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
      const { multiSelectedPaths } = useLibraryStore.getState();
      const pathsToReset = paths || multiSelectedPaths;
      if (pathsToReset.length === 0) return;
      // One operation in these photographs' queue, in the order asked for:
      // its write and editor update after those asked for before, before
      // those asked for after (exclusiveRecipeOperation).
      void exclusiveRecipeOperation(pathsToReset, async () => {
        const { libraryActivePath, setLibrary } = useLibraryStore.getState();
        const { selectedImage, pushHistory } = useEditorStore.getState();
        if (selectedImage && !selectedImage.isReady && pathsToReset.includes(selectedImage.path)) return;

        pathsToReset.forEach((p) => globalImageCache.delete(p));
        // The last edit stays a step of its own, before the reset.
        debouncedSetHistory.flush();

        await afterRecipeSaves(
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
              debouncedSetHistory.flush();
              pushHistory(resetData);
              setEditor({ adjustments: resetData });
            }
          })
          .catch((err) => toast.error(`Failed to reset adjustments: ${err}`));
      });
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

      // Its reload replaces the whole recipe: after and before the other
      // operations on these photographs (exclusiveRecipeOperation).
      void exclusiveRecipeOperation(pathsToUpdate, () => afterRecipeSaves(pathsToUpdate, () =>
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
              const normalized = normalizeLoadedAdjustments(meta.adjustments, selectedImage.isRaw);
              setEditor({ adjustments: normalized });
              resetHistory(normalized);
            }
          }
          if (libraryActivePath && updated.includes(libraryActivePath)
            && useLibraryStore.getState().libraryActivePath === libraryActivePath) {
            const meta = await invoke<ImageMetadata>(Invokes.LoadMetadata, { path: libraryActivePath });
            if (useLibraryStore.getState().libraryActivePath === libraryActivePath
              && meta.adjustments && !meta.adjustments.is_null) {
              setLibrary({
                libraryActiveAdjustments: normalizeLoadedAdjustments(meta.adjustments, isRawInLibrary(libraryActivePath)),
              });
            }
          }
        })
        .catch((err) => toast.error(`Failed to apply auto lens correction: ${err}`)));
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
          sourceAdjustments = normalizeLoadedAdjustments(meta.adjustments, isRawInLibrary(pathToCopyFrom));
        } else {
          sourceAdjustments = initialAdjustmentsFor(isRawInLibrary(pathToCopyFrom));
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
   * photo), saving each recipe after its pending save; resolves once saved.
   * `openingNow`: the open photograph has only just been selected (its
   * loader has not read its recipe yet), so it may be written too; it then
   * reads its recipe after this write. */
  const applyAdjustmentsToPaths = useCallback(
    (
      partial: Partial<Adjustments>,
      paths?: string[],
      { openingNow = false }: { openingNow?: boolean } = {},
    ): Promise<unknown> | undefined => {
      // Grain from before the structure choice (old presets, other photos)
      // keeps rendering Even on the photos it is applied to.
      const adjustmentsToApply = withDiffractionVersion(withSharpenVersion(withAiDenoiseVersion(withGrainStructure(partial))));
      const { selectedImage: requestedOn } = useEditorStore.getState();
      const { multiSelectedPaths } = useLibraryStore.getState();
      const pathsToUpdate =
        paths || (multiSelectedPaths.length > 0 ? multiSelectedPaths : requestedOn ? [requestedOn.path] : []);
      if (pathsToUpdate.length === 0) return;
      // One operation in these photographs' queue, in the order asked for
      // (after a batch edit asked for before, before one asked for after);
      // with nothing ahead it runs at once.
      return exclusiveRecipeOperation(pathsToUpdate, () => {
        const { selectedImage, adjustments } = useEditorStore.getState();
        if (!openingNow && selectedImage && !selectedImage.isReady && pathsToUpdate.includes(selectedImage.path)) {
          return Promise.resolve();
        }

        pathsToUpdate.forEach((p) => globalImageCache.delete(p));

        if (selectedImage && pathsToUpdate.includes(selectedImage.path)) {
          updateAdjustments({ ...adjustments, ...adjustmentsToApply }, { queued: true });
        }

        const written = afterRecipeSaves(pathsToUpdate, () =>
          savedBatch(
            invoke(Invokes.ApplyAdjustmentsToPaths, { paths: pathsToUpdate, adjustments: adjustmentsToApply }),
            pathsToUpdate,
            'paste adjustments',
          ),
        );
        // A photograph opened meanwhile reads its recipe after this write.
        const settled = written.then(() => undefined, () => undefined);
        pathsToUpdate.forEach((path) => {
          pendingMetadataSaves.set(path, settled);
          void settled.then(() => {
            if (pendingMetadataSaves.get(path) === settled) pendingMetadataSaves.delete(path);
          });
        });
        return written
          .then((failed) => {
            const updated = pathsToUpdate.filter((path) => !failed.includes(path));
            updated.forEach((path) => unsavedRecipes.delete(path));
            if (selectedImage && updated.includes(selectedImage.path)
              && isCurrentEditorImage(selectedImage.path)) {
              // Awaited: the editor update belongs to this queued operation.
              return invoke<ImageMetadata>('load_metadata', { path: selectedImage.path }).then((meta) => {
                if (isCurrentEditorImage(selectedImage.path) && meta.adjustments) {
                  updateAdjustments((prev: Adjustments) => ({
                    ...prev,
                    lensMaker: meta.adjustments!.lensMaker,
                    lensModel: meta.adjustments!.lensModel,
                    lensDistortionParams: meta.adjustments!.lensDistortionParams,
                  }), { queued: true });
                }
              });
            }
          })
          .catch((err) => toast.error(`Failed to paste adjustments: ${err}`));
      });
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
      const adjustmentsToApply = pastedAdjustments(copiedAdjustments, includedAdjustments, mode as PasteMode);

      if (includedAdjustments.includes(LensAdjustment.LensMaker)) {
        if (!adjustmentsToApply.lensMaker) {
          adjustmentsToApply.lensDistortionParams = null;
        }
      }

      if (Object.keys(adjustmentsToApply).length === 0) {
        setProcess({ isPasted: true });
        return;
      }
      applyAdjustmentsToPaths(
        withDiffractionVersion(withAiDenoiseVersion(adjustmentsToApply, copiedAdjustments), copiedAdjustments),
        paths,
      );
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
    handleAutoStraighten,
    handleAutoStraightenPaths,
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
