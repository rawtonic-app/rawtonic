import i18n from "i18next";
import { toast } from "react-toastify";
import { useEditorStore } from "../../store/useEditorStore";
import { MAX_RENDERED_LAYERS, canRenderAnotherLayer, isRenderedLayer } from "../../utils/layerLimit";

// Both read the layers as they are now, not as a menu or button rendered
// them: a menu kept open across an edit still sees the current count.
const currentMasks = () => useEditorStore.getState().adjustments.masks;

/** Refuses one more rendered layer beyond the engine's limit with a message
 * (the engine would keep it but ignore it); true when refused. */
export function warnIfLayerLimit(): boolean {
  if (canRenderAnotherLayer(currentMasks())) return false;
  toast.error(i18n.t("studio.develop.layers.limitReached", { limit: MAX_RENDERED_LAYERS }));
  return true;
}

/** The same for a component added to layer `layerId`: a visible layer
 * without components starts being rendered with its first one. */
export function warnIfComponentStartsLayer(layerId: string): boolean {
  const layer = currentMasks().find((m) => m.id === layerId);
  return Boolean(layer?.visible) && !isRenderedLayer(layer!) && warnIfLayerLimit();
}

/** The same for a copy of layer `layerId`: rendered when the layer is. */
export function warnIfCopyRenders(layerId: string): boolean {
  const layer = currentMasks().find((m) => m.id === layerId);
  return Boolean(layer) && isRenderedLayer(layer!) && warnIfLayerLimit();
}

/** The same for showing layer `layerId` again. */
export function warnIfShowingRenders(layerId: string): boolean {
  const layer = currentMasks().find((m) => m.id === layerId);
  return Boolean(layer) && !layer!.visible && isRenderedLayer({ ...layer!, visible: true }) && warnIfLayerLimit();
}
