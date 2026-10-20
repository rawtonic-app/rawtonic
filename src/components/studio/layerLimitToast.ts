import i18n from "i18next";
import { toast } from "react-toastify";
import { MAX_RENDERED_LAYERS, canRenderAnotherLayer, isRenderedLayer } from "../../utils/layerLimit";
import type { MaskContainer } from "../../utils/adjustments";

/** Refuses one more rendered layer beyond the engine's limit with a message
 * (the engine would keep it but ignore it); true when refused. */
export function warnIfLayerLimit(masks: readonly MaskContainer[]): boolean {
  if (canRenderAnotherLayer(masks)) return false;
  toast.error(i18n.t("studio.develop.layers.limitReached", { limit: MAX_RENDERED_LAYERS }));
  return true;
}

/** The same for a component added to `layer`: a visible layer without
 * components starts being rendered with its first one. */
export function warnIfComponentStartsLayer(masks: readonly MaskContainer[], layer: MaskContainer): boolean {
  return layer.visible && !isRenderedLayer(layer) && warnIfLayerLimit(masks);
}
