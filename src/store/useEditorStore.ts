import { create } from 'zustand';
import debounce from 'lodash.debounce';
import { DEFAULT_BRUSH_SIZE } from '../utils/brushSize';
import { Adjustments, INITIAL_ADJUSTMENTS } from '../utils/adjustments';
import { SelectedImage, WaveformData, BrushSettings, HistogramData } from '../components/ui/AppProperties';
import { ImageDimensions } from '../hooks/useImageRenderSize';
import { ToolType } from '../components/panel/right/Masks';
import type { OverlayMode } from '../components/panel/editor/overlays/CompositionOverlays';
import { maskOverlayAfterEdit, type MaskOverlayAuto, type NativeMaskOverlay } from '../utils/maskDisplay';
import type { OutputColorSpace } from '../components/ui/ExportImportProperties';
import type { LayerSolo } from '../utils/layerSolo';
import { DEFAULT_SPLIT_POSITION } from '../utils/beforeAfter';
import { historyOrders,
  setHistoryOrders,
  inheritHistoryOrders,
  newEditOrder } from '../utils/undoOrder';
import { noteEdit } from '../components/contribute/usageStats';

export interface InteractivePatch {
  url: string;
  normX: number;
  normY: number;
  normW: number;
  normH: number;
}

interface BaseRenderSize extends ImageDimensions {
  containerHeight: number;
  containerWidth: number;
  offsetX: number;
  offsetY: number;
}

interface EditorState {
  // Core Image & Adjustments
  selectedImage: SelectedImage | null;
  adjustments: Adjustments;
  previewOverride: Adjustments | null;

  // History State
  history: Adjustments[];
  historyIndex: number;
  historyPending: boolean;

  // Previews & Overlays
  finalPreviewUrl: string | null;
  uncroppedAdjustedPreviewUrl: string | null;
  interactivePatch: InteractivePatch | null;
  showOriginal: boolean;
  /** Before/after split view; it stays on from photograph to photograph. */
  splitView: boolean;
  /** The split's divider as a share (0–1) of the viewer's width; kept for the session. */
  splitPosition: number;
  /** The snapshot Before/After compares with (`utils/beforeSnapshot`); null: the unedited photograph. */
  beforeSnapshot: { path: string; id: string } | null;
  /** The snapshot (id) whose before the native split shows now; null: the unedited photograph. */
  splitBeforeShows: string | null;

  // Analytics
  histogram: HistogramData | null;
  waveform: WaveformData | null;
  isWaveformVisible: boolean;
  activeWaveformChannel: string;
  waveformHeight: number;

  // Interaction State
  isSliderDragging: boolean;
  /** The open RAW is being developed again for AI Denoise. */
  aiDenoiseBusy: boolean;
  zoom: number;
  displaySize: ImageDimensions;
  previewSize: ImageDimensions;
  baseRenderSize: BaseRenderSize;
  originalSize: ImageDimensions;

