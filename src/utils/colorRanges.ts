import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { updateAdjustments } from '../hooks/useEditorActions';
import { amendHistoryStep, debouncedSetHistory } from '../store/useEditorStore';
import { draftFor, flushSliderDraft } from './sliderDraft';
import { MAX_COLOR_RANGES, newColorRange, type Adjustments, type ColorRange } from './adjustments';
import { srgbToOklch } from './oklab';
import { GEOMETRY_KEYS, ORIENTATION_KEYS } from './geometryKeys';
import { reportPerformance } from '../hooks/usePerformanceDiagnostics';
import {
  colorBeforeRanges,
  inverseGain,
  linearToLab,
  linearToOklab,
  meanDisplayLab,
  meanOklab,
  rangeCentre,
  solveColorTarget,
  solveColorTargetMeasured,
  srgb8ToLinear,
  type ColorRangePick,
  type ColorTarget,
  type RangeInput,
  type Vec3
} from './colorTarget';

/** Error gain above which a colour picked through the ranges is uncertain:
 * a turn or a lift keeps it near 1; taking chroma away raises it (−50 % ≈
 * 2), and near grey several colours end the same (review TGT1 round 3). */
const UNCERTAIN_GAIN = 1.5;

/** The key of a range in `colorPicksThroughRanges`. */
export const rangePickKey = (photo: string, layer: string | null, id: string) => `${photo}|${layer ?? ''}|${id}`;

/** The picker target that samples a target colour; `targetPickFor(id)` also
 * moves range `id` onto it. */
export const TARGET_PICK = 'target';
export const targetPickFor = (id: string) => `${TARGET_PICK}:${id}`;
export const isTargetPick = (target: string | null) =>
  target === TARGET_PICK || !!target?.startsWith(`${TARGET_PICK}:`);

/**
 * The ranges with range `id`'s hue, saturation and lightness shifts solved
 * so that its sampled colour lands on `lab` (CIELAB D50) — one edit, the
 * other ranges and the range's shape untouched. Unchanged when `id` is gone.
 */
export function rangesWithTarget(ranges: ColorRange[], id: string, lab: Vec3, input?: RangeInput | null): ColorRange[] {
  const solve = solveColorTarget(ranges, id, lab, input);
  if (!solve) return ranges;
  return ranges.map((range) => (range.id === id ? { ...range, ...solve.shifts } : range));
}

/** The recipe with the ranges of the photo (`layer` null) or of a mask layer replaced. */
export function withRanges(
  prev: Adjustments,
  layer: string | null,
  next: (ranges: ColorRange[]) => ColorRange[]
): Adjustments {
  return layer
    ? {
        ...prev,
        masks: prev.masks.map((mask) =>
          mask.id === layer
            ? { ...mask, adjustments: { ...mask.adjustments, colorRanges: next(colorRangesOf(mask.adjustments)) } }
            : mask
        )
      }
    : { ...prev, colorRanges: next(colorRangesOf(prev)) };
}

/** The ranges of the photo (`layer` null) or of a mask layer of a recipe. */
export const colorRangesOfLayer = (adjustments: Adjustments, layer: string | null): ColorRange[] =>
  layer ? colorRangesOf(adjustments.masks.find((m) => m.id === layer)?.adjustments ?? {}) : colorRangesOf(adjustments);

/** Colour ranges of a recipe, or none. */
export const colorRangesOf = (adjustments: Partial<Adjustments>): ColorRange[] =>
  (adjustments.colorRanges as ColorRange[] | undefined) ?? [];

/** The skin tone range among a recipe's or layer's ranges, if any. */
export const skinRangeOf = (ranges: ColorRange[]) => ranges.find((r) => r.kind === 'skin');

/**
 * The ranges after a pick of an Oklab colour: 'new' adds a range, 'skin'
 * adds the skin tone range or moves the existing one there, a range id
 * moves that range. Returns the ranges and the picked range's id (null when
 * the limit left no room).
 */
