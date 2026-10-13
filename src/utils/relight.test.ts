import { expect, it } from "vitest";
import relightRs from "../../crates/engine/src/relight.rs?raw";
import {
  createRelightLight,
  kelvinToHex,
  nextRelightNumber,
  normalizeRelight,
  RELIGHT_PATTERN_IDS,
  RELIGHT_PRESETS,
  relightDragPatch,
  relightPresetLight,
  relightSource,
  stripSentRelightData,
  type RelightLight,
} from "./relight";

it("names patterns in the renderer's GPU order", () => {
  const block = relightRs.match(/pub const PATTERNS: \[&str; \d+\] = \[([^\]]*)\]/)![1];
  expect([...block.matchAll(/"([a-z]+)"/g)].map((m) => m[1])).toEqual([
    ...RELIGHT_PATTERN_IDS,
  ]);
});

it("renders absent light fields with a new light's values", () => {
  const constant = (name: string) =>
    relightRs.match(new RegExp(`const DEFAULT_${name}: \\w+ = ([^;]+);`))![1];
  const number = (name: string) => Number(constant(name).replace(/\.$/, ""));
  const light = createRelightLight();
  expect(`#${constant("COLOR").replace("0x", "")}`).toBe(light.color);
  for (const [name, key] of [
    ["INTENSITY", "intensity"],
    ["ELEVATION", "elevation"],
    ["X", "x"],
    ["Y", "y"],
    ["SIZE", "size"],
    ["ROTATION", "rotation"],
    ["DISTANCE", "distance"],
    ["SCALE", "scale"],
    ["SOFTNESS", "softness"],
    ["DIFFUSION", "diffusion"],
    ["SHADOWS", "shadows"],
  ] as const)
    expect(number(name), name).toBe(light[key]);
});

it("migrates legacy lights to explicit values without moving them", () => {
  const legacy: RelightLight = { ...createRelightLight(), type: "spot", x: 30, y: 70, rotation: 40 };
  delete legacy.sourceX;
  delete legacy.sourceY;
  delete legacy.patternRotation;
  delete legacy.distance;
  const second = { ...legacy, id: "b", type: "sun" as const };
  const { relightLights } = normalizeRelight([legacy, second], undefined);
  expect(relightLights![0]).toMatchObject({ number: 1, sourceX: 30, sourceY: 70, patternRotation: 40, distance: 60 });
  // A sun's source handle stays where it was drawn.
  const source = relightSource(second, 1, 1);
  expect(relightLights![1]).toMatchObject({ number: 2, sourceX: source.x, sourceY: source.y });
  expect(source.y).not.toBe(70);
  const depth = { data: "data:image/png;base64,AAAA", geometry: {} };
  const named = normalizeRelight(undefined, depth).relightDepth!;
  expect(named.cacheKey).toBe(normalizeRelight(undefined, depth).relightDepth!.cacheKey);
  expect(normalizeRelight(undefined, { ...depth, cacheKey: "k" }).relightDepth!.cacheKey).toBe("k");
});

it("sends depth and imported patterns once per name", () => {
  const sent = new Set<string>();
  const recipe = () => ({
    relightDepth: { data: "DEPTH", faces: [[[1]]], geometry: {}, cacheKey: "d" },
    relightLights: [
      { ...createRelightLight(), pattern: "custom" as const, customPattern: { id: "p", data: "PNG" } },
      createRelightLight(),
    ],
  });
  const first = recipe();
  stripSentRelightData(first, sent).forEach((n) => sent.add(n));
  expect(first.relightDepth.data).toBe("DEPTH");
  const later = recipe();
  expect(stripSentRelightData(later, sent)).toEqual([]);
  expect(later.relightDepth).toMatchObject({ data: null, faces: null, cacheKey: "d" });
  expect(later.relightLights[0].customPattern).toEqual({ id: "p", data: null });
  // The editor's own recipe is not modified.
  expect(recipe().relightLights[0].customPattern!.data).toBe("PNG");
});

it("numbers lights stably and drags elevation vertically", () => {
  const lights = [1, 3].map((number) => ({ ...createRelightLight(), number }));
  expect(nextRelightNumber(lights)).toBe(2);
  const light = createRelightLight();
  expect(relightDragPatch(light, "elevation", { x: 0, y: 50 }, { x: 0, y: 40 }, 200, 100).elevation).toBe(55);
  expect(relightDragPatch(light, "elevation", { x: 0, y: 0 }, { x: 0, y: 99 }, 100, 100).elevation).toBe(5);
});

it("presets and temperatures produce valid lights", () => {
  expect(kelvinToHex(6600)).toBe("#ffffff");
  expect(kelvinToHex(2700)).toMatch(/^#ff[0-9a-f]{4}$/);
  for (const preset of RELIGHT_PRESETS) {
    const light = relightPresetLight(preset);
    expect(light.color).toBe(kelvinToHex(light.temperature!));
    expect(light.sourceX).toBeGreaterThanOrEqual(-100);
    expect(light.sourceY).toBeLessThanOrEqual(200);
  }
  // Light from the top: its source handle sits above the subject.
  expect(relightPresetLight("butterfly").sourceY).toBeLessThan(50);
});
