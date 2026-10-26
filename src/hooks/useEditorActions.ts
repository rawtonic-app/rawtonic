import { useCallback, type SyntheticEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import debounce from 'lodash.debounce';
import i18next from 'i18next';
import { toast } from 'react-toastify';
import { useEditorStore, debouncedSetHistory } from '../store/useEditorStore';
export { debouncedSetHistory } from '../store/useEditorStore';
import { isRawInLibrary, isRawPhoto, useLibraryStore } from '../store/useLibraryStore';
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
  normalizeLoadedAdjustments,
  sameAdjustmentValue,
  withAiDenoiseVersion,
  withClarityVersion,
  withDiffractionVersion,
  withLumaNoiseVersion,
  withSharpenVersion,
  withGrainStructure,
  withLegacyGrainVersion,
} from '../utils/adjustments';
import { calculateCenteredCrop, reframingInputs } from '../utils/cropUtils';
import { cropGestures } from '../utils/cropGesture';
import { autoWhiteBalance } from '../utils/whiteBalance';
import { splitViewAvailable, type CompareMode } from '../utils/beforeAfter';
import { beforeAdjustments } from '../utils/beforeSnapshot';
import { frameDrawnMasks, keepDrawnMasksOnPhoto } from '../utils/maskGeometry';
import {
  DETECTED_TYPES,
  RESPONSES_CHOSEN,
  RESPONSE_KEYS,
  chosenSettings,
  newEditResponses,
  pasteInto,
  samePhotograph,
} from '../utils/settingsClipboard';
import { MAX_RENDERED_LAYERS } from '../utils/layerLimit';
import { useCopySettingsDialog, type SettingsChoice, type SettingsSource } from '../components/studio/CopySettingsDialog';
import { editorRecipe, withNeverEditedDefaults, type CameraLens } from '../utils/neverEdited';
import { withOwnBitmapIds } from '../utils/bitmapIds';
import { appliedStyleOf, recordStyle, revertStyle, withStyleValues } from '../utils/appliedStyle';
import { ImageMetadata, Invokes } from '../components/ui/AppProperties';
import { globalImageCache } from '../utils/ImageLRUCache';
import { registerQuitFlush } from '../utils/quit';
import { draftFor, flushSliderDraft, renderDraft, updateDraft } from '../utils/sliderDraft';
import { saveFailure } from './useLibraryActions';
import { clampViewerZoom } from '../utils/viewerDisplay';
import { noteTool } from '../components/contribute/usageStats';

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
  // A drag's moves since the last store commit are only a draft.
  flushSliderDraft();
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

/** The saved recipe the open photograph was opened from (with the camera
 * lens data and size its editor recipe was made with, and that recipe,
 * `shown`): a paste onto it while it still shows that recipe pastes onto
 * this, as onto a closed photograph. */
type OpenedRecipe = { saved: unknown; camera: CameraLens; width: number; height: number; shown: Adjustments };
let openedRecipe: ({ path: string } & OpenedRecipe) | null = null;
export function rememberOpenedRecipe(path: string, opened: OpenedRecipe) {
  openedRecipe = { path, ...opened };
}

/** The open photograph `open` with `pasted` (from `paste`, null: a preset),
 * as the backend pastes onto a closed photograph (`pasteInto`, `masks::paste_layers`): onto
 * its saved recipe while it is not edited since it was opened (then made
 * into the editor's recipe as loading does), else onto its edit. Its AI
 * bitmaps a paste changed get ids of their own (the renderer keeps bitmaps
 * by id). Null with a message when it would exceed the layers applied. */
