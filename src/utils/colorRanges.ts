import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { updateAdjustments } from '../hooks/useEditorActions';
import { MAX_COLOR_RANGES, newColorRange, type Adjustments, type ColorRange } from './adjustments';
import { srgbToOklch } from './oklab';
import { GEOMETRY_KEYS, ORIENTATION_KEYS } from './geometryKeys';
import {
  colorBeforeRanges,
  inverseGain,
  linearToLab,
  linearToOklab,
  meanOklab,
  rangeCentre,
  solveColorTarget,
  srgb8ToLinear,
  type ColorRangePick,
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
function withRanges(
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
 * (`sample_color_range_input`), rendered from `adjustments`; null when the
 * renderer could not probe (no display variant or preview base yet).
 */
export async function sampleRangeInput(
  path: string,
  x: number,
  y: number,
  layer: string | null,
  adjustments: Adjustments
): Promise<RangeInput | null> {
  const sample = await invoke<{ pixels: Vec3[]; influence: number[] } | null>(Invokes.SampleColorRangeInput, {
    path,
    normalizedX: x,
    normalizedY: y,
    adjustments,
    layer
  }).catch(() => null);
  if (!sample?.pixels?.length || sample.influence?.length !== sample.pixels.length) return null;
  return { pixels: sample.pixels.map((p) => linearToOklab(p)), influence: sample.influence };
}

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
 * The renderer's sample at range `range`'s pick, taken again now (earlier
 * tools or the mask may have changed since; the store keeps it), or null
 * for a range not picked in the photo (moved on the wheel since).
 */
export async function rangeInputFor(
  photo: string,
  layer: string | null,
  range: ColorRange
): Promise<RangeInput | null> {
  const pick = rangePickOf(photo, layer, range);
  if (!pick) return null;
  const input = await sampleRangeInput(
    photo,
    pick.point[0],
    pick.point[1],
    layer,
    useEditorStore.getState().adjustments
  );
  if (!input) return pick.input;
  const state = useEditorStore.getState();
  const key = rangePickKey(photo, layer, range.id);
  if (state.colorRangeInputs[key] === pick)
    state.setEditor({ colorRangeInputs: { ...state.colorRangeInputs, [key]: { ...pick, input } } });
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
  const [rgb, input] = await Promise.all([
    invoke<[number, number, number] | null>(Invokes.SamplePreviewColor, {
      path,
      normalizedX: x,
      normalizedY: y
    }),
    targetPick ? Promise.resolve(null) : sampleRangeInput(path, x, y, layer, asked.adjustments)
  ]);
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
    const range = rangeId
      ? colorRangesOfLayer(useEditorStore.getState().adjustments, layer).find((r) => r.id === rangeId)
      : undefined;
    const rangeInput = range ? await rangeInputFor(path, layer, range) : null;
    if (!stillArmed()) return false;
    useEditorStore.getState().setEditor({
      colorTarget: { lab, sampledIn: path, appliedTo: rangeId ? { photo: path, layer, rangeId } : null },
      colorPickerTarget: null,
      colorPickerLayer: null
    });
    if (rangeId)
      updateAdjustments((prev: Adjustments) =>
        withRanges(prev, layer, (r) => rangesWithTarget(r, rangeId, lab, rangeInput))
      );
    return true;
  }
  if (input) {
    // The renderer's colour at the pick: the range's colour as it is, no
    // undoing of the ranges from the shown colour.
    const [lightness, a, b] = meanOklab(input.pixels);
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
              [key]: { point: [x, y], centre: rangeCentre(picked), geometry: geometryKeyOf(asked.adjustments), input }
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
