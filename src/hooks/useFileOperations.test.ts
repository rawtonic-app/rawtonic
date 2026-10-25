import {
  catalogAction,
  undo as appUndo,
  useCatalogUndoStore,
  fileOperationTravel,
} from "../utils/appUndo";
import { cleanup, fireEvent, render, renderHook, screen, waitFor } from '@testing-library/react';
import type { ReactElement } from 'react';
import { listen } from '@tauri-apps/api/event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import '../i18n';
import { open } from '@tauri-apps/plugin-dialog';
import {
  describeCrossVolumeFolderMove,
  describeCrossVolumeMove,
  describeEmptyTrash,
  describeFileOperation,
  FileOperationSummary,
  formatBytes,
  type KeptCopies,
  type MovePlan,
  showKeptCopies,
  undoHint,
  type TrashContents,
  useFileOperations,
} from './useFileOperations';
import { useSettingsStore } from '../store/useSettingsStore';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore } from '../store/useProcessStore';
import { useUIStore } from '../store/useUIStore';
import { Invokes } from '../components/ui/AppProperties';
import { OFFLINE_COUNTED } from '../utils/offline';
import { FILE_OPERATIONS_CHANGED } from '../utils/catalogEvents';
import { debouncedSave } from './useEditorActions';
import { INITIAL_ADJUSTMENTS } from '../utils/adjustments';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(() => Promise.resolve(null)) }));
vi.mock('react-toastify', () => ({
  toast: { error: vi.fn(), success: vi.fn(), info: vi.fn(), warn: vi.fn(), update: vi.fn(), dismiss: vi.fn(), isActive: vi.fn(() => false) },
}));

const summary = (part: Partial<FileOperationSummary>): FileOperationSummary => ({
  id: 'op',
  kind: 'trash',
  label: 'Delete 2 photos',
  items: 2,
  sessionTrash: 0,
  systemTrash: 0,
  newPaths: [],
  moves: [],
  ...part,
});

it('says where deleted photos went and how to undo it', () => {
  expect(describeFileOperation(summary({ sessionTrash: 2 }), '⌘Z undoes it.')).toBe(
    "Moved 2 items to the session's Trash folder. ⌘Z undoes it.",
  );
  expect(describeFileOperation(summary({ items: 1, systemTrash: 1 }), 'Ctrl+Z undoes it.')).toBe(
    'Moved 1 item to the Trash. Ctrl+Z undoes it.',
  );
  expect(describeFileOperation(summary({ sessionTrash: 1, systemTrash: 1 }), '')).toContain(
    "the session's Trash folder and the Trash",
  );
});

it('names the folder photos were moved to, and other operations by their label', () => {
  expect(
    describeFileOperation(
      summary({ kind: 'move', label: 'Move 2 photos to Selects', newPaths: ['/job/Selects/A.RAF', '/job/Selects/B.RAF'] }),
      '⌘Z undoes it.',
    ),
  ).toBe('Moved 2 photos to Selects. ⌘Z undoes it.');
  expect(describeFileOperation(summary({ kind: 'renameFolder', label: 'Rename folder Day 1' }), '⌘Z undoes it.')).toBe(
    'Rename folder Day 1. ⌘Z undoes it.',
  );
});

it('names the Library for undoing in Develop, where ⌘Z undoes edits', () => {
  useSettingsStore.setState({ osPlatform: 'macos' });
  useUIStore.setState({ activeView: 'library' });
  expect(undoHint()).toBe('⌘Z undoes it.');
  expect(describeFileOperation(summary({ items: 1, systemTrash: 1 }))).toBe('Moved 1 item to the Trash. ⌘Z undoes it.');
  useUIStore.setState({ activeView: 'editor' });
  expect(undoHint()).toBe('⌘Z in the Library undoes it.');
  expect(describeFileOperation(summary({ items: 1, systemTrash: 1 }))).toBe(
    'Moved 1 item to the Trash. ⌘Z in the Library undoes it.',
  );
  // The Delete dialog in Develop says so too.
  useLibraryStore.setState({ multiSelectedPaths: ['/job/Capture/A.RAF'], imageList: [] });
  const { result } = renderHook(() =>
    useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), vi.fn(), vi.fn(), []),
  );
  result.current.handleDeleteSelected();
  expect(useUIStore.getState().confirmModalState.message).toContain('⌘Z in the Library undoes it.');
  useUIStore.setState({ activeView: 'library' });
});