export function applyColorPick(
  ranges: ColorRange[],
  target: string,
  limit: number,
  hue: number,
  chroma: number,
  lightness: number
): { ranges: ColorRange[]; id: string | null } {
  const skin = target === 'skin' ? skinRangeOf(ranges) : undefined;
  const id = skin?.id ?? target;
  if (target === 'new' || (target === 'skin' && !skin)) {
    if (ranges.length >= limit) return { ranges, id: null };
    const range = newColorRange(hue, chroma, lightness, target === 'skin' ? 'skin' : undefined);
    return { ranges: [...ranges, range], id: range.id };
  }
  return {
    ranges: ranges.map((range) => (range.id === id ? { ...range, hue, chroma, lightness } : range)),
    id
  };
}

/**
 * The renderer's sample of what the colour editor of the photo (`layer`
 * null) or of a mask layer receives around a normalised point
 * (`sample_color_range_input`: the picker's block at full resolution, as
 * the export develops it), rendered from `adjustments`; null when the
 * renderer could not probe (no preview base yet, a layer without a mask).
 */
export async function sampleRangeInput(
  path: string,
  x: number,
  y: number,
  layer: string | null,
  adjustments: Adjustments,
  block: PickBlock | null = null
): Promise<RangeInput | null> {
  const sample = await invoke<{ pixels: Vec3[]; influence: number[]; size?: [number, number] | null } | null>(
    Invokes.SampleColorRangeInput,
    { path, normalizedX: x, normalizedY: y, block, adjustments, layer }
  ).catch(() => null);
  if (!sample?.pixels?.length || sample.influence?.length !== sample.pixels.length) return null;
  const pixels = sample.pixels.map((p) => linearToOklab(p));
  return { ...binned(pixels, sample.influence, sample.size ?? null), mean: meanOklab(pixels) };
}

/** A pick's block: x, y, w, h normalised to the shown photograph. */
export type PickBlock = [number, number, number, number];

/** The eyedropper's block around a normalised point on a `frame`-sized
 * display frame (the GPU's `preview_sample_roi`: 11 × 11 pixels, cut at
 * the edges), normalised. */
export function eyedropperBlock(x: number, y: number, [w, h]: [number, number]): PickBlock | null {
  if (!(w > 0 && h > 0) || !(x >= 0 && x <= 1 && y >= 0 && y <= 1)) return null;
  const cx = Math.floor(x * w);
  const cy = Math.floor(y * h);
  const left = Math.max(cx - 5, 0);
  const top = Math.max(cy - 5, 0);
  return [left / w, top / h, (Math.min(cx + 6, w) - left) / w, (Math.min(cy + 6, h) - top) / h];
}

/** Bins per side the model's solve reads of a block: the picker's 11 × 11. */
const MODEL_BINS = 11;

/**
 * A full-resolution block (`size` columns × rows, row-major) as at most
 * 11 × 11 of its pixels, one from each cell of an 11 × 11 grid at a
 * scattered place in it (stratified sampling): the model's solve runs on
 * every input pixel at each step, so it reads about as many as the
 * picker's preview pixels. Single pixels keep the block's texture, which
 * the ranges act on pixel by pixel (cell means would hide it and leave the
 * measured correction more to do); the scattered places keep a fine
 * repeating pattern from lining up with the grid. The range's colour is the
 * whole block's mean (`RangeInput.mean`), and the measured corrections see
 * every pixel.
 */
