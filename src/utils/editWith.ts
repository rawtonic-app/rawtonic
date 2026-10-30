import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import type { ExportPreset, ExportSettings } from '../components/ui/ExportImportProperties';
import { DEFAULT_RECIPE_DRAFT, exportSettingsFromDraft, exportSettingsFromPreset, savedRecipes } from './exportRecipes';
import { currentEditForExport, debouncedSetHistory, useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore, type ExportJob } from '../store/useProcessStore';
import { flushRecipeSaves } from '../hooks/useEditorActions';
import { photoWritesAndStepsSettled } from './photoWrites';
import { submittedFrom } from './runExportJobs';

/** A program offered by Open With / Edit With (`open_with.rs`). */
export interface AppChoice {
  path: string;
  name: string;
  isDefault: boolean;
  isRecent: boolean;
  icon: string | null;
}

export interface AppChoices {
  /** False where the system cannot list programs (only macOS so far). */
  supported: boolean;
  apps: AppChoice[];
}

export interface OpenWithPrefs {
  recentApps: string[];
  editRecipe: string | null;
}

/** An Edit With recipe: what the renders are exported with. */
export interface EditRecipe {
  id: string;
  /** A built-in recipe's colour space, or a saved recipe's own name. */
  builtIn?: 'adobeRgb' | 'proPhoto';
  name?: string;
  settings: ExportSettings;
  outputFormat: string;
}

/** Full-size 16-bit TIFFs with all metadata, the format image editors take
 * without loss; Adobe RGB unless the recipe says ProPhoto. */
const builtIn = (id: string, colorSpace: 'adobeRgb' | 'proPhoto'): EditRecipe => ({
  id,
  builtIn: colorSpace,
  ...exportSettingsFromDraft({
    ...DEFAULT_RECIPE_DRAFT,
    format: 'tiff',
    depth: 16,
    colorSpace,
    stripGps: false,
    preserveTimestamps: false,
  }),
});

export const EDIT_RECIPE_TIFF_ADOBE = 'builtin-tiff16-adobe';
export const BUILT_IN_EDIT_RECIPES: EditRecipe[] = [
  builtIn(EDIT_RECIPE_TIFF_ADOBE, 'adobeRgb'),
  builtIn('builtin-tiff16-prophoto', 'proPhoto'),
];

/** The built-in recipes, then the saved export recipes that write an image
 * (not a LUT). */
export function editRecipes(presets: ExportPreset[] | undefined): EditRecipe[] {
  return [
    ...BUILT_IN_EDIT_RECIPES,
    ...savedRecipes(presets)
      .map((preset) => ({ id: preset.id, name: preset.name, ...exportSettingsFromPreset(preset) }))
      .filter((recipe) => recipe.outputFormat !== 'cube'),
  ];
}

/** The recipe last used, or the first built-in one. */
export const chosenRecipe = (recipes: EditRecipe[], id: string | null | undefined) =>
  recipes.find((recipe) => recipe.id === id) ?? recipes[0];

interface EditTarget {
  source: string;
  target: string;
  /** The library its render's snapshot was read from (`open_with.rs`). */
  library?: string | null;
}

export interface EditWithResult {
  linked: number;
  problems: string[];
}

/** The export request of an Edit With render, without the photograph and
 * its file (`edit_with_start` names one for each), taken at the click as an
 * Export takes it: the editor's recipe for the open photograph and the
 * library it is submitted from, whose snapshot of each photograph's recipe
 * and metadata the render takes, whatever library is open later. Only
 * where the file goes differs from an Export with that recipe, so the
 * renders are the same bytes; except that Edit With writes no separate mask
 * files: a recipe that exports them gets one flattened image with every
 * layer applied (the other program edits one file), not the main image of
 * such an Export. */
export function editRequest(recipe: EditRecipe) {
  const exportSettings: ExportSettings = {
    ...recipe.settings,
    // An explicit file: not beside the original by template, no mask files.
    destinationType: 'customFolder',
    subfolder: '',
    filenameTemplate: null,
    exportMasks: false,
  };
  return {
    paths: [] as string[],
    outputFolderOrFile: '',
    isExplicitFilePath: true,
    baseOriginFolders: useLibraryStore.getState().rootPaths,
    exportSettings,
    outputFormat: recipe.outputFormat,
    ...currentEditForExport(useEditorStore.getState()),
    library: submittedFrom(useLibraryStore.getState().sessionDirectory),
  };
}

/** Waits until the editor's pending recipe saves and every queued photo
 * write (rating, label, keywords, EXIF, XMP) are written, as before an
 * Export; resolves with the photographs whose edits could not be saved. */
export async function photoWritesWritten(): Promise<string[]> {
  const [unsaved] = await Promise.all([flushRecipeSaves(), photoWritesAndStepsSettled()]);
  return unsaved;
}

const settled = (state: ExportJob['state']) => state !== 'queued' && state !== 'running' && state !== 'paused';

/** Waits until the queue has finished `jobs`: the ids that were done. Each
 * job's last seen state counts, so one the queue dropped after it finished
 * (it keeps only the latest finished jobs) is still known as done; one
 * removed while still waiting counts as not done. */
export function waitForJobs(jobs: string[]): Promise<Set<string>> {
  return new Promise((resolve) => {
    const seen = new Map<string, ExportJob['state']>();
    let stop: (() => void) | null = null;
    let ended = false;
    const check = () => {
      if (ended) return;
      const present = new Set<string>();
      for (const job of useProcessStore.getState().exportQueue) {
        if (!jobs.includes(job.id)) continue;
        present.add(job.id);
        seen.set(job.id, job.state);
      }
      const finished = jobs.every((id) => {
        const state = seen.get(id);
        return state !== undefined && (!present.has(id) || settled(state));
      });
      if (!finished) return;
      ended = true;
      stop?.();
      resolve(new Set(jobs.filter((id) => seen.get(id) === 'done')));
    };
    stop = useProcessStore.subscribe(check);
    check();
    if (ended) stop();
  });
}

export interface EditWithRun extends EditWithResult {
  rendered: number;
  failed: number;
}

/** Renders `paths` with `recipe` beside their originals, adds the renders to
 * the library stacked with their originals and opens them in `program`. The
 * editor's undo step is recorded, and the request (library, editor recipe)
 * taken at the click; the pending edits and photo writes are written before
 * it is queued, where each render's recipe and metadata are snapshotted
 * once: a library opened meanwhile changes nothing. */
export async function runEditWith(paths: string[], recipe: EditRecipe, program: string): Promise<EditWithRun> {
  debouncedSetHistory.flush();
  const written = photoWritesWritten();
  const request = editRequest(recipe);
  const unsaved = new Set(await written);
  const notSaved = paths.filter((path) => unsaved.has(path));
  if (notSaved.length) throw new Error(`Edits not saved: ${notSaved.join(', ')}`);
  const { targets, jobs } = await invoke<{ targets: EditTarget[]; jobs: string[] }>(Invokes.EditWithStart, {
    paths,
    recipe: request,
  });
  const done = await waitForJobs(jobs);
  const rendered = targets.filter((_, i) => done.has(jobs[i]));
  const counts = { rendered: rendered.length, failed: targets.length - rendered.length };
  if (!rendered.length) return { ...counts, linked: 0, problems: [] };
  const result = await invoke<EditWithResult>(Invokes.EditWithFinish, { items: rendered, program });
  return { ...counts, ...result };
}
