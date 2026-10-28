import { describe, expect, it } from "vitest";
import { curvePointerPosition } from "./curvePointer";

describe("curve pointer mapping", () => {
  it("maps screen points through a letterboxed SVG transform and clamps margins", () => {
    // 275 × 210 plot: a 210 px square centered with 32.5 px margins.
    const inverse = {
      a: 255 / 210,
      b: 0,
      c: 0,
      d: 255 / 210,
      e: (-32.5 * 255) / 210,
      f: 0,
    };
    const point = curvePointerPosition(75, 105, inverse);
    expect(point.x).toBeCloseTo((42.5 * 255) / 210);
    expect(point.y).toBeCloseTo(127.5);
    expect(curvePointerPosition(0, 220, inverse)).toEqual({ x: 0, y: 0 });
  });
  it("handles a tall plot and transformed axes", () => {
    const inverse = { a: 2, b: 0, c: 0, d: 2, e: 0, f: -100 };
    expect(curvePointerPosition(50, 100, inverse)).toEqual({ x: 100, y: 155 });
  });
});
