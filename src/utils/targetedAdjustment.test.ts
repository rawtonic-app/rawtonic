import { describe, expect, it } from "vitest";
import { INITIAL_ADJUSTMENTS } from "./adjustments";
import {
  curveInputMean,
  curveValue,
  dominantChannel,
  linearFromSrgb,
  oklch,
  startTargetedAdjustment,
  targetedPatch,
  wideCurveLevels,
} from "./targetedAdjustment";
import { regionCurveValue } from "./regionCurve";

const base = () => structuredClone(INITIAL_ADJUSTMENTS);
const sky = linearFromSrgb([90, 140, 220]);
const foliage = linearFromSrgb([90, 140, 40]);

describe("targeted adjustment", () => {
  it("moves the colour mixer channels of the colour under the pointer", () => {
    const start = startTargetedAdjustment("saturation", base(), sky, null)!;
    expect(dominantChannel(start)).toBe("blues");
    const patch = targetedPatch(start, 20);
    expect(patch.hsl!.blues.saturation).toBe(20);
    // Neighbours follow less; far channels stay.
    expect(patch.hsl!.aquas.saturation).toBeLessThan(20);
    expect(patch.hsl!.reds.saturation).toBe(0);
    expect(patch.hsl!.blues.hue).toBe(0);
  });

  it("adds to the values the drag started from and clamps at the slider ends", () => {
    const adjustments = base();
    adjustments.hsl.greens.luminance = 90;
    const start = startTargetedAdjustment(
      "luminance",
      adjustments,
      foliage,
      null,
    )!;
    expect(dominantChannel(start)).toMatch(/yellows|greens/);
    const patch = targetedPatch(start, 40);
    expect(patch.hsl!.greens.luminance).toBeLessThanOrEqual(100);
    expect(targetedPatch(start, 0).hsl!.greens.luminance).toBe(90);
  });

  it("ignores grey for colour targets", () => {
    expect(
      startTargetedAdjustment(
        "hue",
        base(),
        linearFromSrgb([128, 128, 128]),
        null,
      ),
    ).toBeNull();
    expect(startTargetedAdjustment("grayMixer", base(), null, null)).toBeNull();
  });

  it("drives the black & white mix and converts the photo", () => {
    const start = startTargetedAdjustment("grayMixer", base(), sky, null)!;
    const patch = targetedPatch(start, -30);
    expect(patch.blackAndWhite).toBe(true);
    expect(patch.grayMixer!.blue).toBe(-30);
    expect(patch.grayMixer!.red).toBe(0);
  });

  it("adds a curve point at the level under the pointer and then moves it", () => {
    const start = startTargetedAdjustment("curve", base(), null, [
      linearFromSrgb([100, 100, 100]),
    ])!;
    expect(start.curveX).toBe(100);
    const patch = targetedPatch(start, 15);
    // An edit of neutral curves starts them in the current version.
    expect(patch.curves!.version).toBe(3);
    expect(patch.curves!.luma).toEqual([
      { x: 0, y: 0 },
      { x: 100, y: 115 },
      { x: 255, y: 255 },
    ]);
    // The sample is the curve's input, whatever the curve now shows.
    const again = startTargetedAdjustment(
      "curve",
      { ...base(), curves: patch.curves! },
      null,
      [linearFromSrgb([100, 100, 100])],
    )!;
    expect(again.curveX).toBe(100);
    expect(targetedPatch(again, -5).curves!.luma[1]).toEqual({
      x: 100,
      y: 110,
    });
  });

  it("reads the point curve's level behind the region curve", () => {
    const adjustments = base();
    adjustments.regionCurve = { ...adjustments.regionCurve!, darks: 60 };
    const lifted = regionCurveValue(adjustments.regionCurve, 80 / 255) * 255;
    const start = startTargetedAdjustment("curve", adjustments, null, [
      linearFromSrgb([80, 80, 80]),
    ])!;
    // The point curve sees the region curve's output, not the photo's level.
    expect(start.curveX).toBe(Math.round(lifted));
    // With the curve tool off the region curve does not render.
    adjustments.sectionVisibility = {
      ...adjustments.sectionVisibility,
      curves: false,
    };
    expect(
      startTargetedAdjustment("curve", adjustments, null, [
        linearFromSrgb([80, 80, 80]),
      ])!.curveX,
    ).toBe(80);
  });

  it("moves the region of the tone under the pointer", () => {
    const adjustments = base();
    adjustments.regionCurve = { ...adjustments.regionCurve!, lights: 10 };
    const start = startTargetedAdjustment("regionCurve", adjustments, null, [
      linearFromSrgb([150, 150, 150]),
    ])!;
    expect(start.curveX).toBe(150);
    const patch = targetedPatch(start, 25.4);
    expect(patch.regionCurve).toMatchObject({ lights: 35, darks: 0 });
    expect(targetedPatch(start, 500).regionCurve!.lights).toBe(100);
    // Coloured pixels: the luminance-weighted level. Pure green is a
    // light tone.
    const green = startTargetedAdjustment("regionCurve", base(), null, [
      linearFromSrgb([0, 255, 0]),
    ])!;
    expect(green.curveX).toBe(182);
    expect(targetedPatch(green, 5).regionCurve).toMatchObject({
      lights: 5,
      darks: 0,
      highlights: 0,
    });
    const shadow = startTargetedAdjustment("regionCurve", base(), null, [
      linearFromSrgb([20, 20, 20]),
    ])!;
    expect(targetedPatch(shadow, -10).regionCurve).toMatchObject({
      shadows: -10,
      lights: 0,
    });
  });

  it("reads coloured tones in the space the edited curves render in", () => {
    const orange = linearFromSrgb([200, 80, 40]);
    const display = 0.2126 * 200 + 0.7152 * 80 + 0.0722 * 40;
    const wide = wideCurveLevels(orange);
    const wideTone =
      0.2880402 * wide[0] + 0.7118741 * wide[1] + 0.0000857 * wide[2];
    // Neutral curves: an edit starts them in the wide working space.
    expect(
      startTargetedAdjustment("curve", base(), null, [orange])!.curveX,
    ).toBe(Math.round(wideTone));
    // Curves of an earlier version that already change the tone keep
    // rendering on display values.
    const old = base();
    old.curves = {
      ...old.curves,
      version: 2,
      red: [
        { x: 0, y: 0 },
        { x: 128, y: 160 },
        { x: 255, y: 255 },
      ],
    };
    expect(startTargetedAdjustment("curve", old, null, [orange])!.curveX).toBe(
      Math.round(display),
    );
    // Grey is the same level in both.
    const grey = wideCurveLevels(linearFromSrgb([128, 128, 128]));
    grey.forEach((v) => expect(v).toBeCloseTo(128, 1));
    expect(Math.round(wideTone)).not.toBe(Math.round(display));
  });

  it("reads colours beyond sRGB as the wide curves receive them", () => {
    // Linear ProPhoto (0.4, 0.05, 0.02): green below zero in sRGB.
    const prophoto = [0.4, 0.05, 0.02];
    const toSrgb = [
      [2.0340763, -0.7273343, -0.3067417],
      [-0.2288135, 1.2317302, -0.0029169],
      [-0.0085698, -0.1532866, 1.1618565],
    ];
    const srgb = toSrgb.map((row) =>
      row.reduce((sum, m, c) => sum + m * prophoto[c], 0),
    ) as [number, number, number];
    expect(srgb[1]).toBeLessThan(0);
    const encode = (v: number) => (1.055 * v ** (1 / 2.4) - 0.055) * 255;
    const expected =
      0.2880402 * encode(0.4) +
      0.7118741 * encode(0.05) +
      0.0000857 * encode(0.02);
    expect(startTargetedAdjustment("curve", base(), null, [srgb])!.curveX).toBe(
      Math.round(expected),
    );
    expect(Math.round(expected)).toBe(94);
    // A region curve carries the part beyond sRGB along at its ratio.
    const lifted = base();
    lifted.regionCurve = { ...lifted.regionCurve!, darks: 40 };
    expect(
      startTargetedAdjustment("curve", lifted, null, [srgb])!.curveX,
    ).toBeGreaterThan(94);
  });

  it("averages the sampled pixels in the curve's input levels", () => {
    // A textured patch: 54 white and 67 black pixels. Their mean level on
    // the curve is 113.8 (Darks at the default splits); averaging encoded
    // or linear colours first would land elsewhere.
    const patch = [
      ...Array.from({ length: 54 }, () => linearFromSrgb([255, 255, 255])),
      ...Array.from({ length: 67 }, () => linearFromSrgb([0, 0, 0])),
    ];
    const region = startTargetedAdjustment("regionCurve", base(), null, patch)!;
    expect(region.curveX).toBe(114);
    expect(targetedPatch(region, 5).regionCurve).toMatchObject({
      darks: 5,
      lights: 0,
    });
    expect(startTargetedAdjustment("curve", base(), null, patch)!.curveX).toBe(
      114,
    );
    expect(startTargetedAdjustment("curve", base(), null, [])).toBeNull();
  });

  it("needs the curve input sample for tone targets", () => {
    expect(startTargetedAdjustment("curve", base(), null, null)).toBeNull();
    expect(
      startTargetedAdjustment("regionCurve", base(), null, null),
    ).toBeNull();
  });

  it("moves the nearest of dense imported points", () => {
    const dense = base();
    dense.curves = {
      ...dense.curves,
      luma: [
        { x: 0, y: 0 },
        { x: 118, y: 0 },
        { x: 119, y: 13.9 },
        { x: 120, y: 30 },
        { x: 255, y: 255 },
      ],
    };
    const start = startTargetedAdjustment("curve", dense, null, [
      linearFromSrgb([119, 119, 119]),
    ])!;
    expect(start.curveX).toBe(119);
    const luma = targetedPatch(start, 15).curves!.luma;
    expect(luma[1]).toEqual({ x: 118, y: 0 });
    expect(luma[2].x).toBe(119);
    expect(luma[2].y).toBeCloseTo(29, 0);
  });

  it("evaluates the curve as the renderer does", () => {
    expect(
      curveValue(
        [
          { x: 0, y: 0 },
          { x: 128, y: 160 },
          { x: 255, y: 255 },
        ],
        64,
      ),
    ).toBeCloseTo(84.02, 2);
    expect(oklch([1, 1, 1]).chroma).toBeLessThan(1e-3);
  });

  it("sets or moves a point on the selected channel's curve at its own input", () => {
    const lifted = base();
    // The RGB curve lifts 100 to 120: the colour curves receive 120.
    lifted.curves = {
      ...lifted.curves,
      luma: [
        { x: 0, y: 0 },
        { x: 100, y: 120 },
        { x: 255, y: 255 },
      ],
    };
    const grey = [linearFromSrgb([100, 100, 100])];
    expect(curveInputMean("curve", lifted, grey, "luma")).toBe(100);
    for (const channel of ["red", "green", "blue"] as const) {
      const start = startTargetedAdjustment("curve", lifted, null, grey, channel)!;
      expect(start.curveX).toBe(120);
      const patch = targetedPatch(start, 10);
      expect(patch.curves![channel]).toEqual([
        { x: 0, y: 0 },
        { x: 120, y: 130 },
        { x: 255, y: 255 },
      ]);
      // The other curves stay; the panel's point curves follow.
      expect(patch.curves!.luma).toEqual(lifted.curves.luma);
      expect(patch.pointCurves![channel]).toEqual(patch.curves![channel]);
      expect(patch.curveMode).toBe("point");
    }
    // A colour channel reads its own level of a coloured pixel.
    const orange = [linearFromSrgb([200, 80, 40])];
    const wide = wideCurveLevels(orange[0]);
    expect(curveInputMean("curve", base(), orange, "red")).toBe(Math.round(wide[0]));
    expect(curveInputMean("curve", base(), orange, "blue")).toBe(Math.round(wide[2]));
  });

  it("reads the Luminance curve's input after the other curves", () => {
    const grey = [linearFromSrgb([100, 100, 100])];
    expect(curveInputMean("curve", base(), grey, "luminance")).toBe(100);
    const lifted = base();
    lifted.curves = {
      ...lifted.curves,
      luma: [
        { x: 0, y: 0 },
        { x: 100, y: 120 },
        { x: 255, y: 255 },
      ],
    };
    // Grey stays grey through the curves: its luminance is the lifted level.
    expect(curveInputMean("curve", lifted, grey, "luminance")).toBe(120);
    const start = startTargetedAdjustment("curve", lifted, null, grey, "luminance")!;
    expect(targetedPatch(start, -20).curves!.luminance).toEqual([
      { x: 0, y: 0 },
      { x: 120, y: 100 },
      { x: 255, y: 255 },
    ]);
    // Older display-value curves carry the same tone.
    const old = base();
    old.curves = { ...lifted.curves, version: 2 };
    expect(curveInputMean("curve", old, grey, "luminance")).toBe(120);
  });

  it("sets an anchor on a click and moves a nearby point instead of adding one", () => {
    const curved = base();
    curved.curves = {
      ...curved.curves,
      green: [
        { x: 0, y: 0 },
        { x: 64, y: 90 },
        { x: 255, y: 255 },
      ],
    };
    const at = (level: number) =>
      startTargetedAdjustment(
        "curve",
        curved,
        null,
        [linearFromSrgb([level, level, level])],
        "green",
      )!;
    // A click (no travel): a point on the curve as it is, nothing moves.
    const anchor = targetedPatch(at(180), 0).curves!.green;
    expect(anchor).toHaveLength(4);
    expect(anchor[2].x).toBe(180);
    expect(anchor[2].y).toBe(Math.round(curveValue(curved.curves.green, 180)));
    // Within reach of the point at 64: that point moves, none is added.
    const moved = targetedPatch(at(68), 12).curves!.green;
    expect(moved).toEqual([
      { x: 0, y: 0 },
      { x: 64, y: 102 },
      { x: 255, y: 255 },
    ]);
    expect(targetedPatch(at(68), 0).curves!.green).toEqual(curved.curves.green);
  });
});
