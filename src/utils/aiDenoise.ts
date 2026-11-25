import { sourceOf } from '../entities/photo/photoRef';
import type { AiDenoiseQuality, SelectedImage } from '../components/ui/AppProperties';
import {
  borderBlack,
  cameraPlaneWarp,
  colorNoiseMultiscale,
  deadPixelAdjacent,
  defectsIsolated,
  defectsTexture,
  defectsBlack,
  defectsCross,
  defectsMedian,
  highlightRatios,
  unbiasedBlack,
  type Adjustments
} from './adjustments';

/** A recipe's AI Denoise amount (0 … 100) as the backend reads it
 * (`ai_denoise_amount`): off with Detail or its Noise reduction tool
 * switched off. */
export function aiDenoiseAmount(
  adjustments: Pick<Adjustments, 'aiDenoise'> & { sectionVisibility?: Record<string, boolean | undefined> }
): number {
  const visibility = adjustments.sectionVisibility ?? {};
  const switchedOff = visibility.details === false || visibility.noiseReduction === false;
  return switchedOff ? 0 : Math.min(100, Math.max(0, adjustments.aiDenoise ?? 0));
}

/** The network a recipe's AI Denoise runs (`AiDenoiseQuality::of`):
 * 'fast' only when the recipe says so, so every recipe saved before the key
 * renders Full as it did. */
export const aiDenoiseQualityOf = (adjustments: { aiDenoiseQuality?: string | null }): AiDenoiseQuality =>
  adjustments.aiDenoiseQuality === 'fast' ? 'fast' : 'full';

/**
 * `next` with the quality a photo's AI Denoise starts with: when an edit turns
 * AI Denoise on (from 0) in a recipe without `aiDenoiseQuality`, the
 * computer's default for new edits (`ai_denoise_default_quality`; null while
 * unknown: nothing, which renders Full). A recipe that already uses AI Denoise
 * keeps what it renders, and a chosen quality is never replaced.
 */
export function withAiDenoiseQualityOnTurnOn<T extends { aiDenoise?: number; aiDenoiseQuality?: AiDenoiseQuality }>(
  previous: { aiDenoise?: number },
  next: T,
  machineDefault: AiDenoiseQuality | null
): T {
  const turnedOn = (previous.aiDenoise ?? 0) <= 0 && (next.aiDenoise ?? 0) > 0;
  if (!turnedOn || next.aiDenoiseQuality || !machineDefault) return next;
  return { ...next, aiDenoiseQuality: machineDefault };
}

// The decoder casts the stored percentage to f32 before dividing by 100
// (metadata::provenance::ai_denoise_amount).
const decoderAmount = (amount: number): number => Math.fround(Math.fround(amount) / 100);
export const sameAiDenoiseAmount = (a: number, b: number): boolean => decoderAmount(a) === decoderAmount(b);

/** The value a Develop slider stores for `key`: AI Denoise in whole steps
 * (drags, fine Option drags, typed values), the amount Develop develops and
 * every export reads, so a new edit renders the same open or not. Recipes
 * that already hold a fraction keep it until the slider is moved. */
export function storedSliderValue(key: string, value: number): number {
  return key === 'aiDenoise' ? Math.round(value) : value;
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
    | 'isDecoded'
    | 'isRaw'
    | 'aiDenoise'
    | 'aiDenoiseVersion'
    | 'aiDenoiseQuality'
    | 'aiDenoiseFailed'
    | 'aiDenoiseFailedVersion'
    | 'aiDenoiseFailedQuality'
    | 'aiDenoiseStandIn'
    | 'decoderStale'
  > | null,
  adjustments: Pick<Adjustments, 'aiDenoise' | 'aiDenoiseVersion' | 'aiDenoiseQuality'> & {
    sectionVisibility?: Record<string, boolean | undefined>;
  },
  dragging: boolean
): number | null {
  if (!image?.isDecoded || !image.isRaw || dragging) return null;
  const wanted = aiDenoiseAmount(adjustments);
  const have = image.aiDenoise ?? 0;
  // Decoded under other decode preferences (Preferences changed since): the
  // source is developed again, whatever its amount.
  if (image.decoderStale) return wanted;
  // The pixels of a sensor AI Denoise skipped stand in for its amount (they
  // are not Apple's engine's): without AI Denoise they are developed again,
  // once (a failure marks the amount).
  const off = sameAiDenoiseAmount(wanted, 0);
  if (off && image.aiDenoiseStandIn && image.aiDenoiseFailed !== 0) return 0;
  // The version and quality only matter with AI Denoise on (reset or paste
  // can change them).
  const quality = aiDenoiseQualityOf(adjustments);
  const sameVersion =
    off ||
    ((adjustments.aiDenoiseVersion ?? 1) === (image.aiDenoiseVersion ?? 1) &&
      quality === (image.aiDenoiseQuality ?? 'full'));
  // An amount that failed is not retried with the same version and quality.
  const failed =
    image.aiDenoiseFailed !== undefined &&
    sameAiDenoiseAmount(wanted, image.aiDenoiseFailed) &&
    (adjustments.aiDenoiseVersion ?? 1) === (image.aiDenoiseFailedVersion ?? 1) &&
    quality === (image.aiDenoiseFailedQuality ?? 'full');
  if ((sameAiDenoiseAmount(wanted, have) && sameVersion) || failed) return null;
  return wanted;
}

