import { useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useShallow } from 'zustand/react/shallow';
import { interfaceLanguage } from '../i18n/systemLanguage';
import { useSettingsStore } from '../store/useSettingsStore';
import { useUIStore, reconcileWorkspace } from '../store/useUIStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useEditorStore } from '../store/useEditorStore';
import { useProcessStore, ExternalEditSession } from '../store/useProcessStore';
import { COPYABLE_ADJUSTMENT_KEYS, CopyPasteSettings, withAdjustmentLayout } from '../utils/adjustments';
import {
  AppSettings,
  FilterCriteria,
  ImageFile,
  Invokes,
  LibraryViewMode,
  RawStatus,
  EditedStatus,
  ThumbnailSize,
  ThumbnailAspectRatio,
  SupportedTypes
} from '../components/ui/AppProperties';
import type { FolderTree } from '../utils/folderTree';
import { savedViewHeld } from '../utils/restoreView';
import { useTranslation } from 'react-i18next';

type LibraryState = ReturnType<typeof useLibraryStore.getState>;

/** Library data prepared at launch for Continue Session. */
export interface PreloadedLibraryData {
  trees?: Promise<FolderTree[]>;
  images?: Promise<ImageFile[]>;
  rootPaths?: string[];
  currentPath?: string;
}

/** Result of `frontend_ready` (`LaunchPayload` in launch_request.rs). */
interface LaunchPayload {
  openWithFile: string | null;
  editSession: ExternalEditSession | null;
}

interface UseAppInitializationProps {
  preloadedDataRef: React.RefObject<PreloadedLibraryData>;
  thumbnailSize: ThumbnailSize;
  setThumbnailSize: (size: ThumbnailSize) => void;
  thumbnailAspectRatio: ThumbnailAspectRatio;
  setThumbnailAspectRatio: (ratio: ThumbnailAspectRatio) => void;
  libraryViewMode: LibraryViewMode;
  setLibraryViewMode: (mode: LibraryViewMode) => void;
}

