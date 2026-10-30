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

const CHANNELS = ["red", "green", "blue"] as const;

/** A channel's bin counts: an array, or the backend's typed array (`data`)
 * or typed array as it comes. */
function binsOf(channel: unknown): ArrayLike<number> {
  const raw =
    channel && typeof channel === "object" && "data" in channel
      ? (channel as { data?: unknown }).data
      : channel;
  return typeof raw === "object" &&
    raw !== null &&
    "length" in raw &&
    typeof (raw as { length: unknown }).length === "number"
    ? (raw as ArrayLike<number>)
    : [];
}

/**
 * Which side of the histogram is clipped, from its first and last bin: a
 * photo can clip without the warnings being switched on, so the corners of
 * the histogram light up on their own. Counts too small to show in the
 * plot's height still count as clipped.
 */
export function histogramClipping(histogram: unknown): { shadows: boolean; highlights: boolean } {
  const counts = CHANNELS.map((channel) =>
    binsOf((histogram as Record<string, unknown> | null | undefined)?.[channel]),
  ).filter((bins) => bins.length > 0);
  const clipped = (bin: (bins: ArrayLike<number>) => number | undefined) =>
    counts.some((bins) => {
      const value = bin(bins);
      return typeof value === "number" && Number.isFinite(value) && value > 0;
    });
  return {
    shadows: clipped((bins) => bins[0]),
    highlights: clipped((bins) => bins[bins.length - 1]),
  };
}
