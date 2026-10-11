import { renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { describeFileOperation, FileOperationSummary, useFileOperations } from './useFileOperations';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useProcessStore } from '../store/useProcessStore';
import { useUIStore } from '../store/useUIStore';
import { Invokes } from '../components/ui/AppProperties';
import { OFFLINE_COUNTED } from '../utils/offline';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock('react-toastify', () => ({ toast: { error: vi.fn(), success: vi.fn(), info: vi.fn() } }));

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
