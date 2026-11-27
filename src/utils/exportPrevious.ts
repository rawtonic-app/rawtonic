import i18n from 'i18next';
import type { ExportPreset } from '../components/ui/ExportImportProperties';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { runExportJobs, type PlannedExport } from './runExportJobs';
import type { RecipeDraft } from './exportRecipes';
import { dirname } from './path';

/**
 * Lightroom's Export with Previous (⌥⇧⌘E): the recipes and settings of the
 * last export queued from a library, with the folders it went to, so the
 * next selection exports the same way without the dialog. Kept per library
 * in the settings (`lastExports`, keyed by the library's folder; "" for no
 * library), across launches.
 */
export interface LastExport {
  jobs: PlannedExport[];
  /** When it was queued (ISO 8601); the oldest libraries' entries go first. */
  at: string;
}

/** Libraries whose last export is kept. */
const KEPT_LIBRARIES = 20;

const libraryKey = (library: string | null) => library ?? '';

/** Where a queued export went, as a draft that goes there again without a
 * picker: a folder picked (or the folder of a file named for one photo)
 * becomes the draft's folder; the Output folder of a library and the
 * originals' folders stay as they are. */
export function replayableDraft(
  draft: RecipeDraft,
  destination: { path: string; isFile: boolean },
  library: string | null
): RecipeDraft {
  // Output without a library open asked for a folder: that one is kept.
  if ((draft.where === 'output' && library) || draft.where === 'original' || draft.folder) return draft;
  // A drive's or the POSIX root stays a root (E:\ or /), never E: or ''.
  const folder = destination.isFile ? dirname(destination.path) : destination.path;
  return folder ? { ...draft, where: 'choose', folder } : draft;
}

export function lastExportFor(library: string | null): LastExport | null {
  const entry = useSettingsStore.getState().appSettings?.lastExports?.[libraryKey(library)];
  return entry && Array.isArray(entry.jobs) && entry.jobs.length > 0 ? entry : null;
}

/** Remembers an export just queued from `library` (Save As is not one). */
export function rememberLastExport(library: string | null, jobs: PlannedExport[]): void {
  const { appSettings, handleSettingsChange } = useSettingsStore.getState();
  if (!appSettings || !jobs.length) return;
  const entries = Object.entries({
    ...(appSettings.lastExports ?? {}),
    [libraryKey(library)]: { jobs, at: new Date().toISOString() }
  })
    .sort(([, a], [, b]) => (a.at < b.at ? 1 : a.at > b.at ? -1 : 0))
    .slice(0, KEPT_LIBRARIES);
  void handleSettingsChange({ ...appSettings, lastExports: Object.fromEntries(entries) });
}

/** Whether Export with Previous can run for `paths` now, else why not (the
 * menu row's tooltip, the palette's hint). */
export function exportPreviousState(paths: string[]): { enabled: boolean; hint?: string } {
  if (!lastExportFor(useLibraryStore.getState().sessionDirectory))
    return { enabled: false, hint: i18n.t('studio.export.previous.none') };
  if (!paths.length) return { enabled: false, hint: i18n.t('studio.export.previous.noSelection') };
  return { enabled: true };
}

/** The recipes of the last export, by name, for a menu row's tooltip. */
export function lastExportSummary(): string | undefined {
  const last = lastExportFor(useLibraryStore.getState().sessionDirectory);
  if (!last) return undefined;
  const names = last.jobs.map(
    (job: { recipe: ExportPreset | null }) => job.recipe?.name ?? i18n.t('studio.export.dialogs.customName')
  );
  return i18n.t('studio.export.previous.repeats', { recipes: names.join(', ') });
}

/** Exports `paths` as the last export of the open library went: no dialog,
 * the queue's toasts as for any export. Resolves to the number of exports
 * queued (0 when there is none to repeat). */
export async function exportWithPrevious(paths: string[]): Promise<number> {
  const last = lastExportFor(useLibraryStore.getState().sessionDirectory);
  if (!last || !paths.length) return 0;
  return runExportJobs(last.jobs, paths);
}
