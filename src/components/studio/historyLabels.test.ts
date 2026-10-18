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

it("compares copies by value and shared layers by reference", () => {
  const stroke = (n: number) => ({ points: Array.from({ length: n }, (_, i) => ({ x: i, y: i })) });
  const painted = {
    ...sky,
    subMasks: [{ id: "b", type: "brush", parameters: { lines: [stroke(3)] } }],
  } as unknown as typeof sky;
  const withPainted = { ...base, masks: [painted] };
  // A deep copy of an unchanged recipe is no step at all.
  expect(describeHistoryStep(withPainted, structuredClone(withPainted), t, "en")).toBe(t("studio.develop.history.other"));
  const more = {
    ...painted,
    subMasks: [{ ...painted.subMasks[0], parameters: { lines: [stroke(3), stroke(2)] } }],
  };
  expect(step(withPainted, { masks: [more as unknown as typeof sky] })).toBe(`${t("studio.develop.history.mask")} · Sky`);
});

it("names an Auto Mask upgrade", () => {
  const lines = [{ brushSize: 50, points: [{ x: 1, y: 2 }], tool: "brush", autoMask: true }];
  const brush = { id: "b", type: "brush", mode: "additive", visible: true, invert: false, opacity: 100, parameters: { lines } };
  const painted = { ...sky, subMasks: [brush] } as Adjustments["masks"][number];
  const upgraded = {
    ...painted,
    subMasks: [{ ...brush, parameters: { lines: [{ ...lines[0], autoMaskVersion: 3 }] } }],
  } as Adjustments["masks"][number];
  expect(step({ ...base, masks: [painted] }, { masks: [upgraded] })).toBe("Auto Mask upgraded · Sky");
  const other = { ...painted, subMasks: [{ ...brush, invert: true }] } as Adjustments["masks"][number];
  expect(step({ ...base, masks: [painted] }, { masks: [other] })).toBe("Mask · Sky");
});
