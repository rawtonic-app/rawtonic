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
