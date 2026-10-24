import { create } from 'zustand';
import {
  ImageFile,
  Panel,
  UiVisibility,
  CullingSuggestions,
  PanelRegion,
  WorkspaceState,
  CollapsibleSectionsState,
} from '../components/ui/AppProperties';
import { useEditorStore } from './useEditorStore';
import {
  type PanelGroup,
  type PanelRequest,
  type ToolLayout,
  moduleOf,
  presentGroups,
  readModuleLayout,
  requestGroups,
  setPanelGroups,
  togglePanelGroups,
} from '../utils/panelLayout';

export type SwitcherPlacement = 'bottom' | 'right' | 'left' | 'top';

export interface CropSectionsState {
  transform: boolean;
  lens: boolean;
}

interface ConfirmModalState {
  confirmText?: string;
  confirmVariant?: string;
  isOpen: boolean;
  message?: string;
  onConfirm?(): void;
  title?: string;
}

interface CollageModalState {
  isOpen: boolean;
  sourceImages: Array<Pick<ImageFile, 'path'>>;
}

export type MergeKind = 'hdr' | 'panorama' | 'hdrPanorama' | 'focusStack';

/** The merge dialog (MergeModal): the photos, the running or finished
 * merge's preview and what the merge reported. */
export interface MergeModalState {
  error: string | null;
  isOpen: boolean;
  isProcessing: boolean;
  /** The run this dialog waits for (merges.rs): events and results of
   * other runs are stale. */
  job: number | null;
  kind: MergeKind;
  overlayBase64: string | null;
  previewBase64: string | null;
  progressMessage: string | null;
  report: Record<string, unknown> | null;
  sourcePaths: Array<string>;
}

export const CLOSED_MERGE_MODAL: MergeModalState = {
  error: null,
  isOpen: false,
  isProcessing: false,
  job: null,
  kind: 'hdr',
  overlayBase64: null,
  previewBase64: null,
  progressMessage: null,
  report: null,
  sourcePaths: [],
};

interface NegativeConversionModalState {
  isOpen: boolean;
  targetPaths: Array<string>;
}

interface CullingModalState {
  isOpen: boolean;
  suggestions: CullingSuggestions | null;
  progress: { current: number; total: number; stage: string } | null;
  error: string | null;
  pathsToCull: Array<string>;
}

const ALL_PANELS: Panel[] = [
  Panel.Metadata,
  Panel.FolderTree,
  Panel.Export,
  Panel.Adjustments,
  Panel.Crop,
  Panel.Masks,
  Panel.Presets,
];

const DEFAULT_PANEL_DEFAULT_REGIONS: Record<Panel, PanelRegion> = {
  [Panel.Metadata]: 'leftTop',
  [Panel.FolderTree]: 'leftTop',
  [Panel.Export]: 'leftTop',
  [Panel.Adjustments]: 'rightTop',
  [Panel.Crop]: 'rightTop',
  [Panel.Masks]: 'rightTop',
  [Panel.Presets]: 'rightTop',
};

export const DEFAULT_PANEL_WIDTH = 350;
export const DEFAULT_PANEL_SECTION_HEIGHT = 450;
export const DEFAULT_BOTTOM_PANEL_HEIGHT = 144;

