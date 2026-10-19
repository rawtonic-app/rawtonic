import { describe, expect, it } from 'vitest';
import { keepsCropOnEntry } from './cropUtils';

const px = (x: number, y: number, width: number, height: number) => ({ unit: 'px' as const, x, y, width, height });

describe('opening the crop tool', () => {
  it("keeps a saved crop that fits the frame at its rotation (Auto's after a restart)", () => {
    expect(keepsCropOnEntry(px(2287, 305, 1200, 800), 6000, 4000, -5)).toBe(true);
    expect(keepsCropOnEntry(px(1200, 800, 2400, 1600), 6000, 4000, 3)).toBe(true);
  });

  it('fits again only a crop that does not fit the frame at its rotation', () => {
    // The full frame does not fit once turned by 5 degrees.
    expect(keepsCropOnEntry(px(0, 0, 6000, 4000), 6000, 4000, -5)).toBe(false);
    expect(keepsCropOnEntry(px(0, 0, 6000, 4000), 6000, 4000, 0)).toBe(true);
    expect(keepsCropOnEntry(null, 6000, 4000, 0)).toBe(false);
  });
});
