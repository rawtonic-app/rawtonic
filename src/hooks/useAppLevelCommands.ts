import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-shell';
import { toast } from 'react-toastify';
import i18n from 'i18next';
import { Invokes } from '../components/ui/AppProperties';
import { SOURCE_URL } from '../components/panel/AboutSettings';
import { clearLibrarySelection } from '../components/studio/workspaceInteractions';
import { registerAppCommands } from '../utils/appCommands';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { useUpdateStore } from '../store/useUpdateStore';

/** The photograph Show in Finder reveals: the one open in Develop or the
 * full screen, else the Library's single selected one. */
export function finderTarget(): string | null {
  const { activeView, isFullScreen, libraryLoupe } = useUIStore.getState();
  const shown = useEditorStore.getState().selectedImage?.path ?? null;
  if (activeView === 'editor' || isFullScreen || libraryLoupe) return shown;
  const selected = useLibraryStore.getState().multiSelectedPaths;
  return selected.length === 1 ? selected[0] : null;
}

const openPreferences = (category: string) => useUIStore.getState().setUI({ isSettingsOpen: true, settingsCategory: category });

/** Commands of the app as a whole (About, updates, help, selection), for the
 * menu bar and their keys; each runs what its button in the app runs. */
export function useAppLevelCommands(): void {
  useEffect(
    () =>
      registerAppCommands({
        about: { run: () => openPreferences('about') },
        keyboard_shortcuts: { run: () => openPreferences('shortcuts') },
        // Preferences shows the answer (also "none found" or a failure).
        check_updates: {
          enabled: () => !useUpdateStore.getState().checking,
          run: () => {
            openPreferences('general');
            void useUpdateStore.getState().check(true);
          },
        },
        website: { run: () => void open(SOURCE_URL) },
        report_problem: { run: () => useUIStore.getState().setProblemReportOpen(true) },
        select_none: {
          enabled: () => useLibraryStore.getState().multiSelectedPaths.length > 0,
          run: clearLibrarySelection,
        },
        show_in_finder: {
          enabled: () => finderTarget() !== null,
          run: () => {
            const path = finderTarget();
            if (!path) return;
            invoke(Invokes.ShowInFinder, { path }).catch((err) =>
              toast.error(i18n.t('contextMenus.toasts.couldNotShowExplorer', { err })),
            );
          },
        },
      }),
    [],
  );
}
