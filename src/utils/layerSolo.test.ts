import { describe, expect, it } from 'vitest';
import { applyLayerSolo, soloAfterSelect, toggleLayerSolo } from './layerSolo';
import { INITIAL_MASK_CONTAINER, type MaskContainer } from './adjustments';
import { createSubMask } from './maskUtils';
import { Mask, SubMaskMode } from '../components/panel/right/Masks';

const size = { width: 6000, height: 4000 };
const layer = (id: string, patch: Partial<MaskContainer> = {}): MaskContainer => ({
  ...structuredClone(INITIAL_MASK_CONTAINER),
  id,
  name: id,
  subMasks: [],
  ...patch,
});

describe('applyLayerSolo', () => {
  it('hides the other layers and shows the soloed one, even a hidden one', () => {
    const masks = [layer('a'), layer('b', { visible: false }), layer('c')];
    const shown = applyLayerSolo(masks, { layerId: 'b', componentId: null });
    expect(shown.map((m) => m.visible)).toEqual([false, true, false]);
    // The recipe's own objects are not edited.
    expect(masks.map((m) => m.visible)).toEqual([true, false, true]);
  });

  it('shows one component alone, adding, and keeps the luma range', () => {
    const subject = createSubMask(Mask.AiSubject, size);
    const erase = { ...createSubMask(Mask.Brush, size), mode: SubMaskMode.Subtractive };
    const luma = createSubMask(Mask.LumaRange, size);
    const masks = [layer('a', { subMasks: [subject, erase, luma] })];
    const [solo] = applyLayerSolo(masks, { layerId: 'a', componentId: erase.id });
    expect(solo.subMasks.map((s) => [s.visible, s.mode])).toEqual([
      [false, subject.mode],
      [true, SubMaskMode.Additive],
      [luma.visible, luma.mode],
    ]);
  });

  it('shows everything for no solo or an unknown layer', () => {
    const masks = [layer('a'), layer('b')];
    expect(applyLayerSolo(masks, null)).toBe(masks);
    expect(applyLayerSolo(masks, { layerId: 'gone', componentId: null })).toBe(masks);
  });
});

describe('solo clicks and selection', () => {
  it('toggles: the same layer again ends the solo, another one moves it', () => {
    const solo = toggleLayerSolo(null, 'a');
    expect(solo).toEqual({ layerId: 'a', componentId: null });
    expect(toggleLayerSolo(solo, 'a')).toBeNull();
    expect(toggleLayerSolo(solo, 'b')).toEqual({ layerId: 'b', componentId: null });
    expect(toggleLayerSolo(solo, 'a', 'm1')).toEqual({ layerId: 'a', componentId: 'm1' });
  });

  it('follows the selection while on and ends on the Background', () => {
    expect(soloAfterSelect(null, 'b')).toBeNull();
    const solo = { layerId: 'a', componentId: null };
    expect(soloAfterSelect(solo, 'b')).toEqual({ layerId: 'b', componentId: null });
    expect(soloAfterSelect(solo, null)).toBeNull();
  });
});
