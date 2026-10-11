import { describe, expect, it } from 'vitest';
import { createSubMask, deleteMaskSelection } from './maskUtils';
import { Mask, SubMaskMode } from '../components/panel/right/Masks';

const size = { width: 6000, height: 4000 };
const layer = (id: string, types: Mask[]) => ({
  id,
  subMasks: types.map((type, i) => ({ ...createSubMask(type, size, SubMaskMode.Additive), id: `${id}-${i}` })),
});

describe('deleteMaskSelection', () => {
  it('removes only the picked component of a layer with several', () => {
    const masks = [layer('a', [Mask.AiForeground, Mask.AiSky, Mask.Brush]), layer('b', [Mask.Radial])];
    const { masks: next, removed } = deleteMaskSelection(masks, 'a', 'a-1');
    expect(removed).toBe('component');
    expect(next.map((m) => m.id)).toEqual(['a', 'b']);
    expect(next[0].subMasks.map((s) => s.id)).toEqual(['a-0', 'a-2']);
  });

  it('removes the layer when no component was picked', () => {
    const masks = [layer('a', [Mask.AiForeground, Mask.Brush]), layer('b', [Mask.Radial])];
    const { masks: next, removed } = deleteMaskSelection(masks, 'a', null);
    expect(removed).toBe('layer');
    expect(next.map((m) => m.id)).toEqual(['b']);
  });

  it("removes the layer with its only component (the luma range does not count)", () => {
    const masks = [layer('a', [Mask.AiForeground, Mask.LumaRange])];
    const { masks: next, removed } = deleteMaskSelection(masks, 'a', 'a-0');
    expect(removed).toBe('layer');
    expect(next).toEqual([]);
  });

  it("ignores a picked component of another layer and an unknown layer", () => {
    const masks = [layer('a', [Mask.AiForeground, Mask.Brush]), layer('b', [Mask.Radial, Mask.Linear])];
    expect(deleteMaskSelection(masks, 'a', 'b-0')).toEqual({ masks: [masks[1]], removed: 'layer' });
    expect(deleteMaskSelection(masks, 'x', 'a-0')).toEqual({ masks, removed: null });
  });
});
