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

/** Bumped when the tool is reset: an analysis asked for before then is
 * dropped when it arrives. */
let analysisGeneration = 0;
export const cancelNegativeAnalysis = () => {
  analysisGeneration += 1;
};
export const negativeAnalysisGeneration = () => analysisGeneration;

/** Whether two recipes frame the scan alike for the analysis (crop,
 * turns, flips, straightening, transform and lens corrections). */
export function sameAnalysisInputs(a: Partial<Adjustments>, b: Partial<Adjustments>) {
  const inputs = (r: Partial<Adjustments>) =>
    JSON.stringify(
      Object.entries(r)
        .filter(
          ([key]) =>
            ['crop', 'rotation', 'orientationSteps', 'flipHorizontal', 'flipVertical', 'guidedPerspective'].includes(
              key,
            ) || key.startsWith('transform') || key.startsWith('lens'),
        )
        .sort(([x], [y]) => x.localeCompare(y)),
    );
  return inputs(a) === inputs(b);
}

/** Whether two conversions name the same film (base and highlight). */
export function sameFilm(a?: NegativeSettings | null, b?: NegativeSettings | null) {
  return (
    !!a && !!b && a.base.every((v, i) => v === b.base[i]) && a.highlight.every((v, i) => v === b.highlight[i])
  );
}

/** Bumped each time the film base picker is armed or disarmed: a sample
 * from an earlier picker session is dropped. */
let pickSession = 0;
export const filmBasePickSession = () => pickSession;
/** Arms (or disarms) the film base picker, ending any earlier session. */
export function setFilmBasePicking(on: boolean) {
  pickSession += 1;
  useEditorStore.getState().setEditor(
    on ? { filmBasePicking: true, isWbPickerActive: false, colorPickerTarget: null } : { filmBasePicking: false },
  );
}

/** What a film base pick was asked for: the photo, the picker session and
 * the recipe then (its film and the framing and lens corrections the
 * sample was taken with). */
export interface FilmBasePick {
  path: string;
  session: number;
  adjustments: Adjustments;
}

export const startFilmBasePick = (path: string, adjustments: Adjustments): FilmBasePick => ({
  path,
  session: pickSession,
  adjustments,
});

/** The recipe with a picked film base, or `prev` when the pick no longer
 * applies: the picker was cancelled, re-armed or another picker took over,
 * another photo is shown, the conversion is gone or names another film (a
 * preset, a snapshot), or the framing or lens corrections the sample was
 * taken with changed. The tool's eye is left as it is. */
export function withPickedFilmBase(prev: Adjustments, pick: FilmBasePick, base: Rgb): Adjustments {
  const state = useEditorStore.getState();
  if (
    !state.filmBasePicking ||
    pick.session !== pickSession ||
    state.selectedImage?.path !== pick.path ||
    state.colorPickerTarget !== null ||
    state.isWbPickerActive ||
    !prev.negative ||
    !sameFilm(prev.negative, pick.adjustments.negative) ||
    !sameAnalysisInputs(prev, pick.adjustments)
  )
    return prev;
  return { ...prev, negative: { ...prev.negative, base } };
}
