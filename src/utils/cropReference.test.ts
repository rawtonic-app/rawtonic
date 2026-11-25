import { describe, expect, it } from 'vitest';
import { cropReferenceKey, fitRatioCrop, referenceAfterHandEdit, referenceForRatioChange } from './cropReference';
import { calculateCenteredCrop, isCropWithinBounds } from './cropUtils';

const px = (x: number, y: number, width: number, height: number) => ({ unit: 'px' as const, x, y, width, height });

describe('crop reference for ratio changes', () => {
  it('on a straightened photo every ratio is the largest one at that angle, whatever came before', () => {
    const photo = referenceForRatioChange(null, 'k', null, 6000, 4000);
    for (const ratio of [1, 16 / 9, 3 / 2, 4 / 5]) {
      const fitted = fitRatioCrop(6000, 4000, 0, ratio, 4, photo)!;
      expect(fitted).toEqual(calculateCenteredCrop(6000, 4000, 0, ratio, 4));
      expect(isCropWithinBounds(fitted, 6000, 4000, 4)).toBe(true);
    }
  });

  it('a drawn crop becomes the reference; moving it moves the reference', () => {
    const drawn = referenceAfterHandEdit(null, 'k', null, px(100, 100, 3000, 2000), 6000, 4000, 0);
    expect(drawn).toEqual({ key: 'k', rect: px(100, 100, 3000, 2000), cx: 1600, cy: 1100 });
    const moved = referenceAfterHandEdit(drawn, 'k', px(100, 100, 3000, 2000), px(600, 300, 3000, 2000), 6000, 4000, 0);
    expect(moved.rect).toEqual(px(600, 300, 3000, 2000));
    // A move of a crop fitted inside the drawn one carries the drawn one along.
    const inner = referenceAfterHandEdit(
      moved,
      'k',
      px(1100, 300, 2000, 2000),
      px(1300, 400, 2000, 2000),
      6000,
      4000,
      0
    );
    expect(inner.rect).toEqual(px(800, 400, 3000, 2000));
    expect([inner.cx, inner.cy]).toEqual([2300, 1400]);
  });

  it('another photo or frame (turned, straightened, transformed) starts from the whole photo', () => {
    const a = cropReferenceKey('/a.NEF', { rotation: 0, crop: px(0, 0, 10, 10), aspectRatio: 1 });
    expect(cropReferenceKey('/a.NEF', { rotation: 0, crop: null, aspectRatio: null })).toBe(a);
    expect(cropReferenceKey('/a.NEF', { rotation: 2 })).not.toBe(a);
    expect(cropReferenceKey('/a.NEF', { orientationSteps: 1 })).not.toBe(a);
    expect(cropReferenceKey('/a.NEF', { transformVertical: 10 })).not.toBe(a);
    expect(cropReferenceKey('/b.NEF', {})).not.toBe(a);
    const kept = { key: a, rect: px(1, 1, 5, 5), cx: 3, cy: 3 };
    expect(referenceForRatioChange(kept, a, px(2, 2, 2, 2), 100, 100)).toBe(kept);
    // No crop any more (a reset): the whole photo.
    expect(referenceForRatioChange(kept, a, null, 100, 100).rect).toBeNull();
    expect(referenceForRatioChange(kept, 'other', px(0, 0, 20, 10), 100, 100)).toEqual({
      key: 'other',
      rect: null,
      cx: 10,
      cy: 5
    });
  });
});
