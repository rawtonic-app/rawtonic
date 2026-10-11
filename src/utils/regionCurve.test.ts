import { describe, expect, it } from 'vitest';
import { regionCurveValue } from './regionCurve';
import { INITIAL_REGION_CURVE } from './adjustments';

describe('regionCurveValue', () => {
  it('is the identity when neutral', () => {
    for (const x of [0, 0.2, 0.5, 0.9, 1]) expect(regionCurveValue(INITIAL_REGION_CURVE, x)).toBeCloseTo(x, 6);
  });
  it('matches the renderer where Lightroom puts the regions', () => {
    // Same expectations as the region_curve.rs tests.
    const lift = (key: string, x: number) => regionCurveValue({ ...INITIAL_REGION_CURVE, [key]: 100 }, x) - x;
    expect(lift('shadows', 0.15)).toBeGreaterThan(0.08);
    expect(Math.abs(lift('shadows', 0.7))).toBeLessThan(1e-4);
    expect(lift('darks', 0.35)).toBeGreaterThan(0.15);
    expect(lift('lights', 0.6)).toBeGreaterThan(0.2);
    expect(lift('highlights', 0.85)).toBeGreaterThan(0.1);
  });
});