  // Tools State
  isRotationActive: boolean;
  overlayMode: OverlayMode;
  overlayRotation: number;
  isStraightenActive: boolean;
  isWbPickerActive: boolean;
  /** Targeted Adjustment Tool target, or null when the tool is off. */
  tatTarget: import('../utils/targetedAdjustment').TatTarget | null;
  /** The colour mixer channel the last targeted drag acted on most. */
  tatFocus: string | null;
  /** Spot removal tool: on/off, the selected spot and the settings new
   * spots get (and the selected one shows). */
  isSpotToolActive: boolean;
  activeSpotId: string | null;
  /** Lightroom's Remove tool modes: content-aware remove (an AI patch), heal, clone.
   * `detectObjects` (Remove only): a click or box selects an object with the
   * object mask model and removes it, instead of painting the area. */
  spotSettings: {
    /** `redEye`: red eye and pet eye corrections (`redEyeSettings`). */
    mode: 'remove' | 'heal' | 'clone' | 'redEye';
    size: number;
    feather: number;
    opacity: number;
    detectObjects?: boolean;
    /** Detect objects: brush loosely over the object (Lightroom's way) or
     * draw a box around it; a click selects in both. */
    objectSelect?: 'brush' | 'box';
  };
  /** Lightroom's Visualize Spots: the band-pass view under the spots. */
  spotVisualize: { on: boolean; sensitivity: number };
  /** Red eye mode of the spot tool: what new corrections get (and the
   * selected one shows). `size`: radius of the circle a click searches
   * (a drag sizes it), mask-frame pixels. */
  redEyeSettings: {
    kind: 'red' | 'pet';
    size: number;
    pupilSize: number;
    darken: number;
    catchlight: boolean;
  };
  /** The face retouch's blemishes on the photograph (Retouch tab). */
  showFaceBlemishes: boolean;
  /** Colour editor picker: 'new' adds a range, an id re-samples that range. */
  colorPickerTarget: string | null;
  /** The mask layer the picker edits, or null for the photo. */
  colorPickerLayer: string | null;
  /** The colour editor range shown in the tool. */
  activeColorRangeId: string | null;
  /** View only, never in the recipe: the colour range whose selection is
   * shown, and clipping warnings (Lightroom's J). */
  colorRangeShow: string | null;
  clippingWarnings: boolean;
  /** View only: one layer or component shown alone (utils/layerSolo). */
  layerSolo: LayerSolo | null;
  /** View only: soft proofing (Lightroom's Soft Proofing) shows the photo as
   * an export in `softProofSpace` would look; the warning marks colours
   * outside that space. */
  softProof: boolean;
  softProofSpace: OutputColorSpace;
  softProofWarning: boolean;
  isGuidedPerspectiveActive: boolean;
  liveRotation: number | null;
  /** The last Auto straighten answer applied (rotation and the crop object
   * fitted to it): the crop tool keeps that crop as it is. */
  autoStraightened: { rotation: number; crop: Adjustments['crop'] } | null;
  brushSettings: BrushSettings | null;

  // Masks & AI
  activeMaskContainerId: string | null;
  activeMaskId: string | null;
  /** The component picked in the layer's component list (or just added to
   * the layer): Delete removes it instead of the whole layer. Null after a
   * layer is selected. */
  focusedMaskComponentId: string | null;
  isMaskControlHovered: boolean;
  /** A slider that shapes the mask itself (luma range) is dragged: the mask
   * is shown instead of hidden as for adjustment sliders (Capture One). */
  isMaskRangeDragging: boolean;
  isGeneratingAiMask: boolean;
  hasRenderedFirstFrame: boolean;
  /** While the next photograph opens, its predecessor's native frame stays
   * on screen (no placeholder) until the first new frame or a timeout. */
  holdingNativeFrame: boolean;
  /** The native display shows this photograph's cached thumbnail (a still
   * preview, marked as such) until its first developed frame. */
  stillPreview: string | null;
  /** Stepping from one ready photograph to the next: the tool panels keep
   * their look (not dimmed, histogram kept) until the next photograph's
   * values arrive and replace them in place; they stay inert meanwhile.
   * See `panelsShowReady`. */
  panelsHeld: boolean;
  /** The native display shows the uncropped crop-editing frame. */
  nativeCropFrame: boolean;
  /** One tool's copied values (Capture One's per-tool copy/apply). */
  toolClipboard: { tool: string; source: string; values: Partial<Adjustments> } | null;
  /** The mask overlay the native display draws with each preview. */
  nativeMaskOverlay: NativeMaskOverlay | null;
  /** M with the select tool shows the selected layer's mask (mask tools show
   * it anyway). */
  showMaskWithSelectTool: boolean;
  /** Lightroom's automatic mask overlay (`MaskOverlayAuto`). */
  maskOverlayAuto: MaskOverlayAuto;
  /** The layer whose luma range the eyedropper sets from the photo, or null
   * when it is off (Lightroom's Luminance Range sampling). */
  lumaPickerLayer: string | null;
  /** A new linear or radial component waiting to be drawn: the first drag
   * on the photo draws it (Lightroom, Capture One); a click keeps its
   * default shape. Never part of the recipe. */
  shapeDrawTarget: string | null;
  patchesSentToBackend: Set<string>;