/** How the open source must be developed again (`apply_ai_denoise`). */
export interface SourceDevelop {
  /** AI Denoise amount (0 … 100). */
  aiDenoise: number;
  /** Its `aiDenoiseVersion` (undefined: 1). */
  aiDenoiseVersion?: number;
  /** Its `aiDenoiseQuality` (undefined: 'full'). */
  aiDenoiseQuality?: AiDenoiseQuality;
  /** For the multi-scale colour noise filter: without the decode-time chroma
   * clean-up (undefined: the stored recipe's filter). */
  multiscale?: boolean;
  /** With the second dead-photosite test (`renderVersion` 3; undefined: the
   * stored recipe's). */
  deadPixelAdjacent?: boolean;
  /** With the isolation test for hot and dead photosites (`renderVersion`
   * 15; undefined: the stored recipe's). */
  defectsIsolated?: boolean;
  /** With its rule for dead photosites in busy texture (`renderVersion` 16;
   * undefined: the stored recipe's). */
  defectsTexture?: boolean;
  /** With the rule for dead photosites at black (clear of the noise) and
   * the colour-ratio repair (`renderVersion` 17;
   * undefined: the stored recipe's). */
  defectsBlack?: boolean;
  /** With the looser line guard at black and the green colour-ratio repair
   * (`renderVersion` 18; undefined: the stored recipe's). */
  defectsCross?: boolean;
  /** With the median of three repairs for dead photosites (`renderVersion`
   * 19; undefined: the stored recipe's). */
  defectsMedian?: boolean;
  /** With unbiased blacks (`renderVersion` 8; undefined: the stored
   * recipe's). */
  unbiasedBlack?: boolean;
  /** With the masked border's black beyond 3 extra levels (`renderVersion`
   * 11; undefined: the stored recipe's). */
  borderBlack?: boolean;
  /** With the ratio highlight rebuild (`renderVersion` 9; undefined: the
   * stored recipe's). */
  highlightRatios?: boolean;
  /** With a DNG lens warp of the camera planes (`renderVersion` 14;
   * undefined: the stored recipe's). */
  cameraPlaneWarp?: boolean;
}

/**
 * How the open source must be developed again so it matches Preferences and the
 * recipe, or null when it does: its AI Denoise amount (`aiDenoiseToApply`)
 * its colour noise filter (`colorNoiseMultiscale`: reset of a legacy
 * edit, paste or a preset can change `colorNoiseVersion`) or its
 * dead-photosite test (`deadPixelAdjacent`, `defectsIsolated`, `defectsTexture`,
 * `defectsBlack`, `defectsCross`, `defectsMedian`), its black handling
 * (`unbiasedBlack`, `borderBlack`), its highlight rebuild (`highlightRatios`)
 * or a DNG's lens warp (`cameraPlaneWarp`) differs from the source's. Other
 * formats only develop again when their preprocessing preferences change.
 */
