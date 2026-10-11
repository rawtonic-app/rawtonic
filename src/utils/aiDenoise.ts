import type { SelectedImage } from '../components/ui/AppProperties';
import type { Adjustments } from './adjustments';

/**
 * The AI Denoise amount the open RAW must be developed with, or null when
 * its source already carries the recipe's amount, the amount failed before,
 * or a slider drag is still going on. The recipe (`aiDenoise`) is the only
 * truth, so undo, paste and presets develop the source again too.
 */
export function aiDenoiseToApply(
  image: Pick<SelectedImage, 'isDecoded' | 'isRaw' | 'aiDenoise' | 'aiDenoiseFailed'> | null,
  adjustments: Pick<Adjustments, 'aiDenoise'> & { sectionVisibility?: Record<string, boolean | undefined> },
  dragging: boolean,
): number | null {
  if (!image?.isDecoded || !image.isRaw || dragging) return null;
  // As the backend reads it (ai_denoise_amount): off with Detail or its
  // Noise reduction tool switched off.
  const visibility = adjustments.sectionVisibility ?? {};
  const switchedOff = visibility.details === false || visibility.noiseReduction === false;
  const wanted = switchedOff ? 0 : Math.round(Math.min(100, Math.max(0, adjustments.aiDenoise ?? 0)));
  const have = Math.round(image.aiDenoise ?? 0);
  if (wanted === have || wanted === image.aiDenoiseFailed) return null;
  return wanted;
}
