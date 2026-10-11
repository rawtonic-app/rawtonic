import "../../i18n";
import i18n from "i18next";
import { expect, it } from "vitest";
import { describeHistoryStep } from "./historyLabels";
import {
  INITIAL_ADJUSTMENTS,
  INITIAL_MASK_CONTAINER,
  type Adjustments,
} from "../../utils/adjustments";

const t = i18n.getFixedT("en");
const step = (before: Adjustments, after: Partial<Adjustments>) =>
  describeHistoryStep(before, { ...before, ...after }, t, "en");
const base = structuredClone(INITIAL_ADJUSTMENTS);
const sky = { ...structuredClone(INITIAL_MASK_CONTAINER), id: "sky", name: "Sky" };

it("names one slider with its new value", () => {
  expect(step(base, { exposure: 0.35 })).toBe("Exposure +0.35");
  expect(step(base, { shadows: -20 })).toBe("Shadows −20");
});

it("names the tools when several values changed", () => {
  expect(step(base, { exposure: 1, contrast: 10 })).toBe("Exposure, Contrast");
  expect(step(base, { exposure: 1, contrast: 10, whites: 5 })).toBe(
    "Exposure, Contrast +1",
  );
  expect(step(base, { grainAmount: 20, grainSize: 30 })).toBe("Grain, Effects");
  expect(step(base, { crop: { x: 0, y: 0, width: 10, height: 10, unit: "px" } as Adjustments["crop"] })).toBe("Crop");
});

it("names the layer a step changed", () => {
  const withSky = { ...base, masks: [sky] };
  expect(step(base, { masks: [sky] })).toBe("New layer Sky");
  expect(
    step(withSky, {
      masks: [{ ...sky, adjustments: { ...sky.adjustments, exposure: 0.35 } }],
    }),
  ).toBe("Exposure +0.35 · Sky");
  expect(step(withSky, { masks: [] })).toBe("Deleted layer Sky");
});
