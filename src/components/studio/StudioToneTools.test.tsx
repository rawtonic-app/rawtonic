import "../../i18n";
import { describe, expect, it } from "vitest";
import { wheelPosition, wheelValue } from "./StudioColorWheel";

describe("colour wheel geometry", () => {
  it("puts hue 0 at the right and turns counter-clockwise", () => {
    const red = wheelPosition(0, 100);
    expect(red.x).toBeCloseTo(1);
    expect(red.y).toBeCloseTo(0);
    const up = wheelPosition(90, 100);
    expect(up.x).toBeCloseTo(0);
    expect(up.y).toBeCloseTo(-1);
  });

  it("round-trips positions and clamps to the rim", () => {
    for (const [hue, saturation] of [
      [0, 0],
      [45, 30],
      [220, 75],
      [359, 100],
    ]) {
      const p = wheelPosition(hue, saturation);
      const v = wheelValue(p.x, p.y);
      expect(v.saturation).toBe(saturation);
      if (saturation > 0) expect(v.hue).toBe(hue);
    }
    expect(wheelValue(3, 0)).toEqual({ hue: 0, saturation: 100 });
  });
});