export const useAppInitialization = ({
  preloadedDataRef,
  thumbnailSize,
  setThumbnailSize,
  thumbnailAspectRatio,
  setThumbnailAspectRatio,
  libraryViewMode,
  setLibraryViewMode
}: UseAppInitializationProps) => {
  const isInitialMount = useRef(true);
  const { i18n } = useTranslation();

  const { appSettings, setAppSettings, setSupportedTypes, initPlatform, handleSettingsChange } = useSettingsStore(
    useShallow((state) => ({
      appSettings: state.appSettings,
      setAppSettings: state.setAppSettings,
      setSupportedTypes: state.setSupportedTypes,
      initPlatform: state.initPlatform,
      handleSettingsChange: state.handleSettingsChange
    }))
  );

  const { uiVisibility, collapsibleSectionsState, setUI } = useUIStore(
    useShallow((state) => ({
      uiVisibility: state.uiVisibility,
      collapsibleSectionsState: state.collapsibleSectionsState,
      setUI: state.setUI
    }))
  );

  const workspaceProps = useUIStore(
    useShallow((state) => ({
      leftPanelWidth: state.leftPanelWidth,
      rightPanelWidth: state.rightPanelWidth,
      leftTopHeight: state.leftTopHeight,
      rightTopHeight: state.rightTopHeight,
      panelLayout: state.panelLayout,
      activePanels: state.activePanels,
      panelSwitcherPlacement: state.panelSwitcherPlacement
    }))
  );

  const {
    sortCriteria,
    filterCriteria,
    currentFolderPath,
    expandedFolders,
    activeAlbumId,
    expandedAlbumGroups,
    sessionDirectory,
    setSortCriteria,
    setFilterCriteria,
    setLibrary
  } = useLibraryStore(
    useShallow((state) => ({
      sortCriteria: state.sortCriteria,
      filterCriteria: state.filterCriteria,
      currentFolderPath: state.currentFolderPath,
      expandedFolders: state.expandedFolders,
      activeAlbumId: state.activeAlbumId,
      expandedAlbumGroups: state.expandedAlbumGroups,
      sessionDirectory: state.sessionDirectory,
      setSortCriteria: state.setSortCriteria,
      setFilterCriteria: state.setFilterCriteria,
      setLibrary: state.setLibrary
    }))
  );

  const { setEditor } = useEditorStore(
    useShallow((state) => ({
      setEditor: state.setEditor
    }))
  );

  const defaultThumbnailSize = ThumbnailSize.Medium;
  const defaultLibraryViewMode = LibraryViewMode.Flat;
  const prevImageCountsNeed = useRef<boolean | undefined>(undefined);

  useEffect(() => {
    initPlatform();
  }, [initPlatform]);

  useEffect(() => {
    invoke<SupportedTypes>(Invokes.GetSupportedFileTypes)
      .then((types) => setSupportedTypes(types))
      .catch((err) => console.error('Failed to load supported file types:', err));
  }, [setSupportedTypes]);

  useEffect(() => {
    invoke<AppSettings>(Invokes.LoadSettings)
      .then(async (settings: AppSettings) => {
        if (
          !settings.copyPasteSettings ||
          !settings.copyPasteSettings.includedAdjustments ||
          settings.copyPasteSettings.includedAdjustments.length === 0
        ) {
          // knownAdjustments and autoSync are left to the backend's serde defaults.
          settings.copyPasteSettings = {
            mode: 'merge',
            includedAdjustments: COPYABLE_ADJUSTMENT_KEYS
          } as CopyPasteSettings;
        }

        const savedRawStatus = settings?.filterCriteria?.rawStatus as string | undefined;
        if (savedRawStatus === 'groupVariants' || savedRawStatus === 'rawOverNonRaw') {
          const legacyPref = settings?.groupPreferredType === 'jpeg' ? 'jpeg' : 'raw';
          settings.grouping = legacyPref;
          settings.filterCriteria = { ...settings.filterCriteria!, rawStatus: RawStatus.All };
          handleSettingsChange(settings);
        }

        const reconciledWorkspace = reconcileWorkspace(settings?.workspace);
        settings.workspace = reconciledWorkspace;

        setAppSettings(settings);
        // No language chosen in Preferences: the system's (UXR4L-03). It is
        // not saved, so the interface follows a later change of the system's.
        void interfaceLanguage(settings.language).then((language) => i18n.changeLanguage(language));

        if (settings?.sortCriteria) setSortCriteria(settings.sortCriteria);

        if (settings?.filterCriteria) {
          setFilterCriteria((prev: FilterCriteria) => ({
            ...prev,
            ...settings.filterCriteria,
            rawStatus: settings.filterCriteria!.rawStatus || RawStatus.All,
            editedStatus: settings.filterCriteria!.editedStatus || EditedStatus.All,
            colors: settings.filterCriteria!.colors || []
          }));
        }

        if (settings?.uiVisibility) {
          setUI((state) => ({ uiVisibility: { ...state.uiVisibility, ...settings.uiVisibility } }));
        }

        if (settings?.adjustmentLayout?.openSections) {
          setUI((state) => ({
            collapsibleSectionsState: { ...state.collapsibleSectionsState, ...settings.adjustmentLayout!.openSections }
          }));
        }

        setUI({
          leftPanelWidth: reconciledWorkspace.leftPanelWidth,
          rightPanelWidth: reconciledWorkspace.rightPanelWidth,
          leftTopHeight: reconciledWorkspace.leftTopHeight,
          rightTopHeight: reconciledWorkspace.rightTopHeight,
          panelLayout: reconciledWorkspace.panelLayout,
          activePanels: reconciledWorkspace.activePanels,
          panelSwitcherPlacement: reconciledWorkspace.panelSwitcherPlacement
        });

        if (settings?.isWaveformVisible !== undefined) setEditor({ isWaveformVisible: settings.isWaveformVisible });
        if (settings?.activeWaveformChannel) setEditor({ activeWaveformChannel: settings.activeWaveformChannel });
        if (typeof settings?.waveformHeight === 'number') setEditor({ waveformHeight: settings.waveformHeight });

        setLibraryViewMode(settings?.libraryViewMode ?? defaultLibraryViewMode);
        setThumbnailSize(settings?.thumbnailSize ?? defaultThumbnailSize);
        if (settings?.thumbnailAspectRatio) setThumbnailAspectRatio(settings.thumbnailAspectRatio);

        if (settings?.pinnedFolders && settings.pinnedFolders.length > 0) {
          try {
            const trees = await invoke<FolderTree[]>(Invokes.GetPinnedFolderTrees, {
              paths: settings.pinnedFolders,
              expandedFolders: settings.lastFolderState?.expandedFolders || [],
              showImageCounts: settings.enableFolderImageCounts || settings.folderTreeSort?.key === 'imageCount'
            });
            setLibrary({ pinnedFolderTrees: trees });
          } catch (err) {
            console.error('Failed to load pinned folder trees:', err);
          }
        }

        const rootFolders = settings.rootFolders?.length
          ? settings.rootFolders
          : settings.lastRootPath
            ? [settings.lastRootPath]
            : [];

        if (rootFolders.length > 0) {
          const currentPath = settings.lastFolderState?.currentFolderPath || rootFolders[0];
          preloadedDataRef.current = {
            rootPaths: rootFolders,
            currentPath: currentPath,
            trees: undefined,
            // SessionWorkspace restores the active session independently. An
            // eager full image scan here was usually unused and could stall the
            // native UI before that restore even started. Continue Session loads
            // its chosen folder through the same bounded listing as navigation.
            images: undefined
          };
        }

        if (settings?.lastFolderState) {
          setLibrary({
            expandedFolders: new Set(settings.lastFolderState.expandedFolders || []),
            expandedAlbumGroups: new Set(settings.lastFolderState.expandedAlbumGroups || [])
          });
        }

        invoke<LaunchPayload>(Invokes.FrontendReady)
          .then((launch) => {
            if (launch?.editSession) {
              useProcessStore.getState().setProcess({ externalEditSession: launch.editSession });
            } else if (launch?.openWithFile) {
              useProcessStore.getState().setProcess({ initialFileToOpen: launch.openWithFile });
            }
          })
          .catch((e) => console.error('Failed to notify backend of readiness:', e));
      })
      .catch((err) => {
        console.error('Failed to load settings:', err);
        setAppSettings({
          lastRootPath: null,
          thumbnailSize: defaultThumbnailSize,
          libraryViewMode: defaultLibraryViewMode
        });
      })
      .finally(() => {
        isInitialMount.current = false;
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- one-shot settings load: react-i18next 17 returns a new `i18n` wrapper after every language change (including the saved language applied here), which would rerun the load and frontend_ready; its methods are bound to the shared instance
  }, [
    setAppSettings,
    setUI,
    defaultLibraryViewMode,
    defaultThumbnailSize,
    setSortCriteria,
    setFilterCriteria,
    setEditor,
    setLibrary,
    preloadedDataRef,
    setLibraryViewMode,
    setThumbnailSize,
    setThumbnailAspectRatio,
    handleSettingsChange
  ]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;

    const currentWorkspaceStr = JSON.stringify(appSettings.workspace || {});
    const newWorkspaceStr = JSON.stringify(workspaceProps);

    if (currentWorkspaceStr !== newWorkspaceStr) {
      const timeoutId = setTimeout(() => {
        handleSettingsChange({ ...appSettings, workspace: workspaceProps });
      }, 500);

      return () => clearTimeout(timeoutId);
    }
  }, [workspaceProps, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (JSON.stringify(appSettings.uiVisibility) !== JSON.stringify(uiVisibility)) {
      handleSettingsChange({ ...appSettings, uiVisibility });
    }
  }, [uiVisibility, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (JSON.stringify(appSettings.adjustmentLayout?.openSections) !== JSON.stringify(collapsibleSectionsState)) {
      handleSettingsChange(withAdjustmentLayout(appSettings, { openSections: collapsibleSectionsState }));
    }
  }, [collapsibleSectionsState, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (appSettings.thumbnailSize !== thumbnailSize) {
      handleSettingsChange({ ...appSettings, thumbnailSize });
    }
  }, [thumbnailSize, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (appSettings.thumbnailAspectRatio !== thumbnailAspectRatio) {
      handleSettingsChange({ ...appSettings, thumbnailAspectRatio });
    }
  }, [thumbnailAspectRatio, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (appSettings.libraryViewMode !== libraryViewMode) {
      handleSettingsChange({ ...appSettings, libraryViewMode });
    }
  }, [libraryViewMode, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (JSON.stringify(appSettings.sortCriteria) !== JSON.stringify(sortCriteria)) {
      handleSettingsChange({ ...appSettings, sortCriteria });
    }
  }, [sortCriteria, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (JSON.stringify(appSettings.filterCriteria) !== JSON.stringify(filterCriteria)) {
      handleSettingsChange({ ...appSettings, filterCriteria });
    }
  }, [filterCriteria, appSettings, handleSettingsChange]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    let current = true;
    void interfaceLanguage(appSettings.language).then((language) => {
      if (current && language !== i18n.language) void i18n.changeLanguage(language);
    });
    return () => {
      current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed on the two languages it compares; `appSettings` changes on every settings save, and the react-i18next `i18n` wrapper only changes identity together with i18n.language
  }, [appSettings?.language, i18n.language]);

  useEffect(() => {
    if (isInitialMount.current || !appSettings) return;
    if (!currentFolderPath && !activeAlbumId) return;
    // A stand-in view (the saved folder is not on disk) is not the user's selection.
    if (savedViewHeld(sessionDirectory, currentFolderPath, activeAlbumId)) return;

    const currentExpanded = Array.from(expandedFolders);
    const currentExpandedAlbums = Array.from(expandedAlbumGroups);

    const prevFolderState = appSettings.lastFolderState || {
      currentFolderPath: null,
      expandedFolders: [],
      activeAlbumId: null,
      expandedAlbumGroups: [],
      sessionDirectory: null
    };

    const pathChanged = prevFolderState.currentFolderPath !== currentFolderPath;
    const expandedChanged = JSON.stringify(prevFolderState.expandedFolders || []) !== JSON.stringify(currentExpanded);
    const albumChanged = prevFolderState.activeAlbumId !== activeAlbumId;
    const albumExpandedChanged =
      JSON.stringify(prevFolderState.expandedAlbumGroups || []) !== JSON.stringify(currentExpandedAlbums);

    const libraryChanged = (prevFolderState.sessionDirectory ?? null) !== (sessionDirectory ?? null);

    if (pathChanged || expandedChanged || albumChanged || albumExpandedChanged || libraryChanged) {
      handleSettingsChange({
        ...appSettings,
        lastFolderState: {
          currentFolderPath,
          expandedFolders: currentExpanded,
          activeAlbumId,
          expandedAlbumGroups: currentExpandedAlbums,
          sessionDirectory
        }
      });
    }
  }, [
    currentFolderPath,
    expandedFolders,
    activeAlbumId,
    expandedAlbumGroups,
    sessionDirectory,
    appSettings,
    handleSettingsChange
  ]);

  useEffect(() => {
    if (!appSettings) return;

    const needsImageCounts = Boolean(
      appSettings.enableFolderImageCounts || appSettings.folderTreeSort?.key === 'imageCount'
    );

    if (prevImageCountsNeed.current === undefined) {
      prevImageCountsNeed.current = needsImageCounts;
      return;
    }

    if (prevImageCountsNeed.current !== needsImageCounts) {
      prevImageCountsNeed.current = needsImageCounts;

      const rootFolders = appSettings.rootFolders?.length
        ? appSettings.rootFolders
        : appSettings.lastRootPath
          ? [appSettings.lastRootPath]
          : [];
      const pinnedFolders = appSettings.pinnedFolders || [];

      const currentExpanded = Array.from(useLibraryStore.getState().expandedFolders);

      setLibrary({ isTreeLoading: true });

      const promises = [];

      if (pinnedFolders.length > 0) {
        promises.push(
          invoke<FolderTree[]>(Invokes.GetPinnedFolderTrees, {
            paths: pinnedFolders,
            expandedFolders: currentExpanded,
            showImageCounts: needsImageCounts
          }).then((trees) => ({ type: 'pinned', trees }))
        );
      }

      if (rootFolders.length > 0) {
        promises.push(
          invoke<FolderTree[]>(Invokes.GetPinnedFolderTrees, {
            paths: rootFolders,
            expandedFolders: currentExpanded,
            showImageCounts: needsImageCounts
          }).then((trees) => ({ type: 'root', trees }))
        );
      }

      Promise.all(promises)
        .then((results) => {
          useLibraryStore.getState().setLibrary((_state) => {
            const updates: Partial<LibraryState> = { isTreeLoading: false };
            results.forEach((res) => {
              if (res.type === 'pinned') updates.pinnedFolderTrees = res.trees;
              if (res.type === 'root') updates.folderTrees = res.trees;
            });
            return updates;
          });
        })
        .catch((err) => {
          console.error('Failed to re-fetch trees for image counts:', err);
          setLibrary({ isTreeLoading: false });
        });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- re-fetch the trees only when image counts or the sort key change, not on every settings save
  }, [appSettings?.enableFolderImageCounts, appSettings?.folderTreeSort?.key]);

  useEffect(() => {
    const root = document.documentElement;

    // Rawtonic uses the platform's interface font throughout.
    root.style.setProperty('--font-family', '-apple-system, BlinkMacSystemFont, system-ui, sans-serif');
  }, []);
};
