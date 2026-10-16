import { useCallback, useEffect, useRef } from 'react';
import { toast } from 'react-toastify';
import { BrushSettings, ImageFile, Panel } from '../components/ui/AppProperties';
import { buildComboMap, normalizeCombo } from '../utils/keyboardUtils';
import { toggleWindowFullScreen } from '../utils/photoFullScreen';
import { isMaskOverlayShown, maskDisplayOf, toggleGrayscaleMask, toggleMaskDisplay } from '../utils/maskDisplay';
import { Mask } from '../components/panel/right/Masks';
import { DEFAULT_BRUSH_SIZE, stepBrushSize } from '../utils/brushSize';
import { deleteMaskSelection } from '../utils/maskUtils';
import { useEditorStore } from '../store/useEditorStore';
import { useLibraryStore } from '../store/useLibraryStore';
import { useSettingsStore } from '../store/useSettingsStore';
import { isAnyModalOpen, useUIStore } from '../store/useUIStore';
import { useProcessStore } from '../store/useProcessStore';
import { endBeforePeek, startBeforePeek, updateAdjustments, useEditorActions } from './useEditorActions';
import { useLibraryActions } from './useLibraryActions';
import { libraryTool, ownsPanelShortcut, revealLibraryTool } from '../components/studio/workspaceInteractions';
import { useMultiViewStore, type CellZoomKey } from '../store/useMultiViewStore';
import { orderedSelection, stepPrimary } from '../utils/multiView';

interface KeyboardShortcutsProps {
  sortedImageList: Array<ImageFile>;
  handleBackToLibrary(): void;
  handleDeleteSelected(): void;
  /** Undoes the last rename, move or delete (Library). */
  handleUndoFileOperation?(): void;
  /** Redoes the file operation undone last (Library). */
  handleRedoFileOperation?(): void;
  /** Moves the selection into the session's Selects folder. */
  handleMoveToSelects?(): void;
  handleImageSelect(path: string, openInEditor?: boolean): void;
  handlePasteFiles(str: string): void;
  handleZoomChange(zoomValue: number, fitToWindow?: boolean): void;
}

interface ShortcutState {
  editor: ReturnType<typeof useEditorStore.getState>;
  library: ReturnType<typeof useLibraryStore.getState>;
  ui: ReturnType<typeof useUIStore.getState>;
  settings: ReturnType<typeof useSettingsStore.getState>;
  process: ReturnType<typeof useProcessStore.getState>;
}

interface ShortcutAction {
  shouldFire?(s: ShortcutState): boolean | undefined;
  execute(e: KeyboardEvent, s: ShortcutState): void;
}

