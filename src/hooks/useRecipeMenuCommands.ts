import { useUIStore } from '../store/useUIStore';
import { viewerShown } from '../utils/viewerMode';
import { useEffect, useRef } from 'react';
import { useSettingsStore } from '../store/useSettingsStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { notifyAppCommandsChanged, registerAppCommands } from '../utils/appCommands';
import { draftFromPreset, exportSettingsFromDraft, savedRecipes } from '../utils/exportRecipes';
import { runExportJobs } from '../utils/runExportJobs';
import { exportPreviousState, exportWithPrevious } from '../utils/exportPrevious';

/** The photographs the File menu's exports act on: the selection, else the
 * one the viewer shows. */
export const exportSelection = () => {
  const selected = useLibraryStore.getState().multiSelectedPaths;
  const image = useEditorStore.getState().selectedImage;
  return selected.length ? [...selected] : viewerShown(useUIStore.getState()) && image ? [image.path] : [];
};

/** Recipe commands live outside the Export dialog, including while it is closed. */
export function useRecipeMenuCommands(): void {
  const presets = useSettingsStore((state) => state.appSettings?.exportPresets);
  const choosing = useRef(false);
  useEffect(
    () =>
      registerAppCommands(
        Object.fromEntries(
          savedRecipes(presets).map((recipe) => [
            `recipe:${recipe.id}`,
            {
              menu: { group: 'recipes' as const, label: recipe.name },
              enabled: () => !choosing.current && exportSelection().length > 0,
              run: () => {
                const paths = exportSelection();
                const draft = draftFromPreset(recipe);
                choosing.current = true;
                notifyAppCommandsChanged();
                void runExportJobs([{ recipe, draft, ...exportSettingsFromDraft(draft) }], paths).finally(() => {
                  choosing.current = false;
                  notifyAppCommandsChanged();
                });
              }
            }
          ])
        )
      ),
    [presets]
  );
  // Lightroom's Export with Previous: the last export of this library, for
  // the selection, without the dialog.
  const repeating = useRef(false);
  useEffect(
    () =>
      registerAppCommands({
        export_previous: {
          enabled: () => !repeating.current && exportPreviousState(exportSelection()).enabled,
          disabledHint: () => (repeating.current ? undefined : exportPreviousState(exportSelection()).hint),
          run: () => {
            repeating.current = true;
            notifyAppCommandsChanged();
            void exportWithPrevious(exportSelection()).finally(() => {
              repeating.current = false;
              notifyAppCommandsChanged();
            });
          }
        }
      }),
    []
  );
}
