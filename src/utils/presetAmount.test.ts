import { expect, it } from 'vitest';
import { amountLayers, blendPreset } from './presetAmount';
import { INITIAL_MASK_ADJUSTMENTS, INITIAL_MASK_CONTAINER, type MaskContainer } from './adjustments';

it('moves numbers part of the way and blends nested settings per value', () => {
  const base = {
    exposure: 0,
    contrast: 10,
    hsl: { reds: { hue: 0, saturation: 20 } },
    curve: [1],
    bw: false
  };
  const preset = {
    exposure: 1,
    contrast: 30,
    hsl: { reds: { hue: 10 } },
    curve: [2],
    bw: true
  };
  expect(blendPreset(base, preset, 0.5)).toEqual({
    exposure: 0.5,
    contrast: 20,
    hsl: { reds: { hue: 5, saturation: 20 } },
    curve: [2],
    bw: true
  });
  expect(blendPreset(base, preset, 0.25)).toMatchObject({
    exposure: 0.25,
    curve: [1],
    bw: false
  });
  expect(blendPreset(base, preset, 1)).toEqual({
    ...preset,
    hsl: { reds: { hue: 10, saturation: 20 } }
  });
  // Settings the photo lacks take the preset's (from half strength).
  expect(blendPreset({}, { exposure: 1 }, 0.5)).toEqual({ exposure: 1 });
});

it('keeps version keys whole', () => {
  // A legacy preset's AI Denoise (version 1) on a version 2 photo.
  const base = { aiDenoise: 0, aiDenoiseVersion: 2 };
  const preset = { aiDenoise: 60, aiDenoiseVersion: 1 };
  expect(blendPreset(base, preset, 0.5)).toEqual({
    aiDenoise: 30,
    aiDenoiseVersion: 1
  });
  expect(blendPreset(base, preset, 0.25)).toEqual({
    aiDenoise: 15,
    aiDenoiseVersion: 2
  });
  expect(blendPreset({ toneVersion: 1 }, { toneVersion: 2 }, 0.75)).toEqual({
    toneVersion: 2
  });
});

it('keeps curves whole with their version', () => {
  // A legacy preset's long curve (no version) on a version 2 photo.
  const legacy = {
    luma: [
      { x: 0, y: 0 },
      { x: 255, y: 255 }
    ]
  };
  const base = {
    curves: {
      luma: [
        { x: 0, y: 10 },
        { x: 255, y: 255 }
      ],
      version: 2
    }
  };
  for (const amount of [0.5, 0.99, 1]) {
    expect(blendPreset(base, { curves: legacy }, amount)).toEqual({
      curves: legacy
    });
  }
  expect(blendPreset(base, { curves: legacy }, 0.25)).toEqual(base);
});

it('scales an adaptive preset’s layers from neutral and keeps the photo’s own layers and detections', () => {
  const layer = (id: string, adjustments: Record<string, unknown>, bitmap: string | null) =>
    ({
      ...INITIAL_MASK_CONTAINER,
      id,
      adjustments: { ...INITIAL_MASK_ADJUSTMENTS, ...adjustments },
      subMasks: [
        {
          id: `${id}-c`,
          type: 'ai-sky',
          visible: true,
          invert: false,
          opacity: 100,
          mode: 'additive',
          parameters: { maskDataBase64: bitmap }
        }
      ]
    }) as unknown as MaskContainer;
  const own = layer('brush', { exposure: 1 }, 'data:own');
  const preset = [
    { ...layer('sky', {}, null), adjustments: { exposure: -0.4, saturation: 20 } } as unknown as MaskContainer
  ];
  // As shown now: the preset's layer with its detection.
  const current = [own, layer('sky', { exposure: -0.4, saturation: 20 }, 'data:sky')];
  const half = amountLayers([own], current, preset, 0.5);
  expect(half.map((l) => l.id)).toEqual(['brush', 'sky']);
  expect(half[0]).toBe(own);
  expect(half[1].adjustments).toMatchObject({ exposure: -0.2, saturation: 10, contrast: 0 });
  expect(half[1].subMasks[0].parameters).toMatchObject({ maskDataBase64: 'data:sky' });
  expect(amountLayers([own], current, preset, 0)[1].adjustments).toMatchObject({ exposure: 0, saturation: 0 });
});