it('renaming the selected photo in the Library stays in the Library and keeps it open', async () => {
  vi.mocked(invoke).mockResolvedValueOnce(['/job/Capture/Grandpa.RAF']);
  const handleImageSelect = vi.fn();
  const { result } = renderHook(() =>
    useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), handleImageSelect, vi.fn(), []),
  );
  useUIStore.setState({ activeView: 'library', renameTargetPaths: ['/job/Capture/A.RAF'] });
  useEditorStore.setState({ selectedImage: { path: '/job/Capture/A.RAF' } as never });
  useLibraryStore.setState({ libraryActivePath: '/job/Capture/A.RAF' });
  await result.current.handleSaveRename('Grandpa');
  expect(handleImageSelect).toHaveBeenCalledWith('/job/Capture/Grandpa.RAF', false);
  expect(useUIStore.getState().activeView).toBe('library');
  expect(useLibraryStore.getState().libraryActivePath).toBe('/job/Capture/Grandpa.RAF');
  expect(useLibraryStore.getState().multiSelectedPaths).toEqual(['/job/Capture/Grandpa.RAF']);
});

it('a renamed pick stays picked, so the pick filter still finds it', async () => {
  vi.mocked(invoke).mockResolvedValueOnce(['/job/Capture/renamed.png']);
  let flagsWhenListed: Record<string, number> | null = null;
  // The list is read again only after the flags have followed.
  const refresh = vi.fn(() => {
    flagsWhenListed = useLibraryStore.getState().imageFlags;
    return Promise.resolve();
  });
  const { result } = renderHook(() =>
    useFileOperations(refresh, vi.fn(() => Promise.resolve()), vi.fn(), vi.fn(), []),
  );
  useUIStore.setState({ activeView: 'library', renameTargetPaths: ['/job/Capture/chart0.png'] });
  useEditorStore.setState({ selectedImage: null });
  useLibraryStore.setState({ imageFlags: { '/job/Capture/chart0.png': 1 } });
  await result.current.handleSaveRename('renamed');
  expect(flagsWhenListed).toEqual({ '/job/Capture/renamed.png': 1 });
  expect(useLibraryStore.getState().imageFlags).toEqual({ '/job/Capture/renamed.png': 1 });
});

