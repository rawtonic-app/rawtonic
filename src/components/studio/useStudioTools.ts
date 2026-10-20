import { useShallow } from "zustand/react/shallow";
import i18n from "i18next";
import { useEditorStore } from "../../store/useEditorStore";
import { useUIStore } from "../../store/useUIStore";
import { useSettingsStore } from "../../store/useSettingsStore";
import { useEditorActions } from "../../hooks/useEditorActions";
import { INITIAL_MASK_CONTAINER, MASK_VERSION } from "../../utils/adjustments";
import { addComponentBeforeLumaRange, createSubMask } from "../../utils/maskUtils";
import { Mask, SubMaskMode, ToolType } from "../panel/right/Masks";
import { createComponents, type MaskKind } from "../../utils/maskKinds";
import { Panel } from "../ui/AppProperties";
import { DEFAULT_BRUSH_SIZE } from "../../utils/brushSize";
import { warnIfComponentStartsLayer, warnIfLayerLimit } from "./layerLimitToast";

/** Cursor tools of the viewer bar, after Capture One's cursor tools. */
export type StudioTool =
  | "select"
  | "crop"
  | "straighten"
  | "linear"
  | "radial"
  | "brush"
  | "erase"
  | "spot"
  | "wb";

export {
  BRUSH_SIZE_MAX,
  BRUSH_SIZE_MIN,
  stepBrushSize,
} from "../../utils/brushSize";

/** Shift+Q: the spot removal tool's Remove with Detect objects (click or
 * box an object to remove it). Opens the tool in that mode; pressed again
 * while it is on, it ends the tool, as Q does. */
export function toggleRemoveObjects(selectTool: (tool: StudioTool) => void) {
  const { spotSettings, isSpotToolActive, setEditor } = useEditorStore.getState();
  const on = isSpotToolActive && spotSettings.mode === "remove" && Boolean(spotSettings.detectObjects);
  if (!on) setEditor({ spotSettings: { ...spotSettings, mode: "remove", detectObjects: true } });
  if (!isSpotToolActive || on) selectTool("spot");
}

/** Tool state and actions shared by the layer panel, the viewer's tool bar
 * and the tool settings popover. */
