import { describe, expect, it } from 'vitest';
import {
  shadowsAmountV21,
  shadowsValueV21,
  withCarriedWhitesFor,
  withPortableWhites,
  type Adjustments
} from './adjustments';

const recipe = (values: Record<string, unknown>) => values as unknown as Adjustments;
const layer = (shadows: number) =>
  ({ id: 'l', adjustments: { shadows } }) as unknown as NonNullable<Adjustments['masks']>[number];

describe('Shadows carried between photos', () => {
  it('maps the slider as the engine does, and back exactly', () => {
    expect(shadowsAmountV21(25)).toBeCloseTo(35.4, 12);
    expect(shadowsAmountV21(50)).toBe(61);
    expect(shadowsAmountV21(75)).toBeCloseTo(83, 12);
    expect(shadowsAmountV21(100)).toBe(100);
    expect(shadowsAmountV21(-60)).toBe(-60);
    expect(shadowsValueV21(80)).toBeCloseTo(71.211231, 5);
    for (let value = -100; value <= 100; value++) expect(shadowsValueV21(shadowsAmountV21(value))).toBe(value);
  });

  it('keeps the amount of a version-20 source on a version-21 photo', () => {
    const carried = withPortableWhites<Partial<Adjustments>>({ shadows: 61 }, recipe({ toneVersion: 20 }), true);
    expect(carried.shadows).toBe(61);
    const onto21 = withCarriedWhitesFor(carried, recipe({ toneVersion: 21 }));
    expect(onto21.toneVersion).toBeUndefined();
    expect(onto21.shadows).toBe(50);
  });

  it("leaves a version-20 destination's tone version and layers as they were", () => {
    const source = recipe({ toneVersion: 21, shadows: 50 });
    const destination = recipe({ toneVersion: 20, shadows: 0, masks: [layer(50)] });
    const carried = withCarriedWhitesFor(withPortableWhites({ shadows: 50 }, source, true), destination);
    const pasted = { ...destination, ...carried };
    expect(pasted.toneVersion).toBe(20);
    expect(pasted.shadows).toBe(61);
    expect(pasted.masks![0].adjustments.shadows).toBe(50);
  });

  it("re-expresses carried layers' Shadows, and leaves settings that name their tone version", () => {
    const carried = withPortableWhites({ masks: [layer(50)] }, recipe({ toneVersion: 21 }), true);
    expect(carried.masks![0].adjustments.shadows).toBe(61);
    expect(withCarriedWhitesFor(carried, recipe({ toneVersion: 21 })).masks![0].adjustments.shadows).toBe(50);
    const named = { shadows: 50, toneVersion: 21 };
    expect(withPortableWhites(named, recipe({ toneVersion: 21 }), true)).toBe(named);
    expect(withCarriedWhitesFor(named, recipe({ toneVersion: 20 }))).toBe(named);
  });
});