describe('undo follows the paths it moves back', () => {
  const renamed = '/job/Capture/x_renamed.ARW';
  const original = '/job/Capture/x.ARW';
  const undoneRename = summary({
    kind: 'rename',
    label: 'Rename 1 photo',
    items: 1,
    newPaths: [original],
    moves: [{ from: renamed, to: original }],
  });

  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
    useLibraryStore.setState({
      sessionDirectory: null,
      currentFolderPath: '/job/Capture',
      rootPaths: ['/job'],
      multiSelectedPaths: [renamed, `${renamed}?vc=c0ffee`],
      libraryActivePath: renamed,
      selectionAnchorPath: renamed,
      imageFlags: { [renamed]: 1 },
      imageRatings: { [renamed]: 4, [`${renamed}?vc=c0ffee`]: 2 },
    });
    useProcessStore.setState({ thumbnails: { [renamed]: 'blob:thumb' } });
    useEditorStore.setState({ selectedImage: { path: renamed } as never });
  });

  it('puts the open photo, the selection, picks, ratings and thumbnails back at the old name', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undoneRename);
    const handleImageSelect = vi.fn();
    let selectedWhenListed: string[] = [];
    const refresh = vi.fn(() => {
      selectedWhenListed = useLibraryStore.getState().multiSelectedPaths;
      return Promise.resolve();
    });
    const { result } = renderHook(() =>
      useFileOperations(refresh, vi.fn(() => Promise.resolve()), handleImageSelect, vi.fn(), []),
    );
    useUIStore.setState({ activeView: 'library' });
    await result.current.handleUndoFileOperation();

    expect(invoke).toHaveBeenCalledWith(Invokes.UndoFileOperation);
    expect(selectedWhenListed).toEqual([original, `${original}?vc=c0ffee`]);
    const library = useLibraryStore.getState();
    expect(library.libraryActivePath).toBe(original);
    expect(library.selectionAnchorPath).toBe(original);
    expect(library.imageFlags).toEqual({ [original]: 1 });
    expect(library.imageRatings).toEqual({ [original]: 4, [`${original}?vc=c0ffee`]: 2 });
    expect(useProcessStore.getState().thumbnails).toEqual({ [original]: 'blob:thumb' });
    // The open photo opens again at its old name, without leaving the Library.
    expect(handleImageSelect).toHaveBeenCalledWith(original, false);
    expect(useUIStore.getState().activeView).toBe('library');
  });

  it('reopens the photo in Develop when it was open there', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undoneRename);
    const handleImageSelect = vi.fn();
    const { result } = renderHook(() =>
      useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), handleImageSelect, vi.fn(), []),
    );
    useUIStore.setState({ activeView: 'editor' });
    await result.current.handleUndoFileOperation();
    expect(handleImageSelect).toHaveBeenCalledWith(original, true);
  });

  it('a folder rename undone takes the shown folder and the photos in it back', async () => {
    useLibraryStore.setState({
      currentFolderPath: '/job/Day 2',
      rootPaths: ['/job/Day 2'],
      multiSelectedPaths: ['/job/Day 2/A.RAF'],
      libraryActivePath: '/job/Day 2/A.RAF',
    });
    useEditorStore.setState({ selectedImage: null });
    vi.mocked(invoke).mockResolvedValueOnce(
      summary({ kind: 'renameFolder', label: 'Rename folder Day 1', moves: [{ from: '/job/Day 2', to: '/job/Day 1' }] }),
    );
    const handleImageSelect = vi.fn();
    const { result } = renderHook(() =>
      useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), handleImageSelect, vi.fn(), []),
    );
    await result.current.handleUndoFileOperation();
    const library = useLibraryStore.getState();
    expect(library.currentFolderPath).toBe('/job/Day 1');
    expect(library.rootPaths).toEqual(['/job/Day 1']);
    expect(library.multiSelectedPaths).toEqual(['/job/Day 1/A.RAF']);
    expect(library.libraryActivePath).toBe('/job/Day 1/A.RAF');
    expect(handleImageSelect).not.toHaveBeenCalled();
  });

  it('counts the offline photographs again after the undo', async () => {
    useLibraryStore.setState({ sessionDirectory: '/job' });
    vi.mocked(invoke).mockResolvedValueOnce(undoneRename);
    const counted = vi.fn();
    const catalog = vi.fn();
    window.addEventListener(FILE_OPERATIONS_CHANGED, catalog);
    window.addEventListener(OFFLINE_COUNTED, counted);
    const { result } = renderHook(() =>
      useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), vi.fn(), vi.fn(), []),
    );
    await result.current.handleUndoFileOperation();
    await waitFor(() => expect(counted).toHaveBeenCalled());
    window.removeEventListener(OFFLINE_COUNTED, counted);
    window.removeEventListener(FILE_OPERATIONS_CHANGED, catalog);
    expect(catalog).toHaveBeenCalledOnce();
    expect(invoke).toHaveBeenCalledWith(Invokes.SessionOffline, { directory: '/job' });
  });
});

describe('redo', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
    vi.mocked(toast.info).mockClear();
    vi.mocked(toast.success).mockClear();
    vi.mocked(toast.error).mockClear();
    useSettingsStore.setState({ osPlatform: 'macos' });
  });

  it('redoes the operation undone last and follows its moves', async () => {
    const from = '/job/Capture/x.ARW';
    const to = '/job/Capture/x_renamed.ARW';
    useLibraryStore.setState({ multiSelectedPaths: [from], libraryActivePath: from, imageFlags: { [from]: 1 } });
    useEditorStore.setState({ selectedImage: null });
    vi.mocked(invoke).mockResolvedValueOnce(
      summary({ kind: 'rename', label: 'Rename 1 photo', items: 1, newPaths: [to], moves: [{ from, to }] }),
    );
    const { result } = renderHook(() =>
      useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), vi.fn(), vi.fn(), []),
    );
    await result.current.handleRedoFileOperation();
    expect(invoke).toHaveBeenCalledWith(Invokes.RedoFileOperation);
    expect(useLibraryStore.getState().libraryActivePath).toBe(to);
    expect(useLibraryStore.getState().imageFlags).toEqual({ [to]: 1 });
    expect(toast.success).toHaveBeenCalledWith('Redone: Rename 1 photo. ⌘Z undoes it.');
  });

  it('says when there is nothing to redo, and why a redo was refused', async () => {
    const { result } = renderHook(() =>
      useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), vi.fn(), vi.fn(), []),
    );
    vi.mocked(invoke).mockResolvedValueOnce(null);
    await result.current.handleRedoFileOperation();
    expect(toast.info).toHaveBeenCalledWith('There is no file operation to redo.');
    // A newly undone operation makes a fresh native redo available.
    fileOperationTravel("undo", summary({ kind: "rename" }));
    vi.mocked(invoke).mockRejectedValueOnce('/job/Capture/b.RAF already exists: Nothing was redone');
    await result.current.handleRedoFileOperation();
    expect(toast.error).toHaveBeenCalledWith('Could not redo: /job/Capture/b.RAF already exists: Nothing was redone');
  });

  it('says after an undo how to redo it', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(summary({ kind: 'trash', label: 'Delete 2 photos' }));
    const { result } = renderHook(() =>
      useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), vi.fn(), vi.fn(), []),
    );
    await result.current.handleUndoFileOperation();
    expect(toast.success).toHaveBeenCalledWith('Undone: Delete 2 photos. ⇧⌘Z redoes it.');
  });
});

