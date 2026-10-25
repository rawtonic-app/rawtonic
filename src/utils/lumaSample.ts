import { Invokes } from '../components/ui/AppProperties';
import { Mask } from '../components/panel/right/Masks';
import { useEditorStore } from '../store/useEditorStore';
import { updateAdjustments } from '../hooks/useEditorActions';
import { adjustmentsForMaskCommand } from '../hooks/useMaskOverlay';
import type { Adjustments } from './adjustments';
import { invokeEditStep } from './editGate';

/** The tones under a sample on the photo, in percent of white as the luma
 * range compares them (`sample_luma_tones` in the engine): a robust darkest
 * (2nd percentile), the median and a robust brightest tone (98th), and the
 * photo's tone at every whole percentile 0–100 (`luma_levels`). */
export interface LumaSample {
  low: number;
  median: number;
  high: number;
  levels?: number[];
}

export interface LumaRangeBounds {
  rangeLow: number;
  rangeHigh: number;
}

/** The luma range component's settings a sample starts from: its range and
 * falloff, and a Lightroom range imported with a preset (percentiles). */
export interface LumaRangeState extends LumaRangeBounds {
  falloff?: number;
  lrRange?: number[] | null;
}

/** A sample in full-resolution pixels of the displayed photo, before the
 * crop (the viewer's mask coordinates); an area has a size. */
export interface LumaSampleArea {
  x: number;
  y: number;
  width?: number;
  height?: number;
}

/*
 * Lightroom Classic's Luminance Range picker, measured on synthetic ramps and
 * patches and on a raw (notes: reference/luminance-sampling.md). Everything
 * is in the photo's luminance percentiles, not in absolute tones:
 * - a click on a new range sets 20 percentiles around the clicked tone; a
 *   click on a range that has a width keeps that width and centres it on the
 *   clicked tone;
 * - a drag sets the area's darkest to brightest tones;
 * - an edge closer than 15 percentiles to black or white goes to it;
 * - the feather is 25 percentiles on each side.
 */
/** Width (percentiles) a click sets on a new range. */
export const LUMA_CLICK_WIDTH = 0.2;
/** Feather (percentiles) on either side of a sampled range. */
export const LUMA_SAMPLE_FEATHER = 0.25;
/** An edge this close (percentiles) to black or white goes to it. */
export const LUMA_EDGE_SNAP = 0.15;

const IDENTITY_LEVELS = Array.from({ length: 101 }, (_, i) => i);

function usableLevels(levels?: number[]): number[] {
  return levels && levels.length >= 2 ? levels : IDENTITY_LEVELS;
}

/** The percentile (0–1) of a tone among the photo's tones (`levels`: the tone
 * at each evenly spaced percentile); a tone that many pixels share gets the
 * middle of their percentiles. */
export function toPercentile(levels: number[], tone: number): number {
  const n = levels.length - 1;
  if (tone < levels[0]) return 0;
  if (tone > levels[n]) return 1;
  let first = levels.findIndex((level) => level >= tone);
  let last = n;
  while (last > 0 && levels[last] > tone) last--;
  if (first <= last) return (first + last) / 2 / n;
  // Between two levels (first = last + 1).
  first = last + 1;
  const span = levels[first] - levels[last];
  return (last + (span > 0 ? (tone - levels[last]) / span : 0.5)) / n;
}

/** The tone (percent of white) at a percentile (0–1) of the photo's tones. */
export function toTone(levels: number[], percentile: number): number {
  const n = levels.length - 1;
  const at = Math.min(n, Math.max(0, percentile * n));
  const below = Math.floor(at);
  const above = Math.min(n, below + 1);
  return levels[below] + (levels[above] - levels[below]) * (at - below);
}

/**
 * The luma range a sample sets, by Lightroom's rule above, as tones (whole
 * percents, widened outwards so the sample stays covered) and a falloff
 * matching the 25-percentile feather. A click keeps the width of the
 * current range (`current`) unless that is the full range. With `add`
 * (Shift; Lightroom has no adding) the range grows to take in the sample, so
 * several tones can be gathered.
 */