export function binned(pixels: Vec3[], influence: number[], size: [number, number] | null): RangeInput {
  if (!size || size[0] * size[1] !== pixels.length || size[0] * size[1] <= MODEL_BINS * MODEL_BINS)
    return { pixels, influence };
  const [w, h] = size;
  const out: RangeInput = { pixels: [], influence: [] };
  for (let cy = 0; cy < MODEL_BINS; cy += 1) {
    const y0 = Math.floor((cy * h) / MODEL_BINS);
    const y1 = Math.floor(((cy + 1) * h) / MODEL_BINS);
    for (let cx = 0; cx < MODEL_BINS; cx += 1) {
      const x0 = Math.floor((cx * w) / MODEL_BINS);
      const x1 = Math.floor(((cx + 1) * w) / MODEL_BINS);
      if (x1 <= x0 || y1 <= y0) continue;
      // A fixed scatter (the same block always gives the same pixels).
      const k = cy * MODEL_BINS + cx;
      const x = x0 + (scatter(k, 1) % (x1 - x0));
      const y = y0 + (scatter(k, 2) % (y1 - y0));
      out.pixels.push(pixels[y * w + x]);
      out.influence.push(influence[y * w + x]);
    }
  }
  return out;
}

/** A small integer hash of a cell and an axis, for `binned`'s scatter. */
const scatter = (cell: number, axis: number) => {
  let v = Math.imul(cell + 1, 0x9e3779b1) ^ Math.imul(axis, 0x85ebca77);
  v ^= v >>> 15;
  v = Math.imul(v, 0x2c1b3c6d);
  v ^= v >>> 12;
  return v >>> 0;
};

const sameColour = (a: Vec3, b: Vec3) => Math.hypot(a[0] - b[0], a[1] - b[1], a[2] - b[2]) < 1e-6;

/** The pick behind range `range` of `photo`/`layer`, while the range still has the colour that pick set. */
export function rangePickOf(photo: string, layer: string | null, range: ColorRange): ColorRangePick | null {
  const state = useEditorStore.getState();
  const pick = state.colorRangeInputs[rangePickKey(photo, layer, range.id)];
  // The point is normalised to the photo as it was framed then: a crop,
  // rotation, flip or warp since moves it onto other pixels.
  return pick && sameColour(pick.centre, rangeCentre(range)) && pick.geometry === geometryKeyOf(state.adjustments)
    ? pick
    : null;
}

/** The recipe values that decide where a normalised preview point lies on the photograph. */
export function geometryKeyOf(adjustments: Adjustments): string {
  const values = adjustments as unknown as Record<string, unknown>;
  return JSON.stringify(
    [...GEOMETRY_KEYS, ...ORIENTATION_KEYS, 'crop', 'aspectRatio'].map((key) => values[key] ?? null)
  );
}

/**
 * What the colour entering the ranges of the photo (`layer` null) or of a
 * mask layer depends on: the recipe without those ranges (their own shifts,
 * a target, a new range change nothing before them).
 */
export const inputKeyOf = (adjustments: Adjustments, layer: string | null): string =>
  JSON.stringify(withRanges(adjustments, layer, () => []));

/**
 * The renderer's sample at range `range`'s pick: the one kept while nothing
 * before the ranges changed, else taken again now (earlier tools or the mask
 * changed since; the store keeps it), or null
 * for a range not picked in the photo (moved on the wheel since) and when
 * the renderer has no sample there now (the pick keeps its point, without
 * a sample, until one comes).
 */
export async function rangeInputFor(
  photo: string,
  layer: string | null,
  range: ColorRange
): Promise<RangeInput | null> {
  const pick = rangePickOf(photo, layer, range);
  if (!pick) return null;
  const adjustments = useEditorStore.getState().adjustments;
  const inputKey = inputKeyOf(adjustments, layer);
  // A full-resolution probe of the block (two renders) is most of a
  // target's time: only an edit before the ranges asks for it again.
  if (pick.input && pick.inputKey === inputKey) return pick.input;
  const input = await sampleRangeInput(photo, pick.point[0], pick.point[1], layer, adjustments, pick.block ?? null);
  const state = useEditorStore.getState();
  const key = rangePickKey(photo, layer, range.id);
  if (state.colorRangeInputs[key] !== pick) return input;
  // No sample now (a hidden layer, a layer without a mask, no preview): the
  // old one no longer stands for the photo, but the point stays, so the
  // next sample (the layer shown again) picks it up.
  state.setEditor({
    colorRangeInputs: { ...state.colorRangeInputs, [key]: { ...pick, input, inputKey: input ? inputKey : undefined } }
  });
  return input;
}

