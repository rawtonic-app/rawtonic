import { expect, it } from "vitest";
import {
  SHOW_CLIPPING_SIDES,
  histogramClipping,
  nextClipSides,
  showClippingValue,
} from "./clippingWarnings";

it("sends both flags as the old both-sides value and each side as its own", () => {
  expect(showClippingValue(false, false)).toBe(0);
  expect(showClippingValue(true, true)).toBe(1);
  expect(showClippingValue(true, false)).toBe(SHOW_CLIPPING_SIDES.highlights);
  expect(showClippingValue(false, true)).toBe(SHOW_CLIPPING_SIDES.shadows);
  // The engine's colour-range view is 2 + index for at most eight ranges;
  // the side flags stay out of that range (crates/engine
  // `clipping_side_flags_stay_apart_from_the_colour_range_view`).
  for (const side of Object.values(SHOW_CLIPPING_SIDES)) {
    expect(side).toBeGreaterThan(1 + 8);
  }
  expect(new Set([0, 1, ...Object.values(SHOW_CLIPPING_SIDES)]).size).toBe(4);
});

it("switches both warnings on unless one is on: then both off", () => {
  expect(nextClipSides({ highlights: false, shadows: false })).toEqual({
    highlights: true,
    shadows: true,
  });
  expect(nextClipSides({ highlights: true, shadows: false })).toEqual({
    highlights: false,
    shadows: false,
  });
  expect(nextClipSides({ highlights: false, shadows: true })).toEqual({
    highlights: false,
    shadows: false,
  });
  expect(nextClipSides({ highlights: true, shadows: true })).toEqual({
    highlights: false,
    shadows: false,
  });
});

it("shows a side as clipped when its end of any channel's histogram is not empty", () => {
  const bins = (red: number[], blue = red) => ({
    red,
    green: [0, 0],
    blue,
  });
  expect(histogramClipping(null)).toEqual({ shadows: false, highlights: false });
  expect(histogramClipping({})).toEqual({ shadows: false, highlights: false });
  expect(histogramClipping(bins([0, 0, 10, 3, 0]))).toEqual({
    shadows: false,
    highlights: false,
  });
  expect(histogramClipping(bins([4, 0, 0, 0, 0]))).toEqual({
    shadows: true,
    highlights: false,
  });
  expect(histogramClipping(bins([0, 0, 0, 0, 7]))).toEqual({
    shadows: false,
    highlights: true,
  });
  // One channel enough, and a bin too small to show in the plot still counts.
  expect(histogramClipping({ red: [0, 0], green: [1, 0], blue: [0, 1] })).toEqual({
    shadows: true,
    highlights: true,
  });
  // The typed-array form the backend sends.
  expect(histogramClipping({ red: { data: new Uint32Array([0, 2]) } })).toEqual({
    shadows: false,
    highlights: true,
  });
  // A one-bin histogram is both its first and its last bin.
  expect(histogramClipping(bins([9]))).toEqual({ shadows: true, highlights: true });
  // Not a number: no count, no warning.
  expect(histogramClipping({ red: [null, undefined], green: ["x", NaN] })).toEqual({
    shadows: false,
    highlights: false,
  });
});
