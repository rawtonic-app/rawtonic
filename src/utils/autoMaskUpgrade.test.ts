import { describe, expect, it } from 'vitest';
import { AUTO_MASK_VERSION, canUpgradeAutoMask, layerAutoMaskVersions, upgradeLayerAutoMask } from './autoMaskUpgrade';
import { INITIAL_MASK_CONTAINER, type MaskContainer } from './adjustments';
import { createSubMask } from './maskUtils';
import { Mask, ToolType, type MaskStrokeLine } from '../components/panel/right/Masks';

const size = { width: 6000, height: 4000 };
const line = (patch: Partial<MaskStrokeLine>): MaskStrokeLine => ({
  brushSize: 50,
  points: [{ x: 1, y: 2 }],
  tool: ToolType.Brush,
  ...patch,
});
const brushLayer = (...lines: MaskStrokeLine[]): MaskContainer => {
  const brush = createSubMask(Mask.Brush, size);
  return {
    ...structuredClone(INITIAL_MASK_CONTAINER),
    id: 'l',
    name: 'Brush',
    subMasks: [{ ...brush, parameters: { ...brush.parameters, lines } }],
  };
};

describe('Auto Mask upgrade (D6)', () => {
  it('names the versions of Auto Mask strokes only', () => {
    const layer = brushLayer(line({ autoMask: true }), line({}), line({ autoMask: true, autoMaskVersion: 3 }));
    expect(layerAutoMaskVersions(layer)).toEqual([1, 3]);
    expect(canUpgradeAutoMask(layer)).toBe(true);
    expect(layerAutoMaskVersions(brushLayer(line({})))).toEqual([]);
  });

  it('moves older Auto Mask strokes to the current version and leaves the rest', () => {
    const plain = line({});
    const current = line({ autoMask: true, autoMaskVersion: AUTO_MASK_VERSION });
    const layer = brushLayer(line({ autoMask: true }), plain, line({ autoMask: true, autoMaskVersion: 2 }), current);
    const before = JSON.stringify(layer);
    const upgraded = upgradeLayerAutoMask(layer);
    const lines = upgraded.subMasks[0].parameters.lines!;
    expect(lines.map((l) => l.autoMaskVersion)).toEqual([AUTO_MASK_VERSION, undefined, AUTO_MASK_VERSION, AUTO_MASK_VERSION]);
    expect(lines[1]).toBe(plain);
    expect(lines[3]).toBe(current);
    // New parameters object: the preview sends the strokes again (maskParameterRefs).
    expect(upgraded.subMasks[0].parameters).not.toBe(layer.subMasks[0].parameters);
    expect(JSON.stringify(layer)).toBe(before);
    expect(canUpgradeAutoMask(upgraded)).toBe(false);
  });

  it('returns a layer without older strokes unchanged', () => {
    const layer = brushLayer(line({ autoMask: true, autoMaskVersion: AUTO_MASK_VERSION }), line({}));
    expect(upgradeLayerAutoMask(layer)).toBe(layer);
    expect(canUpgradeAutoMask(layer)).toBe(false);
  });
});
