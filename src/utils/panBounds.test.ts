import { describe, expect, it } from 'vitest';
import { lockedAxes, panStep } from './panBounds';

describe('viewer panning', () => {
  it('does not move a photograph that fits the viewer', () => {
    const fit = { minX: 40, maxX: 40, minY: 0, maxY: 0 };
    expect(lockedAxes(fit)).toEqual({ x: true, y: true });
    expect(panStep(fit, 40, 0, 25, -12)).toEqual({ dx: 0, dy: 0 });
  });

  it('moves only along an axis with room, resisting beyond its edges', () => {
    const wide = { minX: -800, maxX: 0, minY: 30, maxY: 30 };
    expect(panStep(wide, -100, 30, 20, 15)).toEqual({ dx: 20, dy: 0 });
    expect(panStep(wide, 10, 30, 20, 0)).toEqual({ dx: 7, dy: 0 });
    expect(panStep(wide, 10, 30, -20, 0)).toEqual({ dx: -20, dy: 0 });
  });
});