describe('emptying the session Trash', () => {
  const contents: TrashContents = { photos: 3, files: 7, entries: 2 };

  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
    vi.mocked(toast.info).mockClear();
    vi.mocked(toast.success).mockClear();
    useUIStore.setState({ activeView: 'library', confirmModalState: { isOpen: false } });
    useEditorStore.setState({ selectedImage: { path: '/job/Trash/Capture/A.RAF' } as never });
    useLibraryStore.setState({
      sessionDirectory: '/job',
      currentFolderPath: '/job/Trash/Capture',
      multiSelectedPaths: ['/job/Trash/Capture/A.RAF', '/job/Capture/K.RAF'],
      libraryActivePath: '/job/Trash/Capture/A.RAF',
    });
  });

  it('names the count, says it is permanent and not undoable', () => {
    expect(describeEmptyTrash(contents)).toBe(
      "Permanently delete 3 photos in this session's Trash folder? 7 files are removed from disk, sidecars and XMP included. This cannot be undone, and file changes made before it can no longer be undone either.",
    );
    expect(describeEmptyTrash({ photos: 1, files: 1, entries: 1 })).toContain('Permanently delete 1 photo in');
    expect(describeEmptyTrash({ photos: 0, files: 2, entries: 1 })).toContain('Permanently delete 2 files in');
    expect(describeEmptyTrash({ photos: 0, files: 0, entries: 1 })).toContain('Remove the empty folders');
  });

  it('asks first, then empties exactly what was counted and lets nothing from the Trash stay open', async () => {
    const handleBackToLibrary = vi.fn();
    vi.mocked(invoke).mockResolvedValueOnce(contents);
    const { result } = renderHook(() =>
      useFileOperations(
        vi.fn(() => Promise.resolve()),
        vi.fn(() => Promise.resolve()),
        vi.fn(),
        handleBackToLibrary,
        [],
      ),
    );
    await result.current.handleEmptySessionTrash();
    expect(invoke).toHaveBeenCalledWith(Invokes.SessionTrashContents, { session: '/job' });
    const modal = useUIStore.getState().confirmModalState;
    expect(modal.isOpen).toBe(true);
    expect(modal.title).toBe('Empty the Session Trash?');
    expect(modal.confirmText).toBe('Delete Permanently');
    expect(modal.confirmVariant).toBe('destructive');
    expect(modal.message).toContain('3 photos');
    // Nothing is deleted before the confirmation.
    expect(invoke).not.toHaveBeenCalledWith(Invokes.EmptySessionTrash, expect.anything());

    vi.mocked(invoke).mockResolvedValueOnce(contents);
    await modal.onConfirm?.();
    expect(invoke).toHaveBeenCalledWith(Invokes.EmptySessionTrash, {
      session: '/job',
      photos: 3,
      files: 7,
      entries: 2,
    });
    expect(toast.success).toHaveBeenCalledWith('3 photos deleted permanently.');
    expect(useEditorStore.getState().selectedImage).toBeNull();
    const library = useLibraryStore.getState();
    expect(library.multiSelectedPaths).toEqual(['/job/Capture/K.RAF']);
    expect(library.libraryActivePath).toBeNull();
    expect(library.currentFolderPath).toBe('/job/Trash');
  });

  it('an empty Trash asks nothing', async () => {
    vi.mocked(invoke).mockResolvedValueOnce({ photos: 0, files: 0, entries: 0 });
    const { result } = renderHook(() =>
      useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), vi.fn(), vi.fn(), []),
    );
    await result.current.handleEmptySessionTrash();
    expect(useUIStore.getState().confirmModalState.isOpen).toBe(false);
    expect(toast.info).toHaveBeenCalledWith("The session's Trash folder is empty.");
  });
});

