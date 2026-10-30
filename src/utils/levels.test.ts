import { describe, expect, it } from "vitest";
import { INITIAL_LEVELS, type Level } from "./adjustments";
import {
  autoLevels,
  gammaOf,
  handlePosition,
  levelOutput,
  midtoneOf,
  nudgedLevel,
  pickedLevels,
  withHandleAt,
  withLevelValue,
} from "./levels";
import { linearFromSrgb } from "./targetedAdjustment";

const identity: Level = { ...INITIAL_LEVELS.rgb };

/** The old sliders' change for a value (StudioToneTools before the
 * histogram handles), for comparison. */
function sliderLevel(level: Level, key: string, raw: number): Level {
  const bounded = (min: number, max: number) =>
    Number(Math.max(min, Math.min(max, raw)).toFixed(0));
  switch (key) {
    case "inBlack": {
      const inBlack = bounded(0, 254);
      return { ...level, inBlack, inWhite: Math.max(level.inWhite, inBlack + 1) };
    }
    case "inWhite": {
      const inWhite = bounded(1, 255);
      return { ...level, inWhite, inBlack: Math.min(level.inBlack, inWhite - 1) };
    }
    case "midtone":
      return { ...level, gamma: gammaOf(bounded(-100, 100)) };
    default:
      return { ...level, [key]: bounded(0, 255) };
  }
}

describe("levels values", () => {
  it("maps the midtone value to gamma and back", () => {
    expect(gammaOf(0)).toBe(1);
    expect(gammaOf(50)).toBe(2);
    expect(gammaOf(-100)).toBe(0.25);
    for (const m of [-100, -37, 0, 12, 100])
      expect(midtoneOf(gammaOf(m))).toBe(m);
  });

  it("gives the old sliders' recipe values for the same input", () => {
    const start: Level = { inBlack: 30, inWhite: 200, gamma: 1.32, outBlack: 10, outWhite: 240 };
    for (const key of ["inBlack", "inWhite", "midtone", "outBlack", "outWhite"] as const)
      for (const raw of [-300, -100, -12, 0, 0.4, 1, 17.6, 99, 128, 199, 200, 201, 254, 255, 400]) {
        const next = withLevelValue(start, key, raw);
        expect(next).toEqual(sliderLevel(start, key, raw));
        // Same key order, so the stored recipe is the same text.
        expect(JSON.stringify(next)).toBe(JSON.stringify(sliderLevel(start, key, raw)));
      }
  });

  it("keeps the input black below the white, pushing the other", () => {
    const start: Level = { ...identity, inBlack: 40, inWhite: 120 };
    expect(withLevelValue(start, "inBlack", 150)).toMatchObject({ inBlack: 150, inWhite: 151 });
    expect(withLevelValue(start, "inWhite", 20)).toMatchObject({ inBlack: 19, inWhite: 20 });
    expect(withLevelValue(start, "inBlack", 255)).toMatchObject({ inBlack: 254, inWhite: 255 });
    expect(withLevelValue(start, "inWhite", 0)).toMatchObject({ inBlack: 0, inWhite: 1 });
    expect(withLevelValue(start, "midtone", 500).gamma).toBe(4);
    expect(withLevelValue(start, "inBlack", Number.NaN)).toBe(start);
  });
});