function pastedOpen(
  open: { path: string; isRaw: boolean },
  current: Adjustments,
  pasted: Partial<Adjustments>,
  paste: { source: string; replaceLayers: boolean; newEdit?: ReturnType<typeof newEditResponses> } | null,
  style: StyleOp | null = null,
): Adjustments | null {
  const { history, historyPending } = useEditorStore.getState();
  // Not edited since loading: its only step is the recipe loading made (a
  // Reset or Auto starts a history of their own).
  // A style goes onto the editor's recipe (a new edit's processing versions
  // included), as presets always did.
  const opened =
    !style && history.length === 1 && !historyPending && openedRecipe?.path === open.path && history[0] === openedRecipe.shown
      ? openedRecipe
      : null;
  const saved = opened ? (opened.saved as Adjustments | null) : current;
  let base = saved && !(saved as { is_null?: boolean }).is_null ? saved : null;
  // A style replaces the last applied one (`appliedStyle`): onto the photo
  // with that one taken back, as the backend applies it to the others.
  const reverted = style && base ? revertStyle(base) : null;
  if (reverted) base = reverted.recipe;
  const merged = style?.remove
    ? base
    : pasteInto(base, styleGroupsJoined(base, pasted, style), {
        samePhoto: Boolean(paste && samePhotograph(paste.source, open.path)),
        replaceLayers: paste?.replaceLayers ?? true,
        keepNulls: !paste,
        newEdit: paste?.newEdit ? (open.isRaw ? paste.newEdit.raw : paste.newEdit.other) : undefined,
      });
  if (!merged) {
    toast.error(i18next.t('studio.develop.layers.limitReached', { limit: MAX_RENDERED_LAYERS }));
    return null;
  }
  // A setting the style added goes back to not being there: the editor's
  // recipe fills it in again as loading does (the version a recipe without
  // it renders with).
  const shown = opened
    ? editorRecipe(merged, open.isRaw, opened.width, opened.height, opened.camera)
    : reverted?.removed
      ? normalizeLoadedAdjustments(merged, open.isRaw)
      : merged;
  return withOwnBitmapIds(shown, current);
}

/** `pasted` with a style's groups of settings joined to the photo's own
 * (`withStyleValues`), as the backend applies a style. */
function styleGroupsJoined(base: Adjustments | null, pasted: Partial<Adjustments>, style: StyleOp | null) {
  if (!style || !base) return pasted;
  const joined = withStyleValues(base, pasted) as Record<string, unknown>;
  return Object.fromEntries(Object.keys(pasted).map((key) => [key, joined[key]])) as Partial<Adjustments>;
}

/** A style clicked in the styles list (`StudioPresets`): it replaces the
 * photos' last applied style, or with `remove` takes `id` back from the
 * photos it is the last applied style of (src/utils/appliedStyle.ts). */
export type StyleOp = { id: string; remove?: boolean };

/** The settings that choose a lens correction; its data
 * (`lensDistortionParams`) is each photograph's own (its camera, lens,
 * focal length). */
const LENS_CHOICE_KEYS = [
  'lensCorrectionMode',
  'lensMaker',
  'lensModel',
  'cameraCaVersion',
  'cameraGeometryVersion',
  'lensDistortionParams',
];

/** `lensProfileFor` of a recipe whose lens data is not resolved for its lens
 * choice yet (`lens_recipe::LENS_UNRESOLVED`): every save and render
 * resolves it (`prepare_recipe_lens`). */
const LENS_UNRESOLVED = 'unresolved';

/** A recipe's lens choice with its data, as `lens_recipe::lens_profile_stamp`
 * tells them apart. */
function lensChoice(recipe: Adjustments): unknown[] {
  const field = (key: string) => (recipe as unknown as Record<string, unknown>)[key] ?? null;
  const mode = field('lensCorrectionMode') ?? 'manual';
  if (mode !== 'camera') return [mode, field('lensMaker'), field('lensModel'), field('lensDistortionParams')];
  // Recipes from before the geometry version keep their stamps.
  const geometry = field('cameraGeometryVersion');
  const stamp = [mode, field('cameraCaVersion'), field('lensDistortionParams')];
  return geometry === null ? stamp : [...stamp, geometry];
}

/** `next` (`current` with `pasted`, for the open photograph): a pasted lens
 * choice that changes its lens without its data, marked unresolved, so an
 * export or save before `resolvePastedLens` brought its data resolves it
 * for this photograph rather than render the source's or the previous
 * lens's data. */
function withPastedLensUnresolved(next: Adjustments, current: Adjustments, pasted: Partial<Adjustments>): Adjustments {
  if (!LENS_CHOICE_KEYS.some((key) => key in pasted)) return next;
  if (sameAdjustmentValue(lensChoice(next), lensChoice(current))) return next;
  return { ...next, lensDistortionParams: null, lensProfileFor: LENS_UNRESOLVED };
}

/** Resolves the lens data of a lens choice pasted onto the open photograph
 * `path` (`pastedStep`, its paste's history step) as its save does, and
 * puts it into that step and the steps after it (undone ones included) that
 * still have that choice unresolved; a later lens edit is left alone.
 * Nothing while that step is gone from the history (a new edit after
 * undoing it; its save resolves it anyway) or the data cannot be resolved
 * yet (export and save try again). */