export function sourceDevelopToApply(
  image: Pick<
    SelectedImage,
    | 'isDecoded'
    | 'isRaw'
    | 'aiDenoise'
    | 'aiDenoiseVersion'
    | 'aiDenoiseQuality'
    | 'aiDenoiseFailed'
    | 'aiDenoiseFailedVersion'
    | 'aiDenoiseFailedQuality'
    | 'aiDenoiseStandIn'
    | 'decoderStale'
    | 'colorNoiseMultiscale'
    | 'colorNoiseMultiscaleFailed'
    | 'deadPixelAdjacent'
    | 'deadPixelAdjacentFailed'
    | 'defectsIsolated'
    | 'defectsIsolatedFailed'
    | 'defectsTexture'
    | 'defectsTextureFailed'
    | 'defectsBlack'
    | 'defectsBlackFailed'
    | 'defectsCross'
    | 'defectsCrossFailed'
    | 'defectsMedian'
    | 'defectsMedianFailed'
    | 'unbiasedBlack'
    | 'unbiasedBlackFailed'
    | 'borderBlack'
    | 'borderBlackFailed'
    | 'highlightRatios'
    | 'highlightRatiosFailed'
    | 'cameraPlaneWarp'
    | 'cameraPlaneWarpFailed'
    | 'offline'
  > | null,
  adjustments: Pick<Adjustments, 'aiDenoise' | 'aiDenoiseVersion' | 'aiDenoiseQuality'> &
    Pick<Partial<Adjustments>, 'colorNoiseVersion' | 'colorNoiseReduction' | 'renderVersion'> & {
      sectionVisibility?: Record<string, boolean | undefined>;
    },
  dragging: boolean
): SourceDevelop | null {
  if (!image?.isDecoded || dragging) return null;
  if (!image.isRaw) return image.decoderStale ? { aiDenoise: 0 } : null;
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
  // And for the isolation test (`renderVersion` 15).
  const haveIsolated = image.defectsIsolated;
  const wantedIsolated = image.offline || haveIsolated === undefined ? haveIsolated : defectsIsolated(adjustments);
  const isolated = wantedIsolated === image.defectsIsolatedFailed ? haveIsolated : wantedIsolated;
  // And for its texture rule (`renderVersion` 16).
  const haveTexture = image.defectsTexture;
  const wantedTexture = image.offline || haveTexture === undefined ? haveTexture : defectsTexture(adjustments);
  const texture = wantedTexture === image.defectsTextureFailed ? haveTexture : wantedTexture;
  // And for the rule for dead photosites at black (`renderVersion` 17).
  const haveDeadBlack = image.defectsBlack;
  const wantedDeadBlack = image.offline || haveDeadBlack === undefined ? haveDeadBlack : defectsBlack(adjustments);
  const deadBlack = wantedDeadBlack === image.defectsBlackFailed ? haveDeadBlack : wantedDeadBlack;
  // And for the looser guard and green ratio repair (`renderVersion` 18).
  const haveCross = image.defectsCross;
  const wantedCross = image.offline || haveCross === undefined ? haveCross : defectsCross(adjustments);
  const cross = wantedCross === image.defectsCrossFailed ? haveCross : wantedCross;
  // And for the median of three dead repairs (`renderVersion` 19).
  const haveMedian = image.defectsMedian;
  const wantedMedian = image.offline || haveMedian === undefined ? haveMedian : defectsMedian(adjustments);
  const median = wantedMedian === image.defectsMedianFailed ? haveMedian : wantedMedian;
  // And for the black handling (`renderVersion` 8).
  const haveBlack = image.unbiasedBlack;
  const wantedBlack = image.offline || haveBlack === undefined ? haveBlack : unbiasedBlack(adjustments);
  const black = wantedBlack === image.unbiasedBlackFailed ? haveBlack : wantedBlack;
  // And for the border's black (`renderVersion` 11).
  const haveBorder = image.borderBlack;
  const wantedBorder = image.offline || haveBorder === undefined ? haveBorder : borderBlack(adjustments);
  const border = wantedBorder === image.borderBlackFailed ? haveBorder : wantedBorder;
  // And for the highlight rebuild (`renderVersion` 9).
  const haveRatios = image.highlightRatios;
  const wantedRatios = image.offline || haveRatios === undefined ? haveRatios : highlightRatios(adjustments);
  const ratios = wantedRatios === image.highlightRatiosFailed ? haveRatios : wantedRatios;
  // And for a DNG's lens warp (`renderVersion` 14; unknown where it does
  // not change the decode).
  const haveWarp = image.cameraPlaneWarp;
  const wantedWarp = image.offline || haveWarp === undefined ? haveWarp : cameraPlaneWarp(adjustments);
  const warp = wantedWarp === image.cameraPlaneWarpFailed ? haveWarp : wantedWarp;
  if (
    aiDenoise === null &&
    multiscale === have &&
    dead === haveDead &&
    isolated === haveIsolated &&
    texture === haveTexture &&
    deadBlack === haveDeadBlack &&
    cross === haveCross &&
    median === haveMedian &&
    black === haveBlack &&
    border === haveBorder &&
    ratios === haveRatios &&
    warp === haveWarp
  )
    return null;
  const version = aiDenoise === null ? image.aiDenoiseVersion : adjustments.aiDenoiseVersion;
  const quality = aiDenoise === null ? (image.aiDenoiseQuality ?? 'full') : aiDenoiseQualityOf(adjustments);
  return {
    aiDenoise: aiDenoise ?? image.aiDenoise ?? 0,
    ...(version !== undefined && version !== null ? { aiDenoiseVersion: version } : {}),
    // Full is what a missing quality develops.
    ...(quality === 'fast' ? { aiDenoiseQuality: quality } : {}),
    multiscale,
    deadPixelAdjacent: dead,
    defectsIsolated: isolated,
    defectsTexture: texture,
    defectsBlack: deadBlack,
    defectsCross: cross,
    defectsMedian: median,
    unbiasedBlack: black,
    borderBlack: border,
    highlightRatios: ratios,
    cameraPlaneWarp: warp
  };
}

