/**
 * The clipping warnings' view state (Lightroom's J): clipped highlights are
 * marked red and clipped shadows blue, each side on its own. View only, never
 * in the recipe; exports reset it (`export_processing`).
 */

/** `showClipping` as the engine's `GlobalAdjustments::show_clipping` reads
 * it (`show_clipping_value` in crates/engine). It shares the field with the
 * colour editor's range view, `2 + range index` (at most eight ranges), so
 * the both-sides value stays 1 and the single sides take two values beyond
 * any range index. */
export const SHOW_CLIPPING_SIDES = { highlights: 0x4000_0001, shadows: 0x4000_0002 } as const;

/** What the renderer is sent for the two warnings. */
export function showClippingValue(highlights: boolean, shadows: boolean): number {
  if (highlights && shadows) return 1;
  if (highlights) return SHOW_CLIPPING_SIDES.highlights;
  if (shadows) return SHOW_CLIPPING_SIDES.shadows;
  return 0;
}

/** J switches both warnings on unless one is already on: then it switches
 * both off (Lightroom's behaviour after either side was chosen alone). */
export function nextClipSides(current: { highlights: boolean; shadows: boolean }) {
  if (current.highlights || current.shadows) return { highlights: false, shadows: false };
  return { highlights: true, shadows: true };
}

/**
 * Which side of the histogram is clipped, as the renderer finds it
 * (shader.wgsl `analytics_output`): the clipping warnings' own tests on every
 * pixel, whether a warning is on or not. The bins cannot say it, as they are
 * smoothed (a value near an end reaches the end's bin). A histogram without
 * the flags lights neither side.
 */
export function histogramClipping(histogram: unknown): { shadows: boolean; highlights: boolean } {
  const flags = (histogram ?? {}) as Record<string, unknown>;
  return {
    shadows: flags.clippedShadows === true,
    highlights: flags.clippedHighlights === true,
  };
}
