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

/**
 * Samples the displayed colour (11 × 11 preview pixels) at a normalised
 * position and adds a colour editor range there ('new') or moves an
 * existing range to it. Returns false when nothing was sampled.
 */
export async function pickColorRange(path: string, target: string, x: number, y: number) {
  const rgb = await invoke<[number, number, number] | null>(Invokes.SamplePreviewColor, {
    path,
    normalizedX: x,
    normalizedY: y,
  });
  const state = useEditorStore.getState();
  if (!rgb || state.selectedImage?.path !== path) return false;
  const { hue, chroma, lightness } = srgbToOklch(rgb);
  let activeId = target;
  // A picker started on a layer edits that layer's ranges.
  const layer = state.colorPickerLayer;
  const next = (ranges: ColorRange[], limit: number) => {
    if (target === 'new') {
      if (ranges.length >= limit) return ranges;
      const range = newColorRange(hue, chroma, lightness);
      activeId = range.id;
      return [...ranges, range];
    }
    return ranges.map((range) => (range.id === target ? { ...range, hue, chroma, lightness } : range));
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