describe('moving to the Trash', () => {
  const hook = () =>
    renderHook(() =>
      useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), vi.fn(), vi.fn(), []),
    ).result.current;
  const calls = () => vi.mocked(invoke).mock.calls.map((call) => call[0]);

  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
  });

  it('announces a settled delete so import and album views reload the catalog', async () => {
    useLibraryStore.setState({ sessionDirectory: '/job' });
    const changed = vi.fn();
    window.addEventListener(FILE_OPERATIONS_CHANGED, changed);
    try {
      await hook().executeDelete(['/job/Capture/a.RAF']);
      expect(changed).toHaveBeenCalledOnce();
    } finally {
      window.removeEventListener(FILE_OPERATIONS_CHANGED, changed);
    }
  });

  it('saves a pending edit before the photo moves, not afterwards at its old place', async () => {
    debouncedSave('/p/A.RAF', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    await hook().executeDelete(['/p/A.RAF']);
    expect(calls().indexOf(Invokes.SaveMetadataAndUpdateThumbnail)).toBeGreaterThanOrEqual(0);
    expect(calls().indexOf(Invokes.SaveMetadataAndUpdateThumbnail)).toBeLessThan(calls().indexOf('delete_files_from_disk'));
  });

  const failSaves = () =>
    vi.mocked(invoke).mockImplementation((command) =>
      command === Invokes.SaveMetadataAndUpdateThumbnail ? Promise.reject('disk full') : Promise.resolve(undefined),
    );

  it('moves nothing when the edit of a photo to move could not be saved', async () => {
    failSaves();
    debouncedSave('/p/A.RAF', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    await hook().executeDelete(['/p/A.RAF']);
    expect(calls()).not.toContain('delete_files_from_disk');
    expect(toast.error).toHaveBeenCalledWith('Edits of A.RAF could not be saved, so nothing was moved.');

    debouncedSave('/p/A.RAF', { ...INITIAL_ADJUSTMENTS, exposure: 2 });
    hook().handleTrashFolder('/p');
    await useUIStore.getState().confirmModalState.onConfirm?.();
    expect(calls()).not.toContain(Invokes.DeleteFolder);
  });

  it('with the associated files, a failed save of the RAW+JPEG partner holds the trash up too', async () => {
    failSaves();
    debouncedSave('/s/D.JPG', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    await hook().executeDelete(['/s/D.RAF'], { includeAssociated: true });
    expect(calls()).not.toContain('delete_files_with_associated');
    await hook().executeDelete(['/s/D.RAF']);
    expect(calls()).toContain('delete_files_from_disk');
  });

  it('a failed save of another photo does not hold the operation up', async () => {
    failSaves();
    debouncedSave('/q/B.RAF', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    await hook().executeDelete(['/r/C.RAF']);
    expect(calls()).toContain('delete_files_from_disk');
  });

  it('saves a pending edit before a folder moves', async () => {
    debouncedSave('/p/A.RAF', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    hook().handleTrashFolder('/p');
    await useUIStore.getState().confirmModalState.onConfirm?.();
    expect(calls().indexOf(Invokes.SaveMetadataAndUpdateThumbnail)).toBeGreaterThanOrEqual(0);
    expect(calls().indexOf(Invokes.SaveMetadataAndUpdateThumbnail)).toBeLessThan(calls().indexOf(Invokes.DeleteFolder));
  });

  it('leaves a trashed Windows folder shown below it too', async () => {
    useLibraryStore.setState({ currentFolderPath: 'C:\\job\\Day 1\\Picks' });
    hook().handleTrashFolder('C:\\job\\Day 1');
    await useUIStore.getState().confirmModalState.onConfirm?.();
    expect(useLibraryStore.getState().currentFolderPath).toBeNull();
  });
});

describe('moving photos to another drive', () => {
  const plan = (part: Partial<MovePlan>): MovePlan => ({
    crossVolume: true,
    photos: 2,
    files: 5,
    bytes: 123_400_000,
    drive: 'Archive SSD',
    freeBytes: 1_900_000_000_000,
    sourceDrives: ['Macintosh HD'],
    refusal: null,
    ...part,
  });
  const moved = summary({ kind: 'move', label: 'Move 2 photos to 2026', newPaths: ['/Volumes/Archive SSD/2026/A.RAF'] });
  const hook = () => renderHook(() => useFileOperations(vi.fn(), vi.fn(), vi.fn(), vi.fn(), []));

  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
    vi.mocked(toast.error).mockClear();
    vi.mocked(toast.warn).mockClear();
    useUIStore.setState({ confirmModalState: { isOpen: false } });
  });

  it('formats sizes as file browsers do', () => {
    expect(formatBytes(512)).toBe('512 bytes');
    expect(formatBytes(123_400_000)).toBe('123.4 MB');
    expect(formatBytes(1_900_000_000_000)).toBe('1.9 TB');
  });

  it('asks first, naming the drive, the bytes to copy, the free space and where the originals go', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(plan({})).mockResolvedValueOnce(moved);
    const { result } = hook();
    await result.current.handleMoveToFolder(['/Users/a/A.RAF', '/Users/a/B.RAF'], '/Volumes/Archive SSD/2026');
    expect(vi.mocked(invoke)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith(Invokes.PlanMoveFiles, {
      sourcePaths: ['/Users/a/A.RAF', '/Users/a/B.RAF'],
      destinationFolder: '/Volumes/Archive SSD/2026',
    });
    const asked = useUIStore.getState().confirmModalState;
    expect(asked.title).toBe('Move to Archive SSD?');
    expect(asked.message).toBe(describeCrossVolumeMove(plan({})));
    expect(asked.message).toContain('5 files, 123.4 MB');
    expect(asked.message).toContain('which has 1.9 TB free');
    expect(asked.message).toContain('the Trash of Macintosh HD');
    asked.onConfirm?.();
    await waitFor(() =>
      expect(vi.mocked(invoke)).toHaveBeenCalledWith(Invokes.MoveFiles, {
        sourcePaths: ['/Users/a/A.RAF', '/Users/a/B.RAF'],
        destinationFolder: '/Volumes/Archive SSD/2026',
      }),
    );
  });

  it('says why a move cannot go to that drive and moves nothing', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(plan({ refusal: 'Archive SSD is a network drive' }));
    const { result } = hook();
    await result.current.handleMoveToFolder(['/Users/a/A.RAF'], '/Volumes/Archive SSD');
    expect(toast.error).toHaveBeenCalledWith('Could not move: Archive SSD is a network drive');
    expect(useUIStore.getState().confirmModalState.isOpen).toBe(false);
    expect(vi.mocked(invoke)).not.toHaveBeenCalledWith(Invokes.MoveFiles, expect.anything());
  });

  it('moves on one drive without asking, and tells about originals a move kept', async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce(plan({ crossVolume: false }))
      .mockResolvedValueOnce({ ...moved, note: '/Users/a/A.xmp changed while it was moved and was kept' });
    const { result } = hook();
    await result.current.handleMoveToFolder(['/Users/a/A.RAF'], '/Users/a/Selects');
    expect(useUIStore.getState().confirmModalState.isOpen).toBe(false);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith(Invokes.MoveFiles, expect.anything());
    expect(toast.warn).toHaveBeenCalledWith('/Users/a/A.xmp changed while it was moved and was kept', {
      autoClose: false,
    });
  });
});

