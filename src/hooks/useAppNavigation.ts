import { useCallback, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { openDialog } from '../utils/fileDialog';
import { homeDir } from '@tauri-apps/api/path';
import { toast } from 'react-toastify';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { useUIStore } from '../store/useUIStore';
import { useProcessStore } from '../store/useProcessStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { Invokes, LibraryViewMode, ImageFile, type Activation } from '../components/ui/AppProperties';
import type { FolderTree } from '../utils/folderTree';
import { Adjustments, INITIAL_ADJUSTMENTS } from '../utils/adjustments';
import { globalImageCache } from '../utils/ImageLRUCache';
import { debouncedSave, debouncedSetHistory } from './useEditorActions';
import { loadLibraryImages, markLibraryPublication } from '../utils/libraryListing';
import { noteListedOffline } from '../utils/offline';
import { noteBackgroundAiError } from '../utils/aiErrors';
import { performanceDiagnosticsEnabled, reportPerformance } from './usePerformanceDiagnostics';
import { isBelow } from '../utils/catalogFolders';
import { viewerShown } from '../utils/viewerMode';

/** The folder of `roots` that `path` is in (or is), the innermost one: a
 * name prefix is not a parent (`/photos/2024-old` is not in `/photos/2024`). */
export function rootContaining(path: string, roots: string[]): string | undefined {
  return roots
    .filter((root) => path === root || isBelow(path, root))
    .reduce<string | undefined>((best, root) => (!best || root.length > best.length ? root : best), undefined);
}

/** Longest time the previous photograph's frame stands in for the next one. */
const HOLD_FRAME_MS = 700;
/** Longest time the panels keep the previous photograph's look while the
 * next one loads (after that they show it is not ready yet). */
export const HOLD_PANELS_MS = 2000;

type ImageCacheEntry = Parameters<typeof globalImageCache.set>[1];

export interface AppNavigationProps {
  clearThumbnailQueue: () => void;
  refs: {
    transformWrapperRef: React.RefObject<{ resetTransform(time?: number): void } | null>;
    cachedEditStateRef: React.RefObject<ImageCacheEntry | null>;
    selectedImagePathRef: React.RefObject<string | null>;
    isBackendReadyRef: React.RefObject<boolean>;
    latestRenderedJobIdRef: React.RefObject<number>;
    previewJobIdRef: React.RefObject<number>;
    currentResRef: React.RefObject<number>;
    prevAdjustmentsRef: React.RefObject<{ path: string; adjustments: Adjustments } | null>;
  };
}


/** Another folder or album opens: the photo shown in Develop closes (its
 * pending edits saved first), so the file details, metadata and export
 * never act on a photo the new view does not list. */
function closeEditorImage() {
  const { selectedImage, resetHistory, setEditor } = useEditorStore.getState();
  if (!selectedImage) return;
  debouncedSave.flush();
  debouncedSetHistory.cancel();
  setEditor({ selectedImage: null, finalPreviewUrl: null, uncroppedAdjustedPreviewUrl: null, histogram: null });
  setEditor({ adjustments: INITIAL_ADJUSTMENTS });
  resetHistory(INITIAL_ADJUSTMENTS);
  useEditorStore.getState().patchesSentToBackend.clear();
}

export function useAppNavigation({ clearThumbnailQueue, refs }: AppNavigationProps) {
  const libraryLoad = useRef<AbortController | null>(null);
  useEffect(() => () => libraryLoad.current?.abort(), []);
  const {
    transformWrapperRef,
    cachedEditStateRef,
    selectedImagePathRef,
    isBackendReadyRef,
    latestRenderedJobIdRef,
    previewJobIdRef,
    currentResRef,
    prevAdjustmentsRef,
  } = refs;

  const handleGoHome = useCallback(() => {
    libraryLoad.current?.abort();
    useLibraryStore.getState().setLibrary({
      rootPaths: [],
      currentFolderPath: null,
      activeAlbumId: null,
      imageList: [],
      imageRatings: {},
      folderTrees: [],
      multiSelectedPaths: [],
      libraryActivePath: null,
      expandedFolders: new Set(),
      isViewLoading: false,
    });
  }, []);

  const handleBackToLibrary = useCallback(() => {
    const { selectedImage } = useEditorStore.getState();
    const { setLibrary } = useLibraryStore.getState();
    const { setUI } = useUIStore.getState();

    if (selectedImage?.path && cachedEditStateRef.current) {
      globalImageCache.set(selectedImage.path, cachedEditStateRef.current);
    }
    if (transformWrapperRef.current) {
      transformWrapperRef.current.resetTransform(0);
    }
    useEditorStore.getState().setEditor({
      zoom: 1,
      showOriginal: false,
      previewOverride: null,
    });

    debouncedSave.flush();
    debouncedSetHistory.flush();

    const lastActivePath = selectedImage?.path ?? null;

    setLibrary({ libraryActivePath: lastActivePath });
    setUI({ activeView: 'library', slideDirection: 1 });
  }, [cachedEditStateRef, transformWrapperRef]);

  const handleImageSelect = useCallback(
    async (path: string, openInEditor: boolean = true) => {
      const { selectedImage, resetHistory, setEditor } = useEditorStore.getState();
      const { setLibrary, multiSelectedPaths } = useLibraryStore.getState();
      const { setUI } = useUIStore.getState();

      // Develop, or the Library's loupe or full screen: the native viewer
      // is showing a photograph, so the next one swaps in as there.
      const wasInEditor = viewerShown(useUIStore.getState());
      if (openInEditor) {
        setUI({ activeView: 'editor' });
      }

      if (selectedImage?.path === path) return;

      // Capture One-style switch: the native side shows the next photograph's
      // developed preview at once, before any of the webview's own work below.
      if (performanceDiagnosticsEnabled) {
        reportPerformance('activate_decision', {
          wasInEditor: wasInEditor ? 1 : 0,
          shown: useEditorStore.getState().hasRenderedFirstFrame ? 1 : 0,
        });
      }
      useEditorStore.getState().setEditor({ stillPreview: null });
      if (wasInEditor && useEditorStore.getState().hasRenderedFirstFrame) {
        void invoke<Activation>(Invokes.ActivatePhoto, {
          path,
          adjustments: globalImageCache.get(path)?.adjustments ?? null,
        })
          .then((shown) => {
            // Its thumbnail is on screen natively: held (instead of the
            // previous photograph's frame) until its first developed frame,
            // and marked as a preview meanwhile.
            const now = useEditorStore.getState();
            if (shown === 'still' && now.selectedImage?.path === path && !now.hasRenderedFirstFrame) {
              now.setEditor({ stillPreview: path, holdingNativeFrame: true });
            }
          })
          .catch(() => {});
      }

      useEditorStore.getState().patchesSentToBackend.clear();
      debouncedSave.flush();
      debouncedSetHistory.cancel();

      if (selectedImage?.path && cachedEditStateRef.current) {
        globalImageCache.set(selectedImage.path, cachedEditStateRef.current);
      }

      const cachedThumb = useProcessStore.getState().thumbnails[path];
      const cachedMedium = useProcessStore.getState().mediumThumbnails[path] || cachedThumb;

      const cached = globalImageCache.get(path);
      const isFrontendCached = Boolean(cached && cached.selectedImage?.isReady);
      // Every source switch waits for native activation. Meanwhile the old
      // frame stays on screen (as Lightroom and Capture One swap previews)
      // when Develop was already showing one, for at most HOLD_FRAME_MS;
      // the new photograph never renders into the old frame's placement.
      const editorState = useEditorStore.getState();
      const holdingNativeFrame =
        wasInEditor &&
        editorState.hasRenderedFirstFrame &&
        useSettingsStore.getState().appSettings?.useWgpuRenderer !== false;
      // Stepping from a ready photograph (arrows, filmstrip, multi view):
      // the panels keep showing its values until the next one's arrive.
      const panelsHeld = wasInEditor && Boolean(selectedImage?.isReady);
      setEditor({ hasRenderedFirstFrame: false, nativeCropFrame: false, holdingNativeFrame, panelsHeld });
      if (panelsHeld) {
        setTimeout(() => {
          const now = useEditorStore.getState();
          if (now.selectedImage?.path === path && now.panelsHeld && !now.selectedImage.isReady) {
            useEditorStore.getState().setEditor({ panelsHeld: false });
          }
        }, HOLD_PANELS_MS);
      }
      if (holdingNativeFrame) {
        setTimeout(() => {
          const now = useEditorStore.getState();
          // A still of this photograph stays until its developed frame.
          if (now.selectedImage?.path === path && now.holdingNativeFrame && now.stillPreview !== path) {
            useEditorStore.getState().setEditor({ holdingNativeFrame: false });
          }
        }, HOLD_FRAME_MS);
      }

      selectedImagePathRef.current = path;

      const newMultiSelectedPaths = multiSelectedPaths.includes(path) ? multiSelectedPaths : [path];

      setLibrary({
        multiSelectedPaths: newMultiSelectedPaths,
        libraryActivePath: path,
        selectionAnchorPath: path,
      });

      setEditor({
        showOriginal: false,
        activeMaskId: null,
        activeMaskContainerId: null,
        isWbPickerActive: false,
        colorPickerTarget: null,
        colorRangeShow: null,
        layerSolo: null,
        activeColorRangeId: null,
        previewOverride: null,
      });

      setUI({ compactEditorPanelHeightOverride: null });

      if (isFrontendCached && cached) {
        setEditor({
          selectedImage: {
            ...cached.selectedImage,
            isReady: false,
            isDecoded: false,
            isPreviewReady: false,
            thumbnailUrl: cachedMedium || cached.selectedImage.thumbnailUrl,
            thumbnailSource: cachedMedium ? undefined : cached.selectedImage.thumbnailSource,
          },
          originalSize: cached.originalSize,
          previewSize: cached.previewSize,
          histogram: cached.histogram,
          waveform: cached.waveform,
          finalPreviewUrl: cached.finalPreviewUrl,
          uncroppedAdjustedPreviewUrl: cached.uncroppedPreviewUrl,
        });

        setEditor({ adjustments: cached.adjustments });
        resetHistory(cached.adjustments);
        prevAdjustmentsRef.current = null;

        setLibrary({ isViewLoading: true });
        latestRenderedJobIdRef.current = previewJobIdRef.current;
        isBackendReadyRef.current = false;
        currentResRef.current = 0;
        // useImageLoader handles cached and uncached activation together; it
        // refreshes metadata concurrently and publishes readiness only once.

        return;
      }

      isBackendReadyRef.current = true;

      const imageFile = useLibraryStore.getState().imageList.find((img) => img.path === path);
      setEditor({
        selectedImage: {
          exif: imageFile?.exif ?? null,
          group_id: imageFile?.group_id ?? null,
          height: 0,
          isRaw: false,
          isReady: false,
          isDecoded: false,
          metadata: null,
          path,
          thumbnailUrl: cachedMedium,
          width: 0,
        },
        originalSize: { width: 0, height: 0 },
        previewSize: { width: 0, height: 0 },
        // Held panels keep the last histogram until the next one arrives.
        ...(panelsHeld ? {} : { histogram: null, waveform: null }),
        uncroppedAdjustedPreviewUrl: null,
      });

      setLibrary({ isViewLoading: true });

      setEditor((state) => {
        const prev = state.finalPreviewUrl;
        if (prev?.startsWith('blob:') && !globalImageCache.isProtected(prev)) {
          setTimeout(() => {
            if (!globalImageCache.isProtected(prev)) {
              URL.revokeObjectURL(prev);
            }
          }, 250);
        }
        return { finalPreviewUrl: null };
      });

      setEditor((state) => {
        if (state.interactivePatch?.url) URL.revokeObjectURL(state.interactivePatch.url);
        return { interactivePatch: null };
      });
    },
    [
      cachedEditStateRef,
      currentResRef,
      isBackendReadyRef,
      latestRenderedJobIdRef,
      prevAdjustmentsRef,
      previewJobIdRef,
      selectedImagePathRef,
    ],
  );

  const handleSelectSubfolder = useCallback(
    async (
      path: string | null,
      isNewRoot = false,
      preloadedImages?: ImageFile[],
      expandParents = true,
      preserveEditor = false,
      skipHistory = false,
    ) => {
      libraryLoad.current?.abort();
      const load = new AbortController();
      libraryLoad.current = load;
      if (!path) {
        handleGoHome();
        return;
      }
      const { appSettings, handleSettingsChange } = useSettingsStore.getState();
      const { pinnedFolders } = appSettings || { pinnedFolders: [] };
      const { setLibrary } = useLibraryStore.getState();
      const { setUI } = useUIStore.getState();
      const { setProcess } = useProcessStore.getState();
      const libraryViewMode = appSettings?.libraryViewMode;

      if (!skipHistory && path) {
        useLibraryStore.getState().pushNavHistory({ type: 'folder', path });
      }

      if (!preserveEditor) {
        await invoke(Invokes.CancelThumbnailGeneration);
        if (load.signal.aborted) return;
        clearThumbnailQueue();
        setLibrary({ isViewLoading: true, activeAlbumId: null, libraryScrollTop: 0 });
        setProcess({ thumbnails: {} });
        globalImageCache.clear();
        setUI({ activeView: 'library' });
      } else {
        setLibrary({ isViewLoading: true });
      }

      try {
        const { rootPaths, expandedFolders: currentExpandedFolders } = useLibraryStore.getState();
        let newExpandedFolders = new Set(currentExpandedFolders);

        if (isNewRoot && path) {
          newExpandedFolders = new Set([path]);
          if (appSettings) {
            handleSettingsChange({ ...appSettings, lastRootPath: path });
          }
        } else if (path && expandParents) {
          const allRoots = [...(rootPaths || []), ...(pinnedFolders || [])].filter(Boolean) as string[];
          const relevantRoot = rootContaining(path, allRoots);

          if (relevantRoot) {
            const separator = path.includes('/') ? '/' : '\\';
            const parentSeparatorIndex = path.lastIndexOf(separator);

            if (parentSeparatorIndex > -1 && path.length > relevantRoot.length) {
              let current = path.substring(0, parentSeparatorIndex);
              while (current && current.length >= relevantRoot.length) {
                newExpandedFolders.add(current);
                const nextParentIndex = current.lastIndexOf(separator);
                if (nextParentIndex === -1 || current === relevantRoot) break;
                current = current.substring(0, nextParentIndex);
              }
            }
            newExpandedFolders.add(relevantRoot);
          }
        }

        setLibrary({
          currentFolderPath: path,
          expandedFolders: newExpandedFolders,
          ...(preserveEditor ? {} : { imageList: [], multiSelectedPaths: [], libraryActivePath: null }),
        });

        if (!preserveEditor) closeEditorImage();

        let files: ImageFile[];
        if (preloadedImages) {
          files = preloadedImages;
        } else {
          files = await loadLibraryImages(path, libraryViewMode === LibraryViewMode.Recursive, load.signal);
        }
        if (load.signal.aborted) return;

        const publishStarted = performanceDiagnosticsEnabled ? performance.now() : 0;
        const initialRatings: Record<string, number> = {};
        files.forEach((f) => {
          if (f.rating !== undefined) {
            initialRatings[f.path] = f.rating;
          }
        });
        markLibraryPublication(files);
        setLibrary({ imageRatings: initialRatings, imageList: files });
        noteListedOffline(files);
        if (performanceDiagnosticsEnabled) reportPerformance('library_publish', {
          ms: performance.now() - publishStarted, assets: files.length,
        });

        if (!preserveEditor) {
          invoke(Invokes.StartBackgroundIndexing, { folderPath: path }).catch((err) => {
            console.error('Failed to start background indexing:', err);
            // Automatic keywords are on but the AI runtime is missing.
            noteBackgroundAiError(err);
          });
        }
      } catch (err) {
        if (load.signal.aborted) return;
        console.error('Failed to load folder contents:', err);
        toast.error('Failed to load images from the selected folder.');
      } finally {
        if (!load.signal.aborted) useLibraryStore.getState().setLibrary({ isViewLoading: false });
      }
    },
    [clearThumbnailQueue, handleGoHome],
  );

  const handleSelectAlbum = useCallback(
    async (albumId: string, albumName: string, imagePaths: string[], preserveEditor = false, skipHistory = false) => {
      libraryLoad.current?.abort();
      const load = new AbortController();
      libraryLoad.current = load;
      const { setLibrary } = useLibraryStore.getState();
      const { setUI } = useUIStore.getState();

      if (!skipHistory) {
        useLibraryStore.getState().pushNavHistory({ type: 'album', path: albumId, albumName, images: imagePaths });
      }

      if (!preserveEditor) {
        await invoke(Invokes.CancelThumbnailGeneration);
        if (load.signal.aborted) return;
        clearThumbnailQueue();
        setLibrary({ libraryScrollTop: 0 });
        globalImageCache.clear();
        setUI({ activeView: 'library' });
      }

      const albumFolderPath = `Album: ${albumName}`;

      setLibrary({
        isViewLoading: true,
        currentFolderPath: albumFolderPath,
        activeAlbumId: albumId,
        ...(preserveEditor ? {} : { multiSelectedPaths: [], libraryActivePath: null }),
      });
      // As for folders: the previous photo is not left as the edit and export target.
      if (!preserveEditor) closeEditorImage();

      try {
        const files: ImageFile[] = await invoke(Invokes.GetAlbumImages, { paths: imagePaths });
        if (load.signal.aborted) return;

        const initialRatings: Record<string, number> = {};
        files.forEach((f) => {
          if (f.rating !== undefined) initialRatings[f.path] = f.rating;
        });

        markLibraryPublication(files);
        setLibrary({
          imageRatings: initialRatings,
          imageList: files,
          ...(preserveEditor ? {} : { multiSelectedPaths: [], libraryActivePath: null }),
        });
      } catch (err) {
        if (load.signal.aborted) return;
        console.error('Failed to load album images:', err);
        toast.error(`Failed to load album: ${err}`);
      } finally {
        if (!load.signal.aborted) setLibrary({ isViewLoading: false });
      }
    },
    [clearThumbnailQueue],
  );

  const handleNavBack = useCallback(async () => {
    const { navHistory, navIndex, setLibrary } = useLibraryStore.getState();

    if (navIndex > 0) {
      const targetIndex = navIndex - 1;
      const target = navHistory[targetIndex];
      setLibrary({ navIndex: targetIndex });

      if (target.type === 'folder') {
        handleSelectSubfolder(target.path, false, undefined, true, false, true);
      } else if (target.type === 'album') {
        handleSelectAlbum(target.path, target.albumName!, target.images!, false, true);
      }
    }
  }, [handleSelectSubfolder, handleSelectAlbum]);

  const handleNavForward = useCallback(async () => {
    const { navHistory, navIndex, setLibrary } = useLibraryStore.getState();

    if (navIndex < navHistory.length - 1) {
      const targetIndex = navIndex + 1;
      const target = navHistory[targetIndex];
      setLibrary({ navIndex: targetIndex });

      if (target.type === 'folder') {
        handleSelectSubfolder(target.path, false, undefined, true, false, true);
      } else if (target.type === 'album') {
        handleSelectAlbum(target.path, target.albumName!, target.images!, false, true);
      }
    }
  }, [handleSelectSubfolder, handleSelectAlbum]);

  const handleOpenFolder = useCallback(async () => {
    const { appSettings, handleSettingsChange } = useSettingsStore.getState();
    const { rootPaths, folderTrees, setLibrary } = useLibraryStore.getState();

    try {
      let selectedPath = '';
      const selected = await openDialog({ directory: true, multiple: false, defaultPath: await homeDir() });
      if (typeof selected === 'string') {
        selectedPath = selected;
      }

      if (selectedPath) {
        if (!rootPaths.includes(selectedPath)) {
          const newRootPaths = [...rootPaths, selectedPath];
          setLibrary({ rootPaths: newRootPaths });

          if (appSettings) {
            handleSettingsChange({ ...appSettings, rootFolders: newRootPaths });
          }

          setLibrary({ isTreeLoading: true });
          try {
            const newTree = await invoke<FolderTree>(Invokes.GetFolderTree, {
              path: selectedPath,
              expandedFolders: [selectedPath],
              showImageCounts:
                appSettings?.enableFolderImageCounts || appSettings?.folderTreeSort?.key === 'imageCount',
            });
            setLibrary({ folderTrees: [...folderTrees, newTree] });
          } catch (e) {
            toast.error(`Failed to load folder tree: ${e}`);
          } finally {
            setLibrary({ isTreeLoading: false });
          }
        }
        await handleSelectSubfolder(selectedPath, true);
      }
    } catch (err) {
      console.error('Failed to open directory dialog:', err);
      toast.error('Failed to open folder selection dialog.');
    }
  }, [handleSelectSubfolder]);

  return {
    handleGoHome,
    handleBackToLibrary,
    handleImageSelect,
    handleSelectSubfolder,
    handleSelectAlbum,
    handleOpenFolder,
    handleNavBack,
    handleNavForward,
  };
}
