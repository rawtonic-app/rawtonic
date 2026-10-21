import { describe, expect, it } from 'vitest';
import { blendPreset } from './presetAmount';
import {
  ADJUSTMENT_GROUPS,
  COLOR_NOISE_VERSION,
  COPYABLE_ADJUSTMENT_KEYS,
  INITIAL_ADJUSTMENTS,
  NON_COPYABLE_ADJUSTMENT_KEYS,
  RAW_DETAIL_DEFAULTS,
  TONE_VERSION,
  FRINGE_VERSION,
  RENDER_VERSION,
  DEFRINGE_VERSION,
  SHARPEN_VERSION,
  LUMA_NOISE_VERSION,
  AI_DENOISE_VERSION,
  withAiDenoiseVersion,
  withDiffractionVersion,
  withSharpenVersion,
  withLumaNoiseVersion,
  pastedAdjustments,
  PasteMode,
  AUTO_CA_VERSION,
  autoCaOn,
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
    expect(COLOR_NOISE_VERSION).toBe(9);
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
    // Saved versions stay, also the earlier multi-scale tunings (2–8).
    const versioned = { colorNoiseReduction: 25, colorNoiseVersion: 2 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(versioned, true).colorNoiseVersion).toBe(2);
    const third = { colorNoiseReduction: 25, colorNoiseVersion: 3 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(third, true).colorNoiseVersion).toBe(3);
    const fourth = { colorNoiseReduction: 25, colorNoiseVersion: 4 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(fourth, true).colorNoiseVersion).toBe(4);
    const fifth = { colorNoiseReduction: 25, colorNoiseVersion: 5 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(fifth, true).colorNoiseVersion).toBe(5);
    const sixth = { colorNoiseReduction: 25, colorNoiseVersion: 6 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(sixth, true).colorNoiseVersion).toBe(6);
    const seventh = { colorNoiseReduction: 25, colorNoiseVersion: 7 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(seventh, true).colorNoiseVersion).toBe(7);
    const eighth = { colorNoiseReduction: 25, colorNoiseVersion: 8 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(eighth, true).colorNoiseVersion).toBe(8);
    const first = { colorNoiseReduction: 25, colorNoiseVersion: 1 } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(first, true).colorNoiseVersion).toBe(1);
  });

  it('develop versions 2 to 9 on the same base image (multi-scale filter)', () => {
    expect(colorNoiseMultiscale({ colorNoiseVersion: 9 }, true)).toBe(true);
    expect(colorNoiseMultiscale({ colorNoiseVersion: 8 }, true)).toBe(true);
    expect(colorNoiseMultiscale({ colorNoiseVersion: 7 }, true)).toBe(true);
    expect(colorNoiseMultiscale({ colorNoiseVersion: 6 }, true)).toBe(true);
    expect(colorNoiseMultiscale({ colorNoiseVersion: 5 }, true)).toBe(true);
    expect(colorNoiseMultiscale({ colorNoiseVersion: 4 }, true)).toBe(true);
    expect(colorNoiseMultiscale({ colorNoiseVersion: 3 }, true)).toBe(true);
    expect(colorNoiseMultiscale({ colorNoiseVersion: 2 }, true)).toBe(true);
    expect(colorNoiseMultiscale({ colorNoiseVersion: 1 }, true)).toBe(false);
    expect(colorNoiseMultiscale({ colorNoiseReduction: 25 }, true)).toBe(false);
    expect(colorNoiseMultiscale({}, true)).toBe(true);
    expect(colorNoiseMultiscale({}, false)).toBe(false);
  });

  it('starts new edits on the current render version and keeps saved recipes on theirs', () => {
    expect(initialAdjustmentsFor(true).renderVersion).toBe(RENDER_VERSION);
    expect(initialAdjustmentsFor(false).renderVersion).toBe(RENDER_VERSION);
    const saved = { ...INITIAL_ADJUSTMENTS, exposure: 0.5 } as Adjustments;
    expect('renderVersion' in JSON.parse(JSON.stringify(normalizeLoadedAdjustments(saved, true)))).toBe(false);
    expect('renderVersion' in JSON.parse(JSON.stringify(normalizeLoadedAdjustments({ exposure: 0.5 } as Adjustments, true)))).toBe(false);
    expect(normalizeLoadedAdjustments({ ...saved, renderVersion: 2 }, false).renderVersion).toBe(2);
    expect(COPYABLE_ADJUSTMENT_KEYS).not.toContain('renderVersion');
    // Library Reset saves an empty recipe (loading stamps only its lens
    // lookup): it renders as a new edit, here as in the backend.
    expect(normalizeLoadedAdjustments({} as Adjustments, true).renderVersion).toBe(RENDER_VERSION);
    expect(normalizeLoadedAdjustments({ lensProfileFor: '["manual",null,null,null]' } as Adjustments, true).renderVersion).toBe(
      RENDER_VERSION,
    );
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

  it('keep the Defringe detector a saved recipe rendered with', () => {
    expect(initialAdjustmentsFor(true).defringeVersion).toBe(DEFRINGE_VERSION);
    expect(initialAdjustmentsFor(false).defringeVersion).toBe(DEFRINGE_VERSION);
    // A recipe without Defringe renders the same with either detector.
    const unused = { ...INITIAL_ADJUSTMENTS, sharpness: 40 } as Adjustments;
    expect(normalizeLoadedAdjustments(unused, true).defringeVersion).toBe(DEFRINGE_VERSION);
    // One that uses it keeps the earlier detector (missing key).
    const purple = { ...INITIAL_ADJUSTMENTS, defringePurpleAmount: 10 } as Adjustments;
    expect(normalizeLoadedAdjustments(purple, true).defringeVersion).toBeUndefined();
    expect('defringeVersion' in JSON.parse(JSON.stringify(normalizeLoadedAdjustments(purple, true)))).toBe(false);
    const layer = {
      ...INITIAL_ADJUSTMENTS,
      masks: [{ id: "m", name: "m", visible: true, invert: false, opacity: 100, subMasks: [], adjustments: { defringe: 50 } }],
    } as unknown as Adjustments;
    expect(normalizeLoadedAdjustments(layer, true).defringeVersion).toBeUndefined();
    expect(normalizeLoadedAdjustments({ ...purple, defringeVersion: 2 }, true).defringeVersion).toBe(2);
  });

  it('carry the AI Denoise version with a copied amount', () => {
    // The source's version goes along.
    expect(withAiDenoiseVersion({ aiDenoise: 60 }, { aiDenoiseVersion: 2 }).aiDenoiseVersion).toBe(2);
    // Presets and recipes from before the key: version 1 above 0, the current one at 0.
    expect(withAiDenoiseVersion({ aiDenoise: 60 }).aiDenoiseVersion).toBe(1);
    expect(withAiDenoiseVersion({ aiDenoise: 0 }).aiDenoiseVersion).toBe(AI_DENOISE_VERSION);
    expect(withAiDenoiseVersion({ aiDenoise: 60, aiDenoiseVersion: 3 }, { aiDenoiseVersion: 2 }).aiDenoiseVersion).toBe(3);
    // Nothing to carry without an amount.
    expect('aiDenoiseVersion' in withAiDenoiseVersion({ exposure: 1 } as Partial<Adjustments>)).toBe(false);
  });

  it('carry the diffraction version with a copied or preset correction', () => {
    // A recipe or preset from before the key renders version 1; pasted onto a
    // photo whose recipe says 2, it must still render 1.
    expect(withDiffractionVersion({ diffractionCorrection: true }).diffractionVersion).toBe(1);
    expect(withDiffractionVersion({ diffractionCorrection: true }, {}).diffractionVersion).toBe(1);
    // The source's version goes along; an explicit one stays.
    expect(withDiffractionVersion({ diffractionCorrection: true }, { diffractionVersion: 2 }).diffractionVersion).toBe(2);
    expect(withDiffractionVersion({ diffractionCorrection: true, diffractionVersion: 2 }).diffractionVersion).toBe(2);
    // Nothing to carry when the correction is not copied on.
    expect('diffractionVersion' in withDiffractionVersion({ exposure: 1 } as Partial<Adjustments>)).toBe(false);
  });

  it('keep the AI Denoise version a saved recipe rendered with', () => {
    expect(initialAdjustmentsFor(true).aiDenoiseVersion).toBe(AI_DENOISE_VERSION);
    // Without AI Denoise the version does not change the render: the current one.
    const unused = { ...INITIAL_ADJUSTMENTS, aiDenoise: 0 } as Adjustments;
    expect(normalizeLoadedAdjustments(unused, true).aiDenoiseVersion).toBe(AI_DENOISE_VERSION);
    // With it, a recipe without the key stays on version 1 (missing key).
    const used = { ...INITIAL_ADJUSTMENTS, aiDenoise: 60 } as Adjustments;
    expect(normalizeLoadedAdjustments(used, true).aiDenoiseVersion).toBeUndefined();
    expect('aiDenoiseVersion' in JSON.parse(JSON.stringify(normalizeLoadedAdjustments(used, true)))).toBe(false);
    expect(normalizeLoadedAdjustments({ ...used, aiDenoiseVersion: 2 }, true).aiDenoiseVersion).toBe(2);
  });

  it('start new RAW edits with the automatic lateral CA where the lens data has none', () => {
    const fresh = initialAdjustmentsFor(true);
    expect(fresh.autoCaVersion).toBe(AUTO_CA_VERSION);
    expect(initialAdjustmentsFor(false).autoCaVersion).toBeUndefined();
    expect(autoCaOn(fresh)).toBe(true);
    expect(autoCaOn({ ...fresh, autoChromaticAberration: false })).toBe(false);
    const camera = { ...fresh, lensDistortionParams: { k1: 0, k2: 0, k3: 0, tca_vr: 1.001, tca_vb: 1 } } as Adjustments;
    // Version 3 competes with the camera's own CA data; version 2 leaves it.
    expect(autoCaOn(camera)).toBe(true);
    expect(autoCaOn({ ...camera, autoCaVersion: 2 })).toBe(false);
    expect(autoCaOn({ ...camera, lensTcaAmount: 150 })).toBe(false);
    expect(autoCaOn({ ...camera, autoChromaticAberration: false })).toBe(false);
    expect(autoCaOn({ ...camera, lensTcaEnabled: false })).toBe(true);
    expect(autoCaOn({ ...camera, autoChromaticAberration: true })).toBe(true);
    // Saved recipes keep the switch as it was: off unless set.
    const saved = { ...INITIAL_ADJUSTMENTS, sharpness: 40 } as Adjustments;
    expect(normalizeLoadedAdjustments(saved, true).autoCaVersion).toBeUndefined();
    expect(autoCaOn(normalizeLoadedAdjustments(saved, true))).toBe(false);
    expect(autoCaOn({ ...saved, autoChromaticAberration: true })).toBe(true);
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

  it('keep the sharpening response a saved recipe rendered with', () => {
    expect(initialAdjustmentsFor(true).sharpenVersion).toBe(SHARPEN_VERSION);
    expect(initialAdjustmentsFor(false).sharpenVersion).toBe(SHARPEN_VERSION);
    const load = (recipe: object, isRaw = true) =>
      normalizeLoadedAdjustments(recipe as unknown as Adjustments, isRaw).sharpenVersion;
    // Renders differently with the current response: the earliest response, key absent.
    expect(load({ sharpness: 80 })).toBeUndefined();
    expect('sharpenVersion' in JSON.parse(JSON.stringify(normalizeLoadedAdjustments({ sharpness: 80 } as unknown as Adjustments, true)))).toBe(false);
    expect(load({ sharpness: 40, sharpenDetail: 100 })).toBeUndefined();
    expect(load({ sharpenRadius: 2 })).toBeUndefined();
    expect(load({ sharpenMasking: 30 })).toBeUndefined();
    expect(load({ sharpnessFalloff: 50 })).toBeUndefined();
    expect(load({ sharpness: 0, masks: [{ adjustments: { sharpness: 30 }, subMasks: [] }] })).toBeUndefined();
    // Response 3 weights Amount 40 by lightness too (1 and 2 rendered it alike).
    expect(load({})).toBeUndefined();
    expect(load({ sharpness: 40 })).toBeUndefined();
    // No sharpening renders the same with every response: the current one.
    expect(load({ sharpness: 0 })).toBe(SHARPEN_VERSION);
    expect(load({ sharpness: 0, sharpenDetail: 100 })).toBe(SHARPEN_VERSION);
    expect(load({ sharpness: -20 })).toBe(SHARPEN_VERSION);
    expect(load({}, false)).toBe(SHARPEN_VERSION);
    expect(load({ sharpness: 80, sharpenVersion: 1 })).toBe(1);
    expect(load({ sharpness: 80, sharpenVersion: 2 })).toBe(2);
    expect(load({ sharpness: 40, sharpenVersion: 2 })).toBe(2);
    expect(load({ sharpness: 80, sharpenVersion: 3 })).toBe(3);
  });

  it('keep the luminance noise reduction a saved recipe rendered with', () => {
    expect(initialAdjustmentsFor(true).lumaNoiseVersion).toBe(LUMA_NOISE_VERSION);
    expect(initialAdjustmentsFor(false).lumaNoiseVersion).toBe(LUMA_NOISE_VERSION);
    const load = (recipe: object, isRaw = true) =>
      normalizeLoadedAdjustments(recipe as unknown as Adjustments, isRaw).lumaNoiseVersion;
    // Reduces luminance noise (RAWs at the default 15 without an amount):
    // the earlier filter, key absent.
    expect(load({})).toBeUndefined();
    expect(load({ lumaNoiseReduction: 30 })).toBeUndefined();
    expect(load({ lumaNoiseReduction: 0, masks: [{ adjustments: { lumaNoiseReduction: 20 }, subMasks: [] }] })).toBeUndefined();
    expect('lumaNoiseVersion' in JSON.parse(JSON.stringify(normalizeLoadedAdjustments({} as Adjustments, true)))).toBe(false);
    // No luminance noise reduction renders the same with either: the current one.
    expect(load({ lumaNoiseReduction: 0 })).toBe(LUMA_NOISE_VERSION);
    expect(load({}, false)).toBe(LUMA_NOISE_VERSION);
    expect(load({ lumaNoiseReduction: 30, lumaNoiseVersion: 1 })).toBe(1);
    expect(load({ lumaNoiseReduction: 30, lumaNoiseVersion: 2 })).toBe(2);
  });

  it('carry the luminance noise reduction version with luminance values', () => {
    const all = COPYABLE_ADJUSTMENT_KEYS;
    // A recipe from before the key (version 1 on load): its luminance values
    // paste with version 1, also onto a version-2 photo.
    const old = normalizeLoadedAdjustments({ lumaNoiseReduction: 30 } as unknown as Adjustments, true);
    expect(pastedAdjustments(old, all, PasteMode.Merge).lumaNoiseVersion).toBe(1);
    expect(JSON.parse(JSON.stringify(pastedAdjustments(old, all, PasteMode.Merge))).lumaNoiseVersion).toBe(1);
    const now = { ...INITIAL_ADJUSTMENTS, lumaNoiseReduction: 30, lumaNoiseVersion: LUMA_NOISE_VERSION } as Adjustments;
    expect(pastedAdjustments(now, all, PasteMode.Merge)).toMatchObject({ lumaNoiseReduction: 30, lumaNoiseVersion: LUMA_NOISE_VERSION });
    // Without luminance values the version stays out of the patch.
    const exposure = { ...INITIAL_ADJUSTMENTS, exposure: 0.5, lumaNoiseVersion: 1 } as Adjustments;
    expect('lumaNoiseVersion' in pastedAdjustments(exposure, all, PasteMode.Merge)).toBe(false);
    // Tool copy and Edit all selected: an undefined copied key becomes 1.
    expect(withLumaNoiseVersion({ lumaNoiseReduction: 30, lumaNoiseVersion: undefined }).lumaNoiseVersion).toBe(1);
    expect(withLumaNoiseVersion({ lumaNoiseDetail: 70 }, { lumaNoiseVersion: 2 } as Adjustments).lumaNoiseVersion).toBe(2);
    const layered = { masks: [{ adjustments: { lumaNoiseReduction: 20 }, subMasks: [] }] } as unknown as Adjustments;
    expect(withLumaNoiseVersion(layered, {} as Adjustments).lumaNoiseVersion).toBe(1);
    expect('lumaNoiseVersion' in withLumaNoiseVersion({ exposure: 1 }, {} as Adjustments)).toBe(false);
  });

  it('paste the sharpening response only with sharpening values', () => {
    const all = COPYABLE_ADJUSTMENT_KEYS;
    // Neutral sharpening, response 2, changed exposure: Merge keeps exposure
    // only, so a response-1 photo at Amount 80 keeps its sharpening.
    const neutral = { ...INITIAL_ADJUSTMENTS, exposure: 0.5, sharpenVersion: 2 } as Adjustments;
    expect(pastedAdjustments(neutral, all, PasteMode.Merge)).toEqual({ exposure: 0.5 });
    // Sharpening that survives carries the source's response.
    const sharp = { ...INITIAL_ADJUSTMENTS, sharpness: 80, sharpenVersion: 2 } as Adjustments;
    expect(pastedAdjustments(sharp, all, PasteMode.Merge)).toEqual({ sharpness: 80, sharpenVersion: 2 });
    const old = { ...INITIAL_ADJUSTMENTS, sharpness: 80 } as Adjustments;
    expect(pastedAdjustments(old, all, PasteMode.Merge).sharpenVersion).toBe(1);
    // Replace mode copies every included key, sharpening included, so the response goes along.
    expect(pastedAdjustments(neutral, all, PasteMode.Replace).sharpenVersion).toBe(2);
  });

  it('copy the sharpening response explicitly with sharpening values', () => {
    // From a photo with response 1 (no key): 1, so a version-2 target renders as the source did.
    expect(withSharpenVersion({ sharpness: 80 }, { sharpness: 80 } as Adjustments).sharpenVersion).toBe(1);
    expect(withSharpenVersion({ sharpnessThreshold: 30 }, {} as Adjustments).sharpenVersion).toBe(1);
    expect(JSON.parse(JSON.stringify(withSharpenVersion({ sharpenDetail: 100 }, {} as Adjustments))).sharpenVersion).toBe(1);
    // From a photo with response 2: 2.
    expect(withSharpenVersion({ sharpness: 80 }, { sharpenVersion: 2 } as Adjustments).sharpenVersion).toBe(2);
    // Values that carry their own key keep it.
    expect(withSharpenVersion({ sharpness: 80, sharpenVersion: 2 }, {} as Adjustments).sharpenVersion).toBe(2);
    // Presets from before the key (apply, preview, preset Amount: no source): 1,
    // whatever the values — a Radius-only preset must not move a version-1
    // photo at Amount 80 to response 2.
    expect(withSharpenVersion({ sharpness: 80 }).sharpenVersion).toBe(1);
    expect(withSharpenVersion({ sharpness: 40 }).sharpenVersion).toBe(1);
    expect(withSharpenVersion({ sharpenRadius: 1 }).sharpenVersion).toBe(1);
    const target = { sharpness: 80, sharpenRadius: 1 } as Partial<Adjustments>;
    expect({ ...target, ...withSharpenVersion({ sharpenRadius: 1 }) }.sharpenVersion).toBe(1);
    // A preset at full Amount takes its version; blending keeps version keys whole.
    expect(blendPreset({ sharpness: 80 }, withSharpenVersion({ sharpenRadius: 2 }), 1).sharpenVersion).toBe(1);
    // Layers that sharpen count; nothing sharpening, no key.
    const layered = { masks: [{ adjustments: { sharpness: 30 }, subMasks: [] }] } as unknown as Partial<Adjustments>;
    expect(withSharpenVersion(layered, {} as Adjustments).sharpenVersion).toBe(1);
    expect('sharpenVersion' in withSharpenVersion({ exposure: 1 }, {} as Adjustments)).toBe(false);
    // A change back to no key (undo) is response 1, explicitly.
    expect(withSharpenVersion({ sharpenVersion: undefined }, { sharpenVersion: 2 } as Adjustments).sharpenVersion).toBe(1);
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