describe('copies a stopped or failed move left on the other drive', () => {
  const kept: KeptCopies = {
    operationId: 'op-1',
    copies: [
      { path: '/Volumes/CARD/2026/A.RAF', source: '/Users/a/A.RAF', complete: false },
      { path: '/Volumes/CARD/2026/A.xmp', source: '/Users/a/A.xmp', complete: true },
    ],
  };
  const shown = () => {
    const [element, options] = vi.mocked(toast.warn).mock.calls.at(-1)!;
    render(element as ReactElement);
    return options;
  };

  beforeEach(() => {
    cleanup();
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
    vi.mocked(toast.warn).mockClear();
    vi.mocked(toast.error).mockClear();
    vi.mocked(toast.success).mockClear();
    useSettingsStore.setState({ osPlatform: 'macos' });
    useUIStore.setState({ activeView: 'library' });
  });

  it('names each copy, marks one that may be incomplete, and stays until closed', () => {
    showKeptCopies(kept, vi.fn());
    expect(shown()).toEqual({ autoClose: false, closeOnClick: false });
    expect(screen.getByText(/2 copies stay on the other drive; their originals were not touched/)).toBeTruthy();
    expect(screen.getByText('A.RAF (maybe incomplete)')).toBeTruthy();
    expect(screen.getByText('A.xmp')).toBeTruthy();
    expect(screen.getAllByText('Show in Finder')).toHaveLength(2);
  });

  it('shows a copy in the Finder', () => {
    showKeptCopies(kept, vi.fn());
    shown();
    fireEvent.click(screen.getAllByText('Show in Finder')[0]);
    expect(invoke).toHaveBeenCalledWith(Invokes.ShowInFinder, { path: '/Volumes/CARD/2026/A.RAF' });
  });

  it('moves exactly that copy of that operation to the Trash', async () => {
    const onTrashed = vi.fn();
    showKeptCopies(kept, onTrashed);
    shown();
    fireEvent.click(screen.getAllByText('Move to Trash')[1]);
    await waitFor(() => expect(onTrashed).toHaveBeenCalled());
    expect(invoke).toHaveBeenCalledWith(Invokes.TrashKeptCopy, {
      operationId: 'op-1',
      path: '/Volumes/CARD/2026/A.xmp',
    });
    expect(toast.success).toHaveBeenCalledWith('A.xmp is in the Trash of its drive; drag it out of the Trash to get it back.');
  });

  it('says why a copy could not go to the Trash', async () => {
    vi.mocked(invoke).mockRejectedValueOnce('it is no longer the copy the move left');
    const onTrashed = vi.fn();
    showKeptCopies(kept, onTrashed);
    shown();
    fireEvent.click(screen.getAllByText('Move to Trash')[0]);
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith(
        'Could not move A.RAF to the Trash: it is no longer the copy the move left',
      ),
    );
    expect(onTrashed).not.toHaveBeenCalled();
  });

  it('the Library listens for them and reads the folder again after one went', async () => {
    vi.mocked(listen).mockClear();
    const refresh = vi.fn(() => Promise.resolve());
    renderHook(() => useFileOperations(refresh, vi.fn(() => Promise.resolve()), vi.fn(), vi.fn(), []));
    const call = vi.mocked(listen).mock.calls.find(([event]) => event === 'file-operation-kept-copies');
    expect(call).toBeTruthy();
    const handler = call![1] as (event: { payload: KeptCopies }) => void;
    handler({ payload: kept });
    shown();
    fireEvent.click(screen.getAllByText('Move to Trash')[0]);
    await waitFor(() => expect(refresh).toHaveBeenCalled());
  });
});

