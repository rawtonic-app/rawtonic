import { describe, expect, it, vi } from 'vitest';
import { collectPresetAdjustments } from './usePresets';
import { SHARPEN_VERSION, initialAdjustmentsFor, type Adjustments } from '../utils/adjustments';

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
});
