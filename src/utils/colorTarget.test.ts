import { describe, expect, it } from 'vitest';
import { newColorRange, type ColorRange } from './adjustments';
import {
  applyColorRanges,
  centreInput,
  colorBeforeRanges,
  deltaE00,
  inverseGain,
  hexOf,
  labToLch,
  labToLinear,
  lchToLab,
  linearToLab,
  linearToOklab,
  linearToSrgb8,
  oklabToLinear,
  parseHex,
  meanOklab,
  rangeCentre,
  renderedLab,
  solveColorTarget,
  srgb8ToLinear,
  type Vec3
} from './colorTarget';
import { srgbToOklch } from './oklab';

const rangeAt = (rgb: Vec3, patch: Partial<ColorRange> = {}): ColorRange => {
  const { hue, chroma, lightness } = srgbToOklch(rgb);
  return { ...newColorRange(hue, chroma, lightness), ...patch };
};
const labOfSrgb = (rgb: Vec3) => linearToLab(srgb8ToLinear(rgb));

describe('colour conversions', () => {
  it('matches published sRGB → CIELAB D50 values', () => {
    // Bradford-adapted sRGB (as Photoshop's Lab readout and ICC profiles).
    const red = labOfSrgb([255, 0, 0]);
    expect(red[0]).toBeCloseTo(54.29, 1);
    expect(red[1]).toBeCloseTo(80.8, 0);
    expect(red[2]).toBeCloseTo(69.89, 0);
    const white = labOfSrgb([255, 255, 255]);
    expect(white[0]).toBeCloseTo(100, 3);
    expect(Math.abs(white[1]) + Math.abs(white[2])).toBeLessThan(0.01);
  });

  it('round-trips sRGB, Lab, LCh, Oklab and hex', () => {
    for (const rgb of [
      [12, 200, 90],
      [230, 180, 150],
      [40, 90, 200],
      [128, 128, 128]
    ] as Vec3[]) {
      const lab = labOfSrgb(rgb);
      const back = linearToSrgb8(labToLinear(lchToLab(labToLch(lab))));
      back.forEach((v, i) => expect(v).toBeCloseTo(rgb[i], 3));
      const ok = linearToOklab(srgb8ToLinear(rgb));
      linearToSrgb8(oklabToLinear(ok)).forEach((v, i) => expect(v).toBeCloseTo(rgb[i], 3));
      expect(parseHex(hexOf(rgb))).toEqual(rgb);
    }
    expect(parseHex('#C8102E')).toEqual([200, 16, 46]);
    expect(parseHex('fa0')).toEqual([255, 170, 0]);
    expect(parseHex('#12345')).toBeNull();
    expect(parseHex('zzzzzz')).toBeNull();
  });

  it('computes CIEDE2000 as Sharma, Wu and Dalal’s test data', () => {
    const cases: [Vec3, Vec3, number][] = [
      [[50, 2.6772, -79.7751], [50, 0, -82.7485], 2.0425],
      [[50, -1, 2], [50, 0, 0], 2.3669],
      [[50, 2.49, -0.001], [50, -2.49, 0.0009], 7.1792],
      [[60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644],
      [[22.7233, 20.0904, -46.694], [23.0331, 14.973, -42.5619], 2.0373],
      [[2.0776, 0.0795, -1.135], [0.9033, -0.0636, -0.5514], 0.9082]
    ];
    for (const [a, b, expected] of cases) expect(deltaE00(a, b)).toBeCloseTo(expected, 3);
  });
});

describe('solveColorTarget', () => {
  it('moves a range onto a reachable target', () => {
    const range = rangeAt([200, 60, 50]);
    const target = labOfSrgb([200, 20, 40]);
    const solve = solveColorTarget([range], range.id, target)!;
    expect(solve.deltaE).toBeLessThan(0.5);
    expect(solve.limited).toEqual({ hue: false, saturation: false, lightness: false });
    // The reported result is what the shader model renders with the new shifts.
    const rendered = renderedLab([{ ...range, ...solve.shifts }], centreInput(range));
    rendered.forEach((v, i) => expect(v).toBeCloseTo(solve.resultLab[i], 9));
    for (const v of Object.values(solve.shifts)) expect(Number.isInteger(v)).toBe(true);
  });

  it('keeps the other ranges as they are and compensates for their overlap', () => {
    const range = rangeAt([210, 150, 120]);
    // A second range near the same colour, already shifting it.
    const other = rangeAt([205, 140, 115], { hueShift: 20, saturation: 30, lightnessShift: -15 });
    const target = labOfSrgb([220, 160, 140]);
    const solve = solveColorTarget([other, range], range.id, target)!;
    expect(solve.deltaE).toBeLessThan(0.5);
    const rendered = renderedLab([other, { ...range, ...solve.shifts }], centreInput(range));
    rendered.forEach((v, i) => expect(v).toBeCloseTo(solve.resultLab[i], 9));
  });

  it('clamps out-of-reach targets and says which shift ran out', () => {
    const range = rangeAt([200, 60, 50]);
    // Red → blue is far beyond ±50° of hue.
    const solve = solveColorTarget([range], range.id, labOfSrgb([40, 60, 200]))!;
    expect(solve.limited.hue).toBe(true);
    expect(Math.abs(solve.shifts.hueShift)).toBe(100);
    expect(solve.deltaE).toBeGreaterThan(10);
    // Lightness beyond ×1.4.
    const dark = rangeAt([60, 30, 25]);
    const bright = solveColorTarget([dark], dark.id, labOfSrgb([230, 120, 100]))!;
    expect(bright.limited.lightness).toBe(true);
    expect(bright.shifts.lightnessShift).toBe(100);
  });

  it('says hue when a target needs more than the hue reach, also at the edge of sRGB', () => {
    // The renderer maps colours beyond sRGB into an output rather than
    // clipping each channel, so overshooting past the sRGB edge reaches
    // nothing: the solve works in the working gamut (TGT2; TGT1 counted
    // #FF9600 from this pink as reached through such a clip).
    const range = rangeAt([190, 100, 120]);
    const solve = solveColorTarget([range], range.id, labOfSrgb([255, 150, 0]))!;
    expect(solve.limited.hue).toBe(true);
    expect(solve.shifts.hueShift).toBe(100);
    expect(solve.deltaE).toBeGreaterThan(1);
    const rendered = renderedLab([{ ...range, ...solve.shifts }], centreInput(range));
    rendered.forEach((v, i) => expect(v).toBeCloseTo(solve.resultLab[i], 9));
  });

  it('reaches a pinned target through the other sliders, else keeps them on their own part', () => {
    // Hue just beyond +50°: a little more lightness and chroma bring the
    // working-gamut colour within reach of the target.
    const purple = rangeAt([115, 60, 170]);
    const [L, C, h] = labToLch(linearToLab(oklabToLinear(rangeCentre(purple))));
    const near = solveColorTarget([purple], purple.id, lchToLab([L + 1, C * 1.1, h + 40]))!;
    expect(near.deltaE).toBeLessThan(1);
    // Red → blue is out of reach: hue at its limit, saturation and lightness
    // as the closed form sets them (no chroma drained to shave ΔE).
    const red = rangeAt([200, 60, 50]);
    const far = solveColorTarget([red], red.id, labOfSrgb([40, 60, 200]))!;
    expect(far.limited.hue).toBe(true);
    expect(far.deltaE).toBeGreaterThan(10);
    expect(far.shifts.saturation).toBeGreaterThan(-60);
  });

  it('finds the nearest colour with the sliders together for a target beyond the working gamut', () => {
    const green = rangeAt([33, 225, 55]);
    // Far beyond ProPhoto in chroma: the sliders find the nearest it holds.
    const solve = solveColorTarget([green], green.id, lchToLab([80, 400, 140]))!;
    expect(solve.targetOutsideWorkingGamut).toBe(true);
    expect(solveColorTarget([green], green.id, labOfSrgb([30, 170, 70]))!.targetOutsideWorkingGamut).toBe(false);
    const closed = renderedLab([{ ...green, saturation: 100 }], centreInput(green));
    expect(solve.deltaE).toBeLessThanOrEqual(deltaE00(lchToLab([80, 400, 140]), closed) + 1e-9);
  });

  it('measures how much a sample error grows when the ranges are undone', () => {
    const range = rangeAt([180, 85, 55]);
    expect(inverseGain([range], rangeCentre(range))).toBeCloseTo(1, 1);
    expect(inverseGain([{ ...range, hueShift: 100 }], rangeCentre(range))).toBeLessThan(1.5);
    expect(inverseGain([{ ...range, saturation: -50 }], rangeCentre(range))).toBeGreaterThan(1.5);
    expect(inverseGain([{ ...range, saturation: -90 }], rangeCentre(range))).toBeGreaterThan(3);
  });

  it('recovers the colour before the ranges from a sample of the finished photo', () => {
    const range = rangeAt([190, 100, 120], { hueShift: 100, saturation: 30, lightnessShift: -20 });
    const other = rangeAt([200, 90, 100], { hueShift: -40 });
    const before = rangeCentre(range);
    const shown = applyColorRanges([range, other], before);
    // From the range's old colour (a pick again) …
    colorBeforeRanges([range, other], shown, [before]).forEach((v, i) => expect(v).toBeCloseTo(before[i], 6));
    // … and a colour no range touches is its own colour before.
    const untouched = linearToOklab(srgb8ToLinear([40, 160, 60]));
    expect(colorBeforeRanges([range, other], untouched)).toEqual(untouched);
    expect(colorBeforeRanges([], shown)).toEqual(shown);
  });

  it('flags targets outside sRGB', () => {
    const range = rangeAt([40, 160, 60]);
    const solve = solveColorTarget([range], range.id, lchToLab([60, 120, 140]))!;
    expect(solve.targetOutsideSrgb).toBe(true);
    expect(solveColorTarget([range], range.id, labOfSrgb([30, 170, 70]))!.targetOutsideSrgb).toBe(false);
  });

  it('cannot move a grey range and says so', () => {
    const grey = rangeAt([128, 128, 128]);
    const solve = solveColorTarget([grey], grey.id, labOfSrgb([200, 60, 50]))!;
    expect(solve.limited).toEqual({ hue: true, saturation: true, lightness: true });
    expect(solve.shifts).toEqual({ hueShift: 0, saturation: 0, lightnessShift: 0 });
    expect(solve.movable).toBe(false);
  });

  it('solves on the renderer’s sample: the pixels around the pick and a layer’s mask influence', () => {
    // A textured patch: reds spread around the range's colour.
    const centre = linearToOklab(srgb8ToLinear([200, 60, 50]));
    const pixels: Vec3[] = [-1, 0, 1].flatMap((i) =>
      [-1, 0, 1].map((j) => [centre[0] + 0.03 * i, centre[1] + 0.02 * j, centre[2] - 0.02 * i] as Vec3)
    );
    const [L, a, b] = meanOklab(pixels);
    const range = { ...newColorRange(((Math.atan2(b, a) * 180) / Math.PI + 360) % 360, Math.hypot(a, b), L) };
    const target = labOfSrgb([200, 20, 40]);
    const full = { pixels, influence: pixels.map(() => 1) };
    const solve = solveColorTarget([range], range.id, target, full)!;
    expect(solve.deltaE).toBeLessThan(0.5);
    renderedLab([{ ...range, ...solve.shifts }], full).forEach((v, i) => expect(v).toBeCloseTo(solve.resultLab[i], 9));
    // Half the layer's strength at the pick needs larger shifts for the same
    // colour; a solve at full strength misses there.
    const half = { pixels, influence: pixels.map(() => 0.5) };
    const feathered = solveColorTarget([range], range.id, target, half)!;
    expect(feathered.deltaE).toBeLessThan(0.5);
    expect(Math.abs(feathered.shifts.saturation)).toBeGreaterThan(Math.abs(solve.shifts.saturation));
    expect(deltaE00(target, renderedLab([{ ...range, ...solve.shifts }], half))).toBeGreaterThan(1);
  });

  it('reaches a target beyond sRGB in the working gamut from a colour beyond sRGB', () => {
    // A saturated blue the display clips (linear sRGB red below 0).
    const blue = linearToOklab([-0.05, 0.05, 0.6]);
    const range = newColorRange(
      ((Math.atan2(blue[2], blue[1]) * 180) / Math.PI + 360) % 360,
      Math.hypot(blue[1], blue[2]),
      blue[0]
    );
    const input = { pixels: [blue], influence: [1] };
    const [Lb, Cb, hb] = labToLch(linearToLab([-0.05, 0.05, 0.6]));
    const target = lchToLab([Lb, Cb * 1.05, hb + 3]);
    const solve = solveColorTarget([range], range.id, target, input)!;
    expect(solve.targetOutsideSrgb).toBe(true);
    expect(solve.deltaE).toBeLessThan(1);
  });

  it('reaches a target on a patch across two colours under a feathered mask with the sliders together', () => {
    // Review TGT2 round 1: an 11 × 11 red/green patch under a linear mask
    // ramp; the mean-pixel closed form pins Hue while the patch is reachable.
    const red = linearToOklab(srgb8ToLinear([190, 40, 35]));
    const green = linearToOklab(srgb8ToLinear([60, 150, 50]));
    const pixels: Vec3[] = [];
    const influence: number[] = [];
    for (let y = 0; y < 11; y += 1)
      for (let x = 0; x < 11; x += 1) {
        pixels.push(x < 6 ? red : green);
        influence.push(0.2 + (0.8 * y) / 10);
      }
    const input = { pixels, influence };
    const [L, a, b] = meanOklab(pixels);
    const range = newColorRange(((Math.atan2(b, a) * 180) / Math.PI + 360) % 360, Math.hypot(a, b), L);
    // A target the full model reaches: the patch's colour with known shifts.
    const target = renderedLab([{ ...range, hueShift: -45, saturation: -50, lightnessShift: 30 }], input);
    const solve = solveColorTarget([range], range.id, target, input)!;
    expect(solve.deltaE).toBeLessThan(1);
    expect(solve.limited).toEqual({ hue: false, saturation: false, lightness: false });
  });

  it('keeps the shifts and says so when the layer’s mask no longer covers the pick', () => {
    const range = rangeAt([200, 60, 50], { hueShift: 30, saturation: 20, lightnessShift: -10 });
    const solve = solveColorTarget([range], range.id, labOfSrgb([200, 20, 40]), {
      pixels: [rangeCentre(range)],
      influence: [0]
    })!;
    expect(solve.shifts).toEqual({ hueShift: 30, saturation: 20, lightnessShift: -10 });
    expect(solve.movable).toBe(false);
    expect(solve.uncovered).toBe(true);
  });

  it('returns null for an unknown range', () => {
    expect(solveColorTarget([rangeAt([1, 2, 3])], 'missing', [50, 0, 0])).toBeNull();
  });
});