export const useKeyboardShortcuts = ({
  sortedImageList,
  handleBackToLibrary,
  handleDeleteSelected,
  handleUndoFileOperation,
  handleRedoFileOperation,
  handleMoveToSelects,
  handleImageSelect,
  handlePasteFiles,
  handleZoomChange,
}: KeyboardShortcutsProps) => {
  const {
    handleRotate,
    handleCopyAdjustments,
    handlePasteAdjustments,
    toggleSplitView,
    handleAutoAdjustments,
    handleAutoWhiteBalance,
  } = useEditorActions();
  const { handleRate, handleSetColorLabel } = useLibraryActions();

  // The crop as it was when the crop tool opened: Escape restores it and
  // leaves the tool (Lightroom), Enter keeps the change.
  const cropEntry = useRef<{ path: string; values: Record<string, unknown> } | null>(null);
  useEffect(() => {
    const take = (panel: Panel | null) => {
      const { adjustments, selectedImage } = useEditorStore.getState();
      cropEntry.current =
        panel === Panel.Crop && selectedImage
          ? {
              path: selectedImage.path,
              values: {
                crop: adjustments.crop,
                aspectRatio: adjustments.aspectRatio,
                rotation: adjustments.rotation,
                orientationSteps: adjustments.orientationSteps,
              },
            }
          : null;
    };
    take(useUIStore.getState().activePanel);
    return useUIStore.subscribe((state, previous) => {
      if (state.activePanel !== previous.activePanel) take(state.activePanel);
    });
  }, []);

  // The key holding the before peek (show_original), released on its keyup
  // or when the window loses the focus, so the peek can never stick.
  const peekKeyRef = useRef<string | null>(null);
  useEffect(() => {
    const release = () => {
      if (peekKeyRef.current === null) return;
      peekKeyRef.current = null;
      endBeforePeek();
    };
    const onKeyUp = (event: KeyboardEvent) => {
      if (event.code === peekKeyRef.current) release();
    };
    const onVisibility = () => {
      if (document.hidden) release();
    };
    window.addEventListener('keyup', onKeyUp);
    window.addEventListener('blur', release);
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      window.removeEventListener('keyup', onKeyUp);
      window.removeEventListener('blur', release);
      document.removeEventListener('visibilitychange', onVisibility);
      release();
    };
  }, []);

  const sortedListRef = useRef(sortedImageList);
  useEffect(() => {
    sortedListRef.current = sortedImageList;
  }, [sortedImageList]);

  const handleCopyImagePaths = useCallback(async (paths: Array<string>) => {
    const physicalPaths = [...new Set(paths.map((path) => path.split('?vc=')[0]))];
    if (physicalPaths.length === 0) {
      return;
    }
    try {
      await navigator.clipboard.writeText(physicalPaths.join('\n'));
    } catch (err) {
      console.error('Failed to copy image path to clipboard', err);
      toast.error(`Failed to copy path: ${err}`);
    }
  }, []);

  useEffect(() => {
    const getStoreState = (): ShortcutState => ({
      editor: useEditorStore.getState(),
      library: useLibraryStore.getState(),
      ui: useUIStore.getState(),
      settings: useSettingsStore.getState(),
      process: useProcessStore.getState(),
    });


    const getImagePathsForCopy = (s: ShortcutState): Array<string> => {
      if (s.editor.selectedImage) {
        return [s.editor.selectedImage.path];
      }
      const { libraryActivePath, multiSelectedPaths } = s.library;
      if (multiSelectedPaths.length > 0) {
        const listOrder = new Map(sortedListRef.current.map((image: ImageFile, index: number) => [image.path, index]));
        return [...multiSelectedPaths].sort(
          (a: string, b: string) =>
            (listOrder.get(a) ?? Number.MAX_SAFE_INTEGER) - (listOrder.get(b) ?? Number.MAX_SAFE_INTEGER),
        );
      }
      return libraryActivePath ? [libraryActivePath] : [];
    };

    // Multi view: the arrows step the primary through the selection (wrapping)
    // instead of through the folder, and rating keys rate the primary only.
    const multiViewActive = (s: ShortcutState) =>
      s.ui.activeView === 'editor' && !!s.editor.selectedImage && useMultiViewStore.getState().active;
    const stepMultiView = (s: ShortcutState, step: number) => {
      const selection = orderedSelection(
        sortedListRef.current.map((image) => image.path),
        s.library.multiSelectedPaths,
      );
      const next = stepPrimary(selection, s.editor.selectedImage?.path ?? null, step);
      if (next && next !== s.editor.selectedImage?.path) handleImageSelect(next, true);
    };
    const ratedPaths = (s: ShortcutState) =>
      multiViewActive(s) && s.editor.selectedImage ? [s.editor.selectedImage.path] : undefined;
    // In the multi view the zoom keys zoom the primary in its cell; with the
    // zoom lock every other cell follows (MultiViewStage). Without the lock
    // they zoom the cell under the pointer.
    const zoomAction = (kind: CellZoomKey, zoom: (s: ShortcutState) => void): ShortcutAction => ({
      shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
      execute: (e, s) => {
        e.preventDefault();
        if (useMultiViewStore.getState().active && useMultiViewStore.getState().zoomHoveredCell?.(kind)) return;
        zoom(s);
      },
    });

    const actions: Record<string, ShortcutAction> = {
      open_image: {
        shouldFire: (s) => s.ui.activeView === 'library' && s.library.libraryActivePath !== null,
        execute: (e, s) => {
          e.preventDefault();
          handleImageSelect(s.library.libraryActivePath!, true);
        },
      },
      copy_adjustments: {
        shouldFire: () => true,
        execute: (e) => {
          e.preventDefault();
          handleCopyAdjustments();
        },
      },
      paste_adjustments: {
        shouldFire: () => true,
        execute: (e) => {
          e.preventDefault();
          handlePasteAdjustments();
        },
      },
      copy_image_path: {
        shouldFire: (s) => getImagePathsForCopy(s).length > 0,
        execute: (e, s) => {
          e.preventDefault();
          handleCopyImagePaths(getImagePathsForCopy(s));
        },
      },
      move_to_selects: {
        shouldFire: (s) => !!handleMoveToSelects && !!s.library.sessionDirectory,
        execute: (e) => {
          e.preventDefault();
          handleMoveToSelects?.();
        },
      },
      copy_files: {
        shouldFire: (s) => s.library.multiSelectedPaths.length > 0,
        execute: (e, s) => {
          e.preventDefault();
          s.process.setProcess({ copiedFilePaths: s.library.multiSelectedPaths });
        },
      },
      paste_files: {
        shouldFire: () => true,
        execute: (e) => {
          e.preventDefault();
          handlePasteFiles('copy');
        },
      },
      select_all: {
        shouldFire: () => sortedListRef.current.length > 0,
        execute: (e, s) => {
          e.preventDefault();
          s.library.setLibrary({ multiSelectedPaths: sortedListRef.current.map((f: ImageFile) => f.path) });
          if (s.ui.activeView === 'library') {
            const lastPath = sortedListRef.current[sortedListRef.current.length - 1].path;
            s.library.setLibrary({ libraryActivePath: lastPath });
            handleImageSelect(lastPath, false);
          }
        },
      },
      delete_selected: {
        shouldFire: (s) => !s.editor.activeMaskContainerId,
        execute: (e) => {
          e.preventDefault();
          handleDeleteSelected();
        },
      },
      preview_prev: {
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e, s) => {
          e.preventDefault();
          if (multiViewActive(s)) {
            stepMultiView(s, -1);
            return;
          }
          const currentIndex = sortedListRef.current.findIndex((img) => img.path === s.editor.selectedImage!.path);
          if (currentIndex === -1) return;
          const nextIndex = currentIndex - 1 < 0 ? sortedListRef.current.length - 1 : currentIndex - 1;
          handleImageSelect(sortedListRef.current[nextIndex].path, true);
        },
      },
      preview_next: {
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e, s) => {
          e.preventDefault();
          if (multiViewActive(s)) {
            stepMultiView(s, 1);
            return;
          }
          const currentIndex = sortedListRef.current.findIndex((img) => img.path === s.editor.selectedImage!.path);
          if (currentIndex === -1) return;
          const nextIndex = currentIndex + 1 >= sortedListRef.current.length ? 0 : currentIndex + 1;
          handleImageSelect(sortedListRef.current[nextIndex].path, true);
        },
      },
      zoom_in_step: zoomAction('stepIn', (s) => {
        const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
        const currentPercent =
          s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
            ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
            : 1.0;
        handleZoomChange(Math.min(currentPercent + 0.1, 2.0));
      }),
      zoom_out_step: zoomAction('stepOut', (s) => {
        const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
        const currentPercent =
          s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
            ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
            : 1.0;
        handleZoomChange(Math.max(currentPercent - 0.1, 0.1));
      }),
      cycle_zoom: zoomAction('cycle', (s) => {
        const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
        const { originalSize, displaySize, baseRenderSize } = s.editor;
        const currentPercent =
          originalSize?.width > 0 && displaySize?.width > 0
            ? Math.round(((displaySize.width * dpr) / originalSize.width) * 100)
            : 100;
        let fitPercent = 100;

        if (originalSize?.width > 0 && baseRenderSize?.width > 0) {
          const originalAspect = originalSize.width / originalSize.height;
          const baseAspect = baseRenderSize.width / baseRenderSize.height;
          fitPercent =
            originalAspect > baseAspect
              ? Math.round(((baseRenderSize.width * dpr) / originalSize.width) * 100)
              : Math.round(((baseRenderSize.height * dpr) / originalSize.height) * 100);
        }

        const doubleFitPercent = fitPercent * 2;
        if (Math.abs(currentPercent - fitPercent) < 5) {
          handleZoomChange(doubleFitPercent < 100 ? doubleFitPercent / 100 : 1.0);
        } else if (Math.abs(currentPercent - doubleFitPercent) < 5 && doubleFitPercent < 100) {
          handleZoomChange(1.0);
        } else {
          handleZoomChange(0, true);
        }
      }),
      zoom_in: zoomAction('in', (s) => {
        const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
        const currentPercent =
          s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
            ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
            : 1.0;
        handleZoomChange(Math.min(currentPercent * 1.2, 2.0));
      }),
      zoom_out: zoomAction('out', (s) => {
        const dpr = typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1;
        const currentPercent =
          s.editor.originalSize?.width > 0 && s.editor.displaySize?.width > 0
            ? (s.editor.displaySize.width * dpr) / s.editor.originalSize.width
            : 1.0;
        handleZoomChange(Math.max(currentPercent / 1.2, 0.1));
      }),
      zoom_fit: zoomAction('fit', () => handleZoomChange(0, true)),
      zoom_100: zoomAction('actual', () => handleZoomChange(1.0)),
      toggle_zoom_lock: {
        shouldFire: () => useMultiViewStore.getState().active,
        execute: (e) => {
          e.preventDefault();
          const multi = useMultiViewStore.getState();
          multi.setZoomLock(!multi.zoomLock);
        },
      },
      toggle_multi_view: {
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e, s) => {
          e.preventDefault();
          const { osPlatform, appSettings } = s.settings;
          const multi = useMultiViewStore.getState();
          if (osPlatform !== 'linux' && (appSettings?.useWgpuRenderer === false || multi.unavailable)) return;
          multi.toggle();
        },
      },
      pin_reference: {
        // Capture One's ⇧Return: the primary becomes the reference, shown
        // first and kept while the primary steps on through the rest.
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e, s) => {
          e.preventDefault();
          const primary = s.editor.selectedImage!.path;
          const selection = orderedSelection(
            sortedListRef.current.map((image) => image.path),
            s.library.multiSelectedPaths,
          );
          const next = stepPrimary(selection, primary, 1);
          useMultiViewStore.getState().setReference(primary);
          if (!next || next === primary) return;
          s.library.setLibrary({ multiSelectedPaths: selection.filter((path) => path !== primary) });
          handleImageSelect(next, true);
        },
      },
      clear_reference: {
        shouldFire: () => useMultiViewStore.getState().reference !== null,
        execute: (e) => {
          e.preventDefault();
          useMultiViewStore.getState().setReference(null);
        },
      },
      rotate_left: {
        shouldFire: (s) => !!s.editor.selectedImage || !!s.library.libraryActivePath,
        execute: (e) => {
          e.preventDefault();
          handleRotate(-90);
        },
      },
      rotate_right: {
        shouldFire: (s) => !!s.editor.selectedImage || !!s.library.libraryActivePath,
        execute: (e) => {
          e.preventDefault();
          handleRotate(90);
        },
      },
      undo: {
        // Develop: edit history. Library: the last rename, move or delete.
        shouldFire: (s) =>
          s.ui.activeView === 'editor'
            ? s.editor.selectedImage?.isReady && (s.editor.historyPending || s.editor.historyIndex > 0)
            : !!handleUndoFileOperation,
        execute: (e, s) => {
          e.preventDefault();
          if (s.ui.activeView === 'editor') s.editor.undo();
          else handleUndoFileOperation?.();
        },
      },
      redo: {
        // Develop: edit history. Library: the file operation undone last.
        shouldFire: (s) =>
          s.ui.activeView === 'editor'
            ? s.editor.selectedImage?.isReady &&
              !s.editor.historyPending &&
              s.editor.historyIndex < s.editor.history.length - 1
            : !!handleRedoFileOperation,
        execute: (e, s) => {
          e.preventDefault();
          if (s.ui.activeView === 'editor') s.editor.redo();
          else handleRedoFileOperation?.();
        },
      },
      auto_adjust: {
        // Background only, like the Auto button.
        shouldFire: (s) =>
          s.ui.activeView === 'editor' &&
          !!s.editor.selectedImage?.isReady &&
          !(s.ui.activePanel === Panel.Masks && s.editor.activeMaskContainerId),
        execute: (e) => {
          e.preventDefault();
          handleAutoAdjustments();
        },
      },
      auto_white_balance: {
        // Background only, like the Auto button.
        shouldFire: (s) =>
          s.ui.activeView === 'editor' &&
          !!s.editor.selectedImage?.isReady &&
          !(s.ui.activePanel === Panel.Masks && s.editor.activeMaskContainerId),
        execute: (e) => {
          e.preventDefault();
          void handleAutoWhiteBalance();
        },
      },
      toggle_fullscreen: {
        // The window's full screen with the interface as it is (Capture One).
        shouldFire: () => true,
        execute: (e) => {
          e.preventDefault();
          void toggleWindowFullScreen();
        },
      },
      show_original: {
        // Held, not toggled: the key's release (or the window losing the
        // focus) ends the peek. Auto-repeat keeps it as it is.
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e) => {
          e.preventDefault();
          if (e.repeat || peekKeyRef.current) return;
          if (startBeforePeek()) peekKeyRef.current = e.code;
        },
      },
      toggle_split_view: {
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e) => {
          e.preventDefault();
          toggleSplitView();
        },
      },
      toggle_clipping: {
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e, s) => {
          e.preventDefault();
          s.editor.setEditor({ clippingWarnings: !s.editor.clippingWarnings });
        },
      },
      toggle_proof_margin: {
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage && !!s.settings.appSettings,
        execute: (e, s) => {
          e.preventDefault();
          const appSettings = s.settings.appSettings!;
          void s.settings.handleSettingsChange({ ...appSettings, viewerProofMargin: !appSettings.viewerProofMargin });
        },
      },
      toggle_soft_proof: {
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e, s) => {
          e.preventDefault();
          s.editor.setEditor({ softProof: !s.editor.softProof });
        },
      },
      toggle_mask_overlay: {
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e, s) => {
          e.preventDefault();
          const settings = s.settings.appSettings;
          // The layer's first adjustment switched the overlay off (Lightroom,
          // notes: reference/layers-panel-c1-lr.md): M shows it again, and an
          // overlay shown by M stays on until the next mask is made or painted.
          const autoHidden = s.editor.maskOverlayAuto === 'hidden';
          // With the select tool (a layer selected, no mask tool) M shows or
          // hides that layer's mask; mask tools keep the display setting's M.
          if (s.ui.activePanel === Panel.Masks && s.editor.activeMaskContainerId && !s.editor.activeMaskId) {
            const show = autoHidden || !s.editor.showMaskWithSelectTool;
            s.editor.setEditor(show ? { showMaskWithSelectTool: true, maskOverlayAuto: 'pinned' } : { showMaskWithSelectTool: false });
            if (show && settings && maskDisplayOf(settings) === 'never') {
              s.settings.handleSettingsChange({ ...settings, ...toggleMaskDisplay(settings) });
            }
            return;
          }
          if (autoHidden && isMaskOverlayShown(maskDisplayOf(settings), false)) {
            s.editor.setEditor({ maskOverlayAuto: 'pinned' });
            return;
          }
          if (!settings) return;
          const next = toggleMaskDisplay(settings);
          s.settings.handleSettingsChange({ ...settings, ...next });
          if (next.maskDisplay !== 'never') s.editor.setEditor({ maskOverlayAuto: 'pinned' });
        },
      },
      toggle_mask_grayscale: {
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e, s) => {
          e.preventDefault();
          const settings = s.settings.appSettings;
          if (!settings) return;
          // Choosing a display mode shows the overlay, also after the
          // automatic switch-off.
          s.settings.handleSettingsChange({ ...settings, ...toggleGrayscaleMask(settings) });
          s.editor.setEditor({ maskOverlayAuto: 'pinned' });
        },
      },
      toggle_adjustments: {
        shouldFire: () => true,
        execute: (e, s) => {
          e.preventDefault();
          s.ui.setPanel(Panel.Adjustments);
        },
      },
      toggle_masks: {
        shouldFire: () => true,
        execute: (e, s) => {
          e.preventDefault();
          s.ui.setPanel(Panel.Masks);
        },
      },
      // The library column's Metadata and Folders tools, where it is shown
      // (Library, or Develop with the tools split).
      toggle_metadata: {
        shouldFire: () => !!libraryTool('Metadata'),
        execute: (e) => {
          e.preventDefault();
          revealLibraryTool('Metadata');
        },
      },
      toggle_folder_tree: {
        shouldFire: () => !!libraryTool('Folders'),
        execute: (e) => {
          e.preventDefault();
          revealLibraryTool('Folders');
        },
      },
      toggle_analytics: {
        shouldFire: (s) => !!s.editor.selectedImage,
        execute: (e, s) => {
          e.preventDefault();
          // With the brush on a layer, A toggles Auto Mask (Lightroom's key).
          const layer =
            s.ui.activePanel === Panel.Masks
              ? s.editor.adjustments.masks.find((m) => m.id === s.editor.activeMaskContainerId)
              : undefined;
          const component = layer?.subMasks.find((m) => m.id === s.editor.activeMaskId);
          if (component?.type === Mask.Brush) {
            const brush = s.editor.brushSettings || ({ size: DEFAULT_BRUSH_SIZE } as BrushSettings);
            s.editor.setEditor({ brushSettings: { ...brush, autoMask: !brush.autoMask } });
            return;
          }
          const nextVisibility = !s.editor.isWaveformVisible;
          s.editor.setEditor({ isWaveformVisible: nextVisibility });
          // Treated as loaded settings; a null value is spread unchanged, as before.
          s.settings.handleSettingsChange({
            ...s.settings.appSettings!,
            isWaveformVisible: nextVisibility,
          });
        },
      },
      toggle_export: {
        shouldFire: () => true,
        execute: (e, s) => {
          e.preventDefault();
          s.ui.setPanel(Panel.Export);
        },
      },
      toggle_side_panels: {
        execute: (e, s) => {
          e.preventDefault();
          s.ui.togglePanels('sides');
        },
      },
      toggle_all_panels: {
        execute: (e, s) => {
          e.preventDefault();
          s.ui.togglePanels('all');
        },
      },
      toggle_left_panel: {
        execute: (e, s) => {
          e.preventDefault();
          s.ui.togglePanels('left');
        },
      },
      toggle_right_panel: {
        execute: (e, s) => {
          e.preventDefault();
          s.ui.togglePanels('right');
        },
      },
      toggle_bottom_panel: {
        shouldFire: (s) => s.ui.activeView !== 'library',
        execute: (e, s) => {
          e.preventDefault();
          s.ui.togglePanels('filmstrip');
        },
      },
      open_settings: {
        shouldFire: () => true,
        execute: (e, s) => {
          e.preventDefault();
          s.ui.setUI({ isSettingsOpen: true });
        },
      },
      focus_search: {
        shouldFire: (s) => s.ui.activeView === 'library',
        execute: (e, s) => {
          e.preventDefault();
          s.ui.requestSearchFocus();
        },
      },
      toggle_crop: {
        shouldFire: (s) => s.ui.activeView === 'editor' && !!s.editor.selectedImage,
        execute: (e, s) => {
          e.preventDefault();
          if (s.ui.activePanel === Panel.Crop) {
            s.editor.setEditor({ isStraightenActive: !s.editor.isStraightenActive });
          } else {
            s.ui.setPanel(Panel.Crop);
            s.editor.setEditor({ isStraightenActive: true });
          }
        },
      },
      rate_0: {
        shouldFire: () => true,
        execute: (e, s) => {
          e.preventDefault();
          handleRate(0, ratedPaths(s));
        },
      },
      rate_1: {
        shouldFire: () => true,
        execute: (e, s) => {
          e.preventDefault();
          handleRate(1, ratedPaths(s));
        },
      },
      rate_2: {
        shouldFire: () => true,
        execute: (e, s) => {
          e.preventDefault();
          handleRate(2, ratedPaths(s));
        },
      },
      rate_3: {
        shouldFire: () => true,
        execute: (e, s) => {
          e.preventDefault();
          handleRate(3, ratedPaths(s));
        },
      },
      rate_4: {
        shouldFire: () => true,
        execute: (e, s) => {
          e.preventDefault();
          handleRate(4, ratedPaths(s));
        },
      },
      rate_5: {
        shouldFire: () => true,
        execute: (e, s) => {
          e.preventDefault();
          handleRate(5, ratedPaths(s));
        },
      },
      color_label_none: {
        shouldFire: () => true,
        execute: (e) => {
          e.preventDefault();
          handleSetColorLabel(null);
        },
      },
      color_label_red: {
        shouldFire: () => true,
        execute: (e) => {
          e.preventDefault();
          handleSetColorLabel('red');
        },
      },
      color_label_yellow: {
        shouldFire: () => true,
        execute: (e) => {
          e.preventDefault();
          handleSetColorLabel('yellow');
        },
      },
      color_label_green: {
        shouldFire: () => true,
        execute: (e) => {
          e.preventDefault();
          handleSetColorLabel('green');
        },
      },
      color_label_blue: {
        shouldFire: () => true,
        execute: (e) => {
          e.preventDefault();
          handleSetColorLabel('blue');
        },
      },
      color_label_purple: {
        shouldFire: () => true,
        execute: (e) => {
          e.preventDefault();
          handleSetColorLabel('purple');
        },
      },
      brush_size_up: {
        shouldFire: (s) =>
          s.ui.activeView === 'editor' &&
          !!s.editor.selectedImage &&
          s.ui.activePanel === Panel.Masks,
        execute: (e, s) => {
          e.preventDefault();
          // Without brush settings yet, only a size is stored (pre-existing behaviour).
          const currentSettings = s.editor.brushSettings || ({ size: DEFAULT_BRUSH_SIZE } as BrushSettings);
          const newSize = stepBrushSize(currentSettings.size || DEFAULT_BRUSH_SIZE, 1);
          s.editor.setEditor({
            brushSettings: { ...currentSettings, size: newSize },
          });
        },
      },
      brush_size_down: {
        shouldFire: (s) =>
          s.ui.activeView === 'editor' &&
          !!s.editor.selectedImage &&
          s.ui.activePanel === Panel.Masks,
        execute: (e, s) => {
          e.preventDefault();
          // Without brush settings yet, only a size is stored (pre-existing behaviour).
          const currentSettings = s.editor.brushSettings || ({ size: DEFAULT_BRUSH_SIZE } as BrushSettings);
          const newSize = stepBrushSize(currentSettings.size || DEFAULT_BRUSH_SIZE, -1);
          s.editor.setEditor({
            brushSettings: { ...currentSettings, size: newSize },
          });
        },
      },
    };

    const builtinShortcuts = [
      {
        // Capture One's Cmd+Left / Cmd+Right step the primary in its multi view too.
        match: (e: KeyboardEvent, s: ShortcutState) =>
          (e.code === 'ArrowLeft' || e.code === 'ArrowRight') &&
          (s.settings.osPlatform === 'macos' ? e.metaKey : e.ctrlKey) &&
          !e.altKey &&
          !e.shiftKey &&
          multiViewActive(s),
        execute: (e: KeyboardEvent, s: ShortcutState) => {
          e.preventDefault();
          stepMultiView(s, e.code === 'ArrowRight' ? 1 : -1);
        },
      },
      {
        match: (e: KeyboardEvent) => e.code === 'Escape',
        execute: (e: KeyboardEvent, s: ShortcutState) => {
          e.preventDefault();
          if (s.editor.isStraightenActive) s.editor.setEditor({ isStraightenActive: false });
          else if (s.editor.colorPickerTarget !== null) s.editor.setEditor({ colorPickerTarget: null });
          else if (s.editor.isWbPickerActive) s.editor.setEditor({ isWbPickerActive: false });
          else if (s.editor.lumaPickerLayer) s.editor.setEditor({ lumaPickerLayer: null });
          else if (s.editor.tatTarget) s.editor.setEditor({ tatTarget: null });
          else if (s.ui.customEscapeHandler) s.ui.customEscapeHandler();
          else if (s.editor.activeMaskId) s.editor.setEditor({ activeMaskId: null });
          else if (s.editor.activeMaskContainerId) s.editor.setEditor({ activeMaskContainerId: null });
          else if (s.ui.activePanel === Panel.Crop) {
            const entry = cropEntry.current;
            if (entry && entry.path === s.editor.selectedImage?.path) {
              const now = s.editor.adjustments as unknown as Record<string, unknown>;
              const changed = Object.entries(entry.values).some(
                ([key, value]) => JSON.stringify(now[key]) !== JSON.stringify(value),
              );
              if (changed) updateAdjustments((prev) => ({ ...prev, ...entry.values }));
            }
            s.ui.setPanel(Panel.Adjustments);
          }
          else if (s.ui.isFullScreen) s.ui.toggleFullScreen();
          else if (s.ui.activeView === 'editor') handleBackToLibrary();
          // In the Library, Escape does nothing further (Lightroom, Capture One):
          // RapidRAW's step back to its start screen emptied the session's view.
        },
      },
      {
        // Enter applies the crop and leaves the crop tool, as in Lightroom and
        // Capture One (the crop itself is already live), also when a crop
        // button kept the focus after a click (Space still presses it).
        match: (e: KeyboardEvent, s: ShortcutState) =>
          (e.code === 'Enter' || e.code === 'NumpadEnter') &&
          !e.metaKey &&
          !e.ctrlKey &&
          !e.altKey &&
          s.ui.activePanel === Panel.Crop &&
          !s.editor.isStraightenActive &&
          !document.activeElement?.closest('select, [contenteditable="true"]'),
        execute: (e: KeyboardEvent, s: ShortcutState) => {
          e.preventDefault();
          s.ui.setPanel(Panel.Adjustments);
        },
      },
      {
        match: (e: KeyboardEvent, s: ShortcutState) => {
          const isDeleteKey = s.settings.osPlatform === 'macos' ? e.code === 'Backspace' : e.code === 'Delete';
          return isDeleteKey && s.editor.selectedImage?.isReady && !!s.editor.activeMaskContainerId;
        },
        execute: (e: KeyboardEvent, s: ShortcutState) => {
          e.preventDefault();
          // The component picked in the list goes (Lightroom), else the layer.
          const { activeMaskContainerId, activeMaskId, focusedMaskComponentId } = s.editor;
          const picked = focusedMaskComponentId === activeMaskId ? focusedMaskComponentId : null;
          s.editor.setEditor((state) => {
            const { masks, removed } = deleteMaskSelection(state.adjustments.masks, activeMaskContainerId!, picked);
            return {
              adjustments: { ...state.adjustments, masks },
              activeMaskContainerId: removed === 'component' ? activeMaskContainerId : null,
              activeMaskId: null,
              focusedMaskComponentId: null,
            };
          });
        },
      },
      {
        match: (e: KeyboardEvent, s: ShortcutState) =>
          s.ui.activeView === 'library' && ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'].includes(e.code),
        execute: (e: KeyboardEvent, s: ShortcutState) => {
          e.preventDefault();
          const isNext = e.code === 'ArrowRight' || e.code === 'ArrowDown';
          const activePath = s.library.libraryActivePath;
          if (!activePath || sortedListRef.current.length === 0) return;
          const currentIndex = sortedListRef.current.findIndex((img) => img.path === activePath);
          if (currentIndex === -1) return;
          let nextIndex = isNext ? currentIndex + 1 : currentIndex - 1;
          if (nextIndex >= sortedListRef.current.length) nextIndex = 0;
          if (nextIndex < 0) nextIndex = sortedListRef.current.length - 1;
          const nextImage = sortedListRef.current[nextIndex];
          if (nextImage) {
            s.library.setLibrary({ libraryActivePath: nextImage.path, multiSelectedPaths: [nextImage.path] });
            handleImageSelect(nextImage.path, false);
          }
        },
      },
    ];

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      const state = getStoreState();
      // Preferences is an aria-modal overlay: Escape closes it (unless a dialog
      // opened over it), and no other shortcut reaches the app behind it.
      if (state.ui.isSettingsOpen) {
        if (event.code === 'Escape' && !document.querySelector('dialog[open]')) {
          event.preventDefault();
          state.ui.setUI({ isSettingsOpen: false });
        }
        return;
      }
      if (document.querySelector('dialog[open], [aria-modal="true"]')) return;

      if (isAnyModalOpen(state.ui)) return;

      const isInputFocused =
        document.activeElement?.tagName === 'INPUT' || document.activeElement?.tagName === 'TEXTAREA';
      const rangeFocused = document.activeElement instanceof HTMLInputElement && document.activeElement.type === 'range';
      // Range controls have no text undo stack. Keep their arrow keys native,
      // but let Undo/Redo reach edit history immediately after a slider drag.
      if (isInputFocused && !rangeFocused) return;
      // Tab moves the focus between fields and controls; it hides panels only
      // from the photograph, the browser tiles or with nothing focused.
      if (event.code === 'Tab' && !ownsPanelShortcut(document.activeElement)) return;

      for (const builtin of builtinShortcuts) {
        if (!rangeFocused && builtin.match(event, state)) {
          builtin.execute(event, state);
          return;
        }
      }

      // Read per key press so that edited keybinds apply at once. Cursor tool
      // keys (tool_*) are handled by the studio workspace.
      const comboMap = buildComboMap(state.settings.appSettings?.keybinds);
      const normalized = normalizeCombo(event, state.settings.osPlatform);
      for (const action of comboMap.get(normalized.join('+')) ?? []) {
        if (rangeFocused && action !== 'undo' && action !== 'redo') continue;
        const handler = actions[action];
        if (handler && (!handler.shouldFire || handler.shouldFire(state))) {
          handler.execute(event, state);
          return;
        }
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
    };
  }, [
    handleBackToLibrary,
    handleDeleteSelected,
    handleUndoFileOperation,
    handleRedoFileOperation,
    handleMoveToSelects,
    handleImageSelect,
    handlePasteFiles,
    handleZoomChange,
    handleRotate,
    handleCopyAdjustments,
    handleCopyImagePaths,
    handlePasteAdjustments,
    handleRate,
    handleSetColorLabel,
    toggleSplitView,
    handleAutoAdjustments,
    handleAutoWhiteBalance,
  ]);
};
