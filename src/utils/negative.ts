import { invoke } from '@tauri-apps/api/core';
import { Invokes } from '../components/ui/AppProperties';
import { useEditorStore } from '../store/useEditorStore';
import { newNegative, type Adjustments, type NegativeKind, type NegativeSettings } from './adjustments';

type Rgb = [number, number, number];

interface NegativeAnalysis {
  base: Rgb;
  highlight: Rgb;
  baseFromBorder: boolean;
}

/** The loaded photo's film base (found beside the frame: outside the crop,
 * else on the border) and the frame's highlight (`negative::analyze`); null
 * when `path` is no longer the loaded photo. */
export const analyzeNegative = (path: string, adjustments: Adjustments) =>
  invoke<NegativeAnalysis | null>(Invokes.AnalyzeNegative, { path, jsAdjustments: adjustments });

/** The negative conversion of the loaded photo `path` from its analysis
 * with the geometry and crop of `adjustments`, keeping their conversion's
 * type, balance, exposure and grade when they have one; null when the photo
 * changed meanwhile. */
export async function autoNegative(
  path: string,
  adjustments: Adjustments,
  type: NegativeKind = 'color',
): Promise<NegativeSettings | null> {
  const previous = adjustments.negative;
  const analysis = await analyzeNegative(path, adjustments);
  if (!analysis || useEditorStore.getState().selectedImage?.path !== path) return null;
  const fresh = newNegative(analysis.base, analysis.highlight, previous?.type ?? type);
  return previous ? { ...previous, enabled: true, base: fresh.base, highlight: fresh.highlight } : fresh;
}

/** The film base under the picker at a normalised position of the shown
 * photo (the base before the conversion); the recipe with it as its
 * negative's base, or null when nothing was sampled. */
export async function pickFilmBase(
  path: string,
  adjustments: Adjustments,
  normalizedX: number,
  normalizedY: number,
): Promise<Rgb | null> {
  const rgb = await invoke<Rgb | null>(Invokes.SampleFilmBase, {
    path,
    normalizedX,
    normalizedY,
    jsAdjustments: adjustments,
  });
  if (!rgb || !rgb.every((v) => Number.isFinite(v) && v > 0)) return null;
  return rgb;
}