/** Whether a file may be a DNG, the one format whose files carry lens
 * opcodes (crates/engine/src/image_loader.rs `may_hold_dng_opcodes`): a
 * `.dng`, or a TIFF the RAW path takes by its content. */
const mayHoldDngOpcodes = (path: string | undefined): boolean => /\.(dng|tiff?)$/i.test(sourceOf(path ?? ''));

/** Whether the open photograph's decode may depend on its lens warp's planes
 * (`cameraPlaneWarp`): the backend reports them only for DNGs whose warp
 * has different coefficients per plane. Where no decode reports them (preview
 * data of a RAW not decoded yet, an offline photograph) it is not known
 * which files those are, so the planes count for a file that may be a DNG:
 * a snapshot of another render version is not shown through the preview
 * proxy built for the edit's version. */
export const warpPlanesMatter = (
  image: Partial<Pick<SelectedImage, 'cameraPlaneWarp' | 'isRaw' | 'isDecoded' | 'offline' | 'path'>> | null | undefined
): boolean =>
  image?.cameraPlaneWarp !== undefined ||
  (image?.isRaw === true && (image.isDecoded === false || !!image.offline) && mayHoldDngOpcodes(image.path));

/**
 * How a recipe develops the RAW before any adjustment (AI Denoise amount and
 * version, colour noise filter, dead-photosite test, black handling and,
 * where they change the decode (`warpPlanes`, `warpPlanesMatter`), its
 * lens warp's planes), as a string: equal for
 * recipes whose renders can share one source. Empty for other photographs,
 * whose source does not depend on it.
 */
export function developmentSignature(
  adjustments: Pick<Adjustments, 'aiDenoise' | 'aiDenoiseVersion'> &
    Pick<Partial<Adjustments>, 'colorNoiseVersion' | 'colorNoiseReduction' | 'renderVersion'> & {
      sectionVisibility?: Record<string, boolean | undefined>;
    },
  isRaw: boolean,
  warpPlanes = false
): string {
  if (!isRaw) return '';
  const amount = aiDenoiseAmount(adjustments);
  return JSON.stringify([
    amount,
    amount > 0 ? (adjustments.aiDenoiseVersion ?? 1) : 1,
    colorNoiseMultiscale(adjustments, true),
    deadPixelAdjacent(adjustments),
    unbiasedBlack(adjustments),
    borderBlack(adjustments),
    defectsIsolated(adjustments),
    defectsTexture(adjustments),
    defectsBlack(adjustments),
    defectsCross(adjustments),
    defectsMedian(adjustments),
    ...(warpPlanes ? [cameraPlaneWarp(adjustments)] : [])
  ]);
}