/** Bumped whenever a pick ends without a photo sample (on the range wheel):
 * samples still on their way then belong to a finished pick. */
let pickGeneration = 0;
export const invalidatePendingColorPicks = () => {
  pickGeneration += 1;
};

/**
 * Samples the displayed colour (11 × 11 preview pixels) at a normalised
 * position and adds a colour editor range there ('new'), adds or moves the
 * skin tone range ('skin') or moves an existing range to it. Returns false
 * when nothing was sampled.
 */
export async function pickColorRange(path: string, target: string, x: number, y: number) {
  // The picker as it was when the sample was asked for: a picker finished
  // or changed meanwhile (e.g. completed on the range wheel) drops it.
  const asked = useEditorStore.getState();
  const armed = asked.colorPickerTarget;
  // A picker started on a layer edits that layer's ranges.
  const layer = asked.colorPickerLayer;
  const generation = pickGeneration;
  const targetPick = isTargetPick(target);
  // A range pick reads the colour entering the colour editor (unclipped,
  // before the ranges' own change); the shown colour is the fallback and
  // a target pick's colour.
  // The eyedropper's block on the shown frame: its mean, and where it lies
  // (the range's later samples and the target's measurements read the same
  // block at full resolution).
  const patch = await invoke<{ mean: [number, number, number] | null; frame: [number, number] } | null>(
    Invokes.SamplePreviewPatch,
    { path, normalizedX: x, normalizedY: y }
  ).catch(() => null);
  const rgb = patch?.mean ?? null;
  const block = patch?.frame ? eyedropperBlock(x, y, patch.frame) : null;
  const input = targetPick || !rgb ? null : await sampleRangeInput(path, x, y, layer, asked.adjustments, block);
  const stillArmed = () => {
    const state = useEditorStore.getState();
    return (
      state.selectedImage?.path === path &&
      generation === pickGeneration &&
      state.colorPickerTarget === armed &&
      state.colorPickerLayer === layer
    );
  };
  if (!rgb || !stillArmed()) return false;
  if (targetPick) {
    const lab = linearToLab(srgb8ToLinear(rgb));
    const rangeId = target === TARGET_PICK ? null : target.slice(TARGET_PICK.length + 1);
    useEditorStore.getState().setEditor({
      colorTarget: { lab, sampledIn: path, appliedTo: rangeId ? { photo: path, layer, rangeId } : null },
      colorPickerTarget: null,
      colorPickerLayer: null
    });
    if (rangeId) void applyColorTarget(path, layer, rangeId, lab, path);
    return true;
  }
  if (input) {
    // The renderer's colour at the pick: the range's colour as it is, no
    // undoing of the ranges from the shown colour (the whole block's mean).
    const [lightness, a, b] = input.mean ?? meanOklab(input.pixels);
    const hue = ((Math.atan2(b, a) * 180) / Math.PI + 360) % 360;
    const chroma = Math.hypot(a, b);
    let activeId: string | null = null;
    updateAdjustments((prev: Adjustments) =>
      withRanges(prev, layer, (ranges) => {
        const picked = applyColorPick(ranges, target, MAX_COLOR_RANGES, hue, chroma, lightness);
        activeId = picked.id;
        return picked.ranges;
      })
    );
    const after = useEditorStore.getState();
    const key = activeId ? rangePickKey(path, layer, activeId) : null;
    const picked = activeId ? colorRangesOfLayer(after.adjustments, layer).find((r) => r.id === activeId) : undefined;
    after.setEditor({
      colorPickerTarget: null,
      colorPickerLayer: null,
      activeColorRangeId: activeId,
      colorPicksThroughRanges: after.colorPicksThroughRanges.filter((k) => k !== key),
      colorRangeInputs:
        key && picked
          ? {
              ...after.colorRangeInputs,
              [key]: {
                point: [x, y],
                centre: rangeCentre(picked),
                geometry: geometryKeyOf(asked.adjustments),
                block,
                input,
                inputKey: inputKeyOf(asked.adjustments, layer)
              }
            }
          : after.colorRangeInputs
    });
    return true;
  }
  const shown = srgbToOklch(rgb);
  let activeId: string | null = target;
  let uncertain = false;
  const next = (ranges: ColorRange[], limit: number) => {
    // The sample shows the ranges' work (a range picked again after its own
    // shift, or a colour another range moves): select by the colour before.
    const h = (shown.hue * Math.PI) / 180;
    const again = target === 'skin' ? skinRangeOf(ranges) : ranges.find((r) => r.id === target);
    const sample: Vec3 = [shown.lightness, shown.chroma * Math.cos(h), shown.chroma * Math.sin(h)];
    const [lightness, a, b] = colorBeforeRanges(ranges, sample, again ? [rangeCentre(again)] : []);
    const hue = ((Math.atan2(b, a) * 180) / Math.PI + 360) % 360;
    const picked = applyColorPick(ranges, target, limit, hue, Math.hypot(a, b), lightness);
    const moved = Math.hypot(lightness - sample[0], a - sample[1], b - sample[2]) > 1e-4;
    uncertain = moved && inverseGain(ranges, [lightness, a, b]) > UNCERTAIN_GAIN;
    activeId = picked.id;
    return picked.ranges;
  };
  updateAdjustments((prev: Adjustments) => withRanges(prev, layer, (ranges) => next(ranges, MAX_COLOR_RANGES)));
  const after = useEditorStore.getState();
  const key = activeId ? rangePickKey(path, layer, activeId) : null;
  const through = after.colorPicksThroughRanges.filter((k) => k !== key);
  after.setEditor({
    colorPickerTarget: null,
    colorPickerLayer: null,
    activeColorRangeId: activeId,
    colorPicksThroughRanges: uncertain && key ? [...through, key] : through
  });
  return true;
}

