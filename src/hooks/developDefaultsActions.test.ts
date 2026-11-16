import { describe, expect, it, vi } from 'vitest';
import { HSL_VERSION, initialAdjustmentsFor, normalizeLoadedAdjustments, type Adjustments } from '../utils/adjustments';
import { photoDefaultSettings, withToolDefaults } from './developDefaultsActions';

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
});
