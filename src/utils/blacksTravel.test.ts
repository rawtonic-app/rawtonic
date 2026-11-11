import { describe, expect, it } from 'vitest';
import {
  blacksAmountV18,
  blacksValueV18,
  withCarriedWhitesFor,
  withPortableWhites,
  type Adjustments
} from './adjustments';

const recipe = (values: Record<string, unknown>) => values as unknown as Adjustments;
const layer = (blacks: number) =>
  ({ id: 'l', adjustments: { blacks } }) as unknown as NonNullable<Adjustments['masks']>[number];

describe('Blacks carried between photos', () => {
  it('maps the slider as the engine does, and back exactly', () => {
    expect(blacksAmountV18(-50)).toBe(-50);
    expect(blacksAmountV18(-30)).toBe(-30);
    expect(blacksAmountV18(-100)).toBeCloseTo(-200, 9);
    expect(blacksAmountV18(60)).toBe(60);
    expect(blacksValueV18(-100)).toBeCloseTo(-71.551724, 5);
    expect(blacksValueV18(-250)).toBe(-100);
    for (let value = -100; value <= 100; value++) expect(blacksValueV18(blacksAmountV18(value))).toBe(value);
  });

  it('carries version 18 Blacks as the amount and re-expresses it on each destination', () => {
    const carried = withPortableWhites({ blacks: -100, masks: [layer(-50)] }, recipe({ toneVersion: 18 }));
    expect(carried.blacks).toBeCloseTo(-200, 9);
    expect(carried.masks![0].adjustments.blacks).toBe(-50);
    // Onto version 18 the slider values come back; onto 16 / 17 the deeper amount ends at −100.
    const onto18 = withCarriedWhitesFor(carried, recipe({ toneVersion: 18 }));
    expect(onto18.blacks).toBe(-100);
    expect(onto18.masks![0].adjustments.blacks).toBe(-50);
    expect(withCarriedWhitesFor(carried, recipe({ toneVersion: 17 })).blacks).toBe(-100);
  });

  it("puts a version-16 -100 at version 18's -71.55, and leaves settings that name their tone version", () => {
    const carried = withPortableWhites({ blacks: -100 }, recipe({ toneVersion: 16 }));
    expect(carried.blacks).toBe(-100);
    expect(withCarriedWhitesFor(carried, recipe({ toneVersion: 18 })).blacks).toBeCloseTo(-71.551724, 5);
    const named = { blacks: -100, toneVersion: 18 };
    expect(withPortableWhites(named, recipe({ toneVersion: 18 }))).toBe(named);
    expect(withCarriedWhitesFor(named, recipe({ toneVersion: 16 }))).toBe(named);
  });
});