  // Clipboard
  /** The settings copied (only those chosen: `settingsClipboard`). */
  copiedAdjustments: Adjustments | null;
  /** The photograph they were copied from. */
  copiedFrom: string | null;
  /** The photograph open before this one (Lightroom's Previous). */
  previousPhoto: string | null;
  /** Its size as it was shown (drawn masks leave a photo with its frame). */
  previousPhotoSize: { width: number; height: number } | null;

  // Actions
  setEditor: (updater: Partial<EditorState> | ((state: EditorState) => Partial<EditorState>)) => void;
  pushHistory: (newAdjustments: Adjustments) => void;
  undo: () => void;
  redo: () => void;
  resetHistory: (initialState: Adjustments) => void;
  goToHistoryIndex: (index: number) => void;
}

/** Whether the tool panels look ready (controls not dimmed): the open
 * photograph is ready, or the next one is still loading after a ready one
 * (`panelsHeld`; the panels are inert then). Actions still check
 * `selectedImage.isReady`. */
export const panelsShowReady = (state: Pick<EditorState, 'selectedImage' | 'panelsHeld'>) =>
  Boolean(state.selectedImage && (state.selectedImage.isReady || state.panelsHeld));

/** The open photograph's unsaved recipe for an export or size estimate. While
 * the next photograph loads, `adjustments` still hold the previous one's, so
 * only a ready photograph names itself; the export then reads saved recipes. */
export const currentEditForExport = (state: Pick<EditorState, 'selectedImage' | 'adjustments'>) =>
  state.selectedImage?.isReady
    ? { currentEditPath: state.selectedImage.path, currentEditAdjustments: state.adjustments }
    : { currentEditPath: null, currentEditAdjustments: null };

