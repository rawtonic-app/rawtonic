import { describe, expect, it } from 'vitest';
import { guideCount, guideOrientation, guideRejection } from './guideLines';
import type { GuideLine } from './adjustments';

const line = (type: 'vertical' | 'horizontal', id: string): GuideLine => ({
  id,
  type,
  p1: { x: 0, y: 0 },
  p2: { x: 1, y: 1 },
});

describe('guided upright guides', () => {
  it('counts lines within 35° of an axis for that axis', () => {
    expect(guideOrientation(10, 100)).toBe('vertical');
    expect(guideOrientation(100, -30)).toBe('horizontal');
    expect(guideOrientation(100, 100)).toBeNull();
  });

  it('rejects short, diagonal and surplus guides', () => {
    const two = [line('vertical', 'a'), line('vertical', 'b')];
    expect(guideRejection([], 'vertical', 10)).toBe('short');
    expect(guideRejection([], null, 100)).toBe('angle');
    expect(guideRejection(two, 'vertical', 100)).toBe('full');
    expect(guideRejection(two, 'horizontal', 100)).toBeNull();
    expect(guideCount([...two, line('horizontal', 'c')])).toBe('2 vertical · 1 horizontal');
  });
});
