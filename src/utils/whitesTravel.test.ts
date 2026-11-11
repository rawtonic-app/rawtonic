import { describe, expect, it } from 'vitest';
import {
  normalizeLoadedAdjustments,
  whitesAmountV17,
  whitesValueV17,
  withCarriedWhitesFor,
  withPortableWhites,
  type Adjustments
} from './adjustments';

const recipe = (values: Record<string, unknown>) => values as unknown as Adjustments;
const layer = (whites: number) =>
  ({ id: 'l', adjustments: { whites } }) as unknown as NonNullable<Adjustments['masks']>[number];

describe('Whites carried between photos', () => {
  it('maps the slider as the engine does, and back exactly', () => {
    expect(whitesAmountV17(50)).toBe(74);
    expect(whitesAmountV17(100)).toBe(100);
    expect(whitesAmountV17(-60)).toBe(-60);
    for (let value = -100; value <= 100; value++) expect(whitesValueV17(whitesAmountV17(value))).toBe(value);
  });

  it('keeps the amount of a keyless legacy source on a version-17 photo', () => {
    const legacy = normalizeLoadedAdjustments(recipe({ whites: 50 }), true);
    const carried = withPortableWhites<Partial<Adjustments>>({ whites: legacy.whites }, legacy, true);
    // Renders 50 at the source; on a version-17 photo the slider value that renders 50.
    const onto17 = withCarriedWhitesFor(carried, recipe({ toneVersion: 17 }));
    expect(onto17.toneVersion).toBeUndefined();
    expect(whitesAmountV17(onto17.whites!)).toBeCloseTo(50, 9);
  });

  it("leaves a version-16 destination's tone version and layers as they were", () => {
    const source = recipe({ toneVersion: 17, whites: 50 });
    const destination = recipe({ toneVersion: 16, whites: 0, masks: [layer(50)] });
    const carried = withCarriedWhitesFor(withPortableWhites({ whites: 50 }, source, true), destination);
    const pasted = { ...destination, ...carried };
    expect(pasted.toneVersion).toBe(16);
    expect(pasted.whites).toBe(74);
    expect(pasted.masks![0].adjustments.whites).toBe(50);
  });

  it("re-expresses carried layers' Whites, and leaves settings that name their tone version", () => {
    const carried = withPortableWhites({ masks: [layer(50)] }, recipe({ toneVersion: 17 }), true);
    expect(carried.masks![0].adjustments.whites).toBe(74);
    expect(withCarriedWhitesFor(carried, recipe({ toneVersion: 17 })).masks![0].adjustments.whites).toBe(50);
    const named = { whites: 50, toneVersion: 17 };
    expect(withPortableWhites(named, recipe({ toneVersion: 17 }), true)).toBe(named);
    expect(withCarriedWhitesFor(named, recipe({ toneVersion: 16 }))).toBe(named);
  });
});
