import { describe, expect, it } from "vitest";
import { pointCurveValue, renderedPoints } from "./pointCurve";

// A Lightroom-style curve with 20 points (zig-zag of ±10 levels).
const xs = [
  0, 13, 27, 40, 54, 67, 81, 94, 107, 121, 134, 148, 161, 174, 188, 201, 215,
  228, 242, 255,
];
const long = xs.map((x, i) => ({
  x,
  y: i === 0 || i === 19 ? x : x + (i % 2 ? 10 : -10),
}));

describe("point curve", () => {
  it("draws legacy long curves as rendered: cut after the 16th point", () => {
    // Curves saved before version 2 hold the 16th point's level beyond it.
    expect(pointCurveValue(renderedPoints(long, false), 242)).toBe(211);
    expect(pointCurveValue(renderedPoints(long, true), 242)).toBe(232);
    expect(renderedPoints(long.slice(0, 5), false)).toHaveLength(5);
  });

  it("is flat beyond moved ends and passes through its points", () => {
    const moved = [
      { x: 32, y: 0 },
      { x: 128, y: 128 },
      { x: 224, y: 255 },
    ];
    expect(pointCurveValue(moved, 10)).toBe(0);
    expect(pointCurveValue(moved, 240)).toBe(255);
    expect(pointCurveValue(moved, 128)).toBe(128);
  });
});
