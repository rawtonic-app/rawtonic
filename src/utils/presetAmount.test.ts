import { expect, it } from 'vitest';
import { blendPreset } from './presetAmount';

it('moves numbers part of the way and blends nested settings per value', () => {
  const base = { exposure: 0, contrast: 10, hsl: { reds: { hue: 0, saturation: 20 } }, curve: [1], bw: false };
  const preset = { exposure: 1, contrast: 30, hsl: { reds: { hue: 10 } }, curve: [2], bw: true };
  expect(blendPreset(base, preset, 0.5)).toEqual({
    exposure: 0.5,
    contrast: 20,
    hsl: { reds: { hue: 5, saturation: 20 } },
    curve: [2],
    bw: true,
  });
  expect(blendPreset(base, preset, 0.25)).toMatchObject({ exposure: 0.25, curve: [1], bw: false });
  expect(blendPreset(base, preset, 1)).toEqual({ ...preset, hsl: { reds: { hue: 10, saturation: 20 } } });
  // Settings the photo lacks take the preset's (from half strength).
  expect(blendPreset({}, { exposure: 1 }, 0.5)).toEqual({ exposure: 1 });
});
