import { expect, it } from "vitest";
import { blendPreset } from "./presetAmount";

it("moves numbers part of the way and blends nested settings per value", () => {
  const base = {
    exposure: 0,
    contrast: 10,
    hsl: { reds: { hue: 0, saturation: 20 } },
    curve: [1],
    bw: false,
  };
  const preset = {
    exposure: 1,
    contrast: 30,
    hsl: { reds: { hue: 10 } },
    curve: [2],
    bw: true,
  };
  expect(blendPreset(base, preset, 0.5)).toEqual({
    exposure: 0.5,
    contrast: 20,
    hsl: { reds: { hue: 5, saturation: 20 } },
    curve: [2],
    bw: true,
  });
  expect(blendPreset(base, preset, 0.25)).toMatchObject({
    exposure: 0.25,
    curve: [1],
    bw: false,
  });
  expect(blendPreset(base, preset, 1)).toEqual({
    ...preset,
    hsl: { reds: { hue: 10, saturation: 20 } },
  });
  // Settings the photo lacks take the preset's (from half strength).
  expect(blendPreset({}, { exposure: 1 }, 0.5)).toEqual({ exposure: 1 });
});

it("keeps version keys whole", () => {
  // A legacy preset's AI Denoise (version 1) on a version 2 photo.
  const base = { aiDenoise: 0, aiDenoiseVersion: 2 };
  const preset = { aiDenoise: 60, aiDenoiseVersion: 1 };
  expect(blendPreset(base, preset, 0.5)).toEqual({
    aiDenoise: 30,
    aiDenoiseVersion: 1,
  });
  expect(blendPreset(base, preset, 0.25)).toEqual({
    aiDenoise: 15,
    aiDenoiseVersion: 2,
  });
  expect(blendPreset({ toneVersion: 1 }, { toneVersion: 2 }, 0.75)).toEqual({
    toneVersion: 2,
  });
});

it("keeps curves whole with their version", () => {
  // A legacy preset's long curve (no version) on a version 2 photo.
  const legacy = {
    luma: [
      { x: 0, y: 0 },
      { x: 255, y: 255 },
    ],
  };
  const base = {
    curves: {
      luma: [
        { x: 0, y: 10 },
        { x: 255, y: 255 },
      ],
      version: 2,
    },
  };
  for (const amount of [0.5, 0.99, 1]) {
    expect(blendPreset(base, { curves: legacy }, amount)).toEqual({
      curves: legacy,
    });
  }
  expect(blendPreset(base, { curves: legacy }, 0.25)).toEqual(base);
});
