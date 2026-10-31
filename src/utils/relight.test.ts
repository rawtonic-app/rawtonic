import { expect, it } from "vitest";
import relightRs from "../../crates/engine/src/relight.rs?raw";
import {
  createRelightLight,
  kelvinToHex,
  nextRelightNumber,
  normalizeRelight,
  RELIGHT_PATTERN_IDS,
  RELIGHT_PRESETS,
  RELIGHT_RANGE_VERSION,
  relightBallPatch,
  relightBallPoint,
  relightControlReason,
  relightDragPatch,
  relightFaceAnchor,
  relightPresetAnchor,
  relightWithCurrentRanges,
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
  // A version-five subject is sent once with its depth; older ones get none.
  expect(later.relightDepth).not.toHaveProperty("subject");
  const withSubject = () => ({
    relightDepth: { data: "DEPTH", faces: [], subject: "SUBJECT", geometry: {}, cacheKey: "s" },
  });
  const sentSubject = new Set<string>();
  stripSentRelightData(withSubject(), sentSubject).forEach((n) => sentSubject.add(n));
  const again = withSubject();
  stripSentRelightData(again, sentSubject);
  expect(again.relightDepth).toMatchObject({ data: null, subject: null });
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
    const { light, originalLight } = relightPresetLight(preset);
    expect(light.color).toBe(kelvinToHex(light.temperature!));
    expect(light.sourceX).toBeGreaterThanOrEqual(-100);
    expect(light.sourceY).toBeLessThanOrEqual(200);
    expect(light.rangeVersion).toBe(RELIGHT_RANGE_VERSION);
    expect(originalLight).toBeGreaterThanOrEqual(0);
    expect(originalLight).toBeLessThanOrEqual(100);
  }
  // Light from the top: its source handle sits above the subject.
  expect(relightPresetLight("butterfly").light.sourceY).toBeLessThan(50);
});

it("places presets on the largest face and keys from the side it turns to", () => {
  // Two faces; the larger one (right) looks to the frame's right (nose right of centre).
  const small = [[0.1, 0.1, 0, 1], [0.12, 0.12, 0, 1], [0.14, 0.14, 0, 1]];
  const large = [[0.5, 0.3, 0, 1], [0.78, 0.45, 0, 1], [0.8, 0.6, 0, 1]];
  const depth = { data: "d", geometry: {}, faces: [small, large] };
  const anchor = relightFaceAnchor(depth)!;
  expect(anchor.x).toBeCloseTo(65);
  expect(anchor.y).toBeCloseTo(45);
  expect(anchor.turn).toBe(1);
  const left = relightPresetLight("loop").light;
  const right = relightPresetLight("loop", anchor).light;
  // Mirrored to the right, pattern centre on the face.
  expect(right.rotation).toBe(-left.rotation);
  expect([right.x, right.y]).toEqual([anchor.x, anchor.y]);
  expect(right.sourceX!).toBeGreaterThan(anchor.x);
  // No face: the subject's centre, else nothing (the frame's centre).
  expect(relightPresetAnchor({ data: "d", geometry: {}, subjectCenter: [0.3, 0.6] })).toEqual({ x: 30, y: 60, turn: 0 });
  expect(relightPresetAnchor({ data: "d", geometry: {} })).toBeNull();
  expect([left.x, left.y]).toEqual([50, 50]);
});

it("maps the direction ball to rotation and elevation and back", () => {
  const light = createRelightLight();
  for (const [rotation, elevation] of [[90, 10], [-135, 45], [180, 70], [0, 30]]) {
    const back = relightBallPatch(light, relightBallPoint({ ...light, rotation, elevation }));
    expect(back.rotation).toBeCloseTo(rotation, 0);
    expect(back.elevation).toBeCloseTo(elevation, 0);
  }
  // Left of centre: light from the frame's left (rotation 90); the rim grazes.
  expect(relightBallPatch(light, { x: -1, y: 0 })).toEqual({ rotation: 90, elevation: 5 });
  // The centre lights from the camera and keeps the direction.
  expect(relightBallPatch(light, { x: 0, y: 0 })).toEqual({ rotation: light.rotation, elevation: 90 });
  // Beyond the rim is clamped to it.
  expect(relightBallPatch(light, { x: 0, y: -3 })).toEqual({ rotation: 180, elevation: 5 });
});

it("says why a control cannot act on a light", () => {
  const sun = createRelightLight();
  const none = { valid: false, faces: 0 };
  const scene = { valid: true, faces: 0 };
  const portrait = { valid: true, faces: 1 };
  expect(relightControlReason("direction", sun, none)).toBe("needsAnalysis");
  expect(relightControlReason("direction", sun, scene)).toBeNull();
  expect(relightControlReason("direction", { ...sun, type: "spot" }, portrait)).toBe("spotDirection");
  expect(relightControlReason("skinReflections", sun, scene)).toBe("needsFace");
  expect(relightControlReason("skinReflections", sun, portrait)).toBeNull();
  const old = { ...sun, rangeVersion: undefined };
  expect(relightControlReason("diffusion", { ...old, elevation: 80 }, portrait)).toBe("frontalLight");
  // From range 2 Diffusion evens out a frontal light too.
  expect(relightControlReason("diffusion", { ...sun, elevation: 80 }, portrait)).toBeNull();
  expect(relightControlReason("diffusion", { ...sun, elevation: 20 }, portrait)).toBeNull();
  // A pattern's shadows are lifted by diffusion at any height.
  expect(relightControlReason("diffusion", { ...old, elevation: 80, pattern: "blinds" }, portrait)).toBeNull();
  expect(relightControlReason("shadows", sun, scene)).toBe("needsPatternOrSubject");
  expect(relightControlReason("elevation", sun, none)).toBe("needsAnalysis");
});

it("moves an old light to the current ranges as bright as it was", () => {
  const old = { ...createRelightLight(), intensity: 50 };
  delete old.rangeVersion;
  const moved = relightWithCurrentRanges(old);
  expect(moved.rangeVersion).toBe(RELIGHT_RANGE_VERSION);
  // Range 1: 3 × 50 %; range 2: 4 × (intensity / 100)^1.5 (relight.rs).
  expect(4 * (moved.intensity / 100) ** 1.5).toBeCloseTo(1.5, 2);
  expect(relightWithCurrentRanges({ ...old, intensity: 100 }).intensity).toBeLessThanOrEqual(100);
  expect(relightWithCurrentRanges(moved)).toBe(moved);
  // Range 2's constants are the renderer's.
  expect(relightRs).toMatch(/const RANGE_2_MAX_GAIN: f32 = 4\.;/);
  expect(relightRs).toMatch(/const RANGE_2_EXPONENT: f32 = 1\.5;/);
});

it("turns a sun by the angle its source moves, keeping a direction set with the ball", () => {
  // Ball set the light from the right; the source handle still sits upper left.
  const light = { ...createRelightLight(), rotation: -90, sourceX: 20, sourceY: 20 };
  const patch = relightDragPatch(light, "source", { x: 20, y: 20 }, { x: 20.5, y: 20 }, 100, 100);
  expect(Math.abs(patch.rotation! + 90)).toBeLessThan(2);
  // Half a turn around the centre turns the light half a turn.
  const half = relightDragPatch(light, "source", { x: 20, y: 20 }, { x: 80, y: 80 }, 100, 100);
  expect(half.rotation).toBeCloseTo(90);
});
