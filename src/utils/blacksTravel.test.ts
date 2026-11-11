import { describe, expect, it } from 'vitest';
import {
  blacksAmountV18,
  blacksSetBlackPoint,
  blacksValueV18,
  withCarriedBlacksFor,
  withPortableWhites,
  type Adjustments
} from './adjustments';
import { expressPastedSettings } from './pastedVersions';

const recipe = (values: Record<string, unknown>) => values as unknown as Adjustments;
const layer = (blacks: number) =>
  ({ id: 'l', adjustments: { blacks } }) as unknown as NonNullable<Adjustments['masks']>[number];
const RAW = true;
const JPEG = false;

/** Blacks copied from `source` and applied to `destination` (keyless or keyed). */
const paste = (
  settings: Partial<Adjustments>,
  source: Partial<Adjustments>,
  sourceIsRaw: boolean,
  destination: Partial<Adjustments>,
  destinationIsRaw: boolean
) =>
  expressPastedSettings(withPortableWhites(settings, source, sourceIsRaw), {
    recipe: destination,
    isRaw: destinationIsRaw,
    replaceLayers: true,
    isNew: false
  });

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

  it('sets the black point only on a RAW with the RAW look', () => {
    expect(blacksSetBlackPoint(recipe({}), RAW)).toBe(true);
    expect(blacksSetBlackPoint(recipe({ toneMapper: 'standard' }), RAW)).toBe(true);
    expect(blacksSetBlackPoint(recipe({ toneMapper: 'agx' }), RAW)).toBe(false);
    expect(blacksSetBlackPoint(recipe({ toneMapper: 'basic' }), RAW)).toBe(false);
    expect(blacksSetBlackPoint(recipe({}), JPEG)).toBe(false);
  });

  it('carries a RAW-look version 18 Blacks as the amount and re-expresses it on each destination', () => {
    const v18 = recipe({ toneVersion: 18 });
    const carried = withPortableWhites({ blacks: -100, masks: [layer(-50)] }, v18, RAW);
    expect(carried.blacks).toBeCloseTo(-200, 9);
    expect(carried.masks![0].adjustments.blacks).toBe(-50);
    const onto18 = withCarriedBlacksFor(carried, v18, RAW);
    expect(onto18.blacks).toBe(-100);
    expect(onto18.masks![0].adjustments.blacks).toBe(-50);
    // Older RAW edits: their deepest. Photos without the black point: the amount, at most −100.
    expect(withCarriedBlacksFor(carried, recipe({ toneVersion: 17 }), RAW).blacks).toBe(-100);
    expect(withCarriedBlacksFor(carried, v18, JPEG).blacks).toBe(-100);
    expect(withCarriedBlacksFor(carried, recipe({ toneVersion: 18, toneMapper: 'agx' }), RAW).blacks).toBe(-100);
  });

  it('keeps Blacks of photos without the black point as their slider value, keyless and keyed', () => {
    const jpeg17 = recipe({ toneVersion: 17 });
    const jpeg18 = recipe({ toneVersion: 18 });
    // JPEG onto JPEG across versions: unchanged (the review's case).
    expect(paste({ blacks: -100 }, jpeg17, JPEG, jpeg18, JPEG).blacks).toBe(-100);
    expect(paste({ blacks: -72 }, jpeg18, JPEG, jpeg17, JPEG).blacks).toBe(-72);
    expect(paste({ blacks: -100, toneVersion: 17 }, jpeg17, JPEG, { ...jpeg18, shadows: 10 }, JPEG).blacks).toBe(-100);
    // AgX / Classic RAWs alike.
    const agx18 = recipe({ toneVersion: 18, toneMapper: 'agx' });
    expect(paste({ blacks: -80 }, agx18, RAW, recipe({ toneVersion: 17, toneMapper: 'basic' }), RAW).blacks).toBe(-80);
    // A JPEG's −100 is the amount −100: on a version-18 RAW-look photo that is version 16's −100.
    expect(paste({ blacks: -100 }, jpeg17, JPEG, jpeg18, RAW).blacks).toBeCloseTo(-71.551724, 5);
  });

  it('translates RAW-look Blacks exactly, keyless and keyed, and only for the RAW look', () => {
    const raw17 = recipe({ toneVersion: 17, shadows: 10 });
    const raw18 = recipe({ toneVersion: 18, shadows: 10 });
    // Keyless.
    expect(paste({ blacks: -100 }, raw17, RAW, raw18, RAW).blacks).toBeCloseTo(-71.551724, 5);
    expect(paste({ blacks: -100 }, raw18, RAW, raw17, RAW).blacks).toBe(-100);
    expect(paste({ blacks: -50 }, raw18, RAW, raw17, RAW).blacks).toBe(-50);
    // Keyed, the destination keeping its key (its Shadows renders with it).
    const keyed = paste({ blacks: -100, toneVersion: 17 }, raw17, RAW, raw18, RAW);
    expect(keyed.toneVersion).toBe(18);
    expect(keyed.blacks).toBeCloseTo(-71.551724, 5);
    // Keyed, the key going along (nothing on the destination renders with it): the value as it was.
    const along = paste({ blacks: -100, toneVersion: 18 }, raw18, RAW, recipe({ toneVersion: 17 }), RAW);
    expect(along.blacks).toBe(-100);
    // RAW-look Blacks onto a JPEG or an AgX RAW: the amount, at most −100.
    expect(paste({ blacks: -72 }, raw18, RAW, recipe({ toneVersion: 18 }), JPEG).blacks).toBeCloseTo(-100, 4);
    expect(paste({ blacks: -40 }, raw18, RAW, recipe({ toneVersion: 18, toneMapper: 'agx' }), RAW).blacks).toBe(-40);
  });
});