describe('moving a folder', () => {
  const plan = (part: Partial<MovePlan>): MovePlan => ({
    crossVolume: true,
    photos: 3,
    files: 9,
    bytes: 98_700_000,
    drive: 'Archive SSD',
    freeBytes: 1_900_000_000_000,
    sourceDrives: ['Macintosh HD'],
    refusal: null,
    ...part,
  });
  const moved = summary({
    kind: 'moveFolder',
    label: 'Move folder Day 1 to 2026',
    moves: [{ from: '/Users/a/Day 1', to: '/Volumes/Archive SSD/2026/Day 1' }],
  });
  const hook = () => renderHook(() => useFileOperations(vi.fn(), vi.fn(), vi.fn(), vi.fn(), []));

  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
    vi.mocked(open).mockReset();
    vi.mocked(toast.error).mockClear();
    useUIStore.setState({ confirmModalState: { isOpen: false } });
    useLibraryStore.setState({ currentFolderPath: '/Users/a/Day 1/Picks' });
  });

  it('asks where to, then asks before copying it to another drive, naming what it holds', async () => {
    vi.mocked(open).mockResolvedValueOnce('/Volumes/Archive SSD/2026');
    vi.mocked(invoke).mockResolvedValueOnce(plan({})).mockResolvedValueOnce(moved);
    const { result } = hook();
    await result.current.handleMoveFolder('/Users/a/Day 1');
    expect(vi.mocked(open)).toHaveBeenCalledWith(expect.objectContaining({ directory: true, title: 'Move “Day 1” to' }));
    expect(vi.mocked(invoke)).toHaveBeenCalledWith(Invokes.PlanMoveFolder, {
      path: '/Users/a/Day 1',
      destinationFolder: '/Volumes/Archive SSD/2026',
    });
    expect(vi.mocked(invoke)).not.toHaveBeenCalledWith(Invokes.MoveFolder, expect.anything());
    const asked = useUIStore.getState().confirmModalState;
    expect(asked.title).toBe('Move to Archive SSD?');
    expect(asked.message).toBe(describeCrossVolumeFolderMove(plan({}), 'Day 1'));
    expect(asked.message).toContain('The folder “Day 1” with everything in it (3 photos, 9 files, 98.7 MB)');
    expect(asked.message).toContain('the Trash of Macintosh HD');
    asked.onConfirm?.();
    await waitFor(() =>
      expect(vi.mocked(invoke)).toHaveBeenCalledWith(Invokes.MoveFolder, {
        path: '/Users/a/Day 1',
        destinationFolder: '/Volumes/Archive SSD/2026',
      }),
    );
    // The shown folder follows it.
    await waitFor(() =>
      expect(useLibraryStore.getState().currentFolderPath).toBe('/Volumes/Archive SSD/2026/Day 1/Picks'),
    );
  });

  it('moves on one drive without asking, and nothing when no folder was picked', async () => {
    const { result } = hook();
    await result.current.handleMoveFolder('/Users/a/Day 1');
    expect(vi.mocked(invoke)).not.toHaveBeenCalled();
    vi.mocked(invoke).mockResolvedValueOnce(plan({ crossVolume: false })).mockResolvedValueOnce(moved);
    await result.current.handleMoveFolder('/Users/a/Day 1', '/Users/a/2026');
    expect(useUIStore.getState().confirmModalState.isOpen).toBe(false);
    expect(vi.mocked(invoke)).toHaveBeenCalledWith(Invokes.MoveFolder, {
      path: '/Users/a/Day 1',
      destinationFolder: '/Users/a/2026',
    });
  });

  it('says why a folder cannot go to that drive and moves nothing', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(plan({ refusal: '/Users/a/Day 1/x is a link or special file' }));
    const { result } = hook();
    await result.current.handleMoveFolder('/Users/a/Day 1', '/Volumes/Archive SSD');
    expect(toast.error).toHaveBeenCalledWith('Could not move the folder: /Users/a/Day 1/x is a link or special file');
    expect(vi.mocked(invoke)).not.toHaveBeenCalledWith(Invokes.MoveFolder, expect.anything());
  });
});
it("Library Undo after rating then moving calls the file journal before metadata restore", async () => {
  useCatalogUndoStore.setState(useCatalogUndoStore.getInitialState());
  useUIStore.setState({ activeView: "library" });
  useEditorStore.setState({ selectedImage: null });
  const moved = summary({
    id: "move-after-rating",
    kind: "move",
    moves: [{ from: "/a.nef", to: "/other/a.nef" }],
  });
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === Invokes.PlanMoveFiles) return { crossVolume: false };
    if (command === Invokes.MoveFiles || command === Invokes.UndoFileOperation)
      return moved;
    return [];
  });
  const { result } = renderHook(() =>
    useFileOperations(
      vi.fn(async () => {}),
      vi.fn(async () => {}),
      vi.fn(),
      vi.fn(),
      [],
    ),
  );
  await catalogAction("rating", async () => [
    {
      path: "/a.nef",
      before: { kind: "rating", value: 0 },
      after: { kind: "rating", value: 4 },
    },
  ]);
  await result.current.handleMoveToFolder(["/a.nef"], "/other");
  vi.mocked(invoke).mockClear();
  await appUndo();
  expect(invoke).toHaveBeenCalledWith(Invokes.UndoFileOperation);
  expect(invoke).not.toHaveBeenCalledWith(
    Invokes.RestoreCulling,
    expect.anything(),
  );
  cleanup();
});

