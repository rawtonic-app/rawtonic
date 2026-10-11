import { useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'react-toastify';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { Invokes, ImageFile } from '../components/ui/AppProperties';
import { globalImageCache } from '../utils/ImageLRUCache';
import { useSettingsStore } from '../store/useSettingsStore';
import { computeSortedLibrary } from './useSortedLibrary';
import { expandGroupedPaths } from '../utils/imageGrouping';
import { invokeInOrder } from '../utils/catalogWrites';
import type { FolderTree } from '../utils/folderTree';

/** Modifier keys read from a selection click (React or DOM mouse event). */
type SelectionModifiers = Pick<MouseEvent, 'ctrlKey' | 'metaKey' | 'shiftKey'>;

/** A batch save's error: `SaveFailure` in src-tauri/src/file_management/edits.rs
 * names the photos not saved; any other error counts for all of `paths`. */
export function saveFailure(err: unknown, paths: string[]): { message: string; failed: string[] } {
  if (err && typeof err === 'object' && 'message' in err && 'failed' in err && Array.isArray(err.failed)) {
    return { message: String(err.message), failed: err.failed.map(String) };
  }
  return { message: String(err), failed: paths };
}

export function useLibraryActions(handleImageSelect?: (path: string, openInEditor?: boolean) => void) {
  const handleRate = useCallback((newRating: number, paths?: string[]) => {
    const { multiSelectedPaths, imageList, imageRatings, setLibrary } = useLibraryStore.getState();
    const { selectedImage } = useEditorStore.getState();

    const selectedPaths =
      paths || (multiSelectedPaths.length > 0 ? multiSelectedPaths : selectedImage ? [selectedImage.path] : []);
    if (selectedPaths.length === 0) return;

    const groupingMode = useSettingsStore.getState().appSettings?.grouping ?? 'off';
    const pathsToRate = expandGroupedPaths(imageList, selectedPaths, groupingMode);

    const currentRating = imageRatings[selectedPaths[0]] || 0;
    const finalRating = newRating === currentRating ? 0 : newRating;
    const previousRatings = Object.fromEntries(pathsToRate.map((p) => [p, imageRatings[p]]));

    setLibrary((state) => {
      const newRatings = { ...state.imageRatings };
      pathsToRate.forEach((p) => {
        newRatings[p] = finalRating;
      });
      return { imageRatings: newRatings };
    });

    invokeInOrder(Invokes.SetRatingForPaths, { paths: pathsToRate, rating: finalRating }).catch((err) => {
      console.error(err);
      // Photos whose rating was not saved show their previous one again
      // (unless rated anew meanwhile).
      const { message, failed } = saveFailure(err, pathsToRate);
      setLibrary((state) => {
        const ratings = { ...state.imageRatings };
        failed.forEach((p) => {
          if (ratings[p] !== finalRating) return;
          if (previousRatings[p] === undefined) delete ratings[p];
          else ratings[p] = previousRatings[p];
        });
        return { imageRatings: ratings };
      });
      toast.error(`Failed to apply rating: ${message}`);
    });
  }, []);

  const handleSetColorLabel = useCallback(async (color: string | null, paths?: string[]) => {
    const { multiSelectedPaths, libraryActivePath, imageList, setLibrary } = useLibraryStore.getState();
    const { selectedImage } = useEditorStore.getState();

    const selectedPaths =
      paths || (multiSelectedPaths.length > 0 ? multiSelectedPaths : selectedImage ? [selectedImage.path] : []);
    if (selectedPaths.length === 0) return;

    const groupingMode = useSettingsStore.getState().appSettings?.grouping ?? 'off';
    const pathsToUpdate = expandGroupedPaths(imageList, selectedPaths, groupingMode);

    const primaryPath = selectedImage?.path || libraryActivePath;
    const primaryImage = imageList.find((img: ImageFile) => img.path === primaryPath);
    let currentColor = null;
    if (primaryImage && primaryImage.tags) {
      const colorTag = primaryImage.tags.find((tag: string) => tag.startsWith('color:'));
      if (colorTag) currentColor = colorTag.substring(6);
    }
    const finalColor = color !== null && color === currentColor ? null : color;

    const applyLabel = (paths: string[]) =>
      setLibrary((state) => ({
        imageList: state.imageList.map((image: ImageFile) => {
          if (paths.includes(image.path)) {
            const otherTags = (image.tags || []).filter((tag: string) => !tag.startsWith('color:'));
            const newTags = finalColor ? [...otherTags, `color:${finalColor}`] : otherTags;
            return { ...image, tags: newTags };
          }
          return image;
        }),
      }));
    try {
      await invokeInOrder(Invokes.SetColorLabelForPaths, { paths: pathsToUpdate, color: finalColor });
      applyLabel(pathsToUpdate);
    } catch (err) {
      // The photos that were saved show their new label.
      const { message, failed } = saveFailure(err, pathsToUpdate);
      applyLabel(pathsToUpdate.filter((p) => !failed.includes(p)));
      toast.error(`Failed to set color label: ${message}`);
    }
  }, []);

  const handleTagsChanged = useCallback((changedPaths: string[], newTags: { tag: string; isUser: boolean }[]) => {
    const { imageList } = useLibraryStore.getState();
    const groupingMode = useSettingsStore.getState().appSettings?.grouping ?? 'off';
    const pathsToUpdate = expandGroupedPaths(imageList, changedPaths, groupingMode);

    useLibraryStore.getState().setLibrary((state) => ({
      imageList: state.imageList.map((image) => {
        if (pathsToUpdate.includes(image.path)) {
          const colorTags = (image.tags || []).filter((t) => t.startsWith('color:'));
          const prefixedNewTags = newTags.map((t) => (t.isUser ? `user:${t.tag}` : t.tag));
          const finalTags = [...colorTags, ...prefixedNewTags].sort();
          return { ...image, tags: finalTags.length > 0 ? finalTags : null };
        }
        return image;
      }),
    }));
  }, []);

  const handleUpdateExif = useCallback(async (paths: Array<string> | undefined, updates: Record<string, string>) => {
    const { multiSelectedPaths, imageList, setLibrary } = useLibraryStore.getState();
    const { selectedImage, setEditor } = useEditorStore.getState();

    const pathsToUpdate =
      paths && paths.length > 0
        ? paths
        : multiSelectedPaths.length > 0
          ? multiSelectedPaths
          : selectedImage
            ? [selectedImage.path]
            : [];
    if (pathsToUpdate.length === 0) return;

    const physicalPathsSet = new Set(pathsToUpdate.map((p) => p.split('?vc=')[0]));
    const physicalPathsArray = Array.from(physicalPathsSet);

    try {
      await invoke(Invokes.UpdateExifFields, { paths: physicalPathsArray, updates });

      setEditor((state) => {
        if (!state.selectedImage || !physicalPathsSet.has(state.selectedImage.path.split('?vc=')[0])) return state;
        return { selectedImage: { ...state.selectedImage, exif: { ...(state.selectedImage.exif || {}), ...updates } } };
      });

      setLibrary((state) => ({
        imageList: state.imageList.map((img) => {
          if (physicalPathsSet.has(img.path.split('?vc=')[0])) {
            return { ...img, exif: { ...(img.exif || {}), ...updates } };
          }
          return img;
        }),
      }));

      pathsToUpdate.forEach((p) => {
        const cached = globalImageCache.get(p);
        if (cached && cached.selectedImage) {
          globalImageCache.set(p, {
            ...cached,
            selectedImage: { ...cached.selectedImage, exif: { ...(cached.selectedImage.exif || {}), ...updates } },
          });
        }
      });
    } catch (err) {
      toast.error(`Failed to update metadata: ${err}`);
    }
  }, []);

  const handleClearSelection = useCallback(() => {
    const activeView = useUIStore.getState().activeView;
    const { selectedImage } = useEditorStore.getState();

    if (activeView === 'editor' && selectedImage) {
      useLibraryStore.getState().setLibrary({
        multiSelectedPaths: [selectedImage.path],
        libraryActivePath: selectedImage.path,
        selectionAnchorPath: selectedImage.path,
      });
    } else {
      useLibraryStore.getState().setLibrary({
        multiSelectedPaths: [],
        libraryActivePath: null,
        selectionAnchorPath: null,
      });

      useEditorStore.getState().setEditor({
        selectedImage: null,
        finalPreviewUrl: null,
        uncroppedAdjustedPreviewUrl: null,
        histogram: null,
        waveform: null,
        activeMaskId: null,
        activeMaskContainerId: null,
        isWbPickerActive: false,
        colorPickerTarget: null,
        colorRangeShow: null,
        activeColorRangeId: null,
      });
    }
  }, []);

  const handleMultiSelectClick = useCallback(
    (
      path: string,
      event: SelectionModifiers,
      options: {
        onSimpleClick(p: string, isAlreadySelected: boolean): void;
        updateLibraryActivePath: boolean;
        shiftAnchor: string | null;
      },
    ) => {
      const libraryState = useLibraryStore.getState();
      const { multiSelectedPaths, setLibrary } = libraryState;
      const { ctrlKey, metaKey, shiftKey } = event;
      const isCtrlPressed = ctrlKey || metaKey;
      const { shiftAnchor, onSimpleClick, updateLibraryActivePath } = options;

      const isAlreadySelected = multiSelectedPaths.includes(path);

      if (shiftKey && shiftAnchor) {
        const sortedImageList = computeSortedLibrary(libraryState, useSettingsStore.getState());
        const anchorIndex = sortedImageList.findIndex((f) => f.path === shiftAnchor);
        const currentIndex = sortedImageList.findIndex((f) => f.path === path);

        if (anchorIndex !== -1 && currentIndex !== -1) {
          const start = Math.min(anchorIndex, currentIndex);
          const end = Math.max(anchorIndex, currentIndex);
          const range = sortedImageList.slice(start, end + 1).map((f) => f.path);
          const baseSelection = isCtrlPressed ? multiSelectedPaths : [];
          const newSelection = Array.from(new Set([...baseSelection, ...range]));

          setLibrary({ multiSelectedPaths: newSelection, selectionAnchorPath: path });
          if (updateLibraryActivePath) setLibrary({ libraryActivePath: path });
        }
      } else if (isCtrlPressed) {
        const newSelection = new Set(multiSelectedPaths);
        if (newSelection.has(path)) newSelection.delete(path);
        else newSelection.add(path);

        const newSelectionArray = Array.from(newSelection);
        setLibrary({ multiSelectedPaths: newSelectionArray, selectionAnchorPath: path });

        if (updateLibraryActivePath) {
          if (newSelectionArray.includes(path)) setLibrary({ libraryActivePath: path });
          else if (newSelectionArray.length > 0)
            setLibrary({ libraryActivePath: newSelectionArray[newSelectionArray.length - 1] });
          else setLibrary({ libraryActivePath: null });
        }
      } else {
        onSimpleClick(path, isAlreadySelected);
      }
    },
    [],
  );

  const handleLibraryImageSingleClick = useCallback(
    (path: string, event: SelectionModifiers) => {
      const { selectionAnchorPath, libraryActivePath, setLibrary } = useLibraryStore.getState();
      handleMultiSelectClick(path, event, {
        shiftAnchor: selectionAnchorPath ?? libraryActivePath,
        updateLibraryActivePath: true,
        onSimpleClick: (p: string, isAlreadySelected: boolean) => {
          if (isAlreadySelected) {
            setLibrary({ libraryActivePath: p, selectionAnchorPath: p });
          } else {
            setLibrary({ multiSelectedPaths: [p], libraryActivePath: p, selectionAnchorPath: p });
          }
          if (handleImageSelect) {
            handleImageSelect(p, false);
          }
        },
      });
    },
    [handleMultiSelectClick, handleImageSelect],
  );

  const handleImageClick = useCallback(
    (path: string, event: SelectionModifiers) => {
      const { selectionAnchorPath, libraryActivePath, setLibrary } = useLibraryStore.getState();
      const { selectedImage } = useEditorStore.getState();
      const inEditor = !!selectedImage;

      handleMultiSelectClick(path, event, {
        shiftAnchor: selectionAnchorPath ?? (inEditor ? selectedImage.path : libraryActivePath),
        updateLibraryActivePath: !inEditor,
        onSimpleClick: (p: string, isAlreadySelected: boolean) => {
          if (!isAlreadySelected) {
            setLibrary({ multiSelectedPaths: [p] });
          }
          if (handleImageSelect) handleImageSelect(p);
          setLibrary({ selectionAnchorPath: p });
        },
      });
    },
    [handleMultiSelectClick, handleImageSelect],
  );

  const refreshAllFolderTrees = useCallback(async () => {
    const { rootPaths, expandedFolders, catalogTrees, setLibrary } = useLibraryStore.getState();
    const { appSettings } = useSettingsStore.getState();

    const showImageCounts = appSettings?.enableFolderImageCounts ?? false;
    const pinnedFolders = appSettings?.pinnedFolders || [];
    const expandedArray = Array.from(expandedFolders);

    try {
      const updates: { folderTrees?: FolderTree[]; pinnedFolderTrees?: FolderTree[]; catalogTrees?: FolderTree[] } = {};

      if (rootPaths && rootPaths.length > 0) {
        const treesData = await invoke<FolderTree[]>(Invokes.GetPinnedFolderTrees, {
          paths: rootPaths,
          expandedFolders: expandedArray,
          showImageCounts,
        });
        updates.folderTrees = treesData;
      } else {
        updates.folderTrees = [];
      }

      if (pinnedFolders && pinnedFolders.length > 0) {
        const pinnedTreesData = await invoke<FolderTree[]>(Invokes.GetPinnedFolderTrees, {
          paths: pinnedFolders,
          expandedFolders: expandedArray,
          showImageCounts,
        });
        updates.pinnedFolderTrees = pinnedTreesData;
      } else {
        updates.pinnedFolderTrees = [];
      }

      // A catalog's folders outside the library's roots (counted, as in
      // the Folders tool).
      if (catalogTrees.length > 0) {
        updates.catalogTrees = await invoke<FolderTree[]>(Invokes.GetPinnedFolderTrees, {
          paths: catalogTrees.map((tree) => tree.path),
          expandedFolders: expandedArray,
          showImageCounts: true,
        });
      }

      if (Object.keys(updates).length > 0) {
        setLibrary(updates);
      }
    } catch (err) {
      console.error('Failed to refresh folder trees:', err);
    }
  }, []);

  const handleTogglePinFolder = useCallback(async (path: string) => {
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    const { expandedFolders, setLibrary } = useLibraryStore.getState();
    if (!appSettings) return;

    const currentPins = appSettings.pinnedFolders || [];
    const isPinned = currentPins.includes(path);
    const newPins = isPinned
      ? currentPins.filter((p: string) => p !== path)
      : [...currentPins, path].sort((a, b) => a.localeCompare(b));

    handleSettingsChange({ ...appSettings, pinnedFolders: newPins });

    try {
      const trees = await invoke<FolderTree[]>(Invokes.GetPinnedFolderTrees, {
        paths: newPins,
        expandedFolders: Array.from(expandedFolders),
        showImageCounts: appSettings.enableFolderImageCounts ?? false,
      });
      setLibrary({ pinnedFolderTrees: trees });
    } catch (err) {
      toast.error(`Failed to refresh pinned folders: ${err}`);
    }
  }, []);

  /** Lightroom's Remove for a top-level folder: the folder leaves the
   * library list only; nothing on disk is moved or deleted. */
  const handleRemoveRootFolder = useCallback((path: string, onLeaveFolder?: () => void) => {
    const { rootPaths, currentFolderPath, folderTrees, setLibrary } = useLibraryStore.getState();
    if (!rootPaths.includes(path)) return;
    const newRoots = rootPaths.filter((r) => r !== path);
    const inside =
      currentFolderPath === path ||
      !!currentFolderPath?.startsWith(path + '/') ||
      !!currentFolderPath?.startsWith(path + '\\');
    setLibrary({
      rootPaths: newRoots,
      folderTrees: folderTrees.filter((tree) => tree.path !== path),
      ...(inside
        ? {
            currentFolderPath: null,
            imageList: [],
            libraryActivePath: null,
            multiSelectedPaths: [],
            selectionAnchorPath: null,
          }
        : {}),
    });
    if (inside) onLeaveFolder?.();
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    if (!appSettings) return;
    const settings = { ...appSettings, rootFolders: newRoots };
    if (newRoots.length === 0) {
      settings.lastRootPath = null;
      settings.lastFolderState = null;
    } else if (settings.lastRootPath === path) {
      settings.lastRootPath = newRoots[0];
    }
    if (inside) settings.lastFolderState = null;
    handleSettingsChange(settings);
  }, []);

  return {
    handleRate,
    handleSetColorLabel,
    handleTagsChanged,
    handleUpdateExif,
    handleClearSelection,
    handleLibraryImageSingleClick,
    handleImageClick,
    refreshAllFolderTrees,
    handleTogglePinFolder,
    handleRemoveRootFolder,
  };
}
