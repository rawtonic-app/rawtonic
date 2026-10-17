import { describe, expect, it } from 'vitest';
import {
  ADJUSTMENT_GROUPS,
  COLOR_NOISE_VERSION,
  COPYABLE_ADJUSTMENT_KEYS,
  INITIAL_ADJUSTMENTS,
  NON_COPYABLE_ADJUSTMENT_KEYS,
  RAW_DETAIL_DEFAULTS,
  TONE_VERSION,
  FRINGE_VERSION,
  colorNoiseMultiscale,
  initialAdjustmentsFor,
  normalizeLoadedAdjustments,
  sameAdjustmentValue,
  effectiveGrainStructure,
  withGrainStructure,
  withNewGrainStructure,
  type Adjustments,
} from './adjustments';
import settings from '../../src-tauri/src/app_settings.rs?raw';

describe('RAW detail defaults', () => {
  it('start new RAW photos with Lightroom capture sharpening and colour noise reduction', () => {
    const raw = initialAdjustmentsFor(true);
    expect(raw.sharpness).toBe(RAW_DETAIL_DEFAULTS.sharpness);
    expect(raw.colorNoiseReduction).toBe(RAW_DETAIL_DEFAULTS.colorNoiseReduction);
    expect(raw.lumaNoiseReduction).toBe(RAW_DETAIL_DEFAULTS.lumaNoiseReduction);
    const other = initialAdjustmentsFor(false);
    expect(other.sharpness).toBe(0);
    expect(other.colorNoiseReduction).toBe(0);
    expect(INITIAL_ADJUSTMENTS.sharpness).toBe(0);
  });

  it('fill only missing values of saved RAW recipes, keeping saved zeros', () => {
    const saved = { sharpness: 0, colorNoiseReduction: 0 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(saved, true).sharpness).toBe(0);
    expect(normalizeLoadedAdjustments(saved, true).colorNoiseReduction).toBe(0);
    const partial = { exposure: 0.5 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(partial, true).sharpness).toBe(40);
    expect(normalizeLoadedAdjustments(partial, false).sharpness).toBe(0);
  });

  it('keep the colour noise filter a saved recipe rendered with', () => {
    expect(COLOR_NOISE_VERSION).toBe(3);
    expect(initialAdjustmentsFor(true).colorNoiseVersion).toBe(COLOR_NOISE_VERSION);
    expect(initialAdjustmentsFor(false).colorNoiseVersion).toBe(COLOR_NOISE_VERSION);
    // Saved colour amount without a version: the earlier filter.
    const saved = { colorNoiseReduction: 25 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(saved, true).colorNoiseVersion).toBeUndefined();
    expect('colorNoiseVersion' in JSON.parse(JSON.stringify(normalizeLoadedAdjustments(saved, true)))).toBe(false);
    // Left to the RAW default: the current one, as the backend renders it.
    const partial = { exposure: 0.5 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(partial, true).colorNoiseVersion).toBe(COLOR_NOISE_VERSION);
    expect(normalizeLoadedAdjustments(partial, false).colorNoiseVersion).toBeUndefined();
    // Saved versions stay, also the earlier multi-scale tuning (2).
    const versioned = { colorNoiseReduction: 25, colorNoiseVersion: 2 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(versioned, true).colorNoiseVersion).toBe(2);
    const first = { colorNoiseReduction: 25, colorNoiseVersion: 1 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(first, true).colorNoiseVersion).toBe(1);
  });

  it('develop versions 2 and 3 on the same base image (multi-scale filter)', () => {
    expect(colorNoiseMultiscale({ colorNoiseVersion: 3 }, true)).toBe(true);
    expect(colorNoiseMultiscale({ colorNoiseVersion: 2 }, true)).toBe(true);
    expect(colorNoiseMultiscale({ colorNoiseVersion: 1 }, true)).toBe(false);
    expect(colorNoiseMultiscale({ colorNoiseReduction: 25 }, true)).toBe(false);
    expect(colorNoiseMultiscale({}, true)).toBe(true);
    expect(colorNoiseMultiscale({}, false)).toBe(false);
  });

  it('start new RAW edits with the edge-fringe pass and keep saved recipes without it', () => {
    expect(initialAdjustmentsFor(true).fringeVersion).toBe(FRINGE_VERSION);
    expect(initialAdjustmentsFor(false).fringeVersion).toBeUndefined();
    const saved = { ...INITIAL_ADJUSTMENTS, sharpness: 40 } as Adjustments;
    expect(normalizeLoadedAdjustments(saved, true).fringeVersion).toBeUndefined();
    expect('fringeVersion' in JSON.parse(JSON.stringify(normalizeLoadedAdjustments(saved, true)))).toBe(false);
    const versioned = { ...INITIAL_ADJUSTMENTS, fringeVersion: 1 } as Adjustments;
    expect(normalizeLoadedAdjustments(versioned, true).fringeVersion).toBe(1);
  });

  it('keep the Shadows response a saved recipe rendered with', () => {
    expect(initialAdjustmentsFor(true).toneVersion).toBe(TONE_VERSION);
    expect(initialAdjustmentsFor(false).toneVersion).toBe(TONE_VERSION);
    // Shadows above +50 without a version: the earlier response, key absent.
    const strong = { shadows: 80 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(strong, true).toneVersion).toBeUndefined();
    expect('toneVersion' in JSON.parse(JSON.stringify(normalizeLoadedAdjustments(strong, true)))).toBe(false);
    const layered = { shadows: 0, masks: [{ adjustments: { shadows: 100 }, subMasks: [] }] } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(layered, true).toneVersion).toBeUndefined();
    // Nothing that renders differently: the current response.
    const lowered = { highlights: -60 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(lowered, true).toneVersion).toBeUndefined();
    const layerLowered = { masks: [{ adjustments: { highlights: -10 }, subMasks: [] }] } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(layerLowered, true).toneVersion).toBeUndefined();
    const mild = { shadows: 50, highlights: 40, exposure: 1 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(mild, true).toneVersion).toBe(TONE_VERSION);
    const versioned = { shadows: 80, toneVersion: 2 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(versioned, true).toneVersion).toBe(2);
    const old = { shadows: 80, toneVersion: 1 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(old, true).toneVersion).toBe(1);
  });
});