it.each(['move', 'paste', 'trash', 'trash-associated'] as const)('metadata waits for a companion affected by %s', async (operation) => {
  useCatalogUndoStore.setState(useCatalogUndoStore.getInitialState());
  useUIStore.setState({ activeView: 'library' });
  useEditorStore.setState({ selectedImage: null });
  useLibraryStore.setState({ sessionDirectory: null, currentFolderPath: '/dest', imageList: [], libraryActivePath: null });
  useProcessStore.setState({ copiedFilePaths: ['/capture/a.nef'] });
  let finish!: (value: FileOperationSummary) => void;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === Invokes.PlanMoveFiles) return { crossVolume: false };
    if ([Invokes.MoveFiles, Invokes.DeleteFilesFromDisk, Invokes.DeleteFilesWithAssociated].includes(command as Invokes))
      return new Promise<FileOperationSummary>((resolve) => { finish = resolve; });
    return [];
  });
  const hook = renderHook(() => useFileOperations(vi.fn(async () => {}), vi.fn(async () => {}), vi.fn(), vi.fn(), []));
  const running = operation === 'move' ? hook.result.current.handleMoveToFolder(['/capture/a.nef'], '/dest')
    : operation === 'paste' ? hook.result.current.handlePasteFiles('move', '/dest')
      : hook.result.current.executeDelete(['/capture/a.nef'], { includeAssociated: operation === 'trash-associated' });
  await waitFor(() => expect(finish).toBeTypeOf('function'));
  let target: string | null = null;
  const rated = catalogAction('rating', async ({ resolvePaths }) => {
    target = resolvePaths(['/capture/a.jpg'])[0];
    return [{ path: target, before: { kind: 'rating', value: 0 }, after: { kind: 'rating', value: 4 } }];
  }, ['/capture/a.jpg']);
  try {
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(target).toBeNull();
  } finally {
    finish(summary({ kind: 'move', moves: [{ from: '/capture/a.nef', to: '/dest/a.nef' }, { from: '/capture/a.jpg', to: '/dest/a.jpg' }] }));
    await running;
    await rated;
    hook.unmount();
  }
  expect(target).toBe('/dest/a.jpg');
});