export function lumaRangeFromSample(
  sample: LumaSample,
  area: boolean,
  current?: LumaRangeState | null,
  add = false,
): Required<Omit<LumaRangeState, 'lrRange'>> {
  const levels = usableLevels(sample.levels);
  const pct = (tone: number) => (tone <= 0 ? 0 : tone >= 100 ? 1 : toPercentile(levels, tone));
  const curLow = current ? Math.min(current.rangeLow, current.rangeHigh) : 0;
  const curHigh = current ? Math.max(current.rangeLow, current.rangeHigh) : 100;
  const imported = current?.lrRange && current.lrRange.length === 4 ? current.lrRange : null;
  let low: number;
  let high: number;
  if (area) {
    low = pct(sample.low);
    high = pct(sample.high);
  } else {
    const centre = pct(sample.median);
    let width = LUMA_CLICK_WIDTH;
    if (current && !add) {
      if (imported) width = Math.max(0, imported[2] - imported[1]);
      else if (curLow > 0 || curHigh < 100) width = Math.max(0, pct(curHigh) - pct(curLow));
    }
    low = centre - width / 2;
    high = centre + width / 2;
  }
  if (add && current && !imported) {
    low = Math.min(low, pct(curLow));
    high = Math.max(high, pct(curHigh));
  }
  if (low < LUMA_EDGE_SNAP) low = 0;
  if (high > 1 - LUMA_EDGE_SNAP) high = 1;
  const toneAt = (p: number) => toTone(levels, p);
  const feathers: number[] = [];
  if (low > 0) feathers.push(toneAt(low) - toneAt(Math.max(0, low - LUMA_SAMPLE_FEATHER)));
  if (high < 1) feathers.push(toneAt(Math.min(1, high + LUMA_SAMPLE_FEATHER)) - toneAt(high));
  const falloff = feathers.length
    ? feathers.reduce((sum, f) => sum + f, 0) / feathers.length
    : (current?.falloff ?? 20);
  const clamp = (v: number) => Math.min(100, Math.max(0, v));
  // The sampled tone itself stays inside, whatever the rounding.
  let rangeLow = low <= 0 ? 0 : toneAt(low);
  let rangeHigh = high >= 1 ? 100 : toneAt(high);
  if (!area) {
    rangeLow = Math.min(rangeLow, sample.median);
    rangeHigh = Math.max(rangeHigh, sample.median);
  }
  // Rounded outwards, past floating-point noise (0.2 + 0.1 is not 0.3).
  return {
    rangeLow: clamp(Math.floor(rangeLow + 1e-6)),
    rangeHigh: clamp(Math.ceil(rangeHigh - 1e-6)),
    falloff: clamp(Math.round(falloff)),
  };
}

/**
 * Samples the open photo's tones (`sample_luma_range`) and sets the luma
 * range of layer `layerId` from them (replacing a range imported from
 * Lightroom, as moving its sliders does). Returns false when nothing was
 * sampled (outside the photo, another photo opened meanwhile, no range).
 */
export async function sampleLumaRange(layerId: string, sample: LumaSampleArea, add: boolean): Promise<boolean> {
  const { selectedImage, adjustments } = useEditorStore.getState();
  if (!selectedImage?.isReady) return false;
  const path = selectedImage.path;
  const area = (sample.width ?? 0) > 0 && (sample.height ?? 0) > 0;
  const tones = await invokeEditStep<LumaSample | null>(Invokes.SampleLumaRange, {
    x: sample.x,
    y: sample.y,
    width: area ? sample.width : 0,
    height: area ? sample.height : 0,
    jsAdjustments: adjustmentsForMaskCommand(adjustments),
  });
  if (!tones || useEditorStore.getState().selectedImage?.path !== path) return false;
  let changed = false;
  updateAdjustments((prev: Adjustments) => ({
    ...prev,
    masks: prev.masks.map((layer) =>
      layer.id !== layerId
        ? layer
        : {
            ...layer,
            subMasks: layer.subMasks.map((sub) => {
              if (sub.type !== Mask.LumaRange) return sub;
              const { lrRange: imported, ...parameters } = (sub.parameters ?? {}) as Record<string, unknown>;
              const current: LumaRangeState = {
                rangeLow: Number(parameters.rangeLow ?? 0),
                rangeHigh: Number(parameters.rangeHigh ?? 100),
                falloff: Number(parameters.falloff ?? 20),
                lrRange: Array.isArray(imported) ? (imported as number[]) : null,
              };
              changed = true;
              // The sample replaces an imported Lightroom range (in percentiles).
              return { ...sub, parameters: { ...parameters, ...lumaRangeFromSample(tones, area, current, add) } };
            }),
          },
    ),
  }));
  return changed;
}
