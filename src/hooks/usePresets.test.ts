import { describe, expect, it, vi } from 'vitest';
import { collectPresetAdjustments } from './usePresets';
import {
  CLARITY_VERSION,
  LUMA_NOISE_VERSION,
  SHARPEN_VERSION,
  initialAdjustmentsFor,
  withClarityVersion,
  withPresetClarityVersion,
  type Adjustments,
} from '../utils/adjustments';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));

const options = { includeMasks: false, includeCropTransform: false, presetType: 'tool' as const };

describe('collectPresetAdjustments', () => {
  it('saves AI mask intent without bitmaps or source geometry and keeps the photo unchanged', () => {
    const current = { masks: [{ id: 'layer', subMasks: [
      { id: 'sky', type: 'ai-sky', parameters: { maskDataBase64: 'data:source', warp: {}, grow: 10 } },
      { id: 'people', type: 'ai-people', parameters: { parts: [], mask_data_base64: 'data:stale' } },
      { id: 'object', type: 'ai-subject', parameters: { prompts: [{ x: 10, y: 20 }], maskDataBase64: 'data:object' } },
    ] }] } as unknown as Adjustments;
    const saved = collectPresetAdjustments(current, {}, { ...options, includeMasks: true });
    const subs = saved.masks![0].subMasks;
    expect(subs[0].parameters).toEqual({ grow: 10 });
    expect(subs[1].parameters).toEqual({ parts: [] });
    expect(subs[2].parameters).toEqual({ prompts: [{ x: 10, y: 20 }] });
    expect(current.masks[0].subMasks[0].parameters.maskDataBase64).toBe('data:source');
  });

  it("saves film grain's default Colour, so applying the preset sets it", () => {
    const current = { ...initialAdjustmentsFor(true), grainAmount: 40, grainVersion: 3 } as Adjustments;
    for (const presetType of ['tool', 'style'] as const) {
      const saved = JSON.parse(JSON.stringify(collectPresetAdjustments(current, initialAdjustmentsFor(true), { ...options, presetType })));
      expect(saved.grainColor, presetType).toBe(0);
      const destination = { ...initialAdjustmentsFor(true), grainAmount: 40, grainVersion: 3, grainColor: 74 };
      expect({ ...destination, ...saved }.grainColor, presetType).toBe(0);
    }
    // Without grain, or with older grain, no Colour is saved.
    expect(collectPresetAdjustments({ ...initialAdjustmentsFor(true), exposure: 0.5 } as Adjustments, initialAdjustmentsFor(true), options)).not.toHaveProperty('grainColor');
    const v2 = { ...initialAdjustmentsFor(true), grainAmount: 40, grainVersion: 2 } as Adjustments;
    expect(collectPresetAdjustments(v2, initialAdjustmentsFor(true), options)).not.toHaveProperty('grainColor');
  });

  it('never takes the sharpening response on its own', () => {
    // A response-1 photo with RAW defaults and a changed exposure: the tool
    // preset changes exposure only, not the sharpening of where it is applied.
    const current = { ...initialAdjustmentsFor(true), exposure: 0.5, sharpenVersion: 1 } as Adjustments;
    const preset = collectPresetAdjustments(current, initialAdjustmentsFor(true), options);
    expect(preset).toEqual({ exposure: 0.5 });
  });

  it('takes the response along with sharpening values', () => {
    const old = { ...initialAdjustmentsFor(true), sharpness: 80, sharpenVersion: undefined } as Adjustments;
    expect(collectPresetAdjustments(old, initialAdjustmentsFor(true), options)).toEqual({ sharpness: 80, sharpenVersion: 1 });
    const current = { ...initialAdjustmentsFor(true), sharpness: 80 } as Adjustments;
    expect(collectPresetAdjustments(current, initialAdjustmentsFor(true), options)).toEqual({
      sharpness: 80,
      sharpenVersion: SHARPEN_VERSION,
    });
  });

  it('takes the luminance noise reduction version along with luminance values only', () => {
    const current = { ...initialAdjustmentsFor(true), exposure: 0.5, lumaNoiseVersion: 1 } as Adjustments;
    expect(collectPresetAdjustments(current, initialAdjustmentsFor(true), options)).toEqual({ exposure: 0.5 });
    const old = { ...initialAdjustmentsFor(true), lumaNoiseReduction: 30, lumaNoiseVersion: undefined } as Adjustments;
    expect(collectPresetAdjustments(old, initialAdjustmentsFor(true), options)).toEqual({ lumaNoiseReduction: 30, lumaNoiseVersion: 1 });
    const now = { ...initialAdjustmentsFor(true), lumaNoiseReduction: 30 } as Adjustments;
    expect(collectPresetAdjustments(now, initialAdjustmentsFor(true), options)).toEqual({
      lumaNoiseReduction: 30,
      lumaNoiseVersion: LUMA_NOISE_VERSION,
    });
  });

  it('takes the Clarity response along with Clarity only', () => {
    // A Clarity-only tool preset from a current edit keeps version 2 (it is a
    // new edit's default, so a plain difference would drop it).
    const now = { ...initialAdjustmentsFor(true), clarity: 40 } as Adjustments;
    expect(collectPresetAdjustments(now, initialAdjustmentsFor(true), options)).toEqual({
      clarity: 40,
      clarityVersion: CLARITY_VERSION,
    });
    // An exposure preset from a version-1 photo leaves the Clarity response alone.
    const old = { ...initialAdjustmentsFor(true), exposure: 0.5, clarity: 0, clarityVersion: 1 } as Adjustments;
    expect(collectPresetAdjustments(old, initialAdjustmentsFor(true), options)).toEqual({ exposure: 0.5 });
    // A preset from before the key renders its Clarity with version 1 wherever applied.
    expect(withClarityVersion({ clarity: 40 } as Partial<Adjustments>)).toEqual({ clarity: 40, clarityVersion: 1 });
    expect(withClarityVersion({ exposure: 1 } as Partial<Adjustments>)).toEqual({ exposure: 1 });
    // Layers alone (an AI detection refresh, no source): the destination keeps its response.
    const layers = { masks: [{ id: 'm', adjustments: { clarity: 30 }, subMasks: [] }] } as unknown as Partial<Adjustments>;
    expect('clarityVersion' in withClarityVersion(layers)).toBe(false);
    expect(withClarityVersion(layers, { clarityVersion: 2 } as Partial<Adjustments>).clarityVersion).toBe(2);
    // A preset from before the key with Clarity on a layer only: version 1 where applied.
    expect(withPresetClarityVersion(layers).clarityVersion).toBe(1);
    expect(withPresetClarityVersion({ ...layers, clarityVersion: 2 }).clarityVersion).toBe(2);
  });
});
