import { describe, expect, it } from 'vitest';
import { GRAIN_VERSION, INITIAL_ADJUSTMENTS, type Adjustments } from '../../utils/adjustments';
import { pick } from './StudioToolCopy';

describe('Tool copy of grain', () => {
  it('copies the source grain version, never downgrades it', () => {
    // A source that never used grain: its Size goes with the newest version.
    const fresh = { ...INITIAL_ADJUSTMENTS } as Adjustments;
    expect(pick(fresh, ['grainSize']).grainVersion).toBe(GRAIN_VERSION);
    const v1 = { ...INITIAL_ADJUSTMENTS, grainAmount: 30, grainVersion: 1 } as Adjustments;
    expect(pick(v1, ['grainSize']).grainVersion).toBe(1);
    const v2 = { ...INITIAL_ADJUSTMENTS, grainAmount: 30, grainVersion: 2 } as Adjustments;
    expect(pick(v2, ['grainAmount', 'grainSize']).grainVersion).toBe(2);
    expect('grainVersion' in pick(v2, ['exposure'])).toBe(false);
  });
});