describe("levels handles", () => {
  it("places the input and output points at their level", () => {
    const level: Level = { inBlack: 51, inWhite: 204, gamma: 1, outBlack: 0, outWhite: 255 };
    expect(handlePosition(level, "inBlack")).toBeCloseTo(0.2);
    expect(handlePosition(level, "inWhite")).toBeCloseTo(0.8);
    expect(handlePosition(level, "outWhite")).toBe(1);
    // Gamma 1: the midtone halfway between black and white.
    expect(handlePosition(level, "midtone")).toBeCloseTo(0.5);
    // Brighter midtones sit nearer the black point.
    expect(handlePosition({ ...level, gamma: 2 }, "midtone")).toBeLessThan(0.5);
  });

  it("maps a handle drag back to the value it shows", () => {
    expect(withHandleAt(identity, "inBlack", 0.2).inBlack).toBe(51);
    expect(withHandleAt(identity, "inWhite", 0.8).inWhite).toBe(204);
    expect(withHandleAt(identity, "outBlack", -1).outBlack).toBe(0);
    expect(withHandleAt(identity, "outWhite", 2).outWhite).toBe(255);
    for (const m of [-100, -40, 0, 25, 100]) {
      const level = { ...identity, inBlack: 20, inWhite: 220, gamma: gammaOf(m) };
      const at = handlePosition(level, "midtone");
      expect(withHandleAt(level, "midtone", at).gamma).toBe(level.gamma);
    }
  });

  it("keeps the midtone between black and white", () => {
    const level = { ...identity, inBlack: 60, inWhite: 180 };
    expect(withHandleAt(level, "midtone", 0).gamma).toBe(4);
    expect(withHandleAt(level, "midtone", 1).gamma).toBe(0.25);
    for (const gamma of [0.25, 1, 4]) {
      const p = handlePosition({ ...level, gamma }, "midtone") * 255;
      expect(p).toBeGreaterThan(60);
      expect(p).toBeLessThan(180);
    }
    // Dragging black past white pushes white; the order holds.
    const pushed = withHandleAt(level, "inBlack", 0.9);
    expect(pushed.inBlack).toBeLessThan(pushed.inWhite);
  });
});

describe("levels keys", () => {
  it("nudges by one, Shift by ten, within the range", () => {
    const level = { ...identity, inBlack: 10 };
    expect(nudgedLevel(level, "inBlack", { key: "ArrowRight", shiftKey: false })?.inBlack).toBe(11);
    expect(nudgedLevel(level, "inBlack", { key: "ArrowUp", shiftKey: true })?.inBlack).toBe(20);
    expect(nudgedLevel(level, "inBlack", { key: "ArrowLeft", shiftKey: true })?.inBlack).toBe(0);
    expect(nudgedLevel(level, "midtone", { key: "ArrowUp", shiftKey: false })?.gamma).toBe(gammaOf(1));
    expect(nudgedLevel(level, "outWhite", { key: "Home", shiftKey: false })?.outWhite).toBe(0);
    expect(nudgedLevel(level, "midtone", { key: "End", shiftKey: false })?.gamma).toBe(4);
    expect(nudgedLevel(level, "inBlack", { key: "a", shiftKey: false })).toBeNull();
  });
});

describe("levels pickers", () => {
  const grey = (v: number) => linearFromSrgb([v, v, v]);

  it("sets the composite's black and white from the sampled display value", () => {
    const black = pickedLevels(INITIAL_LEVELS, "rgb", "black", [grey(30), grey(34)]);
    expect(black?.rgb).toMatchObject({ inBlack: 32, inWhite: 255 });
    const white = pickedLevels(INITIAL_LEVELS, "rgb", "white", [grey(230)]);
    expect(white?.rgb).toMatchObject({ inBlack: 0, inWhite: 230 });
    // The other channels are kept.
    expect(white?.red).toEqual(INITIAL_LEVELS.red);
  });

  it("lets the darkest or brightest channel decide on the composite", () => {
    const pixel = linearFromSrgb([40, 60, 80]);
    expect(pickedLevels(INITIAL_LEVELS, "rgb", "black", [pixel])?.rgb.inBlack).toBe(40);
    expect(pickedLevels(INITIAL_LEVELS, "rgb", "white", [pixel])?.rgb.inWhite).toBe(80);
  });

  it("sets a colour channel from the composite's output", () => {
    const levels = { ...INITIAL_LEVELS, rgb: { ...identity, inBlack: 20, inWhite: 220 } };
    const pixel = linearFromSrgb([120, 60, 80]);
    const picked = pickedLevels(levels, "red", "white", [pixel]);
    expect(picked?.red.inWhite).toBe(Math.round(levelOutput(levels.rgb, 120)));
    expect(picked?.rgb).toEqual(levels.rgb);
    expect(picked?.green).toEqual(INITIAL_LEVELS.green);
  });

  it("keeps the order and refuses a missing sample", () => {
    const levels = { ...INITIAL_LEVELS, rgb: { ...identity, inWhite: 100 } };
    expect(pickedLevels(levels, "rgb", "black", [grey(180)])?.rgb).toMatchObject({
      inBlack: 180,
      inWhite: 181,
    });
    expect(pickedLevels(levels, "rgb", "white", null)).toBeNull();
    expect(pickedLevels(levels, "rgb", "white", [])).toBeNull();
  });
});

describe("auto levels", () => {
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
