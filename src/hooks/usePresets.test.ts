import { describe, expect, it, vi } from 'vitest';
import { collectPresetAdjustments } from './usePresets';
import {
  CLARITY_VERSION,
  LUMA_NOISE_VERSION,
  SHARPEN_VERSION,
  initialAdjustmentsFor,
  withClarityVersion,
  type Adjustments,
} from '../utils/adjustments';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve()) }));

const options = { includeMasks: false, includeCropTransform: false, presetType: 'tool' as const };

describe('collectPresetAdjustments', () => {
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
  });
});
