import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import i18next from 'i18next';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { useProcessStore } from '../store/useProcessStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { ImageFile, Invokes } from '../components/ui/AppProperties';
import { followPathMoves, type PathMove } from '../utils/pathKeyedState';
import { recountAfterFileOperation } from '../utils/offline';
import { flushRecipeSaves } from './useEditorActions';
import { isBelow } from '../utils/catalogFolders';

/** Mirrors `OperationSummary` in src-tauri/src/file_operations.rs. */
export interface FileOperationSummary {
  id: string;
  kind: string;
  label: string;
  items: number;
  sessionTrash: number;
  systemTrash: number;
  newPaths: string[];
  /** Photos and folders now elsewhere, in the direction just applied (an
   * undo reverses its operation). */
  moves: PathMove[];
}

/** Before files change place: the open photograph's recipe is written to
 * where it is now, not after it moved. */
async function saveEditsFirst() {
  try {
    await flushRecipeSaves();
  } catch (err) {
    console.error('Saving edits before the file operation failed:', err);
  }
}

export const undoKey = () => (useSettingsStore.getState().osPlatform === 'macos' ? '⌘Z' : 'Ctrl+Z');
export const redoKey = () => (useSettingsStore.getState().osPlatform === 'macos' ? '⇧⌘Z' : 'Ctrl+Y');

/** Mirrors `TrashContents` in src-tauri/src/file_operations.rs. */
export interface TrashContents {
  /** Photographs and virtual copies. */
  photos: number;
  /** Every file, sidecars and XMP included. */
  files: number;
  /** Top-level files and folders. */
  entries: number;
  /** Set when some files could not be deleted yet. */
  note?: string | null;
}

/** The Empty Trash dialog's text: what goes for good, and that it cannot be undone. */
export function describeEmptyTrash(contents: TrashContents): string {
  const t = i18next.t.bind(i18next);
  const what =
    contents.photos > 0
      ? `${t('studio.library.trash.messagePhotos', { count: contents.photos })} ${t('studio.library.trash.messageFiles', { count: contents.files })}`
      : contents.files > 0
        ? t('studio.library.trash.messageOnlyFiles', { count: contents.files })
        : t('studio.library.trash.messageEmptyFolders');
  return `${what} ${t('studio.library.trash.cannotUndo')}`;
}

/** What a finished file operation did, with how to undo it. */
export function describeFileOperation(summary: FileOperationSummary, key: string): string {
  if (summary.kind === 'trash') {
    const where =
      summary.systemTrash === 0
        ? "the session's Trash folder"
        : summary.sessionTrash === 0
          ? 'the Trash'
          : "the session's Trash folder and the Trash";
    const what = summary.items === 1 ? '1 item' : `${summary.items} items`;
    return `Moved ${what} to ${where}. ${key} undoes it.`;
  }
  if (summary.kind === 'move' && summary.newPaths.length) {
    const folder = summary.newPaths[0].split(/[\\/]/).slice(-2, -1)[0];
    const what = summary.newPaths.length === 1 ? '1 photo' : `${summary.newPaths.length} photos`;
    return `Moved ${what} to ${folder}. ${key} undoes it.`;
  }
  return `${summary.label}. ${key} undoes it.`;
}

