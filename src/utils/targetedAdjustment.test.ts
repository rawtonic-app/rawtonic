import { describe, expect, it } from "vitest";
import { INITIAL_ADJUSTMENTS } from "./adjustments";
import {
  curveValue,
  dominantChannel,
  linearFromSrgb,
  oklch,
  startTargetedAdjustment,
  targetedPatch,
} from "./targetedAdjustment";
import { regionCurveValue } from "./regionCurve";
import { pointCurveValue } from "./pointCurve";

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
    const start = startTargetedAdjustment(
      "curve",
      base(),
      null,
      [100, 100, 100],
    )!;
    expect(start.curveX).toBe(100);
    const patch = targetedPatch(start, 15);
    // A curve edit renders the whole curve.
    expect(patch.curves!.version).toBe(2);
    expect(patch.curves!.luma).toEqual([
      { x: 0, y: 0 },
      { x: 100, y: 115 },
      { x: 255, y: 255 },
    ]);
    const again = startTargetedAdjustment(
      "curve",
      { ...base(), curves: patch.curves! },
      null,
      [115, 115, 115],
    )!;
    // The displayed level maps back through the curve to the same point.
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
      lifted,
      lifted,
      lifted,
    ])!;
    // The point curve sees the region curve's output, not the photo's level.
    expect(start.curveX).toBe(Math.round(lifted));
  });

  it("moves the region of the tone under the pointer", () => {
    const adjustments = base();
    adjustments.regionCurve = { ...adjustments.regionCurve!, lights: 10 };
    const shown = regionCurveValue(adjustments.regionCurve, 150 / 255) * 255;
    const start = startTargetedAdjustment("regionCurve", adjustments, null, [
      shown,
      shown,
      shown,
    ])!;
    expect(start.curveX).toBe(150);
    const patch = targetedPatch(start, 25.4);
    expect(patch.regionCurve).toMatchObject({ lights: 35, darks: 0 });
    expect(targetedPatch(start, 500).regionCurve!.lights).toBe(100);
    // Channel curves count too: neutral 100 shown as 200 is still Darks.
    const lifted = base();
    const up = [
      { x: 0, y: 0 },
      { x: 100, y: 200 },
      { x: 255, y: 255 },
    ];
    lifted.curves = { ...lifted.curves, red: up, green: up, blue: up };
    const dark = startTargetedAdjustment(
      "regionCurve",
      lifted,
      null,
      [200, 200, 200],
    )!;
    expect(dark.curveX).toBe(100);
    expect(targetedPatch(dark, 5).regionCurve!.darks).toBe(5);
    // Coloured pixels: each channel is recovered on its own. Green shown
    // unchanged through an RGB curve lifting 128 to 200 is a light tone.
    const rgb = base();
    rgb.curves = {
      ...rgb.curves,
      luma: [
        { x: 0, y: 0 },
        { x: 128, y: 200 },
        { x: 255, y: 255 },
      ],
    };
    const green = startTargetedAdjustment(
      "regionCurve",
      rgb,
      null,
      [0, 255, 0],
    )!;
    expect(green.curveX).toBe(182);
    expect(
      Object.keys(targetedPatch(green, 5).regionCurve!).filter(
        (k) =>
          targetedPatch(green, 5).regionCurve![k as "lights"] !==
          rgb.regionCurve![k as "lights"],
      ),
    ).toEqual(["lights"]);
    const shadow = startTargetedAdjustment(
      "regionCurve",
      base(),
      null,
      [20, 20, 20],
    )!;
    expect(targetedPatch(shadow, -10).regionCurve).toMatchObject({
      shadows: -10,
      lights: 0,
    });
  });

  it("declines tone targets when layer curves or a LUT follow", () => {
    const lifted = [
      { x: 0, y: 0 },
      { x: 100, y: 220 },
      { x: 255, y: 255 },
    ];
    const layered = {
      ...base(),
      masks: [
        {
          visible: true,
          subMasks: [{}],
          adjustments: { curves: { luma: lifted } },
        },
      ],
    } as never;
    for (const target of ["curve", "regionCurve"] as const) {
      expect(
        startTargetedAdjustment(target, layered, null, [220, 220, 220]),
      ).toBeNull();
      expect(
        startTargetedAdjustment(
          target,
          { ...base(), lutPath: "/l.cube" },
          null,
          [100, 100, 100],
        ),
      ).toBeNull();
    }
    // A hidden layer or identity layer curves do not count.
    const hidden = {
      ...base(),
      masks: [{ visible: false, adjustments: { curves: { luma: lifted } } }],
    } as never;
    expect(
      startTargetedAdjustment("regionCurve", hidden, null, [100, 100, 100]),
    ).not.toBeNull();
    // Layers that cannot reach a pixel do not count.
    for (const mask of [
      {
        visible: true,
        subMasks: [],
        adjustments: { curves: { luma: lifted } },
      },
      {
        visible: true,
        opacity: 0,
        subMasks: [{}],
        adjustments: { curves: { luma: lifted } },
      },
    ]) {
      expect(
        startTargetedAdjustment(
          "regionCurve",
          { ...base(), masks: [mask] } as never,
          null,
          [100, 100, 100],
        ),
      ).not.toBeNull();
    }
    // A LUT that is off, at zero or scene-referred (before the curves) does not count.
    for (const off of [
      { lutPath: "/l.cube", lutIsSceneReferred: true },
      { lutPath: "/l.cube", lutIntensity: 0 },
      {
        lutPath: "/l.cube",
        sectionVisibility: { ...base().sectionVisibility, effects: false },
      },
    ]) {
      expect(
        startTargetedAdjustment(
          "regionCurve",
          { ...base(), ...off },
          null,
          [100, 100, 100],
        ),
      ).not.toBeNull();
    }
    const switchedOff = {
      ...base(),
      masks: [
        {
          visible: true,
          adjustments: {
            curves: { luma: lifted },
            sectionVisibility: { curves: false },
          },
        },
      ],
    } as never;
    expect(
      startTargetedAdjustment(
        "regionCurve",
        switchedOff,
        null,
        [100, 100, 100],
      ),
    ).not.toBeNull();
  });

  it("traces legacy long curves as the renderer cuts them", () => {
    const xs = Array.from({ length: 15 }, (_, i) => i * 7);
    const luma = [
      ...xs.map((x) => ({ x, y: x })),
      { x: 220, y: 240 },
      { x: 230, y: 240 },
      { x: 240, y: 245 },
      { x: 250, y: 250 },
      { x: 255, y: 255 },
    ];
    const legacy = { ...base(), curves: { ...base().curves, luma } };
    delete legacy.curves.version;
    // Rendered with its first 16 points.
    const cutAt200 = pointCurveValue(luma.slice(0, 16), 200);
    const start = startTargetedAdjustment("regionCurve", legacy, null, [
      cutAt200,
      cutAt200,
      cutAt200,
    ])!;
    expect(Math.abs(start.curveX! - 200)).toBeLessThanOrEqual(1);
    const moved = targetedPatch(start, 5).regionCurve!;
    expect(moved.highlights).toBe(5);
    // At version 2 the whole curve counts.
    const wholeAt200 = pointCurveValue(luma, 200);
    const whole = startTargetedAdjustment(
      "regionCurve",
      { ...legacy, curves: { ...legacy.curves, version: 2 } },
      null,
      [wholeAt200, wholeAt200, wholeAt200],
    )!;
    expect(Math.abs(whole.curveX! - 200)).toBeLessThanOrEqual(1);
  });

  it("declines tones a curve that turns back cannot tell apart", () => {
    const peak = base();
    peak.curves = {
      ...peak.curves,
      luma: [
        { x: 0, y: 0 },
        { x: 127.5, y: 255 },
        { x: 255, y: 0 },
      ],
    };
    const shown = pointCurveValue(peak.curves.luma, 200);
    for (const target of ["curve", "regionCurve"] as const) {
      expect(
        startTargetedAdjustment(target, peak, null, [shown, shown, shown]),
      ).toBeNull();
    }
  });

  it("declines tones a flat stretch hides and legacy long layer curves", () => {
    const flat = base();
    flat.curves = {
      ...flat.curves,
      luma: [
        { x: 0, y: 0 },
        { x: 80, y: 128 },
        { x: 220, y: 128 },
        { x: 255, y: 255 },
      ],
    };
    expect(
      startTargetedAdjustment("regionCurve", flat, null, [128, 128, 128]),
    ).toBeNull();
    // A layer's 20-point diagonal renders its first 16 points: point 16 at
    // 120 clamps brighter tones, so the layer counts.
    const luma = [
      ...Array.from({ length: 16 }, (_, i) => ({ x: i * 8, y: i * 8 })),
      { x: 200, y: 200 },
      { x: 220, y: 220 },
      { x: 240, y: 240 },
      { x: 255, y: 255 },
    ];
    const layered = {
      ...base(),
      masks: [
        { visible: true, subMasks: [{}], adjustments: { curves: { luma } } },
      ],
    } as never;
    expect(
      startTargetedAdjustment("regionCurve", layered, null, [100, 100, 100]),
    ).toBeNull();
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
});
