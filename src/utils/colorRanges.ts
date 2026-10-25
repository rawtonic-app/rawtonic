import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { updateAdjustments } from '../hooks/useEditorActions';
import {
  MAX_COLOR_RANGES,
  MAX_LAYER_COLOR_RANGES,
  newColorRange,
  type Adjustments,
  type ColorRange,
} from './adjustments';
import { srgbToOklch } from './oklab';

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
  lightness: number,
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
    id,
  };
}

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
  const rgb = await invoke<[number, number, number] | null>(Invokes.SamplePreviewColor, {
    path,
    normalizedX: x,
    normalizedY: y,
  });
  const state = useEditorStore.getState();
  if (!rgb || state.selectedImage?.path !== path) return false;
  if (state.colorPickerTarget !== armed || state.colorPickerLayer !== layer) return false;
  const { hue, chroma, lightness } = srgbToOklch(rgb);
  let activeId: string | null = target;
  const next = (ranges: ColorRange[], limit: number) => {
    const picked = applyColorPick(ranges, target, limit, hue, chroma, lightness);
    activeId = picked.id;
    return picked.ranges;
  };
  updateAdjustments((prev: Adjustments) =>
    layer
      ? {
          ...prev,
          masks: prev.masks.map((mask) =>
            mask.id === layer
              ? {
                  ...mask,
                  adjustments: {
                    ...mask.adjustments,
                    colorRanges: next(colorRangesOf(mask.adjustments), MAX_LAYER_COLOR_RANGES),
                  },
                }
              : mask,
          ),
        }
      : { ...prev, colorRanges: next(colorRangesOf(prev), MAX_COLOR_RANGES) },
  );
  useEditorStore.getState().setEditor({ colorPickerTarget: null, colorPickerLayer: null, activeColorRangeId: activeId });
  return true;
}