export function reconcileWorkspace(savedWorkspace: WorkspaceState | undefined): WorkspaceState {
  const allowedPanels = new Set(ALL_PANELS);

  const defaultWorkspace: WorkspaceState = {
    leftPanelWidth: DEFAULT_PANEL_WIDTH,
    rightPanelWidth: DEFAULT_PANEL_WIDTH,
    leftTopHeight: DEFAULT_PANEL_SECTION_HEIGHT,
    rightTopHeight: DEFAULT_PANEL_SECTION_HEIGHT,
    panelLayout: {
      leftTop: [Panel.Metadata, Panel.FolderTree, Panel.Export],
      leftBottom: [],
      rightTop: [Panel.Adjustments, Panel.Crop, Panel.Masks, Panel.Presets],
      rightBottom: [],
    },
    activePanels: {
      leftTop: Panel.FolderTree,
      leftBottom: null,
      rightTop: Panel.Adjustments,
      rightBottom: null,
    },
    panelSwitcherPlacement: {
      leftTop: 'bottom',
      leftBottom: 'bottom',
      rightTop: 'right',
      rightBottom: 'right',
    },
  };

  if (!savedWorkspace || !savedWorkspace.panelLayout) {
    return defaultWorkspace;
  }

  const seenPanels = new Set<Panel>();
  const sanitizedLayout: Record<PanelRegion, Panel[]> = {
    leftTop: [],
    leftBottom: [],
    rightTop: [],
    rightBottom: [],
  };

  (['leftTop', 'leftBottom', 'rightTop', 'rightBottom'] as PanelRegion[]).forEach((region) => {
    const list = savedWorkspace.panelLayout[region] || [];
    list.forEach((panel) => {
      if (allowedPanels.has(panel) && !seenPanels.has(panel)) {
        sanitizedLayout[region].push(panel);
        seenPanels.add(panel);
      }
    });
  });

  allowedPanels.forEach((panel) => {
    if (!seenPanels.has(panel)) {
      const targetRegion = DEFAULT_PANEL_DEFAULT_REGIONS[panel] || 'leftTop';
      sanitizedLayout[targetRegion].push(panel);
      seenPanels.add(panel);
    }
  });

  const sanitizedActive: Record<PanelRegion, Panel | null> = {
    leftTop: null,
    leftBottom: null,
    rightTop: null,
    rightBottom: null,
  };

  (['leftTop', 'leftBottom', 'rightTop', 'rightBottom'] as PanelRegion[]).forEach((region) => {
    const currentActive = savedWorkspace.activePanels?.[region];
    if (currentActive && sanitizedLayout[region].includes(currentActive)) {
      sanitizedActive[region] = currentActive;
    } else {
      sanitizedActive[region] = sanitizedLayout[region].length > 0 ? sanitizedLayout[region][0] : null;
    }
  });

  return {
    leftPanelWidth: savedWorkspace.leftPanelWidth || defaultWorkspace.leftPanelWidth,
    rightPanelWidth: savedWorkspace.rightPanelWidth || defaultWorkspace.rightPanelWidth,
    leftTopHeight: savedWorkspace.leftTopHeight || defaultWorkspace.leftTopHeight,
    rightTopHeight: savedWorkspace.rightTopHeight || defaultWorkspace.rightTopHeight,
    panelLayout: sanitizedLayout,
    activePanels: sanitizedActive,
    panelSwitcherPlacement: {
      ...defaultWorkspace.panelSwitcherPlacement,
      ...(savedWorkspace.panelSwitcherPlacement || {}),
    },
  };
}

/** A RAW the camera help is offered for: one that failed to decode
 * ("unreadable") or shows only its embedded camera preview ("preview"). */
export interface CameraHelpRequest {
  path: string;
  reason: 'unreadable' | 'preview';
}

export interface UIState {
  activeView: string;
  /** Photographs per row of the Library grid while it is shown, else 0
   * (Up/Down move a row there). */
  gridColumns: number;
  /** The photograph alone on the viewer background, without bars, panels
   * or edge strips (F), from the Library and Develop (utils/viewerMode). */
  isFullScreen: boolean;
  /** The Library's single large photograph (loupe) instead of the grid. */
  libraryLoupe: boolean;
  /** Read-only facts of the photograph over the viewer (I in Develop and
   * in the full screen). */
  infoOverlay: boolean;
  isWindowFullScreen: boolean;
  isInstantTransition: boolean;
  isLayoutReady: boolean;
  uiVisibility: UiVisibility;
  isSettingsOpen: boolean;
  /** "Report a problem" (src/components/contribute/ContributionHost.tsx). */
  isProblemReportOpen: boolean;
  setProblemReportOpen(open: boolean): void;
  /** "Help support this camera" for a RAW that could not be developed
   * (src/components/contribute/CameraHelpDialog.tsx), or null. */
  cameraHelp: CameraHelpRequest | null;
  setCameraHelp(request: CameraHelpRequest | null): void;

  leftPanelWidth: number;
  rightPanelWidth: number;
  bottomPanelHeight: number;
  leftTopHeight: number;
  rightTopHeight: number;
  compactEditorPanelHeightOverride: number | null;

