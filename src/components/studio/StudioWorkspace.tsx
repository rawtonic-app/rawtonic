import AppOverflowMenu from "./AppOverflowMenu";
import { useRecipeMenuCommands } from "../../hooks/useRecipeMenuCommands";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ComponentProps,
  type ReactNode,
  type MouseEvent,
} from "react";
import LibraryToolbarPlacement from "./LibraryToolbarPlacement";
import { useLibraryHeaderFits } from "./libraryHeaderLayout";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { useShallow } from "zustand/react/shallow";
import {
  ArrowLeft,
  ArrowUp,
  ArrowDown,
  ArrowRight,
  Grid2X2,
  SlidersHorizontal,
  PanelLeftClose,
  PanelLeftOpen,
  Undo2,
  Redo2,
  Copy,
  ClipboardPaste,
  RotateCcw,
  PanelLeft,
  PanelRight,
  Settings,
  Upload,
  LoaderCircle,
  X,
  Search,
  Star,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  Flag,
  FlagOff,
  History,
  RefreshCw,
  ListChecks,
  Maximize,
} from "lucide-react";
import { canUndo, canRedo, getUndoLabel, getRedoLabel, undo, redo, useCatalogUndoStore } from '../../utils/appUndo';
import { panelsShowReady, useEditorStore } from "../../store/useEditorStore";
import { useLibraryStore } from "../../store/useLibraryStore";
import { useUIStore } from "../../store/useUIStore";
import { useContextMenu } from "../../context/ContextMenuContext";
import { useProcessStore } from "../../store/useProcessStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import CompareButton from "./CompareButton";
import { useCompareSession } from "../../hooks/useCompareSession";
import { type ImageFile, Invokes, Panel, SortDirection } from "../ui/AppProperties";
import Editor from "../panel/Editor";
import StudioExport from "./StudioExport";
import { ExportQueuePanel } from "./StudioExportQueue";
import PreviewBuildStatus from "./PreviewBuildStatus";
import PreferencesDialog from "./PreferencesDialog";
import { ProofMarginButton, ViewerCaptureInfo, ZoomPresetOptions } from "./ViewerControls";
import OfflineBadge, { OfflineCount } from "./OfflineBadge";
import StillPreviewBadge from "./StillPreviewBadge";
import ErrorBoundary from "../ui/ErrorBoundary";
import StudioDevelop from "./StudioDevelop";
import StudioNavigation, { type FolderActions } from "./StudioNavigation";
import StudioBrowser from "./StudioBrowser";
import StudioFilters from "./StudioFilters";
import StudioToolBar from "./StudioToolBar";
import MultiViewStage from "./MultiViewStage";
import MultiViewButtons from "./MultiViewButtons";
import StudioLayerSelect from "./StudioLayerSelect";
import StudioToolPopover, { toolPopoverApplies } from "./StudioToolPopover";
import { stepBrushSize, toggleRemoveObjects, useStudioTools, type StudioTool } from "./useStudioTools";
import { useBrushMemory } from "./useBrushMemory";
import { useEdgePeek } from "./useEdgePeek";
import { useQuickAdjust } from "./useQuickAdjust";
import { storedStudioLayout } from "../../store/useUIStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useRelightStore } from "../../store/useRelightStore";
import { splitViewAvailable } from "../../utils/beforeAfter";
import {
  moduleOf,
  presentGroups,
  readModuleLayout,
} from "../../utils/panelLayout";
import {
  buildComboMap,
  fieldOwnsKey,
  normalizeCombo,
  shortcutHint,
} from "../../utils/keyboardUtils";
import { swallowNextClick } from "../../utils/swallowClick";
import { DEFAULT_BRUSH_SIZE } from "../../utils/brushSize";
import type { FlagCounts } from "../../utils/culling";
import { neighboursToPrepare, stepDirection } from "../../utils/photoStepping";
import { useSessionBar } from "../session/sessionBar";
import { openMaskingTools, useAiModelLifecycle } from "../../hooks/useAiMasking";
import "./studio.css";
import StudioMetadataFilter from "./StudioMetadataFilter";
import { SmartAlbumEditorHost } from "./SmartAlbumEditor";
import { CopySettingsDialogHost } from "./CopySettingsDialog";
import { SeriesMatchDialogHost } from "./SeriesMatchDialog";
import { PerfProfiler } from "../../hooks/usePerformanceDiagnostics";
import { clearLibrarySelection, focusWorkspaceControl, reselectForDevelop } from "./workspaceInteractions";
import { registerAppCommands, runAppCommand } from "../../utils/appCommands";
import ViewerInfoOverlay from "./ViewerInfoOverlay";
import { useIdleCursor } from "./useIdleCursor";
import { followPhotoFullScreen } from "../../utils/photoFullScreen";

interface Props {
  files: ImageFile[];
  flagCounts: FlagCounts;
  /** Forwarded unchanged to the Editor's zoom/pan handle. */
  transformWrapperRef: ComponentProps<typeof Editor>["transformWrapperRef"];
  onSelect(path: string, event: MouseEvent): void;
  onFilmSelect(path: string, event: MouseEvent): void;
  onOpen(path: string): void;
  /** Shows a photograph in the Library's loupe or full screen (no Develop). */
  onShow?(path: string): void;
  onNavigate(path: string): void;
  /** Shows a list of photographs as an album in the grid. */
  onSelectAlbum?(id: string, name: string, paths: string[], preserveEditor?: boolean): void;
  onBack(): void;
  onRate(rating: number, paths?: string[]): void;
  onContextMenu(event: MouseEvent, path: string): void;
  onEditorContextMenu(event: MouseEvent): void;
  /** Copy settings: the picker, or (`quick`) the choice of last time. */
  onCopy(path?: string, options?: { quick?: boolean }): void;
  onPaste(paths?: string[]): void;
  onReset(paths?: string[]): void;
  onZoom(value: number, fit?: boolean): void;
  requestThumbnails(paths: string[]): void;
  settings: ReactNode;
  externalEditBar?: ReactNode;
  onSettings(): void;
  /** Move/trash for folders in the Folders tool. */
  folderActions?: FolderActions;
}
/** A thin strip on a window edge that brings a hidden part of the layout
 * back (Lightroom); its arrow points the way the part will come in. */
function EdgeStrip({
  edge,
  label,
  hint,
  onShow,
  dragRegion,
  peekProps,
}: {
  edge: "left" | "right" | "top" | "bottom";
  label: string;
  hint: string | null;
  onShow(): void;
  dragRegion?: boolean;
  /** Pointer handlers that show the hidden side panel while the pointer rests here. */
  peekProps?: React.HTMLAttributes<HTMLDivElement>;
}) {
  const { t } = useTranslation();
  const Arrow = {
    left: ChevronRight,
    right: ChevronLeft,
    top: ChevronDown,
    bottom: ChevronUp,
  }[edge];
  return (
    <div
      className={`studio-edge is-${edge}`}
      data-tauri-drag-region={dragRegion || undefined}
      {...peekProps}
    >
      <button
        aria-label={label}
        title={hint ? t("studio.workspace.withShortcut", { label, shortcut: hint }) : label}
        onClick={onShow}
      >
        <Arrow size={11} />
      </button>
    </div>
  );
}

const readyNow = () => Boolean(useEditorStore.getState().selectedImage?.isReady);

