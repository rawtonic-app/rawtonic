import { describe, expect, it } from 'vitest';
import { srgbToOklch } from './oklab';

describe('srgbToOklch', () => {
  it('matches reference Oklab values', () => {
    const white = srgbToOklch([255, 255, 255]);
    expect(white.lightness).toBeCloseTo(1, 3);
    expect(white.chroma).toBeLessThan(1e-3);
    const red = srgbToOklch([255, 0, 0]);
    expect(red.lightness).toBeCloseTo(0.628, 3);
    expect(red.chroma).toBeCloseTo(0.2577, 3);
    expect(red.hue).toBeCloseTo(29.23, 1);
    const blue = srgbToOklch([0, 0, 255]);
    expect(blue.hue).toBeCloseTo(264.05, 1);
  });
});