  panelLayout: Record<PanelRegion, Panel[]>;
  activePanels: Record<PanelRegion, Panel | null>;
  activeLayoutDragItem: Panel | null;
  setLayoutDragItem: (panel: Panel | null) => void;
  movePanel: (panel: Panel, toRegion: PanelRegion) => void;
  movePanelToIndex: (panel: Panel, toRegion: PanelRegion, index: number) => void;
  setActivePanel: (region: PanelRegion, panel: Panel | null) => void;

  panelSwitcherPlacement: Record<PanelRegion, SwitcherPlacement>;
  setPanelSwitcherPlacement: (region: PanelRegion, placement: SwitcherPlacement) => void;

  activePanel: Panel | null;
  renderedPanel: Panel | null;
  slideDirection: number;
  collapsibleSectionsState: CollapsibleSectionsState;
  cropSectionsState: CropSectionsState;

  isCreateFolderModalOpen: boolean;
  isRenameFolderModalOpen: boolean;
  isRenameFileModalOpen: boolean;
  renameTargetPaths: Array<string>;
  folderActionTarget: string | null;

  confirmModalState: ConfirmModalState;
  mergeModalState: MergeModalState;
  negativeModalState: NegativeConversionModalState;
  cullingModalState: CullingModalState;
  collageModalState: CollageModalState;

  setUI: (updater: Partial<UIState> | ((state: UIState) => Partial<UIState>)) => void;
  setPanel: (panel: Panel | null) => void;
  searchFocusRequest: number;
  requestSearchFocus: () => void;
  toggleFullScreen: () => void;
  resetWorkspaceLayout: () => WorkspaceState;

  /** Where the Studio workspace puts its tools (stored in localStorage). */
  studioToolLayout: ToolLayout;
  setStudioToolLayout: (layout: ToolLayout) => void;
  /** Tab, Shift+Tab, F7, F8 and F6 in the open module (Library or Develop). */
  togglePanels: (request: PanelRequest) => void;
  /** Shows parts of the open module's layout (the window-edge strips). */
  showPanels: (groups: PanelGroup[]) => void;
}

/** A stored Studio layout value; also reads the key from before the rename to Rawtonic. */
export function storedStudioLayout(key: 'layout' | 'width') {
  try {
    return localStorage.getItem(`rawtonic.studio.${key}`) ?? localStorage.getItem(`retouch.studio.${key}`);
  } catch {
    return null;
  }
}

/** Below this width the Library tools beside Develop's right-hand tools are hidden (studio.css). */
const isNarrowWindow = () =>
  typeof window !== 'undefined' && Boolean(window.matchMedia?.('(max-width: 1200px)').matches);

/** Whether any dialog of the app is open (`is…ModalOpen` flags and
 * `…ModalState.isOpen`); keyboard shortcuts must not act behind one. Derived
 * from the state's names so that a new dialog cannot be forgotten. */
export function isAnyModalOpen(state: UIState): boolean {
  return Object.entries(state).some(
    ([key, value]) =>
      (/^is\w+ModalOpen$/.test(key) && value === true) ||
      (key.endsWith('ModalState') && (value as { isOpen?: boolean } | null)?.isOpen === true),
  );
}

