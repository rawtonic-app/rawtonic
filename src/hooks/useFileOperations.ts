import { createElement, useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { notify } from '../activity/notify';
import { useActivityStore } from '../store/useActivityStore';
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
import {
  fileAction,
  fileOperationTravel,
  registerFileUndoHandlers,
  clearFileUndoHistory,
  undoBranchId,
  useCatalogUndoStore,
} from "../utils/appUndo";
import { isBelow } from '../utils/catalogFolders';
import type { PlannedFile } from '../components/modals/RenameFileModal';

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
  /** What the user should know, e.g. originals a move to another drive kept. */
  note?: string | null;
}

/** Mirrors `MovePlan` in src-tauri/src/file_management/operations.rs. */
export interface MovePlan {
  /** Some files are copied to another drive and their originals trashed. */
  crossVolume: boolean;
  photos: number;
  files: number;
  bytes: number;
  drive: string;
  freeBytes: number;
  sourceDrives: string[];
  refusal?: string | null;
  /** The move would take photos out of their session, which is not done:
   * the whole session can be moved there instead. */
  sessionMove?: SessionMoveOffer | null;
}

/** Mirrors `SessionMoveOffer` in src-tauri/src/session_move.rs. */
export interface SessionMoveOffer {
  session: string;
  name: string;
  destination: string;
  crossVolume: boolean;
  bytes: number;
  drive: string;
  freeBytes: number;
  /** Why the session cannot go there either. */
  refusal?: string | null;
}

