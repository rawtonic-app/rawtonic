import { Mask, SubMaskMode } from '../components/panel/right/Masks';
import type { MaskContainer } from './adjustments';

/**
 * View only, never in the recipe or an export: the display shows one layer,
 * or one component of it, alone (useImageProcessing `withViewStates`).
 * Neither Lightroom nor Capture One has a solo; there, seeing one layer
 * alone means hiding every other one (notes: reference/layers-panel-c1-lr.md
 * task 8, one click per layer).
 */
export interface LayerSolo {
  layerId: string;
  /** Shows only this component of the layer ("show only this mask"). */
  componentId: string | null;
}

/** The layers as the display shows them under `solo`: the other layers
 * hidden, the soloed one shown even when it is hidden; with a component
 * soloed, its siblings hidden and the component adding, so a subtracting or
 * intersecting one shows the area it covers. The luma range stays: it is
 * the layer's own control, not a component. Unknown ids show everything. */
export function applyLayerSolo(masks: MaskContainer[], solo: LayerSolo | null): MaskContainer[] {
  if (!solo || !masks.some((m) => m.id === solo.layerId)) return masks;
  return masks.map((mask) => {
    if (mask.id !== solo.layerId) return mask.visible ? { ...mask, visible: false } : mask;
    const shown = mask.visible ? mask : { ...mask, visible: true };
    const component = solo.componentId;
    if (!component || !mask.subMasks.some((s) => s.id === component)) return shown;
    return {
      ...shown,
      subMasks: mask.subMasks.map((sub) => {
        if (sub.type === Mask.LumaRange) return sub;
        if (sub.id === component) return { ...sub, visible: true, mode: SubMaskMode.Additive };
        return sub.visible ? { ...sub, visible: false } : sub;
      }),
    };
  });
}

/** Solo after a click: the same layer (and component) again ends it,
 * anything else solos that. */
export function toggleLayerSolo(solo: LayerSolo | null, layerId: string, componentId: string | null = null) {
  return solo?.layerId === layerId && solo.componentId === componentId ? null : { layerId, componentId };
}

/** While a solo is on it follows the selection, so stepping through the
 * layers (or a layer's components) shows each alone with one click;
 * selecting the Background (no layer) ends it. */
export function soloAfterSelect(solo: LayerSolo | null, layerId: string | null, componentId: string | null = null) {
  return solo && layerId ? { layerId, componentId } : null;
}