// --- Target colour: live redraw, closed loop, measured readout -----------

/** The recipe the editor shows for `photo`: a pending drag's draft, else the store's. */
const shownRecipe = (photo: string) => draftFor(photo) ?? useEditorStore.getState().adjustments;

/**
 * The renderer's result at range `range`'s pick with the layer's ranges
 * `ranges` (`sample_color_range_result`: the sRGB render with every tool
 * and the output's mapping, the picker's block), and the recipe it rendered;
 * null without a pick or a render.
 */
async function measureRanges(
  photo: string,
  layer: string | null,
  pick: ColorRangePick,
  ranges: ColorRange[]
): Promise<{ lab: Vec3 | null; unmeasured: string | null; recipe: Adjustments }> {
  const recipe = withRanges(shownRecipe(photo), layer, () => ranges);
  const result = await invoke<{ pixels: Vec3[]; unmeasured: string | null } | null>(Invokes.SampleColorRangeResult, {
    path: photo,
    normalizedX: pick.point[0],
    normalizedY: pick.point[1],
    block: pick.block ?? null,
    adjustments: recipe
  }).catch(() => null);
  const lab = result?.pixels?.length ? meanDisplayLab(result.pixels) : null;
  return { lab, unmeasured: lab ? null : (result?.unmeasured ?? 'unavailable'), recipe };
}

const remember = (
  photo: string,
  layer: string | null,
  rangeId: string,
  recipe: Adjustments,
  lab: Vec3 | null,
  unmeasured: string | null = null
) =>
  useEditorStore.getState().setEditor({
    colorTargetMeasure: {
      key: rangePickKey(photo, layer, rangeId),
      recipe: JSON.stringify(recipe),
      lab,
      unmeasured: lab ? null : (unmeasured ?? 'unavailable')
    }
  });

/** Measures the target range's result for the recipe as it is (after a
 * slider move or an edit elsewhere), for the readout. */