describe('sameAdjustmentValue', () => {
  it('ignores object key order and undefined members', () => {
    expect(sameAdjustmentValue({ hue: 0, saturation: 0 }, { saturation: 0, hue: 0 })).toBe(true);
    expect(sameAdjustmentValue({ a: 1, b: undefined }, { a: 1 })).toBe(true);
    expect(sameAdjustmentValue([{ x: 1, y: 2 }], [{ y: 2, x: 1 }])).toBe(true);
  });
  it('still tells different values apart', () => {
    expect(sameAdjustmentValue({ hue: 0 }, { hue: 1 })).toBe(false);
    expect(sameAdjustmentValue([1, 2], [2, 1])).toBe(false);
    expect(sameAdjustmentValue({}, [])).toBe(false);
    expect(sameAdjustmentValue(null, {})).toBe(false);
    expect(sameAdjustmentValue({ a: 1 }, { a: 1, b: 2 })).toBe(false);
  });
});

describe('copy and paste', () => {
  const rustList = (source: string, start: RegExp) => {
    const body = source.slice(source.search(start));
    const list = body.slice(body.indexOf('[') + 1, body.indexOf(']'));
    return [...list.matchAll(/"([^"]+)"/g)].map((match) => match[1]).sort();
  };

  it('copies every adjustment except the per-photo ones', () => {
    const forgotten = Object.keys(INITIAL_ADJUSTMENTS).filter(
      (key) => !COPYABLE_ADJUSTMENT_KEYS.includes(key) && !NON_COPYABLE_ADJUSTMENT_KEYS.includes(key),
    );
    expect(forgotten).toEqual([]);
  });

  it('keeps the backend copy and paste keys equal to the frontend groups', () => {
    // The backend decides what Paste applies (settings.copyPasteSettings) and adds
    // new keys to existing settings; a key missing there is silently not pasted.
    expect(rustList(settings, /pub fn all_available_adjustments\(\)/)).toEqual(
      [...new Set(COPYABLE_ADJUSTMENT_KEYS)].sort(),
    );
    const offByDefault = [...ADJUSTMENT_GROUPS.geometry, ...ADJUSTMENT_GROUPS.masks].flatMap((group) => group.keys);
    expect(rustList(settings, /let off_by_default = /)).toEqual([...offByDefault].sort());
  });
});

describe('Grain structure defaults', () => {
  const old = { ...INITIAL_ADJUSTMENTS, grainAmount: 40 } as Adjustments;

  it('keeps recipes and presets from before the choice Even and starts new grain as Clumped', () => {
    expect(INITIAL_ADJUSTMENTS.grainStructure).toBeUndefined();
    expect(effectiveGrainStructure(old)).toBe('even');
    expect(effectiveGrainStructure(INITIAL_ADJUSTMENTS)).toBe('clumped');
    expect(normalizeLoadedAdjustments(old).grainStructure).toBe('even');
    expect(normalizeLoadedAdjustments({ ...INITIAL_ADJUSTMENTS } as Adjustments).grainStructure).toBeUndefined();
    // Old presets and copied grain carry the structure they render with.
    expect(withGrainStructure({ grainAmount: 30 })).toEqual({ grainAmount: 30, grainStructure: 'even' });
    expect(withGrainStructure({ grainAmount: 30, grainStructure: 'soft' }).grainStructure).toBe('soft');
    expect(withGrainStructure({ exposure: 1 })).toEqual({ exposure: 1 });
  });

  it('gives grain turned on in a recipe without a structure the new default only', () => {
    expect(withNewGrainStructure(INITIAL_ADJUSTMENTS, { ...INITIAL_ADJUSTMENTS, grainAmount: 20 }).grainStructure).toBe(
      'clumped',
    );
    // Changing grain that is already on keeps what it renders with.
    expect(withNewGrainStructure(old, { ...old, grainAmount: 60 }).grainStructure).toBeUndefined();
    expect(
      withNewGrainStructure(INITIAL_ADJUSTMENTS, { ...INITIAL_ADJUSTMENTS, grainAmount: 20, grainStructure: 'gritty' })
        .grainStructure,
    ).toBe('gritty');
  });
});