/** Bytes in decimal units, as file browsers show them. */
export function formatBytes(bytes: number): string {
  const units = ['KB', 'MB', 'GB', 'TB'];
  if (bytes < 1000) return `${bytes} bytes`;
  let value = bytes;
  let unit = -1;
  while (value >= 1000 && unit + 1 < units.length) {
    value /= 1000;
    unit += 1;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}

/** The question before photos are copied to another drive: which drive,
 * how much is copied and how much is free there, where the originals go. */
export function describeCrossVolumeMove(plan: MovePlan): string {
  return i18next.t('studio.library.fileOperations.otherDriveMessage', {
    count: plan.photos,
    files: plan.files,
    size: formatBytes(plan.bytes),
    drive: plan.drive,
    free: formatBytes(plan.freeBytes),
    sources: plan.sourceDrives.join(', '),
  });
}

/** The question before a folder is copied to another drive: the same, for
 * the folder with everything in it (`plan.photos` are the photographs in it). */
export function describeCrossVolumeFolderMove(plan: MovePlan, name: string): string {
  return i18next.t('studio.library.fileOperations.otherDriveFolderMessage', {
    name,
    count: plan.photos,
    files: plan.files,
    size: formatBytes(plan.bytes),
    drive: plan.drive,
    free: formatBytes(plan.freeBytes),
    sources: plan.sourceDrives.join(', '),
  });
}

/** The question instead of moving photos out of their session: why that is
 * not done, in one sentence, and what moving the whole session involves. */
export function describeSessionMove(offer: SessionMoveOffer): string {
  const t = i18next.t.bind(i18next);
  const folder = offer.destination.split(/[\\/]/).filter(Boolean).pop() ?? offer.destination;
  const why = t('studio.library.fileOperations.sessionMoveWhy', { name: offer.name, folder });
  const how = offer.crossVolume
    ? t('studio.library.fileOperations.sessionMoveOtherDrive', {
        size: formatBytes(offer.bytes),
        drive: offer.drive,
        free: formatBytes(offer.freeBytes),
      })
    : t('studio.library.fileOperations.sessionMoveSameDrive');
  return `${why} ${how}`;
}

/** Offers to move the whole session instead of photos or a folder out of
 * it (G40): a session stays one self-contained folder. Says why when the
 * session cannot go there either; moves nothing until the user agreed. */
function offerSessionMove(offer: SessionMoveOffer) {
  const t = i18next.t.bind(i18next);
  const folder = offer.destination.split(/[\\/]/).filter(Boolean).pop() ?? offer.destination;
  if (offer.refusal) {
    notify.error(t('studio.library.fileOperations.sessionMoveRefused', { name: offer.name, error: offer.refusal }));
    return;
  }
  useUIStore.getState().setUI({
    confirmModalState: {
      isOpen: true,
      title: t('studio.library.fileOperations.sessionMoveTitle', { name: offer.name }),
      message: describeSessionMove(offer),
      confirmText: t('studio.library.fileOperations.sessionMoveConfirm', { folder }),
      confirmVariant: 'primary',
      onConfirm: () => void moveSession(offer, folder),
    },
  });
}

/** Moves the whole session (one undoable operation); the open session is
 * opened again at its new place when the backend says it moved
 * (`session-moved`, SessionWorkspace). */
async function moveSession(offer: SessionMoveOffer, folder: string) {
  const t = i18next.t.bind(i18next);
  try {
    if (!(await saveEditsFirst([offer.session]))) return;
    const summary = await fileAction(
      () =>
        withCopyProgress(() =>
          invoke<FileOperationSummary>(Invokes.MoveSession, {
            session: offer.session,
            destination: offer.destination,
          }),
        ),
      'move',
      undefined,
      undefined,
      [offer.session, offer.destination],
    );
    if (summary)
      notify.success(t('studio.library.fileOperations.sessionMoved', { name: offer.name, folder, hint: undoHint() }));
    warnAboutNote(summary);
  } catch (err) {
    notify.error(t('studio.library.fileOperations.sessionMoveFailed', { name: offer.name, error: String(err) }));
  }
}

/** Mirrors `CopyProgress` in src-tauri/src/file_operations.rs. */
interface CopyProgress {
  /** The run Stop names, so it never stops another operation. */
  run: number;
  label: string;
  done: number;
  total: number;
}

/** Runs a file operation; while it copies files to another drive the
 * activity centre shows how far it is, with Cancel (what was copied is
 * removed, nothing moves). */
async function withCopyProgress<T>(run: () => Promise<T>): Promise<T> {
  const activity = useActivityStore.getState();
  let shown: string | null = null;
  const unlisten = await listen<CopyProgress>('file-operation-progress', ({ payload }) => {
    const title = i18next.t('studio.library.fileOperations.copying', {
      label: payload.label,
      done: formatBytes(payload.done),
      total: formatBytes(payload.total),
    });
    // Below the total: a full bar would read as done before the move is.
    const progress = { done: Math.min(payload.done, payload.total * 0.99), total: Math.max(payload.total, 1) };
    if (shown === null) {
      shown = activity.start({
        source: 'files',
        title,
        progress,
        cancel: () => void invoke(Invokes.StopFileOperation, { run: payload.run }),
      });
    } else {
      activity.update(shown, { title, progress });
    }
  });
  try {
    return await run();
  } finally {
    unlisten();
    if (shown !== null) activity.finish(shown);
  }
}

/** Mirrors `KeptCopies` in src-tauri/src/file_operations.rs: copies a
 * stopped or failed move made on the other drive and left there (they are
 * never removed by the app on its own; the originals were not touched). */
export interface KeptCopies {
  operationId: string;
  copies: { path: string; source: string; complete: boolean }[];
}

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** A toast naming the copies a move left, each with Show in Finder and
 * Move to Trash (only for these very copies: the backend refuses any other
 * file; not undoable: it is taken back by dragging it out of the Trash). `onTrashed` runs
 * after one went. */
export function showKeptCopies(kept: KeptCopies, onTrashed: () => void) {
  const t = i18next.t.bind(i18next);
  const button = (label: string, onClick: () => void) =>
    createElement('button', { className: 'ml-3 underline', onClick }, label);
  const show = (path: string) =>
    void invoke(Invokes.ShowInFinder, { path }).catch((err) =>
      notify.error(t('contextMenus.toasts.couldNotShowExplorer', { err })),
    );
  const trash = async (path: string) => {
    try {
      await invoke(Invokes.TrashKeptCopy, { operationId: kept.operationId, path });
      notify.success(ops('keptCopyTrashed', { name: fileName(path) }));
      onTrashed();
    } catch (err) {
      notify.error(ops('keptCopyTrashFailed', { name: fileName(path), error: String(err) }));
    }
  };
  const showLabel =
    useSettingsStore.getState().osPlatform === 'macos'
      ? t('contextMenus.thumbnail.showFinder')
      : t('contextMenus.thumbnail.showExplorer');
  const rows = kept.copies.map((copy) =>
    createElement(
      'li',
      { key: copy.path, title: copy.path },
      copy.complete ? fileName(copy.path) : ops('keptCopyIncomplete', { name: fileName(copy.path) }),
      button(showLabel, () => show(copy.path)),
      button(ops('trashConfirm'), () => void trash(copy.path)),
    ),
  );
  notify.warn(
    createElement(
      'div',
      null,
      opsCount('keptCopies', kept.copies.length),
      createElement('ul', { className: 'mt-1 max-h-40 overflow-y-auto' }, rows),
    ),
    { autoClose: false, closeOnClick: false },
  );
}

/** Tells what a finished operation left for the user to know (originals kept). */
function warnAboutNote(summary: FileOperationSummary | null | undefined) {
  if (summary?.note) notify.warn(summary.note, { autoClose: false });
}

/** Before files change place: the open photograph's recipe is written to
 * where it is now, not after it moved. False (and the user is told) when the
 * recipe of a photograph in `moving` (photographs or folders about to move;
 * with `sameName` also the photographs sharing their name, RAW+JPEG pairs)
 * could not be saved: the operation must not run, or that edit would stay
 * behind at the old place. A failed save elsewhere does not hold it up. */
async function saveEditsFirst(moving: string[] = [], sameName = false): Promise<boolean> {
  const physical = (path: string) => path.split('?vc=')[0];
  const withoutExtension = (path: string) => physical(path).replace(/\.[^.\\/]*$/, '');
  let failed: string[];
  try {
    failed = await flushRecipeSaves();
  } catch (err) {
    console.error('Saving edits before the file operation failed:', err);
    failed = moving;
  }
  const kept = failed.filter((path) =>
    moving.some(
      (target) =>
        physical(path) === physical(target) ||
        isBelow(physical(path), physical(target)) ||
        (sameName && withoutExtension(path) === withoutExtension(target)),
    ),
  );
  if (!kept.length) return true;
  notify.error(
    i18next.t('studio.library.fileOperations.unsavedEdits', {
      files: kept.map((path) => path.split(/[\\/]/).pop()).join(', '),
    }),
  );
  return false;
}

export const undoKey = () => (useSettingsStore.getState().osPlatform === 'macos' ? '⌘Z' : 'Ctrl+Z');
export const redoKey = () => (useSettingsStore.getState().osPlatform === 'macos' ? '⇧⌘Z' : 'Ctrl+Y');

type FileOperationText =
  | 'alreadyInSelects'
  | 'copyFilesFailed'
  | 'createFolderFailed'
  | 'deleteFailed'
  | 'keptCopyIncomplete'
  | 'keptCopyTrashed'
  | 'keptCopyTrashFailed'
  | 'moveFailed'
  | 'moveFilesFailed'
  | 'moveFolderFailed'
  | 'moveFolderTo'
  | 'nothingToUndo'
  | 'renameFilesFailed'
  | 'renameFolderFailed'
  | 'renamed'
  | 'selectForSelects'
  | 'selectsNeedSession'
  | 'trashConfirm'
  | 'trashCopiesConfirm'
  | 'trashCopiesMessage'
  | 'trashCopiesTitle'
  | 'trashFolderFailed'
  | 'trashFolderMessage'
  | 'trashFolderTitle'
  | 'trashOneMessage'
  | 'trashSelectedOnly'
  | 'trashTitle'
  | 'undoFailed'
  | 'undoHint'
  | 'undoHintFromDevelop';

const ops = (key: FileOperationText, options: Record<string, unknown> = {}) =>
  i18next.t(`studio.library.fileOperations.${key}`, options);
/** The texts with a number (plural forms). */
type FileOperationCount = 'keptCopies' | 'movedTo' | 'trashManyMessage' | 'trashedBoth' | 'trashedSession' | 'trashedSystem';
const opsCount = (key: FileOperationCount, count: number, options: Record<string, unknown> = {}) =>
  i18next.t(`studio.library.fileOperations.${key}`, { ...options, count });

/** How to undo a file operation from where the user is: in Develop ⌘Z
 * undoes edits (the edit history), so the hint names the Library. */
export function undoHint(): string {
  const key = undoKey();
  return useUIStore.getState().activeView === 'editor'
    ? ops('undoHintFromDevelop', { key })
    : ops('undoHint', { key });
}

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

/** What a finished file operation did, with how to undo it (`hint`). */
export function describeFileOperation(summary: FileOperationSummary, hint = undoHint()): string {
  if (summary.kind === 'trash') {
    const where =
      summary.systemTrash === 0 ? 'trashedSession' : summary.sessionTrash === 0 ? 'trashedSystem' : 'trashedBoth';
    return `${opsCount(where, summary.items)} ${hint}`;
  }
  if (summary.kind === 'move' && summary.newPaths.length) {
    const folder = summary.newPaths[0].split(/[\\/]/).slice(-2, -1)[0];
    return `${opsCount('movedTo', summary.newPaths.length, { folder })} ${hint}`;
  }
  return `${summary.label}. ${hint}`;
}

/** Runs `move` at once when the photos stay on their drive; when files go
 * to another drive, asks first (naming the drive, the bytes to copy and
 * the free space there) or says why they cannot go. */
async function askBeforeOtherDrive(
  paths: string[],
  destination: string,
  move: () => Promise<void>,
  failed: 'moveFailed' | 'moveFilesFailed',
) {
  let plan: MovePlan;
  try {
    plan = await invoke<MovePlan>(Invokes.PlanMoveFiles, { sourcePaths: paths, destinationFolder: destination });
  } catch (err) {
    notify.error(ops(failed, { error: String(err) }));
    return;
  }
  if (plan?.sessionMove) {
    offerSessionMove(plan.sessionMove);
    return;
  }
  if (!plan?.crossVolume) {
    await move();
    return;
  }
  if (plan.refusal) {
    notify.error(ops(failed, { error: plan.refusal }));
    return;
  }
  useUIStore.getState().setUI({
    confirmModalState: {
      isOpen: true,
      title: i18next.t('studio.library.fileOperations.otherDriveTitle', { drive: plan.drive }),
      message: describeCrossVolumeMove(plan),
      confirmText: i18next.t('studio.library.fileOperations.otherDriveConfirm'),
      onConfirm: () => void move(),
    },
  });
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

  // Copies a stopped or failed move left on the other drive: offered to
  // show and to move to the Trash.
  useEffect(() => {
    const unlisten = listen<KeptCopies>('file-operation-kept-copies', ({ payload }) =>
      showKeptCopies(payload, () => {
        void refreshImageList();
        void refreshAllFolderTrees();
      }),
    );
    return () => void unlisten.then((stop) => stop());
  }, [refreshImageList, refreshAllFolderTrees]);

  const getParentDir = (filePath: string): string => {
    const separator = filePath.includes('/') ? '/' : '\\';
    const lastSeparatorIndex = filePath.lastIndexOf(separator);
    if (lastSeparatorIndex === -1) return '';
    return filePath.substring(0, lastSeparatorIndex);
  };

  // Native photo operations also move registered companion photos. Until the
  // native plan exposes their exact set, protect each source folder as well.
  const withSourceParents = (paths: string[]) => [...paths, ...paths.map(getParentDir)];

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
        if (!(await saveEditsFirst(pathsToDelete, options.includeAssociated))) return;
        const command = options.includeAssociated ? Invokes.DeleteFilesWithAssociated : Invokes.DeleteFilesFromDisk;
        const summary = await fileAction(() =>
          invoke<FileOperationSummary>(command, { paths: pathsToDelete }),
          "trash", undefined, undefined, withSourceParents(pathsToDelete),
        );
        if (summary) notify.success(describeFileOperation(summary));
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
        notify.error(ops('deleteFailed', { error: String(err) }));
      }
    },
    [refreshImageList, refreshAllFolderTrees, handleBackToLibrary, sortedImageList, handleImageSelect],
  );

  const handleUndoFileOperation = useCallback(async () => {
    const startedOnBranch = undoBranchId();
    try {
      await saveEditsFirst();
      const summary = await withCopyProgress(() => invoke<FileOperationSummary | null>(Invokes.UndoFileOperation));
      fileOperationTravel("undo", summary, startedOnBranch);
      if (!summary) {
        notify.info(ops('nothingToUndo'));
        return;
      }
      warnAboutNote(summary);
      // Renamed and moved photographs and folders are back at their old
      // places: the selection, the open photograph, picks and thumbnails
      // go with them, as after a rename.
      const reopen = followMoves(summary.moves ?? []);
      notify.success(i18next.t('studio.library.fileOperations.undone', { label: summary.label, key: redoKey() }));
      await refreshImageList();
      await refreshAllFolderTrees();
      reopen();
      void recountAfterFileOperation();
    } catch (err) {
      notify.error(ops('undoFailed', { error: String(err) }));
    }
  }, [refreshImageList, refreshAllFolderTrees, followMoves]);

  /** Redoes the file operation undone last (validated again; refused when
   * something took a place since). */
  const handleRedoFileOperation = useCallback(async () => {
    if (
      useCatalogUndoStore.getState().fileRedoBlocked &&
      useCatalogUndoStore.getState().filesFuture.length === 0
    ) {
      notify.info(i18next.t("studio.library.fileOperations.nothingToRedo"));
      return;
    }
    const startedOnBranch = undoBranchId();
    try {
      await saveEditsFirst();
      const summary = await withCopyProgress(() => invoke<FileOperationSummary | null>(Invokes.RedoFileOperation));
      fileOperationTravel("redo", summary, startedOnBranch);
      if (!summary) {
        notify.info(i18next.t('studio.library.fileOperations.nothingToRedo'));
        return;
      }
      warnAboutNote(summary);
      // As after the operation itself: path-keyed state follows the moves.
      const reopen = followMoves(summary.moves ?? []);
      notify.success(i18next.t('studio.library.fileOperations.redone', { label: summary.label, key: undoKey() }));
      await refreshImageList();
      await refreshAllFolderTrees();
      reopen();
      void recountAfterFileOperation();
    } catch (err) {
      notify.error(i18next.t('studio.library.fileOperations.redoFailed', { error: String(err) }));
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
      notify.error(t('studio.library.trash.countFailed', { error: String(err) }));
      return;
    }
    if (!contents || contents.entries === 0) {
      notify.info(t('studio.library.trash.alreadyEmpty'));
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
            clearFileUndoHistory();
            if (result?.note) {
              notify.warn(t('studio.library.trash.incomplete', { note: result.note }), { autoClose: false });
            } else if (contents.photos > 0) {
              notify.success(t('studio.library.trash.emptied', { count: contents.photos }));
            } else {
              notify.success(t('studio.library.trash.emptiedNow'));
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
            notify.error(t('studio.library.trash.failed', { error: String(err) }));
          }
        },
      },
    });
  }, [refreshImageList, refreshAllFolderTrees, handleBackToLibrary]);

  /** Moves photos (with their sidecars and virtual copies) into `destination`;
   * to another drive only after the user agreed (`askBeforeOtherDrive`). */
  const handleMoveToFolder = useCallback(
    async (paths: Array<string>, destination: string) => {
      if (paths.length === 0) return;
      const move = async () => {
        try {
          if (!(await saveEditsFirst(paths))) return;
          const summary = await fileAction(() =>
            withCopyProgress(() =>
              invoke<FileOperationSummary>(Invokes.MoveFiles, {
                sourcePaths: paths,
                destinationFolder: destination,
              }),
            ),
            "move", undefined, undefined, [...withSourceParents(paths), destination],
          );
          if (summary) notify.success(describeFileOperation(summary));
          warnAboutNote(summary);
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
          notify.error(ops('moveFailed', { error: String(err) }));
        }
      };
      await askBeforeOtherDrive(paths, destination, move, 'moveFailed');
    },
    [refreshImageList, refreshAllFolderTrees, handleBackToLibrary, followMoves],
  );

  /** Moves a folder with everything in it into a folder the user picks
   * (`destination` skips the picker): a rename on its drive; to another
   * drive the whole folder is copied and checked first, after the user
   * agreed. The shown folders, selection and open photograph follow it. */
  const handleMoveFolder = useCallback(
    async (path: string, destination?: string) => {
      const name = path.split(/[\\/]/).pop() ?? path;
      const picked =
        destination ??
        (await open({ directory: true, multiple: false, title: ops('moveFolderTo', { name }) }).catch(() => null));
      if (typeof picked !== 'string' || !picked) return;
      const move = async () => {
        try {
          if (!(await saveEditsFirst([path]))) return;
          const summary = await fileAction(() =>
            withCopyProgress(() =>
              invoke<FileOperationSummary>(Invokes.MoveFolder, { path, destinationFolder: picked }),
            ),
            "move", undefined, undefined, [path, picked],
          );
          if (summary) notify.success(describeFileOperation(summary));
          warnAboutNote(summary);
          const separator = picked.includes('/') ? '/' : '\\';
          const reopen = followMoves(
            summary?.moves?.length ? summary.moves : [{ from: path, to: `${picked}${separator}${name}` }],
          );
          await refreshAllFolderTrees();
          await refreshImageList();
          reopen();
          void recountAfterFileOperation();
        } catch (err) {
          notify.error(ops('moveFolderFailed', { error: String(err) }));
        }
      };
      let plan: MovePlan;
      try {
        plan = await invoke<MovePlan>(Invokes.PlanMoveFolder, { path, destinationFolder: picked });
      } catch (err) {
        notify.error(ops('moveFolderFailed', { error: String(err) }));
        return;
      }
      if (plan?.sessionMove) {
        offerSessionMove(plan.sessionMove);
        return;
      }
      if (!plan?.crossVolume) {
        await move();
        return;
      }
      if (plan.refusal) {
        notify.error(ops('moveFolderFailed', { error: plan.refusal }));
        return;
      }
      useUIStore.getState().setUI({
        confirmModalState: {
          isOpen: true,
          title: i18next.t('studio.library.fileOperations.otherDriveTitle', { drive: plan.drive }),
          message: describeCrossVolumeFolderMove(plan, name),
          confirmText: i18next.t('studio.library.fileOperations.otherDriveConfirm'),
          onConfirm: () => void move(),
        },
      });
    },
    [refreshImageList, refreshAllFolderTrees, followMoves],
  );

  /** Capture One's Move to Selects (⌘J): the selection into the session's Selects folder. */
  const handleMoveToSelects = useCallback(async () => {
    const { sessionDirectory, multiSelectedPaths, libraryActivePath } = useLibraryStore.getState();
    const { selectedImage } = useEditorStore.getState();
    const { activeView } = useUIStore.getState();
    if (!sessionDirectory) {
      notify.info(ops('selectsNeedSession'));
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
      notify.info(ops(paths.length ? 'alreadyInSelects' : 'selectForSelects'));
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
          confirmText: ops('trashConfirm'),
          // Undoable, so not destructive; only Empty Trash is.
          confirmVariant: 'primary',
          isOpen: true,
          message: ops('trashFolderMessage', { name, hint: undoHint() }),
          onConfirm: async () => {
            try {
              if (!(await saveEditsFirst([path]))) return;
              const summary = await fileAction(() =>
                invoke<FileOperationSummary>(Invokes.DeleteFolder, { path }),
                "trash", undefined, undefined, [path],
              );
              if (summary) notify.success(describeFileOperation(summary));
              const { currentFolderPath, setLibrary } = useLibraryStore.getState();
              if (currentFolderPath && (currentFolderPath === path || isBelow(currentFolderPath, path))) {
                setLibrary({ currentFolderPath: null, imageList: [], multiSelectedPaths: [], libraryActivePath: null });
              }
              await refreshAllFolderTrees();
              await refreshImageList();
              void recountAfterFileOperation();
            } catch (err) {
              notify.error(ops('trashFolderFailed', { error: String(err) }));
            }
          },
          title: ops('trashFolderTitle'),
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

    const hint = undoHint();
    let modalTitle = ops('trashTitle');
    let modalMessage: string;
    let confirmText: string;

    if (selectionHasVirtualCopies) {
      modalTitle = ops('trashCopiesTitle');
      modalMessage = ops('trashCopiesMessage', { hint });
      confirmText = ops('trashCopiesConfirm');
    } else if (isSingle) {
      modalMessage = ops('trashOneMessage', { hint });
      confirmText = ops('trashSelectedOnly');
    } else {
      modalMessage = opsCount('trashManyMessage', pathsToDelete.length, { hint });
      confirmText = ops('trashSelectedOnly');
    }

    setUI({
      confirmModalState: {
        confirmText,
        // Undoable, so not destructive; only Empty Trash is.
        confirmVariant: 'primary',
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
          notify.error(ops('createFolderFailed', { error: String(err) }));
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

          if (!(await saveEditsFirst([oldPath]))) return;
          const summary = await fileAction(() =>
            invoke<FileOperationSummary>(Invokes.RenameFolder, {
              path: oldPath,
              newName: trimmedNewName,
            }),
            "rename", undefined, undefined, [oldPath, getParentDir(oldPath)],
          );
          if (summary) notify.success(describeFileOperation(summary));

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
          notify.error(ops('renameFolderFailed', { error: String(err) }));
        }
      }
    },
    [refreshAllFolderTrees, refreshImageList, followMoves],
  );

  const handleSaveRename = useCallback(
    async (nameTemplate: string, expected: PlannedFile[]) => {
      const { renameTargetPaths, setUI } = useUIStore.getState();

      if (renameTargetPaths.length > 0 && nameTemplate) {
        try {
          if (!(await saveEditsFirst(renameTargetPaths))) return;
          const newPaths: Array<string> = await fileAction(
            () =>
              invoke<string[]>(Invokes.RenameFiles, {
                expected,
                nameTemplate,
                paths: renameTargetPaths,
              }),
            "rename",
            (renamed) =>
              renamed.some((path, index) => path !== renameTargetPaths[index]),
            (renamed) =>
              renameTargetPaths.flatMap((from, index) =>
                renamed[index] ? [{ from, to: renamed[index] }] : [],
              ),
            [...renameTargetPaths, ...renameTargetPaths.map(getParentDir)],
          );
          // The selection, the open photograph, picks, ratings and
          // thumbnails stay with the renamed photos (as on undo).
          const moves = renameTargetPaths.flatMap((from, i) => (newPaths[i] ? [{ from, to: newPaths[i] }] : []));
          const reopen = followMoves(moves);
          notify.success(ops('renamed', { hint: undoHint() }));
          useLibraryStore.getState().setLibrary({ multiSelectedPaths: newPaths });

          await refreshImageList();
          reopen();
          void recountAfterFileOperation();
        } catch (err) {
          notify.error(ops('renameFilesFailed', { error: String(err) }));
        }
      }
      setUI({ renameTargetPaths: [] });
    },
    [refreshImageList, followMoves],
  );

  const handleRenameFiles = useCallback(async (paths: Array<string>) => {
    // Saved first, so the preview already shows the edit sidecars the
    // rename then takes along.
    if (paths && paths.length > 0 && (await saveEditsFirst(paths))) {
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
          const move = async () => {
            try {
              if (!(await saveEditsFirst(copiedFilePaths))) return;
              const summary = await fileAction(() =>
                withCopyProgress(() =>
                  invoke<FileOperationSummary>(Invokes.MoveFiles, {
                    sourcePaths: copiedFilePaths,
                    destinationFolder: target,
                  }),
                ),
                "move", undefined, undefined, [...withSourceParents(copiedFilePaths), target],
              );
              if (summary) notify.success(describeFileOperation(summary));
              warnAboutNote(summary);
              setProcess({ copiedFilePaths: [] });
              setLibrary({ multiSelectedPaths: [] });
              const reopen = followMoves(summary?.moves ?? []);
              await refreshAllFolderTrees();
              await refreshImageList();
              reopen();
              void recountAfterFileOperation();
            } catch (err) {
              notify.error(ops('moveFilesFailed', { error: String(err) }));
            }
          };
          await askBeforeOtherDrive(copiedFilePaths, target, move, 'moveFilesFailed');
          return;
        }
        await refreshImageList();
      } catch (err) {
        notify.error(ops(mode === 'copy' ? 'copyFilesFailed' : 'moveFilesFailed', { error: String(err) }));
      }
    },
    [refreshImageList, refreshAllFolderTrees, followMoves],
  );

  useEffect(
    () =>
      registerFileUndoHandlers({
        undo: handleUndoFileOperation,
        redo: handleRedoFileOperation,
      }),
    [handleUndoFileOperation, handleRedoFileOperation],
  );

  return {
    executeDelete,
    handleDeleteSelected,
    handleUndoFileOperation,
    handleRedoFileOperation,
    handleEmptySessionTrash,
    handleMoveToFolder,
    handleMoveToSelects,
    handleMoveFolder,
    handleTrashFolder,
    handleCreateFolder,
    handleRenameFolder,
    handleSaveRename,
    handleRenameFiles,
    handlePasteFiles,
  };
}