async function resolvePastedLens(path: string, pastedStep: Adjustments) {
  const lens = await invoke<Partial<Adjustments>>(Invokes.ResolvePastedLens, { path, adjustments: pastedStep }).catch(
    () => null,
  );
  if (!lens || lens.lensProfileFor === LENS_UNRESOLVED || !isCurrentEditorImage(path)) return;
  const { history, historyIndex, adjustments, historyPending } = useEditorStore.getState();
  const at = history.indexOf(pastedStep);
  if (at < 0) return;
  const unresolved = lensChoice(pastedStep);
  const waiting = (recipe: Adjustments) =>
    recipe.lensProfileFor === LENS_UNRESOLVED && sameAdjustmentValue(lensChoice(recipe), unresolved);
  const withLens = (recipe: Adjustments) => (waiting(recipe) ? { ...recipe, ...lens } : recipe);
  const patched = history.map((step, index) => (index >= at ? withLens(step) : step));
  // The recipe shown: its step's, or (an edit not yet a step) with the
  // same data; a drag's moves not yet committed get it too, and the drag
  // stays one step.
  const shown =
    historyIndex < at || !waiting(adjustments)
      ? adjustments
      : history[historyIndex] === adjustments
        ? patched[historyIndex]
        : { ...adjustments, ...lens };
  if (historyIndex >= at) updateDraft(path, withLens);
  useEditorStore.setState({ history: patched, ...(shown !== adjustments ? { adjustments: shown } : {}) });
  // Its step still to come is the shown recipe, with the data.
  if (historyPending && shown !== adjustments) debouncedSetHistory(shown);
}

/** The settings a copy, sync or Previous takes from: the open photograph's
 * (its size known) or `path`'s saved recipe. */
async function settingsSource(path?: string): Promise<(SettingsSource & { width?: number; height?: number }) | null> {
  const { selectedImage, adjustments } = useEditorStore.getState();
  const { libraryActivePath, multiSelectedPaths } = useLibraryStore.getState();
  const from = path || (selectedImage ? selectedImage.path : libraryActivePath || multiSelectedPaths[0]);
  if (!from) return null;
  if (selectedImage?.isReady && from === selectedImage.path) {
    return { path: from, adjustments, isRaw: selectedImage.isRaw, width: selectedImage.width, height: selectedImage.height };
  }
  const isRaw = isRawPhoto(from);
  // The photograph open before: its size as it was shown.
  const { previousPhoto, previousPhotoSize } = useEditorStore.getState();
  const size = from === previousPhoto && previousPhotoSize ? previousPhotoSize : {};
  try {
    // Its recipe as last edited: after its save still under way, or the
    // edit a failed save kept.
    debouncedSave.flush();
    const unsaved = await waitForPendingMetadataSave(from);
    if (unsaved) return { path: from, adjustments: unsaved, isRaw, ...size };
    const meta = await invoke<ImageMetadata>(Invokes.LoadMetadata, { path: from });
    // As the editor shows it: a never-edited RAW with its camera's lens data.
    const adjustments = await withNeverEditedDefaults(
      meta?.adjustments && !meta.adjustments.is_null
        ? normalizeLoadedAdjustments(meta.adjustments, isRaw)
        : initialAdjustmentsFor(isRaw),
      meta?.adjustments,
      isRaw,
      () => invoke<CameraLens>(Invokes.GetCameraLensProfile, { path: from }).catch(() => null),
    );
    return { path: from, adjustments, isRaw, ...size };
  } catch (err) {
    toast.error(`Failed to load metadata for copying: ${err}`);
    return null;
  }
}

/** Drawn masks leave their photo with its frame (its size when it is the
 * open one), so the photos they are pasted on place them on the same part
 * of themselves. */
function withSourceFrames(
  chosen: Partial<Adjustments>,
  source: SettingsSource & { width?: number; height?: number },
): Partial<Adjustments> {
  if (!chosen.masks) return chosen;
  return { ...chosen, masks: frameDrawnMasks(chosen.masks, source.adjustments, source.width, source.height) ?? chosen.masks };
}

/** The picker's remembered choice (the preferences' copied keys). */
function rememberedKeys(): string[] {
  return useSettingsStore.getState().appSettings?.copyPasteSettings?.includedAdjustments ?? COPYABLE_ADJUSTMENT_KEYS;
}

