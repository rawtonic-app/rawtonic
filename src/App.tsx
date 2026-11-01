import UpdateNotice from './components/UpdateNotice';
import WhatsNewAfterUpdate from './components/WhatsNewAfterUpdate';
import AppNotices from './components/ui/AppNotices';
import ContributionHost from './components/contribute/ContributionHost';
import StudioWorkspace from './components/studio/StudioWorkspace';
import { FOLDER_SYNC_APPLIED, PREVIOUS_IMPORT_VIEW_ID, openPreviousImport, type SyncApplied } from './utils/folderSync';
import { openSessionAlbum, SESSION_ALBUM_PREFIX } from './components/studio/StudioAlbums';
import {
  DRIVES_CHANGED,
  folderOf,
  locatedPhotograph,
  locateFolderOf,
  MISSING_VIEW_ID,
  openMissingView,
  relinkedPaths,
} from './utils/offline';
import i18n from 'i18next';
import { epochOf, PerfProfiler, reportForcedLayout, reportPerformance, usePerformanceDiagnostics } from './hooks/usePerformanceDiagnostics';
import { useState, useEffect, useLayoutEffect, useCallback, useRef } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { refocusWebview } from './utils/photoFullScreen';
import { notify } from './activity/notify';
import { startActivityBridges } from './activity/bridges';
import {
  ACTIVITY_PHOTOS_VIEW_ID,
  SHOW_ACTIVITY_PHOTOS_EVENT,
  shownActivityPhotos,
  type ShowActivityPhotos,
} from './activity/showPhotos';
import clsx from 'clsx';

import TitleBar from './window/TitleBar';
import WindowControls from './window/WindowControls';
import SessionWorkspace from './components/session/SessionWorkspace';
import { openPendingXmpView, PENDING_XMP_VIEW_ID, SHOW_PENDING_XMP_EVENT } from './components/session/SessionXmpStatus';
import type { FolderTree as FolderTreeNode } from './utils/folderTree';
import SettingsPanel from './components/panel/SettingsPanel';
import GlobalTooltip from './components/ui/GlobalTooltip';
import AppModals from './components/modals/AppModals';

import { ContextMenuProvider } from './context/ContextMenuContext';
import { useSettingsStore } from './store/useSettingsStore';
import { useUIStore } from './store/useUIStore';
import { moduleOf, readModuleLayout } from './utils/panelLayout';
import { reloadSessionFlags } from './utils/pathKeyedState';
import { XMP_METADATA_READ } from './utils/catalogEvents';
import { useTextMetadataCache } from './components/studio/StudioMetadata';
import { useLibraryStore } from './store/useLibraryStore';
import { useEditorStore } from './store/useEditorStore';
import { useProcessStore } from './store/useProcessStore';
import { useShallow } from 'zustand/react/shallow';

import { useThumbnails } from './hooks/useThumbnails';
import { useKeyboardShortcuts } from './hooks/useKeyboardShortcuts';
import { useAppMenu } from './hooks/useAppMenu';
import CommandPalette from './components/ui/CommandPalette';
import { useAppLevelCommands } from './hooks/useAppLevelCommands';
import { useTauriListeners } from './hooks/useTauriListeners';
import { useFileOperations } from './hooks/useFileOperations';
import { useAppContextMenus } from './hooks/useAppContextMenus';
import { useSortedLibrary } from './hooks/useSortedLibrary';
import { useStackCommands } from './utils/stacks';
import { useCatalogSearch } from './hooks/useCatalogSearch';
import { useEyeCheck } from './hooks/useEyeCheck';
import { useAppNavigation } from './hooks/useAppNavigation';
import { useExternalEditSession } from './hooks/useExternalEditSession';
import ExternalEditBar from './components/ui/ExternalEditBar';
import { Status } from './components/ui/ExportImportProperties';

import { useEditorActions } from './hooks/useEditorActions';
import { useLibraryActions } from './hooks/useLibraryActions';
import { useProductivityActions } from './hooks/useProductivityActions';
import { useVirtualCopyCommand } from './hooks/useVirtualCopyCommand';

