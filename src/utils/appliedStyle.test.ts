import { describe, expect, it } from 'vitest';
import cases from '../../crates/engine/src/applied_style_cases.json';
import rustSource from '../../crates/engine/src/applied_style.rs?raw';
import { APPLIED_STYLE, PHOTO_OWN_KEYS, appliedStyleOf, recordStyle, revertStyle, withStyleAmount, withStyleValues } from './appliedStyle';

describe('applied style', () => {
  // The cases the backend checks too (crates/engine/src/applied_style.rs).
  it.each(cases.map((c) => [c.name, c] as const))('%s', (_, c) => {
    const { recipe, kept } = revertStyle(c.recipe);
    expect(recipe).toEqual(c.reverted);
    expect(kept).toEqual(c.kept);
  });

  it('records every setting a style sets, nested settings one by one', () => {
    const before = { exposure: 0, contrast: 10, hsl: { reds: { hue: 0, saturation: 0 } } };
    const after = recordStyle(
      before,
      { exposure: 1, contrast: 10, hsl: { reds: { hue: 5, saturation: 0 } }, grainVersion: 2 },
      { exposure: 1, contrast: 10, hsl: { reds: { hue: 5, saturation: 0 } }, grainVersion: 2, [APPLIED_STYLE]: {} },
      'warm',
    );
    expect(appliedStyleOf(after)).toEqual({
      id: 'warm',
      amount: 100,
      changes: [
        { path: ['exposure'], from: 0, to: 1 },
        { path: ['contrast'], from: 10, to: 10 },
        { path: ['hsl', 'reds', 'hue'], from: 0, to: 5 },
        { path: ['hsl', 'reds', 'saturation'], from: 0, to: 0 },
        { path: ['grainVersion'], to: 2 },
      ],
    });
    const { recipe, removed, kept } = revertStyle({ ...after, contrast: 30 });
    expect(recipe).toEqual({ ...before, contrast: 30 });
    expect(kept).toEqual([['contrast']]);
    expect(removed).toBe(true);
  });

  it('keeps a user change through every later Amount move', () => {
    const base = { saturation: 0, contrast: 0 };
    let recipe = recordStyle(base, { saturation: -100, contrast: 40 }, { saturation: -100, contrast: 40 }, 'bw');
    recipe = { ...recipe, saturation: -30 };
    const values = { saturation: -100, contrast: 40 };
    recipe = withStyleAmount(recipe, revertStyle(recipe).recipe, values, 50);
    expect(recipe).toMatchObject({ saturation: -30, contrast: 20 });
    recipe = withStyleAmount(recipe, revertStyle(recipe).recipe, values, 60);
    expect(recipe).toMatchObject({ saturation: -30, contrast: 24, appliedStyle: { amount: 60 } });
    // Taken back, the user's saturation stays.
    expect(revertStyle(recipe).recipe).toEqual({ saturation: -30, contrast: 0 });
  });

  it('blends from the editor\'s value where a saved recipe had none, and removes it again', () => {
    // A sparse saved recipe without exposure; the style set +1.
    let recipe = recordStyle({ renderVersion: 6 }, { renderVersion: 6, exposure: 1, clarityVersion: 3 }, { exposure: 1, clarityVersion: 3 }, 'b');
    const filled = { ...revertStyle(recipe).recipe, exposure: 0, clarityVersion: 1 };
    const values = { exposure: 1, clarityVersion: 3 };
    recipe = withStyleAmount(recipe, filled, values, 75);
    expect(recipe).toMatchObject({ exposure: 0.75, clarityVersion: 3 });
    recipe = withStyleAmount(recipe, filled, values, 40);
    expect(recipe).toMatchObject({ exposure: 0.4, clarityVersion: 1 });
    // Its full strength is kept in the record once the Amount moved.
    recipe = withStyleAmount(recipe, filled, {}, 80);
    expect(recipe).toMatchObject({ exposure: 0.8, clarityVersion: 3 });
    expect(revertStyle(recipe).recipe).toEqual({ renderVersion: 6 });
  });

  it('joins a style\'s groups to the photo\'s own and takes them back as they were', () => {
    const own = { hsl: { reds: { hue: 0, saturation: 4 }, blues: { hue: 22 } }, masks: [{ id: 'own' }] };
    const values = { hsl: { reds: { hue: 5 } }, masks: [] };
    const after = withStyleValues(own, values);
    expect(after).toEqual({ hsl: { reds: { hue: 5, saturation: 4 }, blues: { hue: 22 } }, masks: [] });
    const recorded = recordStyle(own, after, values, 'red');
    expect(appliedStyleOf(recorded)!.changes.map((c) => c.path)).toEqual([['hsl', 'reds', 'hue']]);
    expect(revertStyle(recorded).recipe).toEqual({ ...own, masks: [] });
  });

  it('blends a slider missing in a partial group from its default', () => {
    // A saved recipe with only the red hue; the style adds the red saturation.
    const own = { hsl: { reds: { hue: 15 } } };
    const values = { hsl: { reds: { saturation: 40 } } };
    const recipe = recordStyle(own, withStyleValues(own, values), values, 'sat');
    const defaults = { hsl: { reds: { hue: 0, saturation: 0, luminance: 0 } } };
    const at75 = withStyleAmount(recipe, revertStyle(recipe).recipe, values, 75, defaults);
    expect(at75.hsl).toEqual({ reds: { hue: 15, saturation: 30 } });
  });

  it('leaves the photo\'s own settings (lens, layers, relight lights) out of the record, as the backend does', () => {
    const own = { lensCorrectionMode: 'manual', lensMaker: 'Mine', exposure: 0, masks: [] };
    const values = { lensCorrectionMode: 'auto', lensMaker: 'Saved', exposure: 1, masks: [{ id: 'sky' }], relightLights: [{ x: 1 }] };
    const recipe = recordStyle(own, { ...own, ...values }, values, 'look');
    expect(appliedStyleOf(recipe)!.changes.map((c) => c.path)).toEqual([['exposure']]);
    expect(revertStyle({ ...recipe, lensMaker: 'Detected' }).recipe).toEqual({ ...own, ...values, lensMaker: 'Detected', exposure: 0 });
    const list = rustSource.slice(rustSource.indexOf('pub const PHOTO_OWN_KEYS'), rustSource.indexOf('];', rustSource.indexOf('pub const PHOTO_OWN_KEYS')));
    expect([...list.matchAll(/"(\w+)"/g)].map((m) => m[1])).toEqual(PHOTO_OWN_KEYS.filter((key) => key !== 'lensProfileFor'));
    expect(list).toContain('LENS_PROFILE_FOR');
  });


  it('keeps an older edit\'s response below half strength (a missing version key stays missing)', () => {
    const own = { highlights: -50 };
    const values = { highlights: 0, toneVersion: 3 };
    const recipe = recordStyle(own, { ...own, ...values }, values, 'tone');
    const at0 = withStyleAmount(recipe, revertStyle(recipe).recipe, values, 0, { highlights: 0, toneVersion: 3 });
    expect(at0).toEqual({ highlights: -50, appliedStyle: expect.anything() });
    expect('toneVersion' in at0).toBe(false);
  });
});