export function useStudioTools() {
  const {
    adjustments,
    selectedImage,
    activeMaskContainerId,
    activeMaskId,
    isWbPickerActive,
    isStraightenActive,
    isSpotToolActive,
    setEditor,
    brushSettings,
  } = useEditorStore(
    useShallow((s) => ({
      adjustments: s.adjustments,
      selectedImage: s.selectedImage,
      activeMaskContainerId: s.activeMaskContainerId,
      activeMaskId: s.activeMaskId,
      isWbPickerActive: s.isWbPickerActive,
      isStraightenActive: s.isStraightenActive,
      isSpotToolActive: s.isSpotToolActive,
      setEditor: s.setEditor,
      brushSettings: s.brushSettings,
    })),
  );
  const { setAdjustments } = useEditorActions();
  const activePanel = useUIStore((s) => s.activePanel);
  const radialDrawInside = useSettingsStore(
    (s) => s.appSettings?.radialDrawInside !== false,
  );
  type Layer = (typeof adjustments.masks)[number];
  const layer =
    activePanel === Panel.Masks
      ? adjustments.masks.find((m) => m.id === activeMaskContainerId)
      : undefined;
  const activeSubMask = layer?.subMasks.find((m) => m.id === activeMaskId);
  const brushActive =
    activeSubMask?.type === Mask.Brush || activeSubMask?.type === Mask.Flow;
  // Whether painting with the active brush component adds visible coverage.
  const activeBrushDraws =
    (activeSubMask?.mode === SubMaskMode.Subtractive) ===
    Boolean(layer?.invert);
  const radialMask =
    activeSubMask?.type === Mask.Radial ? activeSubMask : undefined;
  const activeTool: StudioTool = isWbPickerActive
    ? "wb"
    : isSpotToolActive
      ? "spot"
      : isStraightenActive && activePanel === Panel.Crop
      ? "straighten"
      : activePanel === Panel.Crop
      ? "crop"
      : brushActive
        ? activeBrushDraws
          ? "brush"
          : "erase"
        : activeSubMask?.type === Mask.Linear
          ? "linear"
          : activeSubMask?.type === Mask.Radial
            ? "radial"
            : "select";
  const ready = Boolean(selectedImage?.isReady);

  const brushFor = (size: number) => ({
    size,
    feather: brushSettings?.feather ?? 50,
    hardness: brushSettings?.hardness ?? 50,
    opacity: brushSettings?.opacity,
    flow: brushSettings?.flow,
    tool: ToolType.Brush,
  });

  /** A new layer of `kind` (a drawn shape, an automatic selection or a
   * range), selected with its first component active. */
  const addLayer = (kind: Mask | MaskKind) => {
    if (!selectedImage?.isReady) return;
    if (warnIfLayerLimit(adjustments.masks)) return;
    const maskKind: MaskKind =
      kind === Mask.Brush ? "brush" : kind === Mask.Linear ? "linear" : kind === Mask.Radial ? "radial" : (kind as MaskKind);
    const components = createComponents(maskKind, selectedImage);
    const mask = components[0];
    // Capture One's "Draw Mask Inside" decides which side a new radial covers.
    if (maskKind === "radial") mask.invert = !radialDrawInside;
    const id = crypto.randomUUID();
    setAdjustments((prev) => ({
      ...prev,
      masks: [
        ...prev.masks,
        {
          ...structuredClone(INITIAL_MASK_CONTAINER),
          maskVersion: MASK_VERSION,
          id,
          name: i18n.t("studio.develop.layers.defaultName", {
            number: prev.masks.length + 1,
          }),
          subMasks: components,
        },
      ],
    }));
    setEditor({
      activeMaskContainerId: id,
      activeMaskId: mask.id,
      // A new layer is selected as a whole: Delete removes the layer.
      focusedMaskComponentId: null,
      isWbPickerActive: false,
      isStraightenActive: false,
      // A luminance range starts sampling tones on the photo (Lightroom).
      ...(maskKind === "luminance" ? { lumaPickerLayer: id } : {}),
      // A new layer keeps the current brush size.
      brushSettings: {
        ...brushFor(brushSettings?.size ?? DEFAULT_BRUSH_SIZE),
        feather: 50,
      },
    });
    useUIStore.getState().setPanel(Panel.Masks);
  };

  // Capture One rasterizes a gradient before the brush may change it.
  // Rawtonic keeps the gradient editable instead (as Lightroom does): drawing
  // adds an additive brush component to the layer, erasing a subtractive one.
  // On an inverted layer the two swap, so drawing still adds visible coverage.
  const paintOnLayer = (target: Layer, draw: boolean) => {
    if (!selectedImage?.isReady) return;
    const mode =
      draw !== Boolean(target.invert)
        ? SubMaskMode.Additive
        : SubMaskMode.Subtractive;
    let brush = target.subMasks.find(
      (m) => m.type === Mask.Brush && m.mode === mode,
    );
    if (!brush) {
      if (warnIfComponentStartsLayer(adjustments.masks, target)) return;
      const created = createSubMask(Mask.Brush, selectedImage, mode);
      brush = created;
      setAdjustments((prev) => ({
        ...prev,
        masks: prev.masks.map((m) =>
          m.id === target.id
            ? { ...m, subMasks: addComponentBeforeLumaRange(m.subMasks, created) }
            : m,
        ),
      }));
    }
    setEditor({
      activeMaskContainerId: target.id,
      activeMaskId: brush.id,
      focusedMaskComponentId: brush.id,
      isWbPickerActive: false,
      isStraightenActive: false,
      brushSettings: brushFor(brushSettings?.size ?? DEFAULT_BRUSH_SIZE),
    });
    useUIStore.getState().setPanel(Panel.Masks);
  };

  /** Adds a component of `kind` to a layer (Lightroom's Add, Subtract and
   * Intersect), ahead of its luma range, and makes it the active one. */
  const addComponent = (target: Layer, kind: MaskKind, mode: SubMaskMode) => {
    if (!selectedImage?.isReady) return;
    const components = createComponents(kind, selectedImage, mode);
    // A luminance range added to a layer is its luma range.
    const added =
      kind === "luminance"
        ? components.filter((c) => c.type === Mask.LumaRange && !target.subMasks.some((m) => m.type === Mask.LumaRange))
        : components;
    if (added.length > 0 && warnIfComponentStartsLayer(adjustments.masks, target)) return;
    if (added.length === 0) {
      // The layer has its luma range already: sample tones for it.
      if (kind === "luminance") setEditor({ activeMaskContainerId: target.id, lumaPickerLayer: target.id });
      return;
    }
    setAdjustments((prev) => ({
      ...prev,
      masks: prev.masks.map((m) =>
        m.id === target.id
          ? { ...m, subMasks: added.reduce((subs, c) => addComponentBeforeLumaRange(subs, c), m.subMasks) }
          : m,
      ),
    }));
    setEditor({
      activeMaskContainerId: target.id,
      activeMaskId: added[0].id,
      // The added component is the selection, as in Lightroom.
      focusedMaskComponentId: added[0].id,
      isWbPickerActive: false,
      isStraightenActive: false,
      // A luminance range starts sampling tones on the photo (Lightroom).
      ...(kind === "luminance" ? { lumaPickerLayer: target.id } : {}),
      // A brush component (Intersect's) paints, as `paintOnLayer` does.
      ...(kind === "brush" ? { brushSettings: brushFor(brushSettings?.size ?? DEFAULT_BRUSH_SIZE) } : {}),
    });
    useUIStore.getState().setPanel(Panel.Masks);
  };

  const toggleInvert = (target: Layer) =>
    setAdjustments((prev) => ({
      ...prev,
      masks: prev.masks.map((m) =>
        m.id === target.id ? { ...m, invert: !m.invert } : m,
      ),
    }));

  const selectTool = (tool: StudioTool) => {
    if (!ready) return;
    const { setPanel } = useUIStore.getState();
    // Any other tool ends spot removal.
    if (tool !== "spot" && isSpotToolActive)
      setEditor({ isSpotToolActive: false, activeSpotId: null });
    switch (tool) {
      case "select":
        // Keeps the layer selected, so its shape stays editable on the canvas.
        setEditor({
          activeMaskId: null,
          isWbPickerActive: false,
          isStraightenActive: false,
        });
        if (activePanel === Panel.Crop) setPanel(Panel.Adjustments);
        break;
      case "crop":
        setEditor({
          activeMaskContainerId: null,
          activeMaskId: null,
          isWbPickerActive: false,
          isStraightenActive: false,
        });
        setPanel(
          activePanel === Panel.Crop && !isStraightenActive
            ? Panel.Adjustments
            : Panel.Crop,
        );
        break;
      case "straighten":
        // Capture One's straighten: draw a line along something that should
        // be level or plumb; the photo rotates to it (in the crop view, which
        // shows the whole photo). Pressed again it returns to the crop tool.
        setEditor({
          activeMaskContainerId: null,
          activeMaskId: null,
          isWbPickerActive: false,
          isStraightenActive: !isStraightenActive,
        });
        setPanel(Panel.Crop);
        break;
      case "linear":
        addLayer(Mask.Linear);
        break;
      case "radial":
        addLayer(Mask.Radial);
        break;
      case "brush":
      case "erase":
        // Paints on the selected layer; with none selected the brush starts a
        // new layer (erasing needs a layer to erase from).
        if (layer) paintOnLayer(layer, tool === "brush");
        else if (tool === "brush") addLayer(Mask.Brush);
        break;
      case "spot":
        // Lightroom's Healing / Capture One's Heal and Clone: spots on the
        // photograph itself, not on a layer.
        setEditor({
          activeMaskContainerId: null,
          activeMaskId: null,
          isWbPickerActive: false,
          isStraightenActive: false,
          isSpotToolActive: !isSpotToolActive,
          activeSpotId: null,
        });
        if (activePanel !== Panel.Adjustments) setPanel(Panel.Adjustments);
        break;
      case "wb":
        // The picker sets the photograph's white balance, not a layer's.
        setEditor({
          activeMaskContainerId: null,
          activeMaskId: null,
          isWbPickerActive: !isWbPickerActive,
          isStraightenActive: false,
          isSpotToolActive: false,
        });
        if (activePanel !== Panel.Adjustments) setPanel(Panel.Adjustments);
        break;
    }
  };

  const setBrushSize = (size: number) =>
    setEditor({
      brushSettings: brushSettings
        ? { ...brushSettings, size }
        : { ...brushFor(size), feather: 50 },
    });

  return {
    layer,
    brushActive,
    activeBrushDraws,
    radialMask,
    activeTool,
    ready,
    brushSettings,
    addLayer,
    addComponent,
    paintOnLayer,
    toggleInvert,
    selectTool,
    setBrushSize,
  };
}
