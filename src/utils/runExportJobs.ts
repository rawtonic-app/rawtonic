import i18n from 'i18next';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { toast } from 'react-toastify';
import { Invokes } from '../components/ui/AppProperties';
import { Status, type ExportPreset } from '../components/ui/ExportImportProperties';
import { useSettingsStore } from '../store/useSettingsStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore } from '../store/useProcessStore';
import { currentEditForExport, useEditorStore } from '../store/useEditorStore';
import { exportSettingsFromDraft, savedRecipes, subfolderName, type RecipeDraft } from './exportRecipes';
import { exportJobLabel } from './exportFeedback';
import { noteTool } from '../components/contribute/usageStats';
import { flushRecipeSaves } from '../hooks/useEditorActions';
import { photoWritesAndStepsSettled } from './photoWrites';

/** The library an export is submitted from, as `export_images` takes it:
 * always stated, so an export from no library never takes one opened later. */
export const submittedFrom = (library: string | null) =>
  library ? { library } : 'noLibrary';

export interface PlannedExport {
  recipe: ExportPreset | null;
  draft: RecipeDraft;
  settings: ReturnType<typeof exportSettingsFromDraft>['settings'];
  outputFormat: string;
}

/** The dialog and File menu queue the same export requests and destinations.
 * Cancelling a folder picker cancels the whole export: no export is queued,
 * no error is shown (the photographer meant to stop) and the request settles
 * as 0, which returns the Export button to its normal state. */
export async function runExportJobs(planned: PlannedExport[], paths: string[]): Promise<number> {
  if (!paths.length) return 0;
  const t = i18n.t.bind(i18n);
  // Each photograph's recipe and metadata are read once, when the export is
  // queued, from the library it is queued from: that library, the editor's
  // recipe and the folders are taken together, now (a slider draft first
  // commits), and the edits and metadata (rating, label, keywords, EXIF)
  // being saved are written before it is queued.
  const flushing = Promise.all([flushRecipeSaves(), photoWritesAndStepsSettled()]).then(([unsaved]) => unsaved);
  const session = useLibraryStore.getState().sessionDirectory;
  const openEdit = currentEditForExport(useEditorStore.getState());
  const originFolders = useLibraryStore.getState().rootPaths;
  const status = useProcessStore.getState().exportState.status;
  const queued = status === Status.Exporting || status === Status.Cancelling;
  const changeRecipes = (change: (current: ExportPreset[]) => ExportPreset[]) => {
    const store = useSettingsStore.getState();
    if (!store.appSettings) return;
    const current = savedRecipes(store.appSettings.exportPresets);
    const kept = (store.appSettings.exportPresets ?? []).filter((p) => !current.some((r) => r.id === p.id));
    void store.handleSettingsChange({ ...store.appSettings, exportPresets: [...kept, ...change(current)] });
  };
  const destinationFor = async (
    job: RecipeDraft,
    extension: string,
    name: string,
    single: boolean,
  ): Promise<{ path: string; isFile: boolean } | null> => {
    if (job.where === "output" && session)
      return { path: `${session}/Output`, isFile: false };
    if (job.where === "original")
      return { path: "originalFolderDummy", isFile: false };
    if (job.folder) return { path: job.folder, isFile: false };
    // One photograph asks for a file name, unless folders are made for it.
    // Date tokens must be resolved from the source by the export engine.
    // Choosing a directory avoids saving literal {YYYY} in a single-file name.
    const needsSourceDate = /\{(?:YYYY|MM|DD|hh|mm)\}/.test(job.template);
    if (single && !needsSourceDate && !job.preserveFolders && !subfolderName(job.subfolder)) {
      const filename = paths[0]
        .split("/")
        .pop()!
        .replace(/\.[^.]+$/, "");
      const file = await save({
        title: t("studio.export.dialogs.exportFile", { name }),
        defaultPath:
          job.template
            .replaceAll("{original_filename}", filename)
            .replaceAll("{sequence}", String(job.sequenceStart)) +
          "." +
          extension,
        filters: [{ name: extension.toUpperCase(), extensions: [extension] }],
      });
      return typeof file === "string" ? { path: file, isFile: true } : null;
    }
    const chosen = await open({
      title: t("studio.export.dialogs.exportFolder", {
        count: paths.length,
        name,
      }),
      directory: true,
      multiple: false,
    });
    return typeof chosen === "string" ? { path: chosen, isFile: false } : null;
  };

  try {
    // A photograph whose edits could not be saved would export its older
    // recipe: the export stops and says which.
    const unsaved = (await flushing).filter((path) => paths.includes(path));
    if (unsaved.length)
      throw new Error(
        t("studio.export.toasts.unsavedEdits", {
          names: unsaved.map((path) => path.split(/[\\/]/).pop()).join(", "),
        }),
      );
    // Queued together, the recipes run as one export: each photograph is
    // developed once for all of them.
    const requests: Record<string, unknown>[] = [];
    for (const job of planned) {
      const destination = await destinationFor(
        job.draft,
        job.outputFormat,
        job.recipe?.name ?? t("studio.export.dialogs.customName"),
        paths.length === 1 && !job.recipe,
      );
      // The photographer stopped: no export at all, and none of the later
      // recipes' folders are asked for either, nor remembered.
      if (!destination) return 0;
      // A recipe keeps the folder chosen for it, as Capture One's do.
      if (job.recipe && job.draft.where === "choose" && !job.draft.folder) {
        const { id } = job.recipe;
        changeRecipes((current) =>
          current.map((r) =>
            r.id === id ? { ...r, lastExportPath: destination.path } : r,
          ),
        );
      }
      requests.push({
        paths,
        displayLabel: exportJobLabel(job.recipe?.name ?? t("studio.export.dialogs.customName"), job.draft, job.outputFormat, destination.path, paths),
        outputFolderOrFile: destination.path,
        isExplicitFilePath: destination.isFile,
        baseOriginFolders: originFolders,
        exportSettings: job.settings,
        outputFormat: job.outputFormat,
        ...openEdit,
        library: submittedFrom(session),
      });
    }
    const started = requests.length;
    if (started) {
      if (!queued)
        useProcessStore.getState().setExportState({
          status: Status.Exporting,
          errorMessage: "",
          progress: { current: 0, total: paths.length * started },
        });
      await invoke(Invokes.ExportImagesBatch, { requests });
      noteTool("export");
      if (paths.length > 1) noteTool("batchExport");
    }

    if (started && (queued || started > 1))
      toast.success(
        started > 1
          ? t("studio.export.toasts.queuedMany", { count: started })
          : t("studio.export.toasts.queuedOne"),
      );
    return started;
  } catch (error) {
    useProcessStore
      .getState()
      .setExportState({ status: Status.Error, errorMessage: String(error) });
    return 0;
  }
}