/** The layers chosen last time and on which photograph (layer ids belong
 * to their photograph). */
let lastLayers: { path: string; ids: string[] } | null = null;

/** The layers remembered for `source`: those chosen on it last time, else
 * all of them when layers were chosen. */
function rememberedLayers(source: { path: string; adjustments: Adjustments }, keys: string[]): string[] {
  const ids = (source.adjustments.masks ?? []).map((layer) => layer.id);
  if (!keys.includes('masks')) return [];
  if (lastLayers?.path === source.path) return ids.filter((id) => lastLayers!.ids.includes(id));
  return ids;
}

/** The remembered choice for `source`: its keys (processing versions only
 * when they were chosen, `RESPONSES_CHOSEN`: preferences from before the
 * picker list them without it) and its layers (`rememberedLayers`). */
function rememberedChoice(source: { path: string; adjustments: Adjustments }): SettingsChoice {
  const remembered = rememberedKeys();
  const keys = remembered.includes(RESPONSES_CHOSEN)
    ? remembered
    : remembered.filter((key) => !RESPONSE_KEYS.includes(key));
  return { keys, layerIds: rememberedLayers(source, keys) };
}

function rememberChoice(choice: SettingsChoice, source: string) {
  lastLayers = { path: source, ids: choice.layerIds };
  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
  if (!appSettings?.copyPasteSettings) return;
  void handleSettingsChange({
    ...appSettings,
    copyPasteSettings: { ...appSettings.copyPasteSettings, includedAdjustments: choice.keys },
  });
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
 * slider drag the moves render as a draft and commit every COMMIT_INTERVAL_MS. */
export function updateAdjustments(value: AdjustmentsUpdate) {
  const current = useEditorStore.getState();
  const image = current.selectedImage;
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
 * photograph unedited (or as the snapshot compared with), framed like the
 * edit, until endBeforePeek. With the split on it covers both sides; the
 * split comes back on release.
 */
export function startBeforePeek(): boolean {
  let started = false;
  useEditorStore.getState().setEditor((state) => {
    if (!state.selectedImage?.isReady || state.showOriginal) return state;
    started = true;
    return { showOriginal: true, previewOverride: beforeAdjustments(state.adjustments) };
  });
  return started;
}

/** Ends the held before peek; nothing happens when it is not showing. */
export function endBeforePeek() {
  useEditorStore.getState().setEditor((state) =>
    state.showOriginal ? { showOriginal: false, previewOverride: null } : state,
  );
}

let autoStraightenRunning: Promise<'done' | 'none' | 'stale'> | null = null;

/** A crop rotation gesture in progress (its angle lives outside the recipe
 * until release). */
const rotatingNow = () => {
  const { liveRotation, isRotationActive } = useEditorStore.getState();
  return isRotationActive || (liveRotation !== null && liveRotation !== undefined);
};

/** One Auto straighten request (handleAutoStraighten): the answer belongs to
 * the photograph and the frame it was asked for and is dropped when either
 * changed meanwhile — also in a drag's draft or a rotation gesture. */
async function runAutoStraightenOnce(
  setAdjustments: (value: AdjustmentsUpdate) => unknown,
): Promise<'done' | 'none' | 'stale'> {
  // A slider drag's draft is committed first: the answer is for the frame
  // as it is shown.
  flushSliderDraft();
  const { selectedImage, adjustments } = useEditorStore.getState();
  const gestures = cropGestures();
  if (!selectedImage?.isReady || rotatingNow() || gestures.active) return 'stale';
  const asked = reframingInputs(adjustments as unknown as Record<string, unknown>);
  let result: { rotation: number; crop: Adjustments['crop'] } | null;
  try {
    result = await invoke(Invokes.CalculateAutoStraighten, { path: selectedImage.path, adjustments });
  } catch (err) {
    // The decode was another photograph's (auto_straighten.rs ANOTHER_PHOTO).
    if (String(err).includes('another photograph is open')) return 'stale';
    throw err;
  }
  const now = (draftFor(selectedImage.path) ?? useEditorStore.getState().adjustments) as unknown as Record<
    string,
    unknown
  >;
  // A crop drag since the request (its frame commits on release) too.
  const cropDragged = cropGestures().started !== gestures.started || cropGestures().active;
  if (!isCurrentEditorImage(selectedImage.path) || rotatingNow() || cropDragged || reframingInputs(now) !== asked) {
    return 'stale';
  }
  if (!result) return 'none';
  setAdjustments((prev: Adjustments) => ({ ...prev, rotation: result.rotation, crop: result.crop }));
  // The crop tool keeps this crop with this rotation (Editor's crop effect).
  useEditorStore.getState().setEditor({ autoStraightened: { rotation: result.rotation, crop: result.crop } });
  return 'done';
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
  const handleAutoStraighten = useCallback((): Promise<'done' | 'none' | 'stale'> => {
    // One analysis at a time, whichever way it was asked for (button,
    // shortcut, menu): a second request joins the running one.
    autoStraightenRunning ??= runAutoStraightenOnce(setAdjustments).finally(() => {
      autoStraightenRunning = null;
    });
    return autoStraightenRunning;
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
  const setCompareMode = useCallback((mode: CompareMode) => {
    setEditor((state) => {
      if (mode === 'off') return { splitView: false };
      const { appSettings, osPlatform } = useSettingsStore.getState();
      if (!state.selectedImage?.isReady || !splitViewAvailable(appSettings?.useWgpuRenderer, osPlatform)) return state;
      const multiView = useMultiViewStore.getState();
      if (multiView.active) multiView.setEnabled(false);
      return {
        splitView: true,
        splitOrientation: mode,
        ...(state.showOriginal ? { showOriginal: false, previewOverride: null } : {}),
      };
    });
  }, [setEditor]);
  const toggleSplitView = useCallback(() => {
    const state = useEditorStore.getState();
    setCompareMode(state.splitView ? 'off' : state.splitOrientation);
  }, [setCompareMode]);
  const cycleCompareMode = useCallback(() => {
    const state = useEditorStore.getState();
    setCompareMode(!state.splitView ? 'left-right' : state.splitOrientation === 'left-right' ? 'top-bottom' : 'off');
  }, [setCompareMode]);

  const handleLutSelect = useCallback(
    async (path: string, isBuiltIn: boolean = false, displayName?: string) => {
      const image = useEditorStore.getState().selectedImage;
      if (!image?.isReady) return;
      try {
        const result: { size: number } = await invoke(Invokes.LoadAndParseLut, { path });
        const name = displayName ?? (path.split(/[\\/]/).pop() || 'LUT');
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
      // The last edit stays a step of its own, before the reset.
      debouncedSetHistory.flush();

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
            debouncedSetHistory.flush();
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
          invoke(Invokes.ApplyAutoLensCorrectionToPaths, { paths: pathsToUpdate }),
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
        .catch((err) => toast.error(`Failed to apply auto lens correction: ${err}`));
    },
    [setEditor],
  );

  /** Applies some adjustments to `paths` (default: the selection, else the open
   * photo), saving each recipe after its pending save; resolves once saved.
   * `openingNow`: the open photograph has only just been selected (its
   * loader has not read its recipe yet), so it may be written too; it then
   * reads its recipe after this write. `pasteFrom`: copied or synced from
   * that photograph, so the layers join each photograph's own and their AI
   * detections wait for each photograph (`masks::paste_layers`);
   * `replaceLayers`: they replace them (Edit all selected). */
  const applyAdjustmentsToPaths = useCallback(
    (
      partial: Partial<Adjustments>,
      paths?: string[],
      {
        openingNow = false,
        pasteFrom,
        replaceLayers = false,
        asChosen = false,
        style = null,
      }: {
        openingNow?: boolean;
        pasteFrom?: string | null;
        replaceLayers?: boolean;
        asChosen?: boolean;
        style?: StyleOp | null;
      } = {},
    ) => {
      // Grain from before the structure choice (old presets, other photos)
      // keeps rendering Even on the photos it is applied to. The processing
      // versions values mean go along, unless the settings were chosen in the
      // picker (`asChosen`): those carry them only when chosen there, else
      // each photograph keeps its own (a never-edited one: a new edit's).
      const grain = withGrainStructure(partial);
      const adjustmentsToApply = asChosen
        ? grain
        : withClarityVersion(withDiffractionVersion(withLumaNoiseVersion(withSharpenVersion(withAiDenoiseVersion(withLegacyGrainVersion(grain))))));
      const { selectedImage, adjustments } = useEditorStore.getState();
      const { multiSelectedPaths } = useLibraryStore.getState();
      const pathsToUpdate =
        paths || (multiSelectedPaths.length > 0 ? multiSelectedPaths : selectedImage ? [selectedImage.path] : []);
      if (pathsToUpdate.length === 0) return;
      if (!openingNow && selectedImage && !selectedImage.isReady && pathsToUpdate.includes(selectedImage.path)) return;

      pathsToUpdate.forEach((p) => globalImageCache.delete(p));

      // Copied or synced settings (`pasteFrom`): the open photograph gets
      // them as an edit at once, made by the rules the backend pastes onto
      // the others with (`pastedOpen`), saved as every edit.
      const open = selectedImage?.isReady && pathsToUpdate.includes(selectedImage.path) ? selectedImage : null;
      const paste = pasteFrom
        ? { source: pasteFrom, replaceLayers, ...(asChosen ? { newEdit: newEditResponses() } : {}) }
        : null;
      const presetWithAiMasks = adjustmentsToApply.masks?.some((layer) =>
        layer.subMasks.some((sub) => DETECTED_TYPES.has(sub.type)),
      );
      // A style is taken back from the open photo by its editor's recipe
      // (the saved one may be behind it).
      const pastedOntoOpen = Boolean(open && (paste || presetWithAiMasks || style));
      if (open && pastedOntoOpen) {
        // At once, as one step: a drag's moves since the last commit are
        // part of the edit it is pasted onto.
        flushSliderDraft();
        const current = useEditorStore.getState().adjustments;
        const styleHere = style?.remove && appliedStyleOf(current)?.id !== style.id ? null : style;
        // Preset layers replace the photo's layers, with the same fresh
        // detections as paste. Save the open photo through its editor only:
        // a parallel batch write could replace a newly completed detection.
        const merged = style && !styleHere ? null : pastedOpen(open, current, adjustmentsToApply, paste, styleHere);
        if (merged) {
          // (Taking a style back leaves the lens: it is the photo's own.)
          const unresolved = withPastedLensUnresolved(merged, current, adjustmentsToApply);
          // The style's record, of the recipe as shown (its lens data waiting).
          const next =
            styleHere && !styleHere.remove
              ? recordStyle(revertStyle(current).recipe, unresolved, adjustmentsToApply, styleHere.id)
              : unresolved;
          debouncedSetHistory.flush();
          setAdjustments(() => next);
          // Its own step also during a drag (not one of its moves).
          flushSliderDraft();
          debouncedSetHistory.flush();
          const pastedStep = useEditorStore.getState().adjustments;
          if (unresolved !== merged) void resolvePastedLens(open.path, pastedStep);
        }
      } else if (open) {
        setAdjustments({ ...adjustments, ...adjustmentsToApply });
      }
      const others = pastedOntoOpen ? pathsToUpdate.filter((path) => path !== open!.path) : pathsToUpdate;
      if (others.length === 0) return Promise.resolve();
      const written = afterRecipeSaves(others, () =>
        savedBatch(
          invoke(Invokes.ApplyAdjustmentsToPaths, {
            paths: others,
            adjustments: adjustmentsToApply,
            ...(paste ? { paste } : {}),
            ...(style ? { style } : {}),
          }),
          others,
          'paste adjustments',
        ),
      );
      // A photograph opened meanwhile reads its recipe after this write.
      const settled = written.then(() => undefined, () => undefined);
      others.forEach((path) => {
        pendingMetadataSaves.set(path, settled);
        void settled.then(() => {
          if (pendingMetadataSaves.get(path) === settled) pendingMetadataSaves.delete(path);
        });
      });
      return written
        .then((failed) => {
          const updated = others.filter((path) => !failed.includes(path));
          updated.forEach((path) => unsavedRecipes.delete(path));
          if (!pastedOntoOpen && selectedImage && updated.includes(selectedImage.path)
            && isCurrentEditorImage(selectedImage.path)) {
            invoke<ImageMetadata>(Invokes.LoadMetadata, { path: selectedImage.path }).then((meta) => {
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

  /** Lightroom's Copy Settings: the picker (which tools, which layers; the
   * choice is remembered) puts the chosen settings of the open photo (or of
   * `pathOrEvent`, else the Library's active one) on the clipboard. `quick`
   * (Option-click): with the choice of last time, without the picker. */
  const handleCopyAdjustments = useCallback(
    async (pathOrEvent?: string | SyntheticEvent, { quick = false }: { quick?: boolean } = {}) => {
      const pathOverride = typeof pathOrEvent === 'string' ? pathOrEvent : undefined;
      const source = await settingsSource(pathOverride);
      if (!source) return;
      const copy = (choice: SettingsChoice) => {
        rememberChoice(choice, source.path);
        const chosen = withSourceFrames(chosenSettings(source.adjustments, choice.keys, choice.layerIds), source);
        useEditorStore.getState().setEditor({ copiedAdjustments: chosen as Adjustments, copiedFrom: source.path });
        useProcessStore.getState().setProcess({ isCopied: true });
      };
      if (quick) copy(rememberedChoice(source));
      else
        useCopySettingsDialog.getState().open({
          mode: 'copy',
          source,
          targets: [],
          remembered: rememberedKeys(),
          rememberedLayers: rememberedChoice(source).layerIds,
          onChoose: copy,
        });
    },
    [],
  );

  /** Pastes the clipboard (exactly the settings chosen when copying) onto
   * `paths` (default: the selection, else the open photo). */
  const handlePasteAdjustments = useCallback(
    (paths?: string[]) => {
      const { copiedAdjustments, copiedFrom } = useEditorStore.getState();
      const { setProcess } = useProcessStore.getState();
      if (!copiedAdjustments) return;
      if (Object.keys(copiedAdjustments).length > 0) {
        applyAdjustmentsToPaths(copiedAdjustments, paths, { pasteFrom: copiedFrom, asChosen: true });
        noteTool('copySettings');
      }
      setProcess({ isPasted: true });
    },
    [applyAdjustmentsToPaths],
  );

  /** Lightroom's Sync Settings: the open photo's chosen settings onto the
   * other selected photographs now (the same picker; `quick`: the choice of
   * last time). */
  const handleSyncSettings = useCallback(
    async ({ quick = false }: { quick?: boolean } = {}) => {
      const open = useEditorStore.getState().selectedImage;
      if (!open?.isReady) return;
      const targets = useLibraryStore.getState().multiSelectedPaths.filter((path) => path !== open.path);
      if (targets.length === 0) return;
      const source = await settingsSource(open.path);
      if (!source) return;
      const sync = (choice: SettingsChoice) => {
        rememberChoice(choice, source.path);
        const chosen = withSourceFrames(chosenSettings(source.adjustments, choice.keys, choice.layerIds), source);
        if (Object.keys(chosen).length > 0) {
          applyAdjustmentsToPaths(chosen, targets, { pasteFrom: source.path, asChosen: true });
          noteTool('copySettings');
        }
      };
      if (quick) sync(rememberedChoice(source));
      else
        useCopySettingsDialog.getState().open({
          mode: 'sync',
          source,
          targets,
          remembered: rememberedKeys(),
          rememberedLayers: rememberedChoice(source).layerIds,
          onChoose: sync,
        });
    },
    [applyAdjustmentsToPaths],
  );

  /** Lightroom's Previous: the settings of the photograph open before this
   * one, as last chosen in the picker, onto `paths` (default: the
   * selection, else the open photo). */
  const handlePasteFromPrevious = useCallback(
    async (paths?: string[]) => {
      const { previousPhoto, selectedImage } = useEditorStore.getState();
      if (!previousPhoto || previousPhoto === selectedImage?.path) return;
      // The photographs meant now, not those selected when the source is read.
      const { multiSelectedPaths } = useLibraryStore.getState();
      const targets =
        paths ?? (multiSelectedPaths.length > 0 ? multiSelectedPaths : selectedImage ? [selectedImage.path] : []);
      const source = await settingsSource(previousPhoto);
      if (!source) return;
      const choice = rememberedChoice(source);
      const applied = withSourceFrames(chosenSettings(source.adjustments, choice.keys, choice.layerIds), source);
      if (Object.keys(applied).length > 0) applyAdjustmentsToPaths(applied, targets, { pasteFrom: source.path, asChosen: true });
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
    handleLutSelect,
    setLutPreviewOverride,
    handleResetAdjustments,
    handleAutoLensCorrection,
    handleCopyAdjustments,
    handlePasteAdjustments,
    handleSyncSettings,
    handlePasteFromPrevious,
    applyAdjustmentsToPaths,
    handleZoomChange,
    toggleSplitView,
    setCompareMode,
    cycleCompareMode,
  };
}