import { useAppInitialization } from './hooks/useAppInitialization';
import './i18n';

import {
  Album,
  AlbumItem,
  ImageFile,
  LibraryViewMode,
  Panel,
  type RelinkReport,
  ThumbnailSize,
  ThumbnailAspectRatio,
} from './components/ui/AppProperties';
import type { Adjustments } from './utils/adjustments';
import type { globalImageCache } from './utils/ImageLRUCache';

import ImageProcessingManager from './components/managers/ImageProcessingManager';
import ImageLoaderManager from './components/managers/ImageLoaderManager';
// Imperative zoom/pan handle that Editor exposes on transformWrapperRef via useImperativeHandle.
import type { TransformWrapperHandle } from './components/panel/Editor';
import { viewerShown } from './utils/viewerMode';
import { offerInterruptedBatches } from './utils/interruptedBatches';

type ImageCacheEntry = Parameters<typeof globalImageCache.set>[1];

function App() {
  usePerformanceDiagnostics();
  // Profiling builds: style and layout of the first mount.
  useLayoutEffect(() => reportForcedLayout('mount_layout'), []);

  const { appSettings, osPlatform, handleSettingsChange } = useSettingsStore(
    useShallow((state) => ({
      appSettings: state.appSettings,
      osPlatform: state.osPlatform,
      handleSettingsChange: state.handleSettingsChange,
    })),
  );

  const {
    activeView,
    isFullScreen,
    isWindowFullScreen,
    isInstantTransition,
    activePanel,
    setUI,
  } = useUIStore(
    useShallow((state) => ({
      activeView: state.activeView,
      isFullScreen: state.isFullScreen,
      isWindowFullScreen: state.isWindowFullScreen,
      isInstantTransition: state.isInstantTransition,
      activePanel: state.activePanel,
      setUI: state.setUI,
    })),
  );
  // Develop, the Library's loupe or the full screen: the native viewer shows.
  const viewerOn = useUIStore(viewerShown);
  // Shift+Tab also hides the top bar (Lightroom); the workspace shows a strip instead.
  const barsHidden = useUIStore(
    (state) => !readModuleLayout(state.uiVisibility.modules, moduleOf(state.activeView)).bars,
  );

  const { rootPaths } = useLibraryStore(
    useShallow((state) => ({
      rootPaths: state.rootPaths,
    })),
  );

  const {
    selectedImage,
    activeMaskContainerId,
    hasRenderedFirstFrame,
    holdingNativeFrame,
    setEditor,
  } =
    useEditorStore(
      useShallow((state) => ({
        selectedImage: state.selectedImage,
        activeMaskContainerId: state.activeMaskContainerId,
        hasRenderedFirstFrame: state.hasRenderedFirstFrame,
        holdingNativeFrame: state.holdingNativeFrame,
        setEditor: state.setEditor,
      })),
    );

  const exportState = useProcessStore((state) => state.exportState);

  const defaultThumbnailSize = ThumbnailSize.Medium;
  const defaultLibraryViewMode = LibraryViewMode.Flat;

  const selectedImagePathRef = useRef<string | null>(null);
  useEffect(() => {
    selectedImagePathRef.current = selectedImage?.path ?? null;
  }, [selectedImage?.path]);

  const prevAdjustmentsRef = useRef<{ path: string; adjustments: Adjustments } | null>(null);

  const isBackendReadyRef = useRef(true);
  const previewJobIdRef = useRef<number>(0);
  const latestRenderedJobIdRef = useRef<number>(0);
  const currentResRef = useRef<number>(1280);
  const cachedEditStateRef = useRef<ImageCacheEntry | null>(null);

  const [libraryViewMode, setLibraryViewMode] = useState<LibraryViewMode>(defaultLibraryViewMode);
  const [thumbnailSize, setThumbnailSize] = useState(defaultThumbnailSize);
  const [thumbnailAspectRatio, setThumbnailAspectRatio] = useState(ThumbnailAspectRatio.Contain);

  const { requestThumbnails, clearThumbnailQueue, markGenerated } = useThumbnails();

  const transformWrapperRef = useRef<TransformWrapperHandle | null>(null);
  const preloadedDataRef = useRef<{
    trees?: Promise<FolderTreeNode[]>;
    images?: Promise<ImageFile[]>;
    rootPaths?: string[];
    currentPath?: string;
  }>({});

  useAppInitialization({
    preloadedDataRef,
    thumbnailSize,
    setThumbnailSize,
    thumbnailAspectRatio,
    setThumbnailAspectRatio,
    libraryViewMode,
    setLibraryViewMode,
  });

  const { handleCopyAdjustments, handlePasteAdjustments, handleResetAdjustments, handleZoomChange } =
    useEditorActions();

  const navigationRefs = {
    transformWrapperRef,
    cachedEditStateRef,
    selectedImagePathRef,
    isBackendReadyRef,
    latestRenderedJobIdRef,
    previewJobIdRef,
    currentResRef,
    prevAdjustmentsRef,
  };

  const {
    handleBackToLibrary,
    handleImageSelect,
    handleSelectSubfolder,
    handleSelectAlbum,
    handleOpenFolder,
  } = useAppNavigation({
    clearThumbnailQueue,
    refs: navigationRefs,
  });

  const {
    externalEditSession,
    isFinishing: isExternalEditFinishing,
    finishExternalEdit,
  } = useExternalEditSession(handleImageSelect);

  const {
    handleRate,
    handleLibraryImageSingleClick,
    handleImageClick,
    handleSetColorLabel,
    refreshAllFolderTrees,
    handleRemoveRootFolder,
  } = useLibraryActions(handleImageSelect);

  useCatalogSearch();
  useEyeCheck();
  useStackCommands();
  const { displayList: sortedImageList, flagCounts, stackMarks } = useSortedLibrary();

  const handleLibraryRefresh = useCallback(async () => {
    // Renames, moves, trash and undo change paths; picks are keyed by them.
    const flags = reloadSessionFlags();
    // Read now: a folder rename (or its undo) moves the shown folder just before.
    const { currentFolderPath } = useLibraryStore.getState();
    if (currentFolderPath) {
      if (currentFolderPath.startsWith('Album: ')) {
        const { activeAlbumId, albumTree, sessionDirectory } = useLibraryStore.getState();
        if (activeAlbumId === PREVIOUS_IMPORT_VIEW_ID && sessionDirectory) {
          await openPreviousImport(
            sessionDirectory,
            currentFolderPath.slice('Album: '.length),
            handleSelectAlbum,
            true,
          ).catch((e) => notify.error(String(e)));
        } else if (activeAlbumId === MISSING_VIEW_ID && sessionDirectory) {
          await openMissingView(
            sessionDirectory,
            currentFolderPath.slice('Album: '.length),
            handleSelectAlbum,
            true,
          ).catch((e) => notify.error(String(e)));
        } else if (activeAlbumId === PENDING_XMP_VIEW_ID && sessionDirectory) {
          await openPendingXmpView(
            sessionDirectory,
            currentFolderPath.slice('Album: '.length),
            handleSelectAlbum,
            true,
          ).catch((e) => notify.error(String(e)));
        } else if (activeAlbumId === ACTIVITY_PHOTOS_VIEW_ID) {
          // An activity entry's photographs, at their current places.
          const shown = shownActivityPhotos();
          if (shown) await handleSelectAlbum(ACTIVITY_PHOTOS_VIEW_ID, shown.name, shown.paths, true);
        } else if (activeAlbumId?.startsWith(SESSION_ALBUM_PREFIX) && sessionDirectory) {
          await openSessionAlbum(
            sessionDirectory,
            { id: activeAlbumId.slice(SESSION_ALBUM_PREFIX.length), name: currentFolderPath.slice('Album: '.length) },
            handleSelectAlbum,
            true,
          ).catch((e) => notify.error(`The album could not be refreshed: ${e}`));
        } else if (activeAlbumId) {
          const findObj = (nodes: AlbumItem[]): Album | null => {
            for (const n of nodes) {
              // activeAlbumId is only ever set to an album's id, never a group's.
              if (n.id === activeAlbumId) return n as Album;
              if (n.type === 'group') {
                const f = findObj(n.children);
                if (f) return f;
              }
            }
            return null;
          };
          const album = findObj(albumTree);
          if (album) await handleSelectAlbum(album.id, album.name, album.images, true);
        }
      } else {
        await handleSelectSubfolder(currentFolderPath, false, undefined, false, true);
      }
    }
    await flags;
  }, [handleSelectSubfolder, handleSelectAlbum]);

  // A drive connected, ejected or mounted elsewhere: the shown photographs
  // are listed again, so their Offline badges follow at once.
  const refreshRef = useRef(handleLibraryRefresh);
  refreshRef.current = handleLibraryRefresh;
  useEffect(() => {
    const changed = () => void refreshRef.current().catch(() => {});
    window.addEventListener(DRIVES_CHANGED, changed);
    return () => window.removeEventListener(DRIVES_CHANGED, changed);
  }, []);

  // Batches a quit or crash cut short, offered as the app starts and each
  // library opens (G66, src/utils/interruptedBatches.ts).
  useEffect(() => {
    let last: string | null | undefined;
    const offer = (directory: string | null) => {
      if (directory === last) return;
      last = directory;
      void offerInterruptedBatches(directory, () => refreshRef.current());
    };
    offer(useLibraryStore.getState().sessionDirectory);
    return useLibraryStore.subscribe((state) => offer(state.sessionDirectory));
  }, []);

  // Use XMP or its undo changed ratings, labels, flags, keywords and text
  // in the library: the shown photographs and their text fields follow.
  useEffect(() => {
    const read = (event: Event) => {
      const directory = (event as CustomEvent<string>).detail;
      if (useLibraryStore.getState().sessionDirectory !== directory) return;
      void refreshRef.current().catch(() => {});
      const text = useTextMetadataCache.getState();
      if (text.session === directory) void text.load(directory);
    };
    window.addEventListener(XMP_METADATA_READ, read);
    return () => window.removeEventListener(XMP_METADATA_READ, read);
  }, []);

  // The session bar's "Show photos" for XMP files that are not up to date.
  useEffect(() => {
    const show = (event: Event) => {
      const { sessionDirectory } = useLibraryStore.getState();
      if (!sessionDirectory) return;
      // A refresh after Try again keeps an open photograph in Develop.
      const refresh = Boolean((event as CustomEvent<{ refresh?: boolean } | null>).detail?.refresh);
      void openPendingXmpView(sessionDirectory, i18n.t('session.xmp.view'), handleSelectAlbum, refresh).catch((e) =>
        notify.error(String(e)),
      );
    };
    window.addEventListener(SHOW_PENDING_XMP_EVENT, show);
    return () => window.removeEventListener(SHOW_PENDING_XMP_EVENT, show);
  }, [handleSelectAlbum]);

  // The activity centre follows the export queue, AI tasks, downloads and
  // the photo write queue.
  useEffect(() => startActivityBridges(), []);

  // An activity entry's "Show photos": the photographs it is about.
  useEffect(() => {
    const show = (event: Event) => {
      const { name, paths } = (event as CustomEvent<ShowActivityPhotos>).detail;
      void handleSelectAlbum(ACTIVITY_PHOTOS_VIEW_ID, name, paths).catch((e) => notify.error(String(e)));
    };
    window.addEventListener(SHOW_ACTIVITY_PHOTOS_EVENT, show);
    return () => window.removeEventListener(SHOW_ACTIVITY_PHOTOS_EVENT, show);
  }, [handleSelectAlbum]);

  /** After Locate Photo / Locate Folder: the folders show the new places, the
   * selection (and a photograph open in Develop) follows the relinked files,
   * and a folder view the photographs left shows the folder they are in now. */
  const handleRelinked = useCallback(
    async (report: RelinkReport) => {
      await refreshAllFolderTrees();
      const inEditor = useUIStore.getState().activeView === 'editor';
      const editing = useEditorStore.getState().selectedImage?.path ?? null;
      const reopen = editing ? relinkedPaths(report, [editing])[0] : null;
      const before = useLibraryStore.getState();
      const selection = relinkedPaths(report, before.multiSelectedPaths);
      const active = before.libraryActivePath ? relinkedPaths(report, [before.libraryActivePath])[0] : null;
      await handleLibraryRefresh();
      const { currentFolderPath: folder, imageList } = useLibraryStore.getState();
      const located = locatedPhotograph(report, [reopen, active]);
      if (located && folder && !imageList.some((image) => image.path === located)) {
        // The view lost them (their old folder, Missing Photographs): show
        // the folder they are in now. Develop stays open; its filmstrip
        // shows that folder.
        await handleSelectSubfolder(folderOf(located), false, undefined, true, inEditor);
      }
      if (reopen && reopen !== editing) {
        // The photograph now opens from its file instead of its preview.
        await handleImageSelect(reopen, inEditor);
      }
      const shown = useLibraryStore.getState().imageList;
      const keep = selection.filter((path) => shown.some((image) => image.path === path));
      useLibraryStore.getState().setLibrary({
        multiSelectedPaths: keep,
        libraryActivePath: active && keep.includes(active) ? active : (keep[0] ?? null),
      });
    },
    [refreshAllFolderTrees, handleLibraryRefresh, handleSelectSubfolder, handleImageSelect],
  );

  // Synchronize Folder applied: moved photographs are followed as after a
  // relink; new and removed ones show once the folders and the view reload.
  const relinkedRef = useRef(handleRelinked);
  relinkedRef.current = handleRelinked;
  useEffect(() => {
    const applied = (e: Event) => {
      const { moved } = (e as CustomEvent<SyncApplied>).detail;
      void relinkedRef.current({ relinked: moved, mismatched: [], taken: [], not_found: 0 }).catch(() => {});
    };
    window.addEventListener(FOLDER_SYNC_APPLIED, applied);
    return () => window.removeEventListener(FOLDER_SYNC_APPLIED, applied);
  }, []);

  const {
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
  } = useFileOperations(
    handleLibraryRefresh,
    refreshAllFolderTrees,
    handleImageSelect,
    handleBackToLibrary,
    sortedImageList,
  );

  const {
    handleRunMerge,
    handleRefinishMerge,
    handleSaveMerge,
    handleSaveCollage,
  } = useProductivityActions(handleLibraryRefresh);
  useVirtualCopyCommand(handleLibraryRefresh);

  const {
    handleEditorContextMenu,
    handleThumbnailContextMenu,
  } = useAppContextMenus({
    handleImageSelect,
    handleBackToLibrary,
    handleLibraryRefresh,
    handleRenameFiles,
    handleMoveToFolder,
    refreshImageList: handleLibraryRefresh,
    executeDelete,
    handleShowMissing: () => {
      const { sessionDirectory } = useLibraryStore.getState();
      if (!sessionDirectory) return;
      void openMissingView(sessionDirectory, i18n.t('offline.missingView'), handleSelectAlbum).catch((e) =>
        notify.error(String(e)),
      );
    },
    handleRelinked,
  });

  useTauriListeners({
    refreshAllFolderTrees,
    handleSelectSubfolder,
    refreshImageList: handleLibraryRefresh,
    markGenerated,
    handleImageSelect,
  });


  useAppLevelCommands();
  useAppMenu();
  useKeyboardShortcuts({
    sortedImageList,
    handleBackToLibrary,
    handleDeleteSelected,
    handleUndoFileOperation,
    handleRedoFileOperation,
    handleMoveToSelects,
    handleImageSelect,
    handlePasteFiles,
    handleZoomChange,
  });

  useEffect(() => {
    const handleGlobalContextMenu = (event: MouseEvent) => {
      event.preventDefault();
    };
    window.addEventListener('contextmenu', handleGlobalContextMenu);
    return () => window.removeEventListener('contextmenu', handleGlobalContextMenu);
  }, []);

  useEffect(() => {
    if (activePanel !== Panel.Masks || !activeMaskContainerId) {
      setEditor({ isMaskControlHovered: false });
    }
  }, [activePanel, activeMaskContainerId, setEditor]);

  useEffect(() => {
    // Switching panels ends the previous panel's tool. A tool chosen together
    // with its panel (the viewer's tool bar picks a mask component and opens
    // Layers, or the white balance picker and opens Adjustments) stays: leaving
    // a panel already cleared it, so a set value here is that new choice.
    const editing = activeView === 'editor';
    const current = useEditorStore.getState();
    setEditor({
      isWbPickerActive: editing && activePanel === Panel.Adjustments && current.isWbPickerActive,
      isStraightenActive: false,
      isGuidedPerspectiveActive: false,
      activeMaskId: editing && activePanel === Panel.Masks ? current.activeMaskId : null,
    });
  }, [activePanel, activeView, setEditor]);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    let known: boolean | null = null;
    const checkFullscreen = async () => {
      const isWindowFullScreen = await appWindow.isFullscreen();
      // Resizing sends many events (a zoom animation one per frame): only a
      // change of full screen updates the store.
      if (known === isWindowFullScreen) return;
      setUI({ isWindowFullScreen });
      // Entering or leaving full screen by any means (F, the green button,
      // ⌃⌘F) leaves the web view without keyboard focus on macOS.
      if (known !== null) void refocusWebview();
      known = isWindowFullScreen;
    };
    checkFullscreen();
    const unlistenPromise = appWindow.onResized(checkFullscreen);
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [setUI]);

  // Transparent over the native display once it shows the photograph: a
  // preview proxy frame before the full decode, or the previous frame held
  // while the next photograph opens.
  const isWgpuActive =
    viewerOn &&
    appSettings?.useWgpuRenderer !== false &&
    // (hasRenderedFirstFrame is only set by a frame of the selected photo.)
    (hasRenderedFirstFrame || holdingNativeFrame);
  const hasNativeMacTitlebar = osPlatform === 'macos';
  const hasUnifiedTitlebar = hasNativeMacTitlebar || osPlatform === 'linux';
  const showLinuxWindowControls = osPlatform === 'linux' && !appSettings?.decorations && !isWindowFullScreen;

  // Profiling builds: when App's own mount effects (the last to run) are done.
  useEffect(() => reportPerformance('mount_effects_end', { epoch_ms: Math.round(epochOf(performance.now())) }), []);

  return (
    <>
      <CommandPalette />
      <ContributionHost />
      <ImageProcessingManager
        transformWrapperRef={transformWrapperRef}
        prevAdjustmentsRef={prevAdjustmentsRef}
        previewJobIdRef={previewJobIdRef}
        latestRenderedJobIdRef={latestRenderedJobIdRef}
        currentResRef={currentResRef}
      />
      <ImageLoaderManager cachedEditStateRef={cachedEditStateRef} />
      <div
        className={clsx(
          'studio-app flex flex-col h-screen font-sans text-text-primary overflow-hidden select-none',
          hasNativeMacTitlebar && 'macos-native-chrome',
          osPlatform === 'linux' && 'linux-unified-chrome',
          hasNativeMacTitlebar && !isWindowFullScreen && 'macos-windowed',
          barsHidden && 'bars-hidden',
          isFullScreen && 'is-image-fullscreen',
          isWgpuActive ? 'bg-transparent' : 'bg-bg-primary',
        )}
      >
        {!hasUnifiedTitlebar && (
          <div
            className={clsx(
              'shrink-0 overflow-hidden z-50',
              !isInstantTransition && 'transition-all duration-300 ease-in-out',
              isFullScreen ? 'max-h-0 opacity-0 pointer-events-none' : 'max-h-15 opacity-100',
            )}
          >
            {appSettings?.decorations || (!isWindowFullScreen && <TitleBar />)}
          </div>
        )}
        {/* Hidden, not unmounted, in the full screen (studio.css): the
            session keeps its tethering, ingest and dialogs running. */}
        <PerfProfiler id="session">
          <SessionWorkspace
            onNavigate={handleSelectSubfolder}
            nativeTitlebar={hasNativeMacTitlebar || showLinuxWindowControls}
            windowControls={showLinuxWindowControls ? <WindowControls /> : undefined}
          />
        </PerfProfiler>
        <PerfProfiler id="workspace">
        <StudioWorkspace
          files={sortedImageList}
          flagCounts={flagCounts}
          stackMarks={stackMarks}
          transformWrapperRef={transformWrapperRef}
          onSelect={handleLibraryImageSingleClick}
          onFilmSelect={handleImageClick}
          onOpen={handleImageSelect}
          onShow={(path) => handleImageSelect(path, false)}
          onNavigate={handleSelectSubfolder}
          onSelectAlbum={(id, name, paths, preserveEditor) => handleSelectAlbum(id, name, paths, preserveEditor)}
          onBack={handleBackToLibrary}
          onRate={handleRate}
          onContextMenu={handleThumbnailContextMenu}
          onEditorContextMenu={handleEditorContextMenu}
          onCopy={handleCopyAdjustments}
          onPaste={handlePasteAdjustments}
          onReset={handleResetAdjustments}
          onZoom={handleZoomChange}
          requestThumbnails={requestThumbnails}
          externalEditBar={selectedImage && externalEditSession && <ExternalEditBar session={externalEditSession} isFinishing={isExternalEditFinishing} errorMessage={exportState.status === Status.Error ? exportState.errorMessage : ''} onDone={finishExternalEdit} />}
          onSettings={() => setUI({ isSettingsOpen: true })}
          folderActions={{
            moveHere: handleMoveToFolder,
            moveFolder: (path) => void handleMoveFolder(path),
            trash: handleTrashFolder,
            emptyTrash: () => void handleEmptySessionTrash(),
            addFolder: handleOpenFolder,
            removeFromLibrary: (path) => handleRemoveRootFolder(path, handleBackToLibrary),
            paste: (mode, path) => void handlePasteFiles(mode, path),
            locateFolder: (folder) => {
              const { sessionDirectory } = useLibraryStore.getState();
              if (!sessionDirectory) return;
              locateFolderOf(i18n.t, sessionDirectory, folder)
                .then((report) => (report ? handleRelinked(report) : undefined))
                .catch((err) => notify.error(i18n.t('offline.toasts.failed', { err: String(err) }), { autoClose: false }));
            },
          }}
          settings={
            appSettings && (
              <SettingsPanel
                appSettings={appSettings}
                onBack={() => setUI({ isSettingsOpen: false })}
                onLibraryRefresh={handleLibraryRefresh}
                onSettingsChange={handleSettingsChange}
                rootPaths={rootPaths}
              />
            )
          }
        />
        </PerfProfiler>
        <PerfProfiler id="modals">
        <AppModals
          handleImageSelect={handleImageSelect}
          handleRunMerge={handleRunMerge}
          handleSaveMerge={handleSaveMerge}
          handleRefinishMerge={handleRefinishMerge}
          refreshImageList={handleLibraryRefresh}
          handleCreateFolder={handleCreateFolder}
          handleRenameFolder={handleRenameFolder}
          handleSaveRename={handleSaveRename}
          handleSetColorLabel={handleSetColorLabel}
          handleRate={handleRate}
          executeDelete={executeDelete}
          handleSaveCollage={handleSaveCollage}
        />
        </PerfProfiler>
        <AppNotices>
          <UpdateNotice />
        </AppNotices>
        <WhatsNewAfterUpdate />
      </div>
    </>
  );
}

const AppWrapper = () => (
  <PerfProfiler id="app">
    <ContextMenuProvider>
      <App />
      <GlobalTooltip />
    </ContextMenuProvider>
  </PerfProfiler>
);

export default AppWrapper;
