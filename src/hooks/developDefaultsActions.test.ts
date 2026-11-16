import { describe, expect, it, vi } from 'vitest';
import { withDevelopDefaults } from '../utils/developDefaults';
import { HSL_VERSION, initialAdjustmentsFor, normalizeLoadedAdjustments, type Adjustments } from '../utils/adjustments';
import { photoDefaultSettings, withToolDefaults, withToolResetToDefaults } from './developDefaultsActions';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve(null)) }));

const reds = { hue: 10, saturation: 20, luminance: 0 };

describe('develop defaults from the open photo', () => {
  it("states an older edit's implicit colour mixer response, so a new edit renders its values the same", () => {
    // An edit from before hslVersion: its mixer renders with version 1.
    const older = normalizeLoadedAdjustments({ hsl: { reds } } as unknown as Adjustments, true);
    expect(older.hslVersion).toBeUndefined();
    const settings = photoDefaultSettings(older, true);
    expect(settings.hsl).toMatchObject({ reds });
    expect(settings.hslVersion).toBe(1);
  });

  it("a tool's default keeps the photo's response while its layers use it", () => {
    const base = initialAdjustmentsFor(true);
    // An older edit: a layer's mixer under the implicit response 1.
    const prev = {
      ...base,
      hslVersion: undefined,
      masks: [{ id: 'l', adjustments: { hsl: { reds } }, subMasks: [] }]
    } as unknown as Adjustments;
    const next = withToolDefaults(
      prev,
      { hsl: { ...base.hsl, reds: { hue: 5, saturation: 0, luminance: 0 } }, hslVersion: HSL_VERSION },
      true
    );
    expect(next.hslVersion).not.toBe(HSL_VERSION);
    expect(next.hsl.reds.hue).toBe(5);
  });

  it('states every response an older edit renders with, Tone and colour noise too', () => {
    const older = normalizeLoadedAdjustments({ shadows: 40, colorNoiseReduction: 30 } as unknown as Adjustments, true);
    const settings = photoDefaultSettings(older, true);
    expect(settings.shadows).toBe(40);
    expect(settings.toneVersion).toBe(1);
    expect(settings.colorNoiseVersion).toBe(1);
  });

  it("a tool's reset restores its default's values exactly (Blacks travel as amounts)", () => {
    const base = initialAdjustmentsFor(true);
    const resolved = { levels: ['camera' as const], settings: { blacks: -75 } };
    const photo = { ...withDevelopDefaults(base, resolved), blacks: 0, exposure: 0.5 } as Adjustments;
    const reset = withToolResetToDefaults(photo, resolved, ['blacks', 'whites'], true);
    expect(reset.blacks).toBeCloseTo(-75, 6);
    expect(reset.exposure).toBe(0.5);
    // Nothing the defaults do not set.
    expect(withToolResetToDefaults(photo, resolved, ['grainAmount'], true)).toBe(photo);
  });
});
