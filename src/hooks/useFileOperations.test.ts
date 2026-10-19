import { renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import '../i18n';
import {
  describeEmptyTrash,
  describeFileOperation,
  FileOperationSummary,
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
import { debouncedSave } from './useEditorActions';
import { INITIAL_ADJUSTMENTS } from '../utils/adjustments';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), success: vi.fn(), info: vi.fn(), warn: vi.fn() } }));

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
  expect(describeFileOperation(summary({ sessionTrash: 2 }), '⌘Z')).toBe(
    "Moved 2 items to the session's Trash folder. ⌘Z undoes it.",
  );
  expect(describeFileOperation(summary({ items: 1, systemTrash: 1 }), 'Ctrl+Z')).toBe(
    'Moved 1 item to the Trash. Ctrl+Z undoes it.',
  );
  expect(describeFileOperation(summary({ sessionTrash: 1, systemTrash: 1 }), '⌘Z')).toContain(
    "the session's Trash folder and the Trash",
  );
});

it('names the folder photos were moved to, and other operations by their label', () => {
  expect(
    describeFileOperation(
      summary({ kind: 'move', label: 'Move 2 photos to Selects', newPaths: ['/job/Selects/A.RAF', '/job/Selects/B.RAF'] }),
      '⌘Z',
    ),
  ).toBe('Moved 2 photos to Selects. ⌘Z undoes it.');
  expect(describeFileOperation(summary({ kind: 'renameFolder', label: 'Rename folder Day 1' }), '⌘Z')).toBe(
    'Rename folder Day 1. ⌘Z undoes it.',
  );
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
    window.addEventListener(OFFLINE_COUNTED, counted);
    const { result } = renderHook(() =>
      useFileOperations(vi.fn(() => Promise.resolve()), vi.fn(() => Promise.resolve()), vi.fn(), vi.fn(), []),
    );
    await result.current.handleUndoFileOperation();
    await waitFor(() => expect(counted).toHaveBeenCalled());
    window.removeEventListener(OFFLINE_COUNTED, counted);
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

  it('saves a pending edit before the photo moves, not afterwards at its old place', async () => {
    debouncedSave('/p/A.RAF', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    await hook().executeDelete(['/p/A.RAF']);
    expect(calls().indexOf(Invokes.SaveMetadataAndUpdateThumbnail)).toBeGreaterThanOrEqual(0);
    expect(calls().indexOf(Invokes.SaveMetadataAndUpdateThumbnail)).toBeLessThan(calls().indexOf('delete_files_from_disk'));
  });

  it('saves a pending edit before a folder moves', async () => {
    debouncedSave('/p/A.RAF', { ...INITIAL_ADJUSTMENTS, exposure: 1 });
    hook().handleTrashFolder('/p');
    await useUIStore.getState().confirmModalState.onConfirm?.();
    expect(calls().indexOf(Invokes.SaveMetadataAndUpdateThumbnail)).toBeGreaterThanOrEqual(0);
    expect(calls().indexOf(Invokes.SaveMetadataAndUpdateThumbnail)).toBeLessThan(calls().indexOf(Invokes.DeleteFolder));
  });
});
