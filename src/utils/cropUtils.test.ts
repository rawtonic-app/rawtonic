import { describe, expect, it } from 'vitest';
import { calculateAutoCropForRotation, cropCameWithRotation } from './cropUtils';

const px = (x: number, y: number, width: number, height: number) => ({ unit: 'px' as const, x, y, width, height });

describe('crop tool and a rotation that brings its crop', () => {
  const before = { rotation: 0, aspectRatio: 1.5, orientationSteps: 0 };
  const startCrop = px(2400, 300, 1200, 800);
  // Auto straighten's answer for a 6000 × 4000 photo: −5° and its reframed crop.
  const auto = { rotation: -5, aspectRatio: 1.5, orientationSteps: 0, crop: px(2287, 305, 1200, 800) };

  it('keeps the crop Auto straighten (or undo, paste) set with the rotation', () => {
    expect(cropCameWithRotation(before, startCrop, auto)).toBe(true);
    // Turning it again by the rotation's change would move it.
    const turnedAgain = calculateAutoCropForRotation(6000, 4000, 0, 1.5, -5, auto.crop, -5);
    expect(turnedAgain && Math.abs(turnedAgain.x - auto.crop.x)).toBeGreaterThan(1);
  });

  it('still follows a rotation that leaves the crop as it was', () => {
    expect(cropCameWithRotation(before, startCrop, { ...auto, crop: startCrop })).toBe(false);
    expect(cropCameWithRotation(before, startCrop, { ...auto, crop: px(2401, 300, 1200, 800) })).toBe(false);
  });

  it('recalculates on entering the tool, a ratio or an orientation change', () => {
    expect(cropCameWithRotation(null, startCrop, auto)).toBe(false);
    expect(cropCameWithRotation(before, undefined, auto)).toBe(false);
    expect(cropCameWithRotation(before, startCrop, { ...auto, aspectRatio: 1 })).toBe(false);
    expect(cropCameWithRotation(before, startCrop, { ...auto, orientationSteps: 1 })).toBe(false);
    expect(cropCameWithRotation(before, startCrop, { ...auto, rotation: 0 })).toBe(false);
  });
});
