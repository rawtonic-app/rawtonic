import { describe, expect, it } from 'vitest';
import { withFrameSize } from './frameSize';
import type { Adjustments } from './adjustments';

describe('withFrameSize', () => {
  it('stamps an edited recipe with the photo size', () => {
    const recipe = { exposure: 1 } as Partial<Adjustments>;
    expect(withFrameSize(recipe, 2496, 1662)).toEqual({ exposure: 1, frameSize: { width: 2496, height: 1662 } });
    expect(recipe).toEqual({ exposure: 1 });
  });

  it('replaces another size and keeps the same one', () => {
    const recipe = { exposure: 1, frameSize: { width: 288, height: 192 } } as Partial<Adjustments>;
    expect(withFrameSize(recipe, 2496, 1662).frameSize).toEqual({ width: 2496, height: 1662 });
    const same = { exposure: 1, frameSize: { width: 2496, height: 1662 } } as Partial<Adjustments>;
    expect(withFrameSize(same, 2496, 1662)).toBe(same);
  });

  it('leaves new recipes and unknown sizes alone', () => {
    const reset = {} as Partial<Adjustments>;
    expect(withFrameSize(reset, 2496, 1662)).toBe(reset);
    const frame = { sourceFrameVersion: 3 } as Partial<Adjustments>;
    expect(withFrameSize(frame, 2496, 1662)).toBe(frame);
    const recipe = { exposure: 1 } as Partial<Adjustments>;
    expect(withFrameSize(recipe, 0, 0)).toBe(recipe);
  });
});
