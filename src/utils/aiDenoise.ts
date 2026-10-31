import type { SelectedImage } from '../components/ui/AppProperties';
import { colorNoiseMultiscale, deadPixelAdjacent, unbiasedBlack, type Adjustments } from './adjustments';

/** A recipe's AI Denoise amount (0 … 100) as the backend reads it
 * (`ai_denoise_amount`): off with Detail or its Noise reduction tool
 * switched off. */
export function aiDenoiseAmount(
  adjustments: Pick<Adjustments, 'aiDenoise'> & { sectionVisibility?: Record<string, boolean | undefined> },
): number {
  const visibility = adjustments.sectionVisibility ?? {};
  const switchedOff = visibility.details === false || visibility.noiseReduction === false;
  return switchedOff ? 0 : Math.round(Math.min(100, Math.max(0, adjustments.aiDenoise ?? 0)));
}

/**
 * The AI Denoise amount the open RAW must be developed with, or null when
 * its source already carries the recipe's amount, the amount failed before,
 * or a slider drag is still going on. The recipe (`aiDenoise`) is the only
 * truth, so undo, paste and presets develop the source again too.
 */
export function aiDenoiseToApply(
  image: Pick<
    SelectedImage,
    'isDecoded' | 'isRaw' | 'aiDenoise' | 'aiDenoiseVersion' | 'aiDenoiseFailed' | 'aiDenoiseFailedVersion'
  > | null,
  adjustments: Pick<Adjustments, 'aiDenoise' | 'aiDenoiseVersion'> & {
    sectionVisibility?: Record<string, boolean | undefined>;
  },
  dragging: boolean,
): number | null {
  if (!image?.isDecoded || !image.isRaw || dragging) return null;
  const wanted = aiDenoiseAmount(adjustments);
  const have = Math.round(image.aiDenoise ?? 0);
  // The version only matters with AI Denoise on (reset or paste can change it).
  const sameVersion = wanted === 0 || (adjustments.aiDenoiseVersion ?? 1) === (image.aiDenoiseVersion ?? 1);
  // An amount that failed is not retried with the same version.
  const failed =
    wanted === image.aiDenoiseFailed && (adjustments.aiDenoiseVersion ?? 1) === (image.aiDenoiseFailedVersion ?? 1);
  if ((wanted === have && sameVersion) || failed) return null;
  return wanted;
}

/** How the open RAW's source must be developed again (`apply_ai_denoise`). */
export interface SourceDevelop {
  /** AI Denoise amount (0 … 100). */
  aiDenoise: number;
  /** Its `aiDenoiseVersion` (undefined: 1). */
  aiDenoiseVersion?: number;
  /** For the multi-scale colour noise filter: without the decode-time chroma
   * clean-up (undefined: the stored recipe's filter). */
  multiscale?: boolean;
  /** With the second dead-photosite test (`renderVersion` 3; undefined: the
   * stored recipe's). */
  deadPixelAdjacent?: boolean;
  /** With unbiased blacks (`renderVersion` 8; undefined: the stored
   * recipe's). */
  unbiasedBlack?: boolean;
}

/**
 * How the open RAW must be developed again so its source matches the
 * recipe, or null when it does: its AI Denoise amount (`aiDenoiseToApply`)
 * its colour noise filter (`colorNoiseMultiscale`: reset of a legacy
 * edit, paste or a preset can change `colorNoiseVersion`) or its
 * dead-photosite test (`deadPixelAdjacent`) or its black handling
 * (`unbiasedBlack`) differs from the source's. Only decoded RAWs on their file; never during a slider drag.
 */
export function sourceDevelopToApply(
  image: Pick<
    SelectedImage,
    | 'isDecoded'
    | 'isRaw'
    | 'aiDenoise'
    | 'aiDenoiseVersion'
    | 'aiDenoiseFailed'
    | 'aiDenoiseFailedVersion'
    | 'colorNoiseMultiscale'
    | 'colorNoiseMultiscaleFailed'
    | 'deadPixelAdjacent'
    | 'deadPixelAdjacentFailed'
    | 'unbiasedBlack'
    | 'unbiasedBlackFailed'
    | 'offline'
  > | null,
  adjustments: Pick<Adjustments, 'aiDenoise' | 'aiDenoiseVersion'> &
    Pick<Partial<Adjustments>, 'colorNoiseVersion' | 'colorNoiseReduction' | 'renderVersion'> & {
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
  // The same for the dead-photosite test (`renderVersion`: reset of an
  // older edit, undo).
  const haveDead = image.deadPixelAdjacent;
  const wantedDead = image.offline || haveDead === undefined ? haveDead : deadPixelAdjacent(adjustments);
  const dead = wantedDead === image.deadPixelAdjacentFailed ? haveDead : wantedDead;
  // And for the black handling (`renderVersion` 8).
  const haveBlack = image.unbiasedBlack;
  const wantedBlack = image.offline || haveBlack === undefined ? haveBlack : unbiasedBlack(adjustments);
  const black = wantedBlack === image.unbiasedBlackFailed ? haveBlack : wantedBlack;
  if (aiDenoise === null && multiscale === have && dead === haveDead && black === haveBlack) return null;
  const version = aiDenoise === null ? image.aiDenoiseVersion : adjustments.aiDenoiseVersion;
  return {
    aiDenoise: aiDenoise ?? Math.round(image.aiDenoise ?? 0),
    ...(version !== undefined && version !== null ? { aiDenoiseVersion: version } : {}),
    multiscale,
    deadPixelAdjacent: dead,
    unbiasedBlack: black,
  };
}

/**
 * How a recipe develops the RAW before any adjustment (AI Denoise amount and
 * version, colour noise filter, dead-photosite test, black handling), as a string: equal for
 * recipes whose renders can share one source. Empty for other photographs,
 * whose source does not depend on it.
 */
export function developmentSignature(
  adjustments: Pick<Adjustments, 'aiDenoise' | 'aiDenoiseVersion'> &
    Pick<Partial<Adjustments>, 'colorNoiseVersion' | 'colorNoiseReduction' | 'renderVersion'> & {
      sectionVisibility?: Record<string, boolean | undefined>;
    },
  isRaw: boolean,
): string {
  if (!isRaw) return '';
  const amount = aiDenoiseAmount(adjustments);
  return JSON.stringify([
    amount,
    amount > 0 ? (adjustments.aiDenoiseVersion ?? 1) : 1,
    colorNoiseMultiscale(adjustments, true),
    deadPixelAdjacent(adjustments),
    unbiasedBlack(adjustments),
  ]);
}
