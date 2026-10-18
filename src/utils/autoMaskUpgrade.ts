import type { MaskStrokeLine } from '../components/panel/right/Masks';
import type { MaskContainer } from './adjustments';

/** Auto Mask method of new strokes: 3 is edge-aware with the stroke's dabs
 * modelled together (crates/engine `stroke_models_v3`); strokes with 2 or
 * without it (version 1: old strokes, Lightroom imports) keep their version. */
export const AUTO_MASK_VERSION = 3;

const autoMaskLines = (layer: MaskContainer): MaskStrokeLine[] =>
  layer.subMasks.flatMap((sub) => {
    const lines = sub.parameters?.lines;
    return Array.isArray(lines) ? lines.filter((line) => line?.autoMask) : [];
  });

/** The Auto Mask versions the layer's strokes use, lowest first; empty
 * without Auto Mask strokes. */
export function layerAutoMaskVersions(layer: MaskContainer): number[] {
  const versions = new Set(autoMaskLines(layer).map((line) => line.autoMaskVersion ?? 1));
  return [...versions].sort((a, b) => a - b);
}

/** Whether "Upgrade to current Auto Mask" changes the layer. */
export function canUpgradeAutoMask(layer: MaskContainer): boolean {
  return layerAutoMaskVersions(layer).some((version) => version < AUTO_MASK_VERSION);
}

/**
 * The layer with its older Auto Mask strokes on the current version
 * (D6 of docs/plans/local-adjustments-g29-g31.md: imported Lightroom
 * strokes stay on version 1 until the user asks for this). Only those
 * strokes change, as new objects, so the renderer computes them again;
 * everything else, and a layer without older strokes, stays the same object.
 */
export function upgradeLayerAutoMask(layer: MaskContainer): MaskContainer {
  if (!canUpgradeAutoMask(layer)) return layer;
  const old = (line: MaskStrokeLine) => line?.autoMask && (line.autoMaskVersion ?? 1) < AUTO_MASK_VERSION;
  return {
    ...layer,
    subMasks: layer.subMasks.map((sub) => {
      const lines = sub.parameters?.lines;
      if (!Array.isArray(lines) || !lines.some(old)) return sub;
      return {
        ...sub,
        parameters: {
          ...sub.parameters,
          lines: lines.map((line) => (old(line) ? { ...line, autoMaskVersion: AUTO_MASK_VERSION } : line)),
        },
      };
    }),
  };
}