export default function StudioWorkspace(props: Props) {
  const {
    files,
    onSelect,
    onOpen,
    onNavigate,
    onBack,
    onRate,
    onContextMenu,
    onEditorContextMenu,
    onCopy,
    onPaste,
    onReset,
    onZoom,
    requestThumbnails,
    transformWrapperRef,
  } = props;
  const { t, i18n } = useTranslation();
  // Labels and routing read both histories; subscribe so they update on either change.
  useCatalogUndoStore();
  const { selectedImage, previousPhoto } = useEditorStore(
    useShallow((s) => ({
      selectedImage: s.selectedImage,
      historyIndex: s.historyIndex,
      historyPending: s.historyPending,
      history: s.history,
      previousPhoto: s.previousPhoto,
    })),
  );
  const { handleSyncSettings: syncSettings, handlePasteFromPrevious: pasteFromPrevious } = useEditorActions();
  const { hideContextMenu } = useContextMenu();
  const hideMenuRef = useRef(hideContextMenu);
  hideMenuRef.current = hideContextMenu;
  // Sync settings: the other selected photographs.
  const syncTargets = useLibraryStore(
    (s) => s.multiSelectedPaths.filter((path) => path !== selectedImage?.path).length,
  );
  const splitAvailable = useSettingsStore((s) =>
    splitViewAvailable(s.appSettings?.useWgpuRenderer, s.osPlatform),
  );
  // While the next photograph loads the controls keep their look; clicks
  // wait until it is ready.
  const panelsReady = useEditorStore(panelsShowReady);
  // One before/after tool: the split on Y and, held, the whole photograph
  // before on \ (the peek works without the split too).
  const beforeAfterTitle = useSettingsStore((s) => {
    const noKey = t("studio.workspace.noKey");
    const peek = shortcutHint("show_original", s.appSettings?.keybinds, s.osPlatform) ?? noKey;
    if (!splitViewAvailable(s.appSettings?.useWgpuRenderer, s.osPlatform)) {
      return t("studio.workspace.viewer.beforeAfterUnavailable", { peek });
    }
    const split = shortcutHint("toggle_split_view", s.appSettings?.keybinds, s.osPlatform) ?? noKey;
    return t("studio.workspace.viewer.beforeAfterTitle", { split, peek });
  });
  const tools = useStudioTools();
  const [metadataOpen, setMetadataOpen] = useState(false);
  const [toolPopover, setToolPopover] = useState<{
    x: number;
    y: number;
  } | null>(null);
  const closeToolPopover = useCallback(() => setToolPopover(null), []);
  // With a mask tool active, a right-click on the photograph opens its
  // settings (Capture One); otherwise the photograph's context menu.
  const onViewerContextMenu = (event: MouseEvent) => {
    if (toolPopoverApplies(tools)) {
      event.preventDefault();
      const focused = document.activeElement;
      const trigger = focused instanceof HTMLElement && event.currentTarget.contains(focused)
        ? focused : event.currentTarget;
      const rect = trigger.getBoundingClientRect();
      setToolPopover(
        event.clientX === 0 && event.clientY === 0
          ? { x: rect.left, y: rect.bottom }
          : { x: event.clientX, y: event.clientY },
      );
    } else onEditorContextMenu(event);
  };
  const {
    activeView,
    isSettingsOpen,
    isFullScreen,
    layout,
    storedPanels,
    isWindowFullScreen,
    libraryLoupe,
    infoOverlay,
  } = useUIStore(
    useShallow((s) => ({
      activeView: s.activeView,
      isSettingsOpen: s.isSettingsOpen,
      isFullScreen: s.isFullScreen,
      libraryLoupe: s.libraryLoupe,
      infoOverlay: s.infoOverlay,
      layout: s.studioToolLayout,
      storedPanels: s.uiVisibility.modules,
      isWindowFullScreen: s.isWindowFullScreen,
    })),
  );
  const { keybinds, osPlatform } = useSettingsStore(
    useShallow((s) => ({
      keybinds: s.appSettings?.keybinds,
      osPlatform: s.osPlatform,
    })),
  );
  // Interface size (Preferences): panels and bars grow, the photo's area
  // takes what is left. The viewer itself is never scaled, so the native
  // display below the webview keeps its placement.
  const uiScale = useSettingsStore((s) => s.appSettings?.uiScale ?? 1);
  useEffect(() => {
    const root = document.documentElement;
    root.style.setProperty("--ui-scale", String(uiScale));
    return () => {
      root.style.removeProperty("--ui-scale");
    };
  }, [uiScale]);
  // The focus_search shortcut (Library) puts the cursor into the search field.
  const searchRef = useRef<HTMLInputElement>(null);
  const searchFocusRequest = useUIStore((s) => s.searchFocusRequest);
  useEffect(() => {
    if (!searchFocusRequest) return;
    searchRef.current?.focus();
    searchRef.current?.select();
  }, [searchFocusRequest]);
  const hint = (action: string) => shortcutHint(action, keybinds, osPlatform);
  /** A label with its keyboard shortcut ("no key" when none is assigned). */
  const withShortcut = (label: string, shortcut: string | null | undefined) =>
    t("studio.workspace.withShortcut", { label, shortcut: shortcut ?? t("studio.workspace.noKey") });
  const {
    currentFolderPath,
    sessionDirectory,
    activeAlbumId,
    multiSelectedPaths,
    searchCriteria,
    filterCriteria,
    sortCriteria,
    imageRatings,
  } = useLibraryStore(
    useShallow((s) => ({
      currentFolderPath: s.currentFolderPath,
      sessionDirectory: s.sessionDirectory,
      activeAlbumId: s.activeAlbumId,
      multiSelectedPaths: s.multiSelectedPaths,
      searchCriteria: s.searchCriteria,
      filterCriteria: s.filterCriteria,
      sortCriteria: s.sortCriteria,
      imageRatings: s.imageRatings,
    })),
  );
  useCompareSession();
  const exportState = useProcessStore((s) => s.exportState);
  const develop = activeView === "editor";
  // The Library's loupe (one large photograph beside the Library tools) and
  // its full screen show the photograph in the same native viewer as Develop
  // (utils/viewerMode).
  const libraryViewer = !develop && (libraryLoupe || isFullScreen) && !!selectedImage;
  useEffect(() => {
    // Nothing to show any more (the photograph went away): back to the grid.
    if (develop || selectedImage) return;
    if (libraryLoupe || isFullScreen) useUIStore.getState().setUI({ libraryLoupe: false, isFullScreen: false });
  }, [develop, selectedImage, libraryLoupe, isFullScreen]);
  // The loupe and the full screen are views of the photograph, without
  // edit tools: Develop's crop, mask and picker states must not draw over
  // it or take its clicks (an edit already under way still completes). They
  // are put aside and come back with Develop (same photograph).
  const viewOnly = libraryViewer || isFullScreen;
  const suspended = useRef<{
    path: string | null;
    panel: Panel | null;
    layer: string | null;
    component: string | null;
    spot: boolean;
    faces: boolean;
    protecting: boolean;
    protection: boolean;
    relightHandles: boolean;
  } | null>(null);
  // A ⌘/Ctrl held down, followed from the start, so the wheel guard below
  // also knows a key pressed before the view opened.
  const modifierHeld = useRef(false);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Control" || e.key === "Meta") modifierHeld.current = e.type === "keydown";
    };
    const release = () => {
      modifierHeld.current = false;
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKey, true);
    window.addEventListener("blur", release);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onKey, true);
      window.removeEventListener("blur", release);
    };
  }, []);
  useEffect(() => {
    const ui = useUIStore.getState();
    const editor = useEditorStore.getState();
    const relight = useRelightStore.getState();
    if (viewOnly) {
      suspended.current ??= {
        path: editor.selectedImage?.path ?? null,
        panel: ui.activePanel,
        layer: editor.activeMaskContainerId,
        component: editor.activeMaskId,
        spot: editor.isSpotToolActive,
        faces: editor.showFaceBlemishes,
        protecting: editor.faceProtect.painting,
        protection: editor.showFaceProtection,
        relightHandles: relight.showHandles,
      };
      if (ui.activePanel === Panel.Crop || ui.activePanel === Panel.Masks) ui.setPanel(Panel.Adjustments);
      // Every tool that edits on a click or drag, or draws its handles.
      editor.setEditor({
        activeMaskContainerId: null,
        activeMaskId: null,
        isWbPickerActive: false,
        colorPickerTarget: null,
        lumaPickerLayer: null,
        tatTarget: null,
        isStraightenActive: false,
        isRotationActive: false,
        isGuidedPerspectiveActive: false,
        isSpotToolActive: false,
        showFaceBlemishes: false,
        showFaceProtection: false,
        faceProtect: { ...editor.faceProtect, painting: false },
      });
      useRelightStore.setState({ showHandles: false });
      // A menu or tool popover opened before offers edits: it closes.
      hideMenuRef.current();
      setToolPopover(null);
      return;
    }
    const saved = suspended.current;
    if (!saved) return;
    // Handles of the panels are shown again wherever the view goes.
    useRelightStore.setState({ showHandles: saved.relightHandles });
    if (!develop) return;
    suspended.current = null;
    if (saved.path !== editor.selectedImage?.path) return;
    if (saved.panel === Panel.Crop || saved.panel === Panel.Masks) ui.setPanel(saved.panel);
    // The layer's component (its brush or gradient) too, while it is there.
    const component =
      saved.layer &&
      saved.component &&
      editor.adjustments.masks
        ?.find((layer) => layer.id === saved.layer)
        ?.subMasks.some((sub) => sub.id === saved.component)
        ? saved.component
        : null;
    editor.setEditor({
      ...(saved.layer ? { activeMaskContainerId: saved.layer } : {}),
      ...(component ? { activeMaskId: component } : {}),
      isSpotToolActive: saved.spot,
      showFaceBlemishes: saved.faces,
      showFaceProtection: saved.protection,
      faceProtect: { ...editor.faceProtect, painting: saved.protecting },
    });
  }, [viewOnly, develop]);
  // In the Library a photograph that stops matching the filter (rated
  // while culling) gives way to the one now in its place; with none left
  // the grid comes back.
  const shownIndex = useRef(-1);
  const shownPath = selectedImage?.path;
  useEffect(() => {
    if (!libraryViewer || !shownPath) return;
    const index = files.findIndex((file) => file.path === shownPath);
    if (index >= 0) {
      shownIndex.current = index;
      return;
    }
    const next = files[Math.min(Math.max(shownIndex.current, 0), files.length - 1)];
    if (next && onShowRef.current) onShowRef.current(next.path);
    else useUIStore.getState().setUI({ libraryLoupe: false, isFullScreen: false });
  }, [libraryViewer, shownPath, files]);
  // The photograph alone covers the whole screen (photoFullScreen).
  const fullScreenSeen = useRef(isFullScreen);
  useEffect(() => {
    if (fullScreenSeen.current === isFullScreen) return;
    fullScreenSeen.current = isFullScreen;
    void followPhotoFullScreen(isFullScreen);
  }, [isFullScreen]);
  // In the view, a held ⌘/Ctrl (also one pressed before the view opened)
  // turns the wheel into the editor's crop zoom (Editor tracks the key
  // itself); that wheel does nothing here. A trackpad pinch (ctrlKey
  // without the key held) still zooms the view.
  const viewerRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const element = viewerRef.current;
    if (!viewOnly || !element) return;
    const onWheel = (e: WheelEvent) => {
      if (!modifierHeld.current) return;
      e.preventDefault();
      e.stopPropagation();
    };
    element.addEventListener("wheel", onWheel, { capture: true, passive: false });
    return () => element.removeEventListener("wheel", onWheel, { capture: true });
  }, [viewOnly, selectedImage?.path]);
  // The pointer hides over the full screen until it moves.
  const cursorIdle = useIdleCursor(isFullScreen);
  const onShowRef = useRef(props.onShow);
  onShowRef.current = props.onShow;
  /** The loupe on `path`, or on the Library's active photograph: loaded
   * first when it is not the one open (a ⌘-click moves the active one only). */
  const openLoupe = useCallback((path?: string) => {
    const target = path ?? useLibraryStore.getState().libraryActivePath;
    const shown = useEditorStore.getState().selectedImage?.path;
    if (target && target !== shown) onShowRef.current?.(target);
    else if (!shown) return;
    useUIStore.getState().setUI({ libraryLoupe: true, isFullScreen: false });
  }, []);
  /** G: the Library's grid, from anywhere. */
  const showGrid = useCallback(() => {
    useUIStore.getState().setUI({ libraryLoupe: false, isFullScreen: false });
    onBack();
  }, [onBack]);
  /** The Library button: from Develop back to the Library view it was
   * opened from (the loupe, or the grid); in the Library, the grid. */
  const showLibrary = useCallback(() => {
    const { activeView: view, libraryLoupe: loupe } = useUIStore.getState();
    if (view !== "editor" || !loupe) return showGrid();
    useUIStore.getState().setUI({ isFullScreen: false });
    onBack();
  }, [onBack, showGrid]);
  useEffect(() => {
    if (develop) return;
    const state = useLibraryStore.getState();
    if (!state.multiSelectedPaths.length) return;
    const visible = new Set(files.map((file) => file.path));
    const selected = state.multiSelectedPaths.filter((path) =>
      visible.has(path),
    );
    if (selected.length === state.multiSelectedPaths.length) return;
    // A live filter must not leave invisible photographs targeted by P/X/U.
    state.setLibrary({
      multiSelectedPaths: selected,
      libraryActivePath:
        state.libraryActivePath && visible.has(state.libraryActivePath)
          ? state.libraryActivePath
          : null,
      selectionAnchorPath:
        state.selectionAnchorPath && visible.has(state.selectionAnchorPath)
          ? state.selectionAnchorPath
          : null,
    });
  }, [files, develop]);
  const setLayout = useUIStore((s) => s.setStudioToolLayout);
  const [width, setWidth] = useState(() =>
    Math.max(270, Math.min(390, Number(storedStudioLayout("width")) || 300)),
  );
  // Each module keeps its own layout (Lightroom), saved with the settings.
  const panels = readModuleLayout(storedPanels, moduleOf(activeView));
  const present = presentGroups(moduleOf(activeView), layout);
  // The tool panel's side: the left one, except in Develop's split layout.
  const toolsSide = develop && layout === "split-tools" ? "right" : "left";
  // A hidden side panel comes in over the photograph while the pointer rests
  // on its edge (Lightroom's Auto Hide & Show); clicking the strip keeps it.
  const { peek, edgeProps, panelProps, endPeek } = useEdgePeek(
    `${activeView}|${layout}|${panels.left}|${panels.right}`,
  );
  const toolsShown = panels[toolsSide] || peek === toolsSide;
  const libraryBesideShown =
    develop && layout === "split-tools" && (panels.left || peek === "left");
  const film = develop && panels.filmstrip;
  const sidesHidden = present
    .filter((group) => group === "left" || group === "right")
    .every((group) => !panels[group]);
  const { togglePanels, showPanels } = useUIStore.getState();
  const [size, setSize] = useState(205);
  useRecipeMenuCommands();
  const [drawer, setDrawer] = useState<Panel | null>(null);
  // The export queue stays beside the work (not modal): edit on while it runs.
  const [queueOpen, setQueueOpen] = useState(false);
  const dialog = useRef<HTMLDialogElement>(null);
  const beforeDrawerPanel = useRef<Panel | null>(Panel.Adjustments);
  useEffect(
    () =>
      useUIStore.subscribe((next, previous) => {
        if (next.activePanel === previous.activePanel || !next.activePanel)
          return;
        // The Export dialog is the one tool that opens as a dialog.
        if (next.activePanel === Panel.Export) {
          beforeDrawerPanel.current = [Panel.Crop, Panel.Masks].includes(
            previous.activePanel as Panel,
          )
            ? previous.activePanel
            : Panel.Adjustments;
          setDrawer(next.activePanel);
        }
      }),
    [],
  );
  const openTools = (panel: Panel) => {
    beforeDrawerPanel.current = useUIStore.getState().activePanel;
    useUIStore.getState().setPanel(panel);
    setDrawer(panel);
  };
  const closeTools = () => {
    setDrawer(null);
    useUIStore.getState().setPanel(beforeDrawerPanel.current);
  };
  useEffect(() => {
    localStorage.setItem("rawtonic.studio.width", String(width));
  }, [width]);
  // The masking models load with the first photo of a Develop visit (for
  // someone who used them before) and are dropped in the Library, after
  // minutes without use and under memory pressure.
  useAiModelLifecycle(develop);
  useEffect(() => {
    if (drawer) {
      dialog.current?.showModal();
    } else dialog.current?.close();
  }, [drawer]);
  const showDevelop = () => {
    if (selectedImage) {
      reselectForDevelop(selectedImage.path);
      useUIStore.getState().setUI({ activeView: "editor" });
    } else if (files[0]) onOpen(files[0].path);
  };
  // A press on the photograph while a context menu is open only closes the
  // menu (on its mouse down); the viewer must not also zoom or pan.
  useEffect(() => {
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Element | null;
      const menu = document.querySelector('[role="menu"]');
      if (menu && !menu.contains(target) && target?.closest?.(".studio-viewer")) {
        event.stopPropagation();
        swallowNextClick(["click", "dblclick"]);
      }
    };
    window.addEventListener("pointerdown", onPointerDown, true);
    return () => window.removeEventListener("pointerdown", onPointerDown, true);
  }, []);
  // G, E and D, also the View menu's Library Grid, Loupe and Develop
  // (useAppMenu): one handler for the key and the menu item.
  const moduleKeysRef = useRef({ g: showGrid, e: () => openLoupe(), d: () => {} });
  moduleKeysRef.current = {
    g: showGrid,
    e: () => openLoupe(),
    d: () => {
      useUIStore.getState().setUI({ isFullScreen: false });
      showDevelop();
    },
  };
  const filesRef = useRef(files);
  filesRef.current = files;
  useEffect(
    () =>
      registerAppCommands({
        view_grid: {
          run: () => moduleKeysRef.current.g(),
          checked: () => {
            const ui = useUIStore.getState();
            return ui.activeView === "library" && !ui.libraryLoupe && !ui.isFullScreen;
          },
        },
        view_loupe: {
          // As E: from the Library (in Develop, E is the eraser).
          enabled: () =>
            useUIStore.getState().activeView === "library" &&
            !!(useLibraryStore.getState().libraryActivePath || useEditorStore.getState().selectedImage),
          run: () => moduleKeysRef.current.e(),
          checked: () => {
            const ui = useUIStore.getState();
            return ui.activeView === "library" && ui.libraryLoupe && !ui.isFullScreen;
          },
        },
        view_develop: {
          enabled: () => !!useEditorStore.getState().selectedImage || filesRef.current.length > 0,
          run: () => moduleKeysRef.current.d(),
          checked: () => useUIStore.getState().activeView === "editor",
        },
      }),
    [],
  );
  const brushActiveRef = useRef(tools.brushActive);
  brushActiveRef.current = tools.brushActive;
  const setBrushSizeRef = useRef(tools.setBrushSize);
  setBrushSizeRef.current = tools.setBrushSize;
  const toolsRef = useRef(tools);
  toolsRef.current = tools;
  const developRef = useRef(develop);
  developRef.current = develop;
  // Not over the photograph shown alone: the view offers no edit tools.
  const quickAdjust = useQuickAdjust(develop && !isFullScreen);
  useBrushMemory();
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (
        e.defaultPrevented ||
        document.querySelector('dialog[open],[aria-modal="true"]') ||
        fieldOwnsKey(e.target, e.code)
      )
        return;
      const settings = useSettingsStore.getState();
      const commands = buildComboMap(settings.appSettings?.keybinds).get(normalizeCombo(e, settings.osPlatform).join('+')) ?? [];
      for (const command of commands) {
        if (command.startsWith('view_') && runAppCommand(command)) {
          e.preventDefault();
          e.stopImmediatePropagation();
          return;
        }
      }
      if (e.metaKey || e.ctrlKey) return;
      // [ and ] resize the brush while it is active, with Shift they change
      // its hardness (Capture One); otherwise they keep rotating the
      // photograph. Option is allowed: German Mac layouts type [ ] with it.
      const bracket = ({ "[": -1, "{": -1, "]": 1, "}": 1 } as const)[
        e.key as "[" | "{" | "]" | "}"
      ] as -1 | 1 | undefined;
      if (bracket && brushActiveRef.current) {
        e.preventDefault();
        e.stopImmediatePropagation();
        const { brushSettings, setEditor } = useEditorStore.getState();
        if (e.shiftKey || e.key === "{" || e.key === "}") {
          if (brushSettings) {
            const hardness = brushSettings.hardness ?? 50;
            setEditor({
              brushSettings: {
                ...brushSettings,
                hardness: Math.min(100, Math.max(0, hardness + bracket * 10)),
              },
            });
          }
        } else {
          setBrushSizeRef.current(
            stepBrushSize(brushSettings?.size ?? DEFAULT_BRUSH_SIZE, bracket),
          );
        }
        return;
      }
      if (e.altKey) return;
      // Cursor tools take Capture One's keys (tool_* keybinds), not while
      // the photograph is shown alone.
      if (!developRef.current || useUIStore.getState().isFullScreen) return;
      const { appSettings, osPlatform } = useSettingsStore.getState();
      const action = buildComboMap(appSettings?.keybinds)
        .get(normalizeCombo(e, osPlatform).join("+"))
        ?.find((name) => name.startsWith("tool_") || name.startsWith("mask_") || name === "remove_objects");
      if (action && toolsRef.current.ready) {
        e.preventDefault();
        e.stopImmediatePropagation();
        if (action === "remove_objects") toggleRemoveObjects(toolsRef.current.selectTool);
        else if (action === "mask_subject" || action === "mask_background") {
          // As the new-layer grid: the automatic selections start at once.
          openMaskingTools();
          toolsRef.current.addLayer(action === "mask_subject" ? "subject" : "background");
        }
        else toolsRef.current.selectTool(action.slice("tool_".length) as StudioTool);
      }
    };
    // Document capture: after Quick Adjust (window capture), which defers
    // its keys until it knows whether they were tapped.
    document.addEventListener("keydown", handler, true);
    return () => document.removeEventListener("keydown", handler, true);
    // G, E and D go through moduleKeysRef (the handlers of this render).
  }, []);
  const currentIndex = files.findIndex((f) => f.path === selectedImage?.path);
  // The direction of the last step, for the neighbours prepared next.
  const stepping = useRef<{ index: number; direction: 1 | -1 | null }>({ index: -1, direction: 1 });
  useEffect(() => {
    if (currentIndex < 0 || currentIndex === stepping.current.index) return;
    stepping.current = {
      index: currentIndex,
      direction: stepDirection(stepping.current.index, currentIndex, files.length),
    };
  }, [currentIndex, files.length]);
  // Once the open photograph is decoded (not before, so its own decode keeps
  // the cores), prepare its neighbours (preview proxies, and the next one's
  // decode kept), so stepping through the filmstrip shows them developed at
  // once — in Develop, the Library's loupe and the full screen alike. A new
  // request replaces the last one and cancels a decode it no longer asks
  // for (a jump).
  const decoded = Boolean(selectedImage?.isDecoded);
  const photoShown = develop || libraryViewer;
  useEffect(() => {
    if (!photoShown || !decoded || currentIndex < 0) return;
    const paths = neighboursToPrepare(
      files.map((file) => file.path),
      currentIndex,
      stepping.current.direction,
    );
    if (paths.length) void invoke(Invokes.PreparePreviewProxies, { paths }).catch(() => {});
  }, [photoShown, decoded, currentIndex, files]);
  // At each step, before any decode: the next photographs' camera JPEGs
  // (the stills shown until a developed frame, when stepping faster than
  // their proxies are made).
  useEffect(() => {
    if (!photoShown || currentIndex < 0) return;
    const paths = neighboursToPrepare(
      files.map((file) => file.path),
      currentIndex,
      stepping.current.direction,
    );
    if (paths.length) void invoke(Invokes.PrepareEmbeddedStills, { paths }).catch(() => {});
  }, [photoShown, currentIndex, files]);
  const rating = selectedImage ? imageRatings[selectedImage.path] || 0 : 0;
  const browserProps = {
    files,
    onSelect: onSelect,
    onOpen,
    onRate,
    onContextMenu,
    requestThumbnails,
    currentPath: selectedImage?.path,
  };
  const navigation = (
    <StudioNavigation
      onNavigate={onNavigate}
      onSelectAlbum={props.onSelectAlbum}
      folderActions={props.folderActions}
      // Beside Develop (tools on the right) it only switches photographs.
      develop={develop}
    />
  );
  const sessionBar = useSessionBar(
    useShallow((s) => ({ center: s.center, end: s.end, library: s.library, mark: s.mark })),
  );
  const libraryToolbarRef = useRef<HTMLDivElement>(null);
  const compactLibraryHeader = useLibraryHeaderFits(
    uiScale, libraryToolbarRef,
    JSON.stringify([uiScale, i18n.resolvedLanguage, filterCriteria, sessionDirectory, metadataOpen, !!searchCriteria.text.trim(), searchCriteria.tags, props.flagCounts]),
    !develop && panels.bars && !!sessionBar.library,
  );
  const imageFlags = useLibraryStore((s) => s.imageFlags);
  const flag = selectedImage ? (imageFlags[selectedImage.path] ?? 0) : 0;
  const exporting = exportState.status === "exporting";
  // Interrupted or waiting exports keep the Export dialog reachable.
  const activeExports = useProcessStore(
    (s) => s.exportQueue.filter((job) => job.state !== "done").length,
  );
  const queuedExports = activeExports > 0;
  const modules = (
    <nav className="studio-modules" aria-label={t("studio.workspace.modules.workspace")}>
      <button
        aria-pressed={!develop}
        onClick={showLibrary}
        // G is the grid; from Develop the button returns to the loupe it left.
        title={
          develop && libraryLoupe
            ? t("studio.workspace.modules.library")
            : withShortcut(t("studio.workspace.modules.library"), "G")
        }
      >
        <Grid2X2 size={14} />
        {t("studio.workspace.modules.library")}
      </button>
      <button
        aria-pressed={develop}
        disabled={!selectedImage && !files.length}
        onClick={showDevelop}
        title={withShortcut(t("studio.workspace.modules.develop"), "D")}
      >
        <SlidersHorizontal size={14} />
        {t("studio.workspace.modules.develop")}
      </button>
    </nav>
  );
  const barActions = (
    <>
      <PreviewBuildStatus />
      <button
        className="studio-export-queue-button"
        aria-pressed={queueOpen}
        aria-label={
          activeExports
            ? t("studio.export.queue.buttonActive", { count: activeExports })
            : t("studio.export.queue.button")
        }
        title={t("studio.export.queue.title")}
        onClick={() => setQueueOpen(!queueOpen)}
      >
        <ListChecks size={13} />
        {activeExports > 0 && <span>{activeExports}</span>}
      </button>
      <button
        className="studio-export-button"
        disabled={!exporting && !multiSelectedPaths.length && !selectedImage && !queuedExports}
        title={withShortcut(t("studio.workspace.export.title"), "⇧⌘E")}
        onClick={() => openTools(Panel.Export)}
      >
        {/* A long photo (AI Denoise: up to a minute without a GPU) must not
            look stuck: the icon turns and the button names the stage. */}
        {exporting ? (
          <LoaderCircle size={13} className="animate-spin" aria-hidden />
        ) : (
          <Upload size={13} />
        )}
        {exporting
          ? t(
              exportState.denoising?.length
                ? "studio.workspace.export.progressDenoising"
                : "studio.workspace.export.progress",
              {
                current: exportState.progress.current,
                total: exportState.progress.total,
              },
            )
          : multiSelectedPaths.length > 1
            ? t("studio.workspace.export.buttonCount", { n: multiSelectedPaths.length })
            : t("studio.workspace.export.button")}
      </button>
      {/* The icon shows the side the Develop tools move to; also in the
          Preferences (General). */}
      <button
        className="studio-bar-icon"
        title={
          layout === "split-tools"
            ? t("studio.workspace.layout.moveToolsLeft")
            : t("studio.workspace.layout.moveToolsRight")
        }
        aria-label={
          layout === "split-tools"
            ? t("studio.workspace.layout.moveToolsLeft")
            : t("studio.workspace.layout.moveToolsRight")
        }
        onClick={() =>
          setLayout(layout === "split-tools" ? "tools-left" : "split-tools")
        }
      >
        {layout === "split-tools" ? <PanelLeft size={15} /> : <PanelRight size={15} />}
      </button>
      <button
        className="studio-bar-icon"
        title={withShortcut(t("studio.workspace.preferences"), "⌘,")}
        aria-label={t("studio.workspace.preferences")}
        onClick={props.onSettings}
      >
        <Settings size={15} />
      </button>
      <AppOverflowMenu />
    </>
  );
  // The open photograph's flag (`current`) or the selection's (none).
  const flagButtons = (current?: number) => {
    const one = current !== undefined;
    const pick = one ? t("studio.workspace.flags.pickThis") : t("studio.workspace.flags.pickSelected");
    const reject = one
      ? t("studio.workspace.flags.rejectThis")
      : t("studio.workspace.flags.rejectSelected");
    const clear = t("studio.workspace.flags.clearSelected");
    return (
    <div className="studio-flags" role="group" aria-label={t("studio.workspace.flags.group")}>
      <button
        aria-label={pick}
        title={withShortcut(pick, hint("flag_pick"))}
        aria-pressed={current === 1}
        disabled={!sessionBar.mark}
        onClick={() => void sessionBar.mark?.(current === 1 ? 0 : 1)}
      >
        <Flag size={13} fill={current === 1 ? "currentColor" : "none"} />
      </button>
      <button
        aria-label={reject}
        title={withShortcut(reject, hint("flag_reject"))}
        aria-pressed={current === -1}
        disabled={!sessionBar.mark}
        onClick={() => void sessionBar.mark?.(current === -1 ? 0 : -1)}
      >
        <X size={14} />
      </button>
      {current === undefined && (
        <button
          aria-label={clear}
          title={withShortcut(clear, hint("flag_unflag"))}
          disabled={!sessionBar.mark}
          onClick={() => void sessionBar.mark?.(0)}
        >
          <FlagOff size={13} />
        </button>
      )}
    </div>
    );
  };
  const libraryToolbar = (
    <div
      ref={libraryToolbarRef}
      className={`studio-library-toolbar studio-controls${compactLibraryHeader ? " is-in-header" : ""}`}
    >
      <button
        aria-label={t("studio.workspace.viewer.togglePanels")}
        title={withShortcut(
          sidesHidden
            ? t("studio.workspace.viewer.showPanels")
            : t("studio.workspace.viewer.hidePanels"),
          hint("toggle_side_panels"),
        )}
        onClick={() => togglePanels("sides")}
      >
        {sidesHidden ? (
          <PanelLeftOpen size={15} />
        ) : (
          <PanelLeftClose size={15} />
        )}
      </button>
      <label className="studio-search">
        <Search size={13} />
        <input
          ref={searchRef}
          type="search"
          aria-label={t("studio.workspace.grid.searchLabel")}
          placeholder={t("studio.workspace.grid.search")}
          value={searchCriteria.text}
          onChange={(e) =>
            useLibraryStore
              .getState()
              .setSearchCriteria({ text: e.target.value })
          }
        />
      </label>
      <StudioFilters
        counts={props.flagCounts}
        metadataOpen={metadataOpen}
        onToggleMetadata={() => setMetadataOpen((open) => !open)}
      >
        <label
          className="studio-filter"
          title={t("studio.workspace.grid.filterByRating")}
        >
          <Star size={12} />
          <select
            aria-label={t("studio.workspace.grid.filterByRating")}
            value={filterCriteria.rating}
            onChange={(e) =>
              useLibraryStore
                .getState()
                .setFilterCriteria({ rating: Number(e.target.value) })
            }
          >
            <option value="0">{t("studio.workspace.grid.anyRating")}</option>
            <option value="-1">{t("studio.workspace.grid.unrated")}</option>
            {[1, 2, 3, 4, 5].map((n) => (
              <option key={n} value={n}>
                {t("studio.workspace.grid.starsAndUp", { count: n })}
              </option>
            ))}
          </select>
        </label>
      </StudioFilters>
      <label className="studio-filter" title={t("studio.workspace.grid.sort")}>
        <select
          aria-label={t("studio.workspace.grid.sort")}
          value={sortCriteria.key}
          onChange={(e) =>
            useLibraryStore.getState().setSortCriteria({ key: e.target.value })
          }
        >
          <option value="name">{t("studio.workspace.grid.sortBy.name")}</option>
          <option value="date">{t("studio.workspace.grid.sortBy.date")}</option>
          <option value="rating">
            {t("studio.workspace.grid.sortBy.rating")}
          </option>
        </select>
        <button
          title={t("studio.workspace.grid.reverseSort")}
          aria-label={t("studio.workspace.grid.reverseSort")}
          onClick={() =>
            useLibraryStore.getState().setSortCriteria({
              order:
                sortCriteria.order === SortDirection.Ascending
                  ? SortDirection.Descending
                  : SortDirection.Ascending,
            })
          }
        >
          {sortCriteria.order === SortDirection.Ascending ? <ArrowUp size={12} /> : <ArrowDown size={12} />}
        </button>
      </label>
    </div>
  );
  // Develop's viewer; the Library's loupe and full screen show the same.
  const viewer = (
    <div
      ref={viewerRef}
      className="studio-viewer"
      tabIndex={0}
      data-panel-shortcut="true"
      role="group"
      aria-label={t("studio.workspace.viewer.imageViewer")}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget || !toolPopoverApplies(tools)) return;
        if (e.key !== "ContextMenu" && !(e.key === "F10" && e.shiftKey)) return;
        e.preventDefault();
        e.stopPropagation();
        const rect = e.currentTarget.getBoundingClientRect();
        setToolPopover({ x: rect.left, y: rect.bottom });
      }}
      onContextMenuCapture={(e) => {
        // In the view: no menu of edit commands (undo, paste, auto, reset,
        // or ratings for the whole selection) over the photograph.
        if (!viewOnly) return;
        e.preventDefault();
        e.stopPropagation();
      }}
      onPointerDownCapture={(e) => {
        // In the view (loupe, full screen): Shift-drag (straighten) and a
        // ⌘/Ctrl double-click (crop reset) would edit; they do nothing.
        if (viewOnly && (e.shiftKey || e.ctrlKey || e.metaKey)) {
          e.preventDefault();
          e.stopPropagation();
          return;
        }
        if (!(
          e.target instanceof Element &&
          e.target.closest("button,input,select,textarea")
        ))
          e.currentTarget.focus({ preventScroll: true });
      }}
    >
      {selectedImage ? (
        <>
          <MultiViewStage
            files={files}
            onOpen={onOpen}
            onRate={onRate}
            transformWrapperRef={transformWrapperRef}
          >
            <PerfProfiler id="viewer">
              <Editor
                studioMode
                transformWrapperRef={transformWrapperRef}
                onBackToLibrary={onBack}
                onContextMenu={onViewerContextMenu}
                onImageSelect={onOpen}
              />
            </PerfProfiler>
          </MultiViewStage>
          {toolPopover && (
            <StudioToolPopover
              x={toolPopover.x}
              y={toolPopover.y}
              onClose={closeToolPopover}
            />
          )}
          {infoOverlay && (develop || isFullScreen) && <ViewerInfoOverlay />}
          {quickAdjust && (
            // Capture One's Quick Adjust info box beside the pointer.
            <div
              className="studio-quick-adjust"
              role="status"
              style={{ left: quickAdjust.x + 16, top: quickAdjust.y + 16 }}
            >
              {quickAdjust.label}
              <strong>
                {quickAdjust.value > 0 ? "+" : ""}
                {quickAdjust.value}
              </strong>
            </div>
          )}
        </>
      ) : (
        <div className="studio-empty">
          {t("studio.workspace.viewer.empty")}
        </div>
      )}
    </div>
  );
  return (
    <div
      className={`studio-workspace ${layout} module-${moduleOf(activeView)} ${sidesHidden ? "panels-hidden" : ""} ${panels.bars ? "" : "bars-hidden"} ${isFullScreen ? "is-image-fullscreen" : ""} ${libraryViewer ? "is-library-loupe" : ""} ${cursorIdle ? "is-cursor-idle" : ""}`}
      onClickCapture={(e) => focusWorkspaceControl(e.target)}
      style={{ "--tools-width": `${width}px` } as React.CSSProperties}
    >
      {/* Library and Develop both show the Albums tool (it opens the editor). */}
      <SmartAlbumEditorHost />
      <CopySettingsDialogHost />
      <SeriesMatchDialogHost />
      {sessionBar.center && sessionBar.end ? (
        <>
          {createPortal(modules, sessionBar.center)}
          {createPortal(barActions, sessionBar.end)}
        </>
      ) : (
        <header className="studio-module-bar">
          {modules}
          <span className="studio-spacer" />
          <div className="studio-header-actions">{barActions}</div>
        </header>
      )}
      {!panels.bars && (
        <EdgeStrip
          edge="top"
          label={t("studio.workspace.edges.showBars")}
          hint={hint("toggle_all_panels")}
          onShow={() => showPanels(["bars"])}
          // Stands in for the hidden top bar as the window's title bar.
          dragRegion={osPlatform === "macos" && !isWindowFullScreen}
        />
      )}
      <div className="studio-body">
        {present.includes("left") && !panels.left && (
          <EdgeStrip
            edge="left"
            label={t("studio.workspace.edges.showLeft")}
            hint={hint("toggle_left_panel")}
            onShow={() => {
              endPeek();
              showPanels(["left"]);
            }}
            peekProps={edgeProps("left")}
          />
        )}
        {toolsShown && (
          <aside
            className={`studio-primary-panel ${develop ? "is-develop" : "is-library"}${panels[toolsSide] ? "" : " is-peek"}`}
            {...panelProps(toolsSide)}
          >
            <ErrorBoundary key={develop ? "develop" : "library"} name={
                develop
                  ? t("studio.workspace.errors.developTools")
                  : t("studio.workspace.errors.libraryTools")
              }>
              {develop ? <PerfProfiler id="develop"><StudioDevelop /></PerfProfiler> : navigation}
            </ErrorBoundary>
            <div
              className="studio-panel-resizer"
              role="separator"
              tabIndex={0}
              aria-label={t("studio.workspace.panelWidth")}
              aria-orientation="vertical"
              aria-valuemin={270}
              aria-valuemax={390}
              aria-valuenow={width}
              onKeyDown={(e) => {
                if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
                  e.preventDefault();
                  setWidth((v) =>
                    Math.max(
                      270,
                      Math.min(
                        390,
                        v + (e.key === "ArrowRight" ? 10 : -10) *
                          (toolsSide === "right" ? -1 : 1),
                      ),
                    ),
                  );
                }
              }}
              onPointerDown={(e) => {
                e.preventDefault();
                e.currentTarget.setPointerCapture(e.pointerId);
              }}
              onPointerMove={(e) => {
                if (!e.currentTarget.hasPointerCapture(e.pointerId)) return;
                setWidth((v) =>
                  Math.max(
                    270,
                    Math.min(
                      390,
                      v +
                        e.movementX *
                          (toolsSide === "right" ? -1 : 1),
                    ),
                  ),
                );
              }}
              onPointerUp={(e) =>
                e.currentTarget.releasePointerCapture(e.pointerId)
              }
            />
          </aside>
        )}
        {libraryBesideShown && (
          <aside
            className={`studio-secondary-panel${panels.left ? "" : " is-peek"}`}
            {...panelProps("left")}
          >
            <ErrorBoundary name={t("studio.workspace.errors.libraryTools")}>{navigation}</ErrorBoundary>
          </aside>
        )}
        <main className="studio-center">
          {props.externalEditBar}
          {develop ? (
            <>
              <div className="studio-viewer-toolbar">
                <div className="studio-viewer-side">
                  <button
                    title={withShortcut(
                      sidesHidden
                        ? t("studio.workspace.viewer.showPanels")
                        : t("studio.workspace.viewer.hidePanels"),
                      hint("toggle_side_panels"),
                    )}
                    aria-label={t("studio.workspace.viewer.togglePanels")}
                    onClick={() => togglePanels("sides")}
                  >
                    {sidesHidden ? (
                      <PanelLeftOpen size={15} />
                    ) : (
                      <PanelLeftClose size={15} />
                    )}
                  </button>
                  <div className="studio-toolbar-divider" />
                  <button
                    title={withShortcut(getUndoLabel(), "⌘Z")}
                    aria-label={t("studio.workspace.viewer.undo")}
                    disabled={!panelsReady || !canUndo()}
                    onClick={() => {
                      if (readyNow()) void undo();
                    }}
                  >
                    <Undo2 size={15} />
                  </button>
                  <button
                    title={withShortcut(getRedoLabel(), "⇧⌘Z")}
                    aria-label={t("studio.workspace.viewer.redo")}
                    disabled={!panelsReady || !canRedo()}
                    onClick={() => {
                      if (readyNow()) void redo();
                    }}
                  >
                    <Redo2 size={15} />
                  </button>
                  <div className="studio-toolbar-divider" />
                  <CompareButton disabled={!panelsReady || !splitAvailable} title={beforeAfterTitle} />
                  <MultiViewButtons />
                </div>
                <StudioToolBar />
                <StudioLayerSelect />
                <div className="studio-viewer-side is-end">
                  <ProofMarginButton />
                  <button
                    title={withShortcut(t("studio.workspace.viewer.fitTitle"), ",")}
                    onClick={() => onZoom(1, true)}
                  >
                    {t("studio.workspace.viewer.fit")}
                  </button>
                  <button
                    title={withShortcut(t("studio.workspace.viewer.actualPixels"), ".")}
                    onClick={() => onZoom(1)}
                  >
                    100%
                  </button>
                  <select
                    className="studio-zoom-select"
                    aria-label={t("studio.workspace.viewer.zoom")}
                    title={t("studio.workspace.viewer.zoom")}
                    value=""
                    onChange={(e) => {
                      if (e.target.value) onZoom(Number(e.target.value));
                    }}
                  >
                    <option value="" disabled>
                      {t("studio.workspace.viewer.zoom")}
                    </option>
                    <ZoomPresetOptions />
                  </select>
                  <button
                    aria-label={t("studio.workspace.loupe.fullScreen")}
                    title={withShortcut(t("studio.workspace.loupe.fullScreen"), hint("toggle_fullscreen"))}
                    onClick={() => useUIStore.getState().toggleFullScreen()}
                  >
                    <Maximize size={13} />
                  </button>
                  <div className="studio-toolbar-divider" />
                  {/* Capture One keeps Reset at the right of its tool bar. */}
                  <button
                    title={t("studio.workspace.viewer.resetAllTitle")}
                    aria-label={t("studio.workspace.viewer.resetAll")}
                    disabled={!panelsReady}
                    onClick={() => readyNow() && onReset(selectedImage ? [selectedImage.path] : [])}
                  >
                    <RotateCcw size={13} />
                    <span className="optional-label">{t("studio.workspace.viewer.reset")}</span>
                  </button>
                </div>
              </div>
              {viewer}
              {panels.bars && (
              <div className="studio-viewer-footer">
                <div className="studio-viewer-side">
                  <button
                    aria-label={t("studio.workspace.footer.previous")}
                    title={withShortcut(t("studio.workspace.footer.previous"), "←")}
                    disabled={currentIndex <= 0}
                    onClick={() => onOpen(files[currentIndex - 1].path)}
                  >
                    <ArrowLeft size={14} />
                  </button>
                  <span className="studio-counter">
                    {currentIndex >= 0
                      ? `${currentIndex + 1} / ${files.length}`
                      : t("studio.workspace.footer.photographs", { count: 0, n: "0" })}
                  </span>
                  <button
                    aria-label={t("studio.workspace.footer.next")}
                    title={withShortcut(t("studio.workspace.footer.next"), "→")}
                    disabled={
                      currentIndex < 0 || currentIndex >= files.length - 1
                    }
                    onClick={() => onOpen(files[currentIndex + 1].path)}
                  >
                    <ArrowRight size={14} />
                  </button>
                  <span
                    className="studio-viewer-filename"
                    title={selectedImage?.path}
                  >
                    {selectedImage?.path.split("/").pop() || t("studio.workspace.footer.noPhotograph")}
                    {selectedImage?.width ? (
                      <span className="studio-muted">
                        {` · ${selectedImage.width} × ${selectedImage.height}`}
                      </span>
                    ) : null}
                  </span>
                  <OfflineBadge offline={selectedImage?.offline} />
                  <StillPreviewBadge path={selectedImage?.path} />
                </div>
                {selectedImage && flagButtons(flag)}
                <div className="studio-rating">
                  {[1, 2, 3, 4, 5].map((n) => (
                    <button
                      key={n}
                      aria-label={t("studio.workspace.footer.rate", { count: n })}
                      title={withShortcut(t("studio.workspace.footer.stars", { count: n }), String(n))}
                      aria-pressed={rating >= n}
                      disabled={!selectedImage}
                      onClick={() =>
                        onRate(
                          rating === n ? 0 : n,
                          selectedImage ? [selectedImage.path] : [],
                        )
                      }
                    >
                      <Star
                        size={13}
                        fill={rating >= n ? "currentColor" : "none"}
                      />
                    </button>
                  ))}
                </div>
                <div className="studio-viewer-side is-end">
                  <ViewerCaptureInfo />
                  <button
                    title={withShortcut(t("studio.workspace.footer.copyAdjustments"), "⇧⌘C")}
                    aria-label={t("studio.workspace.footer.copyAdjustments")}
                    onClick={(e) => readyNow() && onCopy(undefined, { quick: e.altKey })}
                    disabled={!panelsReady}
                  >
                    <Copy size={13} />
                    <span className="optional-label">{t("studio.workspace.footer.copy")}</span>
                  </button>
                  <button
                    title={withShortcut(t("studio.workspace.footer.pasteAdjustments"), "⇧⌘V")}
                    aria-label={t("studio.workspace.footer.pasteAdjustments")}
                    disabled={!panelsReady}
                    onClick={() =>
                      readyNow() && onPaste(selectedImage ? [selectedImage.path] : [])
                    }
                  >
                    <ClipboardPaste size={13} />
                    <span className="optional-label">{t("studio.workspace.footer.paste")}</span>
                  </button>
                  <button
                    title={withShortcut(t("studio.workspace.footer.pasteFromPrevious"), "⌥⌘V")}
                    aria-label={t("studio.workspace.footer.pasteFromPrevious")}
                    disabled={!panelsReady || !previousPhoto}
                    onClick={() =>
                      readyNow() && void pasteFromPrevious(selectedImage ? [selectedImage.path] : [])
                    }
                  >
                    <History size={13} />
                    <span className="optional-label">{t("studio.workspace.footer.previousSettings")}</span>
                  </button>
                  {syncTargets > 0 && (
                    <button
                      title={withShortcut(t("studio.workspace.footer.syncSettings"), "⇧⌘S")}
                      aria-label={t("studio.workspace.footer.syncSettings")}
                      disabled={!panelsReady}
                      onClick={(e) => readyNow() && void syncSettings({ quick: e.altKey })}
                    >
                      <RefreshCw size={13} />
                      <span className="optional-label">{t("studio.workspace.footer.sync")}</span>
                    </button>
                  )}
                  <div className="studio-toolbar-divider" />
                  <button
                    aria-expanded={film}
                    aria-label={t("studio.workspace.footer.filmstrip")}
                    title={withShortcut(
                      film
                        ? t("studio.workspace.footer.hideFilmstrip")
                        : t("studio.workspace.footer.showFilmstrip"),
                      hint("toggle_bottom_panel"),
                    )}
                    onClick={() => togglePanels("filmstrip")}
                  >
                    <ChevronDown
                      size={15}
                      style={{ transform: film ? "none" : "rotate(180deg)" }}
                    />
                  </button>
                </div>
              </div>
              )}
              {film && (
                <div className="studio-filmstrip">
                  <StudioBrowser
                    {...browserProps}
                    film
                    onSelect={props.onFilmSelect}
                  />
                </div>
              )}
            </>
          ) : libraryViewer ? (
            <>
              {/* The Library's loupe: organise one photograph at a time
                  (rating, flags, keywords and metadata beside it); adjusting
                  it is Develop's job (D). */}
              <div className="studio-viewer-toolbar studio-loupe-bar">
                <div className="studio-viewer-side">
                  <button
                    aria-label={t("studio.workspace.viewer.togglePanels")}
                    title={withShortcut(
                      sidesHidden
                        ? t("studio.workspace.viewer.showPanels")
                        : t("studio.workspace.viewer.hidePanels"),
                      hint("toggle_side_panels"),
                    )}
                    onClick={() => togglePanels("sides")}
                  >
                    {sidesHidden ? <PanelLeftOpen size={15} /> : <PanelLeftClose size={15} />}
                  </button>
                  <div className="studio-toolbar-divider" />
                  <button
                    onClick={showGrid}
                    title={withShortcut(t("studio.workspace.loupe.backToGrid"), "G")}
                  >
                    <Grid2X2 size={14} />
                    <span className="optional-label">{t("studio.workspace.loupe.grid")}</span>
                  </button>
                  <button
                    onClick={() => moduleKeysRef.current.d()}
                    title={withShortcut(t("studio.workspace.modules.develop"), "D")}
                  >
                    <SlidersHorizontal size={14} />
                    <span className="optional-label">{t("studio.workspace.modules.develop")}</span>
                  </button>
                  <button
                    aria-label={t("studio.workspace.footer.previous")}
                    title={withShortcut(t("studio.workspace.footer.previous"), "←")}
                    disabled={files.length < 2}
                    onClick={(e) => {
                      const index = Math.max(currentIndex, 0);
                      const next = files[(index - 1 + files.length) % files.length];
                      if (next) props.onSelect(next.path, e);
                    }}
                  >
                    <ArrowLeft size={14} />
                  </button>
                  <span className="studio-counter">
                    {currentIndex >= 0 ? `${currentIndex + 1} / ${files.length}` : ""}
                  </span>
                  <button
                    aria-label={t("studio.workspace.footer.next")}
                    title={withShortcut(t("studio.workspace.footer.next"), "→")}
                    disabled={files.length < 2}
                    onClick={(e) => {
                      const next = files[(currentIndex + 1) % files.length];
                      if (next) props.onSelect(next.path, e);
                    }}
                  >
                    <ArrowRight size={14} />
                  </button>
                  <span className="studio-viewer-filename" title={selectedImage?.path}>
                    {selectedImage?.path.split("/").pop()}
                    {selectedImage?.width ? (
                      <span className="studio-muted">{` · ${selectedImage.width} × ${selectedImage.height}`}</span>
                    ) : null}
                  </span>
                  <OfflineBadge offline={selectedImage?.offline} />
                </div>
                {selectedImage && flagButtons(flag)}
                <div className="studio-rating">
                  {[1, 2, 3, 4, 5].map((n) => (
                    <button
                      key={n}
                      aria-label={t("studio.workspace.footer.rate", { count: n })}
                      title={withShortcut(t("studio.workspace.footer.stars", { count: n }), String(n))}
                      aria-pressed={rating >= n}
                      disabled={!selectedImage}
                      onClick={() => onRate(rating === n ? 0 : n, selectedImage ? [selectedImage.path] : [])}
                    >
                      <Star size={13} fill={rating >= n ? "currentColor" : "none"} />
                    </button>
                  ))}
                </div>
                <div className="studio-viewer-side is-end">
                  <ViewerCaptureInfo />
                  <button
                    title={withShortcut(t("studio.workspace.viewer.fitTitle"), ",")}
                    onClick={() => onZoom(1, true)}
                  >
                    {t("studio.workspace.viewer.fit")}
                  </button>
                  <button
                    title={withShortcut(t("studio.workspace.viewer.actualPixels"), ".")}
                    onClick={() => onZoom(1)}
                  >
                    100%
                  </button>
                  <button
                    aria-label={t("studio.workspace.loupe.fullScreen")}
                    title={withShortcut(t("studio.workspace.loupe.fullScreen"), hint("toggle_fullscreen"))}
                    onClick={() => useUIStore.getState().toggleFullScreen()}
                  >
                    <Maximize size={13} />
                  </button>
                  <div className="studio-toolbar-divider" />
                  <button
                    onClick={showDevelop}
                    title={withShortcut(t("studio.workspace.loupe.adjustTitle"), "D")}
                  >
                    <SlidersHorizontal size={13} />
                    <span className="optional-label">{t("studio.workspace.loupe.adjust")}</span>
                  </button>
                </div>
              </div>
              {viewer}
            </>
          ) : (
            <>
              <LibraryToolbarPlacement header={sessionBar.library} compact={compactLibraryHeader}>
                {libraryToolbar}
              </LibraryToolbarPlacement>
              {metadataOpen && <StudioMetadataFilter />}
              <StudioBrowser
                {...browserProps}
                // A double-click opens the photograph in Develop (user
                // decision 2026-10-25); E or Space the loupe, to look through.
                onOpen={onOpen}
                size={size}
                onClearSelection={clearLibrarySelection}
                listing={JSON.stringify([currentFolderPath, activeAlbumId, searchCriteria, filterCriteria, sortCriteria])}
              />
              {panels.bars && (
              <div className="studio-library-footer">
                <span>
                  {t("studio.workspace.footer.photographs", {
                    count: files.length,
                    n: files.length.toLocaleString(),
                  })}
                  <OfflineCount files={files} />
                  {multiSelectedPaths.length > 0 &&
                    ` · ${t("studio.workspace.footer.selected", {
                      count: multiSelectedPaths.length,
                      n: multiSelectedPaths.length.toLocaleString(),
                    })}`}
                </span>
                {multiSelectedPaths.length > 0 && flagButtons()}
                <div className="studio-spacer" />
                <label title={t("studio.workspace.grid.thumbnailSize")}>
                  <Grid2X2 size={12} />
                  <input
                    type="range"
                    aria-label={t("studio.workspace.grid.thumbnailSize")}
                    min="150"
                    max="320"
                    value={size}
                    onChange={(e) => setSize(Number(e.target.value))}
                  />
                </label>
              </div>
              )}
            </>
          )}
        </main>
        {present.includes("right") && !panels.right && (
          <EdgeStrip
            edge="right"
            label={t("studio.workspace.edges.showRight")}
            hint={hint("toggle_right_panel")}
            onShow={() => {
              endPeek();
              showPanels(["right"]);
            }}
            peekProps={edgeProps("right")}
          />
        )}
      </div>
      {!panels.bars && (
        <EdgeStrip
          edge="bottom"
          // The top bar and the status bar come back together (`bars`).
          label={
            develop
              ? t("studio.workspace.edges.showBarsAndFilmstrip")
              : t("studio.workspace.edges.showBars")
          }
          hint={hint("toggle_all_panels")}
          onShow={() => showPanels(develop ? ["bars", "filmstrip"] : ["bars"])}
        />
      )}
      <dialog
        ref={dialog}
        className="studio-tool-dialog"
        aria-labelledby="studio-export-title"
        onCancel={(e) => {
          e.preventDefault();
          closeTools();
        }}
        onClick={(e) => {
          if (e.target === e.currentTarget) closeTools();
        }}
      >
        <header>
          <strong id="studio-export-title">{drawer ? t("studio.workspace.export.title") : ""}</strong>
          <button aria-label={t("studio.workspace.export.close")} onClick={closeTools}>
            <X size={17} />
          </button>
        </header>
        <div className="studio-tool-dialog-content">
          <ErrorBoundary key={drawer ?? ""} name={t("studio.workspace.export.title")}>
            {drawer === Panel.Export && <StudioExport onStarted={closeTools} />}
          </ErrorBoundary>
        </div>
      </dialog>
      {queueOpen && (
        <ErrorBoundary name={t("studio.export.queue.title")}>
          <ExportQueuePanel onClose={() => setQueueOpen(false)} />
        </ErrorBoundary>
      )}
      {isSettingsOpen && <PreferencesDialog>{props.settings}</PreferencesDialog>}
    </div>
  );
}