export const useUIStore = create<UIState>((set, get) => ({
  activeView: 'library',
  gridColumns: 0,
  isFullScreen: false,
  libraryLoupe: false,
  infoOverlay: false,
  isWindowFullScreen: false,
  isInstantTransition: false,
  isLayoutReady: false,
  uiVisibility: { filmstrip: true, leftPanel: true, rightPanel: true, quickFilter: false },
  isSettingsOpen: false,
  isProblemReportOpen: false,
  setProblemReportOpen: (open) => set({ isProblemReportOpen: open }),
  cameraHelp: null,
  setCameraHelp: (request) => set({ cameraHelp: request }),

  leftPanelWidth: DEFAULT_PANEL_WIDTH,
  rightPanelWidth: DEFAULT_PANEL_WIDTH,
  bottomPanelHeight: DEFAULT_BOTTOM_PANEL_HEIGHT,
  leftTopHeight: DEFAULT_PANEL_SECTION_HEIGHT,
  rightTopHeight: DEFAULT_PANEL_SECTION_HEIGHT,
  compactEditorPanelHeightOverride: null,

  panelLayout: {
    leftTop: [Panel.Metadata, Panel.FolderTree, Panel.Export],
    leftBottom: [],
    rightTop: [Panel.Adjustments, Panel.Crop, Panel.Masks, Panel.Presets],
    rightBottom: [],
  },
  activePanels: {
    leftTop: Panel.FolderTree,
    leftBottom: null,
    rightTop: Panel.Adjustments,
    rightBottom: null,
  },
  activeLayoutDragItem: null,

  panelSwitcherPlacement: {
    leftTop: 'bottom',
    leftBottom: 'bottom',
    rightTop: 'right',
    rightBottom: 'right',
  },
  setPanelSwitcherPlacement: (region, placement) =>
    set((state) => ({
      panelSwitcherPlacement: { ...state.panelSwitcherPlacement, [region]: placement },
    })),

  activePanel: Panel.Adjustments,
  renderedPanel: Panel.Adjustments,
  slideDirection: 1,
  collapsibleSectionsState: { basic: true, color: false, curves: true, details: false, effects: false },
  cropSectionsState: { transform: false, lens: false },

  isCreateFolderModalOpen: false,
  isRenameFolderModalOpen: false,
  isRenameFileModalOpen: false,
  renameTargetPaths: [],
  folderActionTarget: null,

  confirmModalState: { isOpen: false },
  mergeModalState: CLOSED_MERGE_MODAL,
  negativeModalState: { isOpen: false, targetPaths: [] },
  cullingModalState: { isOpen: false, suggestions: null, progress: null, error: null, pathsToCull: [] },
  collageModalState: { isOpen: false, sourceImages: [] },

  setUI: (updater) => set((state) => (typeof updater === 'function' ? updater(state) : updater)),

  setLayoutDragItem: (panel) => set({ activeLayoutDragItem: panel }),

  movePanel: (panel, toRegion) =>
    set((state) => {
      const layout = {
        leftTop: [...state.panelLayout.leftTop],
        leftBottom: [...state.panelLayout.leftBottom],
        rightTop: [...state.panelLayout.rightTop],
        rightBottom: [...state.panelLayout.rightBottom],
      };
      const active = { ...state.activePanels };

      let fromRegion: PanelRegion | null = null;
      for (const r of Object.keys(layout) as PanelRegion[]) {
        if (layout[r].includes(panel)) {
          fromRegion = r;
          layout[r] = layout[r].filter((p) => p !== panel);
        }
      }

      if (!layout[toRegion].includes(panel)) layout[toRegion].push(panel);

      if (fromRegion && active[fromRegion] === panel) {
        active[fromRegion] = layout[fromRegion].length > 0 ? layout[fromRegion][0] : null;
      }

      active[toRegion] = panel;

      return {
        panelLayout: layout,
        activePanels: active,
        activeLayoutDragItem: null,
        activePanel: panel,
        renderedPanel: panel,
      };
    }),

  movePanelToIndex: (panel, toRegion, index) =>
    set((state) => {
      const layout = {
        leftTop: [...state.panelLayout.leftTop],
        leftBottom: [...state.panelLayout.leftBottom],
        rightTop: [...state.panelLayout.rightTop],
        rightBottom: [...state.panelLayout.rightBottom],
      };
      const active = { ...state.activePanels };

      let fromRegion: PanelRegion | null = null;
      for (const r of Object.keys(layout) as PanelRegion[]) {
        if (layout[r].includes(panel)) {
          fromRegion = r;
          layout[r] = layout[r].filter((p) => p !== panel);
        }
      }

      const clampedIndex = Math.max(0, Math.min(index, layout[toRegion].length));
      layout[toRegion].splice(clampedIndex, 0, panel);

      if (fromRegion && active[fromRegion] === panel) {
        active[fromRegion] = layout[fromRegion].length > 0 ? layout[fromRegion][0] : null;
      }
      active[toRegion] = panel;

      return {
        panelLayout: layout,
        activePanels: active,
        activeLayoutDragItem: null,
        activePanel: panel,
        renderedPanel: panel,
      };
    }),

  setActivePanel: (region, panel) =>
    set((state) => {
      if (!panel) return state;
      const updates: Partial<UIState> = {
        activePanels: { ...state.activePanels, [region]: panel },
        activePanel: panel,
        renderedPanel: panel,
      };

      const isLeft = region === 'leftTop' || region === 'leftBottom';
      const isRight = region === 'rightTop' || region === 'rightBottom';

      if (isLeft && !state.uiVisibility.leftPanel) {
        updates.uiVisibility = { ...state.uiVisibility, leftPanel: true };
        if (state.leftPanelWidth < DEFAULT_PANEL_WIDTH) {
          updates.leftPanelWidth = DEFAULT_PANEL_WIDTH;
        }
      }

      if (isRight && !state.uiVisibility.rightPanel) {
        updates.uiVisibility = { ...state.uiVisibility, rightPanel: true };
        if (state.rightPanelWidth < DEFAULT_PANEL_WIDTH) {
          updates.rightPanelWidth = DEFAULT_PANEL_WIDTH;
        }
      }

      return updates;
    }),

  setPanel: (panelId) => {
    const state = get();
    if (!panelId) return;

    let targetRegion: PanelRegion | null = null;
    for (const region of Object.keys(state.panelLayout) as PanelRegion[]) {
      if (state.panelLayout[region].includes(panelId)) {
        targetRegion = region;
        break;
      }
    }
    if (targetRegion) state.setActivePanel(targetRegion, panelId);
  },

  toggleFullScreen: () => {
    const { isFullScreen } = get();
    const { zoom, selectedImage } = useEditorStore.getState();

    const isNotFitToScreen = Math.abs(zoom - 1) > 0.01;
    set({ isInstantTransition: isNotFitToScreen });

    if (isFullScreen) {
      set({ isFullScreen: false });
    } else {
      if (!selectedImage) return;
      set({ isFullScreen: true });
    }

    if (isNotFitToScreen) {
      setTimeout(() => set({ isInstantTransition: false }), 100);
    }
  },

  resetWorkspaceLayout: () => {
    const defaultWorkspace = reconcileWorkspace(undefined);
    set({
      leftPanelWidth: defaultWorkspace.leftPanelWidth,
      rightPanelWidth: defaultWorkspace.rightPanelWidth,
      leftTopHeight: defaultWorkspace.leftTopHeight,
      rightTopHeight: defaultWorkspace.rightTopHeight,
      panelLayout: defaultWorkspace.panelLayout,
      activePanels: defaultWorkspace.activePanels,
      panelSwitcherPlacement: defaultWorkspace.panelSwitcherPlacement,
      uiVisibility: { filmstrip: true, leftPanel: true, rightPanel: true, quickFilter: false },
      activePanel: defaultWorkspace.activePanels.rightTop || null,
      renderedPanel: defaultWorkspace.activePanels.rightTop || null,
    });
    return defaultWorkspace;
  },

  searchFocusRequest: 0,
  requestSearchFocus: () => set((state) => ({ searchFocusRequest: state.searchFocusRequest + 1 })),

  studioToolLayout: storedStudioLayout('layout') === 'split-tools' ? 'split-tools' : 'tools-left',
  setStudioToolLayout: (layout) => {
    try {
      localStorage.setItem('rawtonic.studio.layout', layout);
    } catch {
      // Not persisted; the layout still applies for this run.
    }
    set({ studioToolLayout: layout });
  },
  togglePanels: (request) =>
    set((state) => {
      const module = moduleOf(state.activeView);
      const groups = requestGroups(request, presentGroups(module, state.studioToolLayout, isNarrowWindow()));
      if (!groups.length) return state;
      const layout = togglePanelGroups(readModuleLayout(state.uiVisibility.modules, module), groups);
      return { uiVisibility: { ...state.uiVisibility, modules: { ...state.uiVisibility.modules, [module]: layout } } };
    }),
  showPanels: (groups) =>
    set((state) => {
      const module = moduleOf(state.activeView);
      const layout = setPanelGroups(readModuleLayout(state.uiVisibility.modules, module), groups, true);
      return { uiVisibility: { ...state.uiVisibility, modules: { ...state.uiVisibility.modules, [module]: layout } } };
    }),
}));
