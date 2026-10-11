import "../../i18n";
import { describe, expect, it } from "vitest";
import { autoLevels, gammaOf, midtoneOf } from "./StudioToneTools";
import { wheelPosition, wheelValue } from "./StudioColorWheel";
import { INITIAL_LEVELS } from "../../utils/adjustments";

describe("levels helpers", () => {
  it("maps the midtone slider to gamma and back", () => {
    expect(gammaOf(0)).toBe(1);
    expect(gammaOf(50)).toBe(2);
    expect(gammaOf(-100)).toBe(0.25);
    for (const m of [-100, -37, 0, 12, 100])
      expect(midtoneOf(gammaOf(m))).toBe(m);
  });

  it("finds black and white points that clip a small fraction", () => {
    const bins = new Array(256).fill(0);
    for (let i = 20; i <= 230; i++) bins[i] = 100;
    const result = autoLevels([bins, bins, bins], INITIAL_LEVELS.rgb);
    expect(result).toEqual({ inBlack: 20, inWhite: 230 });
  });

  it("maps histogram positions back through the current input levels", () => {
    const bins = new Array(256).fill(0);
    for (let i = 0; i <= 255; i++) bins[i] = 100;
    // Already stretched from 40–200: the full histogram maps back there.
    const result = autoLevels([bins], {
      ...INITIAL_LEVELS.rgb,
      inBlack: 40,
      inWhite: 200,
    });
    expect(result).toEqual({ inBlack: 40, inWhite: 200 });
  });

  it("refuses empty histograms", () => {
    expect(autoLevels([new Array(256).fill(0)], INITIAL_LEVELS.rgb)).toBeNull();
  });
});

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
