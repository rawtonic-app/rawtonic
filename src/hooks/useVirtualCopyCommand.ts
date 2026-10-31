import { useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import i18n from 'i18next';
import { notify } from '../activity/notify';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { registerAppCommands } from '../utils/appCommands';

/** The photo a virtual copy is made of: the one selected in the Library, or
 * the open one elsewhere; none with several selected. */
function sourcePhoto(): string | null {
  if (useUIStore.getState().activeView === 'library') {
    const selected = useLibraryStore.getState().multiSelectedPaths;
    return selected.length === 1 ? selected[0] : null;
  }
  return useEditorStore.getState().selectedImage?.path ?? null;
}

/** Lightroom's ⌘' (Create Virtual Copy): the key and menu item for what the
 * thumbnail menu's Duplicate photo › Virtual copy does. */
export function useVirtualCopyCommand(refreshImageList: () => Promise<void>) {
  const refresh = useRef(refreshImageList);
  refresh.current = refreshImageList;
  useEffect(
    () =>
      registerAppCommands({
        virtual_copy: {
          enabled: () => sourcePhoto() !== null,
          run: () => {
            const source = sourcePhoto();
            if (!source) return;
            const albumId = useLibraryStore.getState().activeAlbumId;
            void (async () => {
              try {
                await invoke(Invokes.CreateVirtualCopy, { sourceVirtualPath: source, targetAlbumId: albumId || null });
                await refresh.current();
              } catch (err) {
                notify.error(i18n.t('contextMenus.toasts.failedCreateVirtualCopy', { err }));
              }
            })();
          },
        },
      }),
    [],
  );
}
