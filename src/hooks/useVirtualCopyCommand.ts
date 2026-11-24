import { useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import i18n from 'i18next';
import { notify } from '../activity/notify';
import { Invokes, type AlbumItem } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useUIStore } from '../store/useUIStore';
import { registerAppCommands } from '../utils/appCommands';
import { photoWritesSettled } from '../utils/photoWrites';
import { flushBeforeQuit } from '../utils/quit';
import { viewerShown } from '../utils/viewerMode';
import { fileNameOf } from '../utils/offline';
import { isCopy, sourceOf } from '../entities/photo/photoRef';
import { SESSION_ALBUM_PREFIX, SESSION_ALBUMS_CHANGED } from '../utils/catalogWrites';

/** The photo a virtual copy is made of: the one selected in the Library, or
 * the open one elsewhere; none with several selected. */
function sourcePhoto(): string | null {
  if (useUIStore.getState().activeView === 'library') {
    const selected = useLibraryStore.getState().multiSelectedPaths;
    return selected.length === 1 ? selected[0] : null;
  }
  return useEditorStore.getState().selectedImage?.path ?? null;
}

/**
 * Makes a virtual copy of `source` and shows it (UXR4M-19): the list is read
 * again, the copy is selected beside its photo (and opened when the viewer
 * shows a photo: Develop, the Library loupe or full screen, as Lightroom
 * switches to the new copy), and a message names it, so nobody presses
 * again. Returns the copy's path, or null when it failed (said as an error).
 */
export async function createVirtualCopy(
  source: string,
  refreshImageList: () => Promise<void>,
  openPhoto?: (path: string, openInEditor?: boolean) => void
): Promise<string | null> {
  const { activeAlbumId: albumId, sessionDirectory } = useLibraryStore.getState();
  const libraryAlbum =
    albumId?.startsWith(SESSION_ALBUM_PREFIX) && sessionDirectory ? albumId.slice(SESSION_ALBUM_PREFIX.length) : null;
  try {
    // The copy takes the photo's recipe as saved: edits still waiting for
    // their save (300 ms) are written first.
    const unsaved = await flushBeforeQuit();
    if (unsaved.length) throw new Error(unsaved.join(', '));
    await photoWritesSettled(source);
    const copy = await invoke<string>(Invokes.CreateVirtualCopy, {
      sourceVirtualPath: source,
      targetAlbumId: libraryAlbum ? null : albumId || null
    });
    // In an album the copy joins it, as a member of its own: its
    // membership first, then the list.
    if (libraryAlbum && copy) {
      await invoke(Invokes.SessionAlbumAdd, { directory: sessionDirectory, id: libraryAlbum, paths: [copy] });
      window.dispatchEvent(new CustomEvent(SESSION_ALBUMS_CHANGED));
    } else if (albumId) {
      const albumTree = await invoke<AlbumItem[]>(Invokes.GetAlbums);
      useLibraryStore.getState().setLibrary({ albumTree });
    }
    await refreshImageList();
    const name = fileNameOf(sourceOf(source));
    if (!copy) {
      notify.success(i18n.t('contextMenus.toasts.virtualCopyMade', { name }));
      return null;
    }
    const list = useLibraryStore.getState().imageList;
    const shown = list.some((image) => image.path === copy);
    if (shown) {
      useLibraryStore
        .getState()
        .setLibrary({ multiSelectedPaths: [copy], libraryActivePath: copy, selectionAnchorPath: copy });
      if (openPhoto && viewerShown(useUIStore.getState()))
        openPhoto(copy, useUIStore.getState().activeView === 'editor');
    }
    const number = list.filter((image) => isCopy(image.path) && sourceOf(image.path) === sourceOf(source)).length;
    const made = number
      ? i18n.t('contextMenus.toasts.virtualCopyNumbered', { name, number })
      : i18n.t('contextMenus.toasts.virtualCopyMade', { name });
    notify.success(shown ? made : `${made} ${i18n.t('contextMenus.toasts.virtualCopyNotShown')}`);
    return copy;
  } catch (err) {
    notify.error(i18n.t('contextMenus.toasts.failedCreateVirtualCopy', { err }));
    return null;
  }
}

/** Lightroom's ⌘' (Create Virtual Copy): the key and menu item for what the
 * thumbnail menu's Duplicate photo › Virtual copy does. */
export function useVirtualCopyCommand(
  refreshImageList: () => Promise<void>,
  openPhoto?: (path: string, openInEditor?: boolean) => void
) {
  const refresh = useRef(refreshImageList);
  refresh.current = refreshImageList;
  const open = useRef(openPhoto);
  open.current = openPhoto;
  useEffect(
    () =>
      registerAppCommands({
        virtual_copy: {
          enabled: () => sourcePhoto() !== null,
          run: () => {
            const source = sourcePhoto();
            if (!source) return;
            void createVirtualCopy(source, () => refresh.current(), open.current);
          }
        }
      }),
    []
  );
}
