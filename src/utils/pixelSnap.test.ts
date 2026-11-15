import { describe, expect, it } from 'vitest';
import { drawsOneToOne, isOneToOne, snapToDevicePixel } from './pixelSnap';

describe('pixel snapping at 100 %', () => {
  it('knows 1:1 from the zoom, the fit scale and the device pixel ratio', () => {
    // A 6000 px photograph fitted to 1035 CSS px.
    const fit = 1035 / 6000;
    expect(isOneToOne(6000 / 1035, fit, 1)).toBe(true);
    expect(isOneToOne(3000 / 1035, fit, 2)).toBe(true);
    expect(isOneToOne(6000 / 1035, fit, 2)).toBe(false);
    expect(isOneToOne(1, fit, 1)).toBe(false);
  });

  it('puts the photograph edge on a whole device pixel', () => {
    const zoom = 6000 / 1035;
    const offset = 32.5;
    const x = snapToDevicePixel(-2000.3, zoom, offset, 1, -5000, 0);
    expect(Math.abs(x - -2000.3)).toBeLessThanOrEqual(0.5);
    const edge = x + zoom * offset;
    expect(edge).toBeCloseTo(Math.round(edge), 9);
  });

  it('snaps in device pixels on HiDPI', () => {
    const x = snapToDevicePixel(-100.3, 2.5, 10.1, 2, -5000, 0);
    const edge = (x + 2.5 * 10.1) * 2;
    expect(edge).toBeCloseTo(Math.round(edge), 9);
  });

  it('stays within the pan bounds', () => {
    // Rounding up would leave the bound: the other side is taken.
    const x = snapToDevicePixel(0, 2, 0.3, 1, -500, 0);
    expect(x).toBeLessThanOrEqual(0);
    expect(x + 2 * 0.3).toBeCloseTo(0, 9);
    // A fixed (centred) axis keeps its position.
    expect(snapToDevicePixel(12.25, 2, 0.3, 1, 12.25, 12.25)).toBe(12.25);
  });

  it('draws only a full-size frame at 1:1 unfiltered', () => {
    const size = { width: 1035, scale: 1035 / 6000 };
    expect(drawsOneToOne(6000, size, 6000 / 1035, 1)).toBe(true);
    // The fit frame still shown while the full one loads stays filtered.
    expect(drawsOneToOne(1536, size, 6000 / 1035, 1)).toBe(false);
    expect(drawsOneToOne(6000, size, 3, 1)).toBe(false);
    expect(drawsOneToOne(0, size, 6000 / 1035, 1)).toBe(false);
  });
});
