import type { SelectedImage } from '../components/ui/AppProperties';
import { colorNoiseMultiscale, type Adjustments } from './adjustments';

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

/** How the open RAW's source must be developed again (`apply_ai_denoise`). */
export interface SourceDevelop {
  /** AI Denoise amount (0 … 100). */
  aiDenoise: number;
  /** For the multi-scale colour noise filter: without the decode-time chroma
   * clean-up (undefined: the stored recipe's filter). */
  multiscale?: boolean;
}

/**
 * How the open RAW must be developed again so its source matches the
 * recipe, or null when it does: its AI Denoise amount (`aiDenoiseToApply`)
 * or its colour noise filter (`colorNoiseMultiscale`: reset of a legacy
 * edit, paste or a preset can change `colorNoiseVersion`) differs from the
 * source's. Only decoded RAWs on their file; never during a slider drag.
 */
export function sourceDevelopToApply(
  image: Pick<
    SelectedImage,
    | 'isDecoded'
    | 'isRaw'
    | 'aiDenoise'
    | 'aiDenoiseFailed'
    | 'colorNoiseMultiscale'
    | 'colorNoiseMultiscaleFailed'
    | 'offline'
  > | null,
  adjustments: Pick<Adjustments, 'aiDenoise'> &
    Pick<Partial<Adjustments>, 'colorNoiseVersion' | 'colorNoiseReduction'> & {
      sectionVisibility?: Record<string, boolean | undefined>;
    },
  dragging: boolean,
): SourceDevelop | null {
  if (!image?.isDecoded || !image.isRaw || dragging) return null;
  const aiDenoise = aiDenoiseToApply(image, adjustments, dragging);
  // Unknown for preview data: the backend then takes the stored recipe's.
  const have = image.colorNoiseMultiscale;
  const wanted = image.offline || have === undefined ? have : colorNoiseMultiscale(adjustments, true);
  // A filter whose development failed before is not retried.
  const multiscale = wanted === image.colorNoiseMultiscaleFailed ? have : wanted;
  if (aiDenoise === null && multiscale === have) return null;
  return { aiDenoise: aiDenoise ?? Math.round(image.aiDenoise ?? 0), multiscale };
}
