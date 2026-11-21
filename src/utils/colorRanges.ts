import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { updateAdjustments } from '../hooks/useEditorActions';
import { MAX_COLOR_RANGES, newColorRange, type Adjustments, type ColorRange } from './adjustments';
import { srgbToOklch } from './oklab';
import {
  colorBeforeRanges,
  inverseGain,
  linearToLab,
  rangeCentre,
  solveColorTarget,
  srgb8ToLinear,
  type Vec3
} from './colorTarget';

/** Error gain above which a colour picked through the ranges is uncertain. */
const UNCERTAIN_GAIN = 3;

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
export function rangesWithTarget(ranges: ColorRange[], id: string, lab: Vec3): ColorRange[] {
  const solve = solveColorTarget(ranges, id, lab);
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
  const rgb = await invoke<[number, number, number] | null>(Invokes.SamplePreviewColor, {
    path,
    normalizedX: x,
    normalizedY: y
  });
  const state = useEditorStore.getState();
  if (!rgb || state.selectedImage?.path !== path) return false;
  if (generation !== pickGeneration || state.colorPickerTarget !== armed || state.colorPickerLayer !== layer)
    return false;
  if (isTargetPick(target)) {
    const lab = linearToLab(srgb8ToLinear(rgb));
    const rangeId = target === TARGET_PICK ? null : target.slice(TARGET_PICK.length + 1);
    useEditorStore.getState().setEditor({
      colorTarget: { lab, sampledIn: path, appliedTo: rangeId ? { photo: path, layer, rangeId } : null },
      colorPickerTarget: null,
      colorPickerLayer: null
    });
    if (rangeId)
      updateAdjustments((prev: Adjustments) => withRanges(prev, layer, (r) => rangesWithTarget(r, rangeId, lab)));
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
