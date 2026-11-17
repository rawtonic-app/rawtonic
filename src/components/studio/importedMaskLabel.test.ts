import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { Mask, MASK_ICON_MAP, SubMaskMode, type SubMask } from '../panel/right/Masks';
import { maskComponentLabel } from './StudioMaskComponents';
import { componentIcon } from './maskKindMenu';

const t = ((key: string) => key) as unknown as TFunction;

const raster: SubMask = {
  id: 'r',
  type: Mask.Raster,
  mode: SubMaskMode.Additive,
  invert: false,
  opacity: 100,
  visible: true,
  parameters: { maskDataBase64: 'data:image/png;base64,' }
};

describe('an imported raster mask in the layer list', () => {
  it('is named and shown with its own icon', () => {
    expect(maskComponentLabel(t, raster)).toBe('masks.types.raster');
    expect(maskComponentLabel(t, { ...raster, name: 'Sky brush' })).toBe('Sky brush');
    expect(componentIcon(raster)).toBe(MASK_ICON_MAP[Mask.Raster]);
  });
});