export function useFileOperations(
  refreshImageList: () => Promise<void>,
  refreshAllFolderTrees: () => Promise<void>,
  handleImageSelect: (path: string, openInEditor?: boolean) => void,
  handleBackToLibrary: () => void,
  sortedImageList: ImageFile[],
) {
  /**
   * Renames, moves and their undo: the path-keyed state follows the moved
   * photographs and folders at once (`followPathMoves`). Returns the step to
   * run once the list was read again: the open photograph opens at its new
   * place — in Develop, or in the Library without leaving it.
   */
  const followMoves = useCallback(
    (moves: PathMove[]) => {
      const { openPhoto } = followPathMoves(moves);
      return () => {
        if (openPhoto && useEditorStore.getState().selectedImage) {
          handleImageSelect(openPhoto, useUIStore.getState().activeView === 'editor');
        }
      };
    },
    [handleImageSelect],
  );

  const getParentDir = (filePath: string): string => {
    const separator = filePath.includes('/') ? '/' : '\\';
    const lastSeparatorIndex = filePath.lastIndexOf(separator);
    if (lastSeparatorIndex === -1) return '';
    return filePath.substring(0, lastSeparatorIndex);
  };

  const executeDelete = useCallback(
    async (pathsToDelete: Array<string>, options = { includeAssociated: false }) => {
      if (!pathsToDelete || pathsToDelete.length === 0) return;

      const { libraryActivePath, setLibrary } = useLibraryStore.getState();
      const { selectedImage } = useEditorStore.getState();
      const { activeView } = useUIStore.getState(); // <-- Check active view

      // Determine the active path based on the current view
      const activePath = selectedImage && activeView === 'editor' ? selectedImage.path : libraryActivePath;
      let nextImagePath: string | null = null;

      if (activePath) {
        const physicalPath = activePath.split('?vc=')[0];
        const isActiveImageDeleted = pathsToDelete.some((p) => p === activePath || p === physicalPath);

        if (isActiveImageDeleted) {
          const currentIndex = sortedImageList.findIndex((img) => img.path === activePath);
          if (currentIndex !== -1) {
            const nextCandidate = sortedImageList
              .slice(currentIndex + 1)
              .find((img) => !pathsToDelete.includes(img.path));

            if (nextCandidate) {
              nextImagePath = nextCandidate.path;
            } else {
              const prevCandidate = sortedImageList
                .slice(0, currentIndex)
                .reverse()
                .find((img) => !pathsToDelete.includes(img.path));

              if (prevCandidate) {
                nextImagePath = prevCandidate.path;
              }
            }
          }
        } else {
          nextImagePath = activePath;
        }
      }

      try {
        const command = options.includeAssociated ? 'delete_files_with_associated' : 'delete_files_from_disk';
        const summary = await invoke<FileOperationSummary>(command, { paths: pathsToDelete });
        if (summary) toast.success(describeFileOperation(summary, undoKey()));
        await refreshImageList();
        await refreshAllFolderTrees();
        void recountAfterFileOperation();

        if (selectedImage && activeView === 'editor') {
          const physicalPath = selectedImage.path.split('?vc=')[0];
          const isFileBeingEditedDeleted = pathsToDelete.some((p) => p === selectedImage.path || p === physicalPath);

          if (isFileBeingEditedDeleted) {
            if (nextImagePath) {
              handleImageSelect(nextImagePath);
            } else {
              handleBackToLibrary();
            }
          }
        } else {
          if (nextImagePath) {
            setLibrary({ multiSelectedPaths: [nextImagePath], libraryActivePath: nextImagePath });
          } else {
            setLibrary({ multiSelectedPaths: [], libraryActivePath: null });
          }

          if (selectedImage) {
            const physicalPath = selectedImage.path.split('?vc=')[0];
            if (pathsToDelete.some((p) => p === selectedImage.path || p === physicalPath)) {
              useEditorStore.getState().setEditor({ selectedImage: null });
            }
          }
        }
      } catch (err) {
        console.error('Failed to delete files:', err);
        toast.error(`Failed to delete files: ${err}`);
      }
    },
    [refreshImageList, refreshAllFolderTrees, handleBackToLibrary, sortedImageList, handleImageSelect],
  );

  const handleUndoFileOperation = useCallback(async () => {
    try {
      await saveEditsFirst();
      const summary = await invoke<FileOperationSummary | null>(Invokes.UndoFileOperation);
      if (!summary) {
        toast.info('There is no file operation to undo.');
        return;
      }
      // Renamed and moved photographs and folders are back at their old
      // places: the selection, the open photograph, picks and thumbnails
      // go with them, as after a rename.
      const reopen = followMoves(summary.moves ?? []);
      toast.success(i18next.t('studio.library.fileOperations.undone', { label: summary.label, key: redoKey() }));
      await refreshImageList();
      await refreshAllFolderTrees();
      reopen();
      void recountAfterFileOperation();
    } catch (err) {
      toast.error(`Could not undo: ${err}`);
    }
  }, [refreshImageList, refreshAllFolderTrees, followMoves]);

  /** Redoes the file operation undone last (validated again; refused when
   * something took a place since). */
  const handleRedoFileOperation = useCallback(async () => {
    try {
      await saveEditsFirst();
      const summary = await invoke<FileOperationSummary | null>(Invokes.RedoFileOperation);
      if (!summary) {
        toast.info(i18next.t('studio.library.fileOperations.nothingToRedo'));
        return;
      }
      // As after the operation itself: path-keyed state follows the moves.
      const reopen = followMoves(summary.moves ?? []);
      toast.success(i18next.t('studio.library.fileOperations.redone', { label: summary.label, key: undoKey() }));
      await refreshImageList();
      await refreshAllFolderTrees();
      reopen();
      void recountAfterFileOperation();
    } catch (err) {
      toast.error(i18next.t('studio.library.fileOperations.redoFailed', { error: String(err) }));
    }
  }, [refreshImageList, refreshAllFolderTrees, followMoves]);

  /** Empties the session's Trash folder for good, after a dialog naming
   * what goes and saying it cannot be undone. */
  const handleEmptySessionTrash = useCallback(async () => {
    const { sessionDirectory } = useLibraryStore.getState();
    if (!sessionDirectory) return;
    const t = i18next.t.bind(i18next);
    let contents: TrashContents;
    try {
      contents = await invoke<TrashContents>(Invokes.SessionTrashContents, { session: sessionDirectory });
    } catch (err) {
      toast.error(t('studio.library.trash.countFailed', { error: String(err) }));
      return;
    }
    if (!contents || contents.entries === 0) {
      toast.info(t('studio.library.trash.alreadyEmpty'));
      return;
    }
    const separator = sessionDirectory.includes('\\') && !sessionDirectory.includes('/') ? '\\' : '/';
    const trashFolder = `${sessionDirectory}${separator}Trash`;
    const inTrash = (path: string) => isBelow(path.split('?vc=')[0], trashFolder);
    useUIStore.getState().setUI({
      confirmModalState: {
        isOpen: true,
        title: t('studio.library.trash.title'),
        message: describeEmptyTrash(contents),
        confirmText: t('studio.library.trash.confirm'),
        confirmVariant: 'destructive',
        onConfirm: async () => {
          try {
            await saveEditsFirst();
            const result = await invoke<TrashContents>(Invokes.EmptySessionTrash, {
              session: sessionDirectory,
              photos: contents.photos,
              files: contents.files,
              entries: contents.entries,
            });
            if (result?.note) {
              toast.warn(t('studio.library.trash.incomplete', { note: result.note }), { autoClose: false });
            } else if (contents.photos > 0) {
              toast.success(t('studio.library.trash.emptied', { count: contents.photos }));
            } else {
              toast.success(t('studio.library.trash.emptiedNow'));
            }
            // Nothing from the Trash stays selected or open.
            const { selectedImage, setEditor } = useEditorStore.getState();
            if (selectedImage && inTrash(selectedImage.path)) {
              if (useUIStore.getState().activeView === 'editor') handleBackToLibrary();
              setEditor({ selectedImage: null });
            }
            const library = useLibraryStore.getState();
            const kept = library.multiSelectedPaths.filter((p) => !inTrash(p));
            library.setLibrary({
              multiSelectedPaths: kept,
              libraryActivePath:
                library.libraryActivePath && inTrash(library.libraryActivePath) ? null : library.libraryActivePath,
              ...(library.currentFolderPath && isBelow(library.currentFolderPath, trashFolder)
                ? { currentFolderPath: trashFolder }
                : {}),
            });
            await refreshAllFolderTrees();
            await refreshImageList();
            void recountAfterFileOperation();
          } catch (err) {
            toast.error(t('studio.library.trash.failed', { error: String(err) }));
          }
        },
      },
    });
  }, [refreshImageList, refreshAllFolderTrees, handleBackToLibrary]);

  /** Moves photos (with their sidecars and virtual copies) into `destination`. */
  const handleMoveToFolder = useCallback(
    async (paths: Array<string>, destination: string) => {
      if (paths.length === 0) return;
      try {
        await saveEditsFirst();
        const summary = await invoke<FileOperationSummary>(Invokes.MoveFiles, {
          sourcePaths: paths,
          destinationFolder: destination,
        });
        if (summary) toast.success(describeFileOperation(summary, undoKey()));
        const { selectedImage } = useEditorStore.getState();
        if (selectedImage && paths.some((p) => p.split('?vc=')[0] === selectedImage.path.split('?vc=')[0])) {
          if (useUIStore.getState().activeView === 'editor') handleBackToLibrary();
          useEditorStore.getState().setEditor({ selectedImage: null });
        }
        // They left the shown folder: nothing stays selected; picks,
        // ratings and thumbnails go with them.
        useLibraryStore.getState().setLibrary({ multiSelectedPaths: [], libraryActivePath: null });
        followMoves(summary?.moves ?? []);
        await refreshImageList();
        await refreshAllFolderTrees();
        void recountAfterFileOperation();
      } catch (err) {
        toast.error(`Could not move: ${err}`);
      }
    },
    [refreshImageList, refreshAllFolderTrees, handleBackToLibrary, followMoves],
  );

  /** Capture One's Move to Selects (⌘J): the selection into the session's Selects folder. */
  const handleMoveToSelects = useCallback(async () => {
    const { sessionDirectory, multiSelectedPaths, libraryActivePath } = useLibraryStore.getState();
    const { selectedImage } = useEditorStore.getState();
    const { activeView } = useUIStore.getState();
    if (!sessionDirectory) {
      toast.info('Move to Selects works in sessions.');
      return;
    }
    const paths =
      activeView === 'editor' && selectedImage
        ? [selectedImage.path]
        : multiSelectedPaths.length
          ? multiSelectedPaths
          : libraryActivePath
            ? [libraryActivePath]
            : [];
    const separator = sessionDirectory.includes('\\') ? '\\' : '/';
    const selects = `${sessionDirectory}${separator}Selects`;
    const outside = paths.filter((p) => getParentDir(p.split('?vc=')[0]) !== selects);
    if (outside.length === 0) {
      toast.info(paths.length ? 'Already in Selects.' : 'Select photographs to move to Selects.');
      return;
    }
    await handleMoveToFolder(outside, selects);
  }, [handleMoveToFolder]);

  /** Moves a folder to the Trash (a session's own Trash folder inside a session). */
  const handleTrashFolder = useCallback(
    (path: string) => {
      const name = path.split(/[\\/]/).pop();
      useUIStore.getState().setUI({
        confirmModalState: {
          confirmText: 'Move to Trash',
          confirmVariant: 'destructive',
          isOpen: true,
          message: `Move the folder "${name}" and everything in it to the Trash? In a session it goes to its Trash folder. ${undoKey()} undoes it.`,
          onConfirm: async () => {
            try {
              const summary = await invoke<FileOperationSummary>(Invokes.DeleteFolder, { path });
              if (summary) toast.success(describeFileOperation(summary, undoKey()));
              const { currentFolderPath, setLibrary } = useLibraryStore.getState();
              if (currentFolderPath === path || currentFolderPath?.startsWith(path + '/')) {
                setLibrary({ currentFolderPath: null, imageList: [], multiSelectedPaths: [], libraryActivePath: null });
              }
              await refreshAllFolderTrees();
              await refreshImageList();
              void recountAfterFileOperation();
            } catch (err) {
              toast.error(`Could not move the folder to the Trash: ${err}`);
            }
          },
          title: 'Move Folder to Trash',
        },
      });
    },
    [refreshAllFolderTrees, refreshImageList],
  );

  const handleDeleteSelected = useCallback(() => {
    const { multiSelectedPaths, imageList } = useLibraryStore.getState();
    const { setUI } = useUIStore.getState();

    const pathsToDelete = multiSelectedPaths;
    if (pathsToDelete.length === 0) {
      return;
    }

    const isSingle = pathsToDelete.length === 1;

    const selectionHasVirtualCopies =
      isSingle &&
      !pathsToDelete[0].includes('?vc=') &&
      imageList.some((image) => image.path.startsWith(`${pathsToDelete[0]}?vc=`));

    let modalTitle = 'Move to Trash';
    let modalMessage: string;
    let confirmText: string;

    if (selectionHasVirtualCopies) {
      modalTitle = 'Move Image and All Virtual Copies to the Trash?';
      modalMessage = `Move this image and all of its virtual copies to the Trash? In a session they go to its Trash folder. ${undoKey()} undoes it.`;
      confirmText = 'Move All';
    } else if (isSingle) {
      modalMessage = `Move this image to the Trash? In a session it goes to its Trash folder. ${undoKey()} undoes it. Right-click for more options (e.g., deleting associated files).`;
      confirmText = 'Move Selected Only';
    } else {
      modalMessage = `Move these ${pathsToDelete.length} images to the Trash? In a session they go to its Trash folder. ${undoKey()} undoes it. Right-click for more options (e.g., deleting associated files).`;
      confirmText = 'Move Selected Only';
    }

    setUI({
      confirmModalState: {
        confirmText,
        confirmVariant: 'destructive',
        isOpen: true,
        message: modalMessage,
        onConfirm: () => executeDelete(pathsToDelete, { includeAssociated: false }),
        title: modalTitle,
      },
    });
  }, [executeDelete]);

  const handleCreateFolder = useCallback(
    async (folderName: string) => {
      const { folderActionTarget } = useUIStore.getState();

      if (folderName && folderName.trim() !== '' && folderActionTarget) {
        try {
          await invoke(Invokes.CreateFolder, { path: `${folderActionTarget}/${folderName.trim()}` });
          await refreshAllFolderTrees();
        } catch (err) {
          toast.error(`Failed to create folder: ${err}`);
        }
      }
    },
    [refreshAllFolderTrees],
  );

  const handleRenameFolder = useCallback(
    async (newName: string) => {
      const { folderActionTarget } = useUIStore.getState();

      if (newName && newName.trim() !== '' && folderActionTarget) {
        try {
          const oldPath = folderActionTarget;
          const trimmedNewName = newName.trim();

          await saveEditsFirst();
          const summary = await invoke<FileOperationSummary>(Invokes.RenameFolder, {
            path: oldPath,
            newName: trimmedNewName,
          });
          if (summary) toast.success(describeFileOperation(summary, undoKey()));

          const parentDir = getParentDir(oldPath);
          const separator = oldPath.includes('/') ? '/' : '\\';
          const newPath = parentDir ? `${parentDir}${separator}${trimmedNewName}` : trimmedNewName;
          // The shown, root and pinned folders, the selection, the open
          // photograph, picks and thumbnails follow the folder.
          const reopen = followMoves(summary?.moves?.length ? summary.moves : [{ from: oldPath, to: newPath }]);

          await refreshAllFolderTrees();
          // The photos in it have new paths; the list and its picks are read again.
          await refreshImageList();
          reopen();
          void recountAfterFileOperation();
        } catch (err) {
          toast.error(`Failed to rename folder: ${err}`);
        }
      }
    },
    [refreshAllFolderTrees, refreshImageList, followMoves],
  );

  const handleSaveRename = useCallback(
    async (nameTemplate: string) => {
      const { renameTargetPaths, setUI } = useUIStore.getState();

      if (renameTargetPaths.length > 0 && nameTemplate) {
        try {
          await saveEditsFirst();
          const newPaths: Array<string> = await invoke(Invokes.RenameFiles, {
            nameTemplate,
            paths: renameTargetPaths,
          });
          // The selection, the open photograph, picks, ratings and
          // thumbnails stay with the renamed photos (as on undo).
          const moves = renameTargetPaths.flatMap((from, i) => (newPaths[i] ? [{ from, to: newPaths[i] }] : []));
          const reopen = followMoves(moves);
          toast.success(`Renamed. ${undoKey()} undoes it.`);
          useLibraryStore.getState().setLibrary({ multiSelectedPaths: newPaths });

          await refreshImageList();
          reopen();
          void recountAfterFileOperation();
        } catch (err) {
          toast.error(`Failed to rename files: ${err}`);
        }
      }
      setUI({ renameTargetPaths: [] });
    },
    [refreshImageList, followMoves],
  );

  const handleRenameFiles = useCallback((paths: Array<string>) => {
    if (paths && paths.length > 0) {
      useUIStore.getState().setUI({ renameTargetPaths: paths, isRenameFileModalOpen: true });
    }
  }, []);

  /** Pastes the copied photos into `destination` (the shown folder by default). */
  const handlePasteFiles = useCallback(
    async (mode = 'copy', destination?: string) => {
      const { copiedFilePaths, setProcess } = useProcessStore.getState();
      const { currentFolderPath, setLibrary } = useLibraryStore.getState();
      const target = destination ?? currentFolderPath;

      if (copiedFilePaths.length === 0 || !target || target.startsWith('Album: ')) return;

      try {
        if (mode === 'copy') {
          await invoke(Invokes.CopyFiles, { sourcePaths: copiedFilePaths, destinationFolder: target });
          if (target !== currentFolderPath) await refreshAllFolderTrees();
        } else {
          await saveEditsFirst();
          const summary = await invoke<FileOperationSummary>(Invokes.MoveFiles, {
            sourcePaths: copiedFilePaths,
            destinationFolder: target,
          });
          if (summary) toast.success(describeFileOperation(summary, undoKey()));
          setProcess({ copiedFilePaths: [] });
          setLibrary({ multiSelectedPaths: [] });
          const reopen = followMoves(summary?.moves ?? []);
          await refreshAllFolderTrees();
          await refreshImageList();
          reopen();
          void recountAfterFileOperation();
          return;
        }
        await refreshImageList();
      } catch (err) {
        toast.error(`Failed to ${mode} files: ${err}`);
      }
    },
    [refreshImageList, refreshAllFolderTrees, followMoves],
  );

  return {
    executeDelete,
    handleDeleteSelected,
    handleUndoFileOperation,
    handleRedoFileOperation,
    handleEmptySessionTrash,
    handleMoveToFolder,
    handleMoveToSelects,
    handleTrashFolder,
    handleCreateFolder,
    handleRenameFolder,
    handleSaveRename,
    handleRenameFiles,
    handlePasteFiles,
  };
}
