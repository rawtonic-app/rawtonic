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

it("lights each side from the backend's clipping flags", () => {
  expect(histogramClipping(null)).toEqual({ shadows: false, highlights: false });
  expect(histogramClipping({})).toEqual({ shadows: false, highlights: false });
  expect(histogramClipping({ clippedShadows: true })).toEqual({
    shadows: true,
    highlights: false,
  });
  expect(histogramClipping({ clippedShadows: false, clippedHighlights: true })).toEqual({
    shadows: false,
    highlights: true,
  });
  expect(histogramClipping({ clippedShadows: true, clippedHighlights: true })).toEqual({
    shadows: true,
    highlights: true,
  });
  // Only the flags count; anything else (a string, a missing flag) is off.
  expect(histogramClipping({ clippedShadows: "yes" })).toEqual({
    shadows: false,
    highlights: false,
  });
});

it("ignores the bins' ends, which are smoothed and carry the warning colours", () => {
  // The smoothed histogram reaches both ends for values near them, and the
  // clipping warnings' overlay colours fill the other end: the bins cannot
  // say which side clips (crates/engine `clipping_indicator_tests`).
  const nearEnds = {
    red: [0.4, 0.6, 1, 1, 0.6, 0.2, 0, 0, 0.2, 0.7],
    green: { data: new Uint32Array([3, 0, 0, 0, 0, 0, 0, 0, 0, 9]) },
    blue: [1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
  };
  expect(histogramClipping(nearEnds)).toEqual({ shadows: false, highlights: false });
  // The backend's flags alone decide, whatever the bins show.
  expect(histogramClipping({ ...nearEnds, clippedHighlights: true })).toEqual({
    shadows: false,
    highlights: true,
  });
});