export const useEditorStore = create<EditorState>((set, get) => ({
  selectedImage: null,
  adjustments: INITIAL_ADJUSTMENTS,
  previewOverride: null,
  history: [INITIAL_ADJUSTMENTS],
  historyIndex: 0,
  historyPending: false,

  finalPreviewUrl: null,
  uncroppedAdjustedPreviewUrl: null,
  showOriginal: false,
  splitView: false,
  splitPosition: DEFAULT_SPLIT_POSITION,
  beforeSnapshot: null,
  splitBeforeShows: null,
  histogram: null,
  waveform: null,
  isWaveformVisible: false,
  activeWaveformChannel: 'luma',
  waveformHeight: 220,

  isSliderDragging: false,
  aiDenoiseBusy: false,
  interactivePatch: null,
  activeMaskContainerId: null,
  activeMaskId: null,
  focusedMaskComponentId: null,

  zoom: 1,
  displaySize: { width: 0, height: 0 },
  previewSize: { width: 0, height: 0 },
  baseRenderSize: { width: 0, height: 0, offsetX: 0, offsetY: 0, containerWidth: 0, containerHeight: 0 },
  originalSize: { width: 0, height: 0 },

  isRotationActive: false,
  overlayMode: 'thirds',
  overlayRotation: 0,
  isStraightenActive: false,
  isWbPickerActive: false,
  tatTarget: null,
  tatFocus: null,
  isSpotToolActive: false,
  activeSpotId: null,
  spotSettings: { mode: 'heal', size: 60, feather: 0, opacity: 100 },
  spotVisualize: { on: false, sensitivity: 50 },
  redEyeSettings: { kind: 'red', size: 40, pupilSize: 50, darken: 50, catchlight: true },
  showFaceBlemishes: false,
  colorPickerTarget: null,
  colorPickerLayer: null,
  activeColorRangeId: null,
  colorRangeShow: null,
  layerSolo: null,
  clippingWarnings: false,
  softProof: false,
  softProofSpace: 'srgb',
  softProofWarning: false,
  isGuidedPerspectiveActive: false,
  liveRotation: null,
  autoStraightened: null,

  brushSettings: { size: DEFAULT_BRUSH_SIZE, feather: 50, tool: ToolType.Brush },
  copiedAdjustments: null,
  copiedFrom: null,
  previousPhoto: null,
  previousPhotoSize: null,

  isGeneratingAiMask: false,
  isMaskControlHovered: false,
  isMaskRangeDragging: false,
  hasRenderedFirstFrame: false,
  holdingNativeFrame: false,
  stillPreview: null,
  panelsHeld: false,
  nativeCropFrame: false,
  toolClipboard: null,
  nativeMaskOverlay: null,
  showMaskWithSelectTool: false,
  maskOverlayAuto: 'auto',
  lumaPickerLayer: null,
  shapeDrawTarget: null,
  patchesSentToBackend: new Set<string>(),

  setEditor: (updater) => set((state) => (typeof updater === 'function' ? updater(state) : updater)),

  pushHistory: (newAdj) => {
    scheduledHistory.cancel();
    heldHistory = null;
    // Usage statistics (only while switched on): the tools this step used.
    noteEdit(get().history[get().historyIndex], newAdj);
    const order = newEditOrder();
    set((state) => {
      const newHistory = state.history.slice(0, state.historyIndex + 1);
      newHistory.push(newAdj);
      const orders = [
        ...historyOrders(state.history).slice(0, state.historyIndex + 1),
        order,
      ];
      if (newHistory.length > 50) {
        newHistory.shift();
        orders.shift();
      }
      setHistoryOrders(newHistory, orders);
      return { history: newHistory, historyIndex: newHistory.length - 1, historyPending: false };
    });
  },

  undo: () => {
    debouncedSetHistory.flush();
    set((state) => {
      if (state.historyIndex > 0) {
        const newIndex = state.historyIndex - 1;
        return { historyIndex: newIndex, adjustments: state.history[newIndex] };
      }
      return state;
    });
  },

  redo: () => {
    // A pending edit branches history and invalidates the old redo chain.
    debouncedSetHistory.flush();
    set((state) => {
      if (state.historyIndex < state.history.length - 1) {
        const newIndex = state.historyIndex + 1;
        return { historyIndex: newIndex, adjustments: state.history[newIndex] };
      }
      return state;
    });
  },

  resetHistory: (initialState) => {
    scheduledHistory.cancel();
    heldHistory = null;
    set({
      history: [initialState],
      historyIndex: 0,
      historyPending: false,
      adjustments: initialState,
    });
  },

  goToHistoryIndex: (index) => {
    // Flushing at the 50-entry limit can shift indexes. Keep the clicked entry.
    const target = useEditorStore.getState().history[index];
    if (!target) return;
    debouncedSetHistory.flush();
    set((state) => {
      const currentIndex = state.history.indexOf(target);
      if (currentIndex >= 0) {
        return { historyIndex: currentIndex, adjustments: target };
      }
      // The clicked snapshot was evicted by the size cap or the new branch.
      // Record returning to it, retaining the just-committed edit for Undo.
      const order = newEditOrder();
      const history = [...state.history, target].slice(-50);
      setHistoryOrders(
        history,
        [...historyOrders(state.history), order].slice(-50),
      );
      return { history, historyIndex: history.length - 1, adjustments: target };
    });
  },
}));