export async function measureColorTarget(photo: string, layer: string | null, range: ColorRange) {
  const pick = rangePickOf(photo, layer, range);
  if (!pick || live || correcting) return;
  const ranges = colorRangesOfLayer(shownRecipe(photo), layer);
  const measured = await measureRanges(photo, layer, pick, ranges);
  // Also when nothing came back: the readout then says it is not measured
  // instead of waiting.
  if (!live && !correcting) remember(photo, layer, range.id, measured.recipe, measured.lab, measured.unmeasured);
}

/** A target being typed (or being solved): one interaction, as a slider
 * drag, so the redraws in between and the result are one undo step. */
let live: { photo: string; before: Adjustments; target: ColorTarget | null } | null = null;
/** The latest target request; an older one's results are dropped. */
let targetRequest = 0;
/** Target corrections on their way: the readout's own measurement waits for them. */
let correcting = 0;

function beginLive(photo: string) {
  if (live) return;
  const state = useEditorStore.getState();
  const target = state.colorTarget;
  // Starting an interaction commits an edit still waiting as its own step.
  state.setEditor({ isSliderDragging: true });
  live = { photo, before: useEditorStore.getState().adjustments, target };
}

function endLive() {
  if (!live) return;
  live = null;
  useEditorStore.getState().setEditor({ isSliderDragging: false });
}

/** Where a target reads and writes the ranges it edits: the colour panel's
 * (its `change`, one edit), or the editor's recipe (the photo pickers). */
export interface TargetRanges {
  ranges(): ColorRange[];
  apply(ranges: ColorRange[]): void;
}

const recipeRanges = (photo: string, layer: string | null): TargetRanges => ({
  ranges: () => colorRangesOfLayer(shownRecipe(photo), layer),
  apply: (next) => updateAdjustments((prev: Adjustments) => withRanges(prev, layer, () => next))
});

/**
 * A valid value typed into a target field, before Return: the photo is
 * redrawn with the model's solve at once (no measurement), as Capture One
 * redraws while digits are typed; Return commits it (`applyColorTarget`).
 */
export function previewColorTarget(
  photo: string,
  layer: string | null,
  rangeId: string,
  lab: Vec3,
  io: TargetRanges = recipeRanges(photo, layer)
) {
  const state = useEditorStore.getState();
  const ranges = io.ranges();
  const range = ranges.find((r) => r.id === rangeId);
  if (state.selectedImage?.path !== photo || !range || !lab.every(Number.isFinite)) return;
  targetRequest += 1;
  beginLive(photo);
  const input = rangePickOf(photo, layer, range)?.input;
  useEditorStore.getState().setEditor({ colorTarget: { lab, sampledIn: null, appliedTo: { photo, layer, rangeId } } });
  io.apply(rangesWithTarget(ranges, rangeId, lab, input));
}

/** Escape while typing: the recipe and the target as they were before. */
export function cancelColorTargetPreview() {
  const was = live;
  if (!was) return;
  targetRequest += 1;
  live = null;
  const state = useEditorStore.getState();
  if (state.selectedImage?.path !== was.photo) {
    state.setEditor({ isSliderDragging: false });
    return;
  }
  // The redraws in between leave no step behind.
  flushSliderDraft();
  debouncedSetHistory.cancel();
  state.setEditor({ adjustments: was.before, colorTarget: was.target, isSliderDragging: false });
}

/**
 * Moves range `rangeId` (of the photo or of mask layer `layer`) onto the
 * target `lab` (CIELAB D50) — one edit, one undo step. With a pick in the
 * photo the model's solve is shown at once, then corrected on the
 * renderer's measured result at the pick (`solveColorTargetMeasured`) and
 * that measurement kept for the readout; without one, the model's solve.
 */
