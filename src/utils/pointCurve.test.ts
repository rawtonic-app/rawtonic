import { describe, expect, it } from "vitest";
import {
  pointCurveValue,
  renderedLuminancePoints,
  renderedPoints,
} from "./pointCurve";

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

  it("takes a Luminance curve as the renderer does", () => {
    // The first 16 of a long curve that changes something.
    const lifted = [
      ...Array.from({ length: 16 }, (_, i) => ({ x: i * 10, y: i * 10 + 5 })),
      { x: 200, y: 240 },
      { x: 255, y: 255 },
    ];
    expect(renderedLuminancePoints(lifted)).toHaveLength(16);
    // A long curve that changes nothing renders as none, not cut.
    const identity = Array.from({ length: 20 }, (_, i) => ({
      x: (i * 255) / 19,
      y: (i * 255) / 19,
    }));
    expect(renderedLuminancePoints(identity)).toEqual([
      { x: 0, y: 0 },
      { x: 255, y: 255 },
    ]);
    expect(renderedLuminancePoints(undefined)).toHaveLength(2);
  });
});