// The store owns pending history so every navigation path can settle it.
// Slider edits still coalesce, but Undo is available from the first edit.
const scheduledHistory = debounce((adjustments: Adjustments) => {
  useEditorStore.getState().pushHistory(adjustments);
}, 500);
// The latest edit of a drag in progress (`isSliderDragging`): a drag is one
// history step however long it pauses; it is scheduled when the drag ends.
let heldHistory: Adjustments | null = null;
export const debouncedSetHistory = Object.assign(
  (adjustments: Adjustments) => {
    if (useEditorStore.getState().isSliderDragging) {
      // An edit scheduled before the drag stays its own step.
      if (!heldHistory) scheduledHistory.flush();
      heldHistory = adjustments;
    } else {
      heldHistory = null;
      scheduledHistory(adjustments);
    }
    if (!useEditorStore.getState().historyPending)
      useEditorStore.setState({ historyPending: true });
  },
  {
    flush: () => {
      if (heldHistory) useEditorStore.getState().pushHistory(heldHistory);
      else scheduledHistory.flush();
    },
    cancel: () => {
      scheduledHistory.cancel();
      heldHistory = null;
      if (useEditorStore.getState().historyPending)
        useEditorStore.setState({ historyPending: false });
    },
  },
);

/** The photograph open last (`previousPhoto`) and its size. */
let lastOpened: { path: string; size: { width: number; height: number } | null } | null = null;

// View state that follows the edits (one place, so every way of editing a
// layer counts: sliders, typed values, presets, undo).
useEditorStore.subscribe((state, previous) => {
  if (state.history !== previous.history)
    inheritHistoryOrders(
      previous.history,
      state.history,
      state.selectedImage?.path === previous.selectedImage?.path,
    );
  const patch: Partial<EditorState> = {};
  if (state.selectedImage?.path !== previous.selectedImage?.path) {
    debouncedSetHistory.cancel();
    // Previous: the photograph open last before this one, also across a
    // closed editor (another folder or album shown in between).
    const opened = state.selectedImage?.path;
    if (opened && lastOpened && lastOpened.path !== opened) {
      patch.previousPhoto = lastOpened.path;
      patch.previousPhotoSize = lastOpened.size;
    }
    if (opened) lastOpened = { path: opened, size: null };
  } else if (previous.isSliderDragging && !state.isSliderDragging && heldHistory) {
    // The drag ended: its last edit becomes one step (a final value sent
    // just after the release still joins it).
    const held = heldHistory;
    heldHistory = null;
    scheduledHistory(held);
  }
  // The open photograph's size once it is known (Previous frames its
  // drawn masks with it).
  if (
    lastOpened &&
    state.selectedImage?.path === lastOpened.path &&
    state.selectedImage.width > 0 &&
    state.selectedImage.height > 0
  ) {
    lastOpened.size = { width: state.selectedImage.width, height: state.selectedImage.height };
  }
  // Lightroom's automatic overlay (notes: reference/layers-panel-c1-lr.md):
  // shown while a mask is made or painted, off at the layer's first
  // adjustment; a new photo starts automatic again.
  const overlay =
    state.selectedImage?.path !== previous.selectedImage?.path
      ? 'auto'
      : maskOverlayAfterEdit(
          state.maskOverlayAuto,
          previous.adjustments.masks,
          state.adjustments.masks,
          state.activeMaskContainerId,
        );
  if (overlay !== state.maskOverlayAuto) patch.maskOverlayAuto = overlay;
  // The luma range eyedropper belongs to the layer and component it was
  // started with: picking another one ends it.
  if (
    state.lumaPickerLayer &&
    state.lumaPickerLayer === previous.lumaPickerLayer &&
    (state.activeMaskId !== previous.activeMaskId ||
      state.activeMaskContainerId !== previous.activeMaskContainerId ||
      state.selectedImage?.path !== previous.selectedImage?.path)
  )
    patch.lumaPickerLayer = null;
  // So does a shape waiting for its drag.
  if (
    state.shapeDrawTarget &&
    state.shapeDrawTarget === previous.shapeDrawTarget &&
    (state.activeMaskId !== previous.activeMaskId || state.selectedImage?.path !== previous.selectedImage?.path)
  )
    patch.shapeDrawTarget = null;
  if (Object.keys(patch).length > 0) state.setEditor(patch);
});