export async function applyColorTarget(
  photo: string,
  layer: string | null,
  rangeId: string,
  lab: Vec3,
  sampledIn: string | null,
  io: TargetRanges = recipeRanges(photo, layer)
) {
  const state = useEditorStore.getState();
  const range = io.ranges().find((r) => r.id === rangeId);
  if (state.selectedImage?.path !== photo || !range || !lab.every(Number.isFinite)) return;
  const mine = ++targetRequest;
  state.setEditor({ colorTarget: { lab, sampledIn, appliedTo: { photo, layer, rangeId } } });
  const pick = rangePickOf(photo, layer, range);
  // The model's solve at once, and the interaction of a typed value (its
  // redraws) ends: the photo renders at full quality now and the edit is
  // one undo step, which the measured correction joins below.
  io.apply(rangesWithTarget(io.ranges(), rangeId, lab, pick?.input));
  endLive();
  if (!pick) return;
  const model = useEditorStore.getState().adjustments;
  // Another target, another photo or range, or any edit meanwhile (Undo,
  // Redo, a slider): the correction belongs to a recipe that is gone.
  const stale = () => {
    const now = useEditorStore.getState();
    const applied = now.colorTarget?.appliedTo;
    return (
      mine !== targetRequest ||
      now.adjustments !== model ||
      now.selectedImage?.path !== photo ||
      applied?.rangeId !== rangeId ||
      applied.layer !== layer
    );
  };
  // The measured correction; false when it was dropped on the way.
  // The correction reads and writes the ranges of the layer it started on,
  // whatever the panel shows meanwhile (another layer selected).
  const own = recipeRanges(photo, layer);
  // Profiling builds: where a target's time goes (the input's sample, each
  // measurement, the correction).
  const began = performance.now();
  const since = () => Math.round(performance.now() - began);
  const measures: number[][] = [];
  const correct = async () => {
    const fresh = (await rangeInputFor(photo, layer, range)) ?? pick.input;
    const inputMs = since();
    if (stale()) return false;
    const current = own.ranges();
    if (!current.some((r) => r.id === rangeId)) return false;
    const recipes = new Map<ColorRange[], Adjustments>();
    let unmeasured: string | null = null;
    const loop = await solveColorTargetMeasured(current, rangeId, lab, fresh, async (ranges) => {
      if (stale()) return null;
      const from = since();
      const measured = await measureRanges(photo, layer, pick, ranges);
      measures.push([from, since()]);
      recipes.set(ranges, measured.recipe);
      unmeasured = measured.unmeasured;
      return measured.lab;
    });
    if (stale() || !loop) return false;
    const solved = shiftsOf(loop.ranges.find((r) => r.id === rangeId)!);
    const first = shiftsOf(current.find((r) => r.id === rangeId)!);
    let corrected = false;
    if (
      solved.hueShift !== first.hueShift ||
      solved.saturation !== first.saturation ||
      solved.lightnessShift !== first.lightnessShift
    ) {
      corrected = true;
      const next = current.map((x) => (x.id === rangeId ? { ...x, ...solved } : x));
      // The first solve's step already written (the measurement came after
      // the history's pause): the correction replaces it, one step. Still
      // pending: the edit joins it.
      if (
        !amendHistoryStep(
          model,
          withRanges(model, layer, () => next)
        )
      )
        own.apply(next);
    }
    reportPerformance('color_target_correction', {
      input_ms: inputMs,
      measures,
      corrections: loop.corrections,
      changed: corrected ? 1 : 0,
      done_ms: since()
    });
    const recipe = recipes.get(loop.ranges);
    if (loop.measured && recipe) remember(photo, layer, rangeId, recipe, loop.measured.lab);
    // Not measurable: the readout says so for the recipe as it is now.
    else remember(photo, layer, rangeId, shownRecipe(photo), null, unmeasured);
    return true;
  };
  correcting += 1;
  const measured = await correct().finally(() => {
    correcting -= 1;
  });
  // Dropped on the way (an edit meanwhile): the readout measures the recipe
  // as it is, as after any edit.
  if (!measured && !correcting) {
    const now = io.ranges().find((r) => r.id === rangeId);
    if (now && useEditorStore.getState().selectedImage?.path === photo) void measureColorTarget(photo, layer, now);
  }
}

const shiftsOf = (r: ColorRange) => ({
  hueShift: r.hueShift,
  saturation: r.saturation,
  lightnessShift: r.lightnessShift
});
