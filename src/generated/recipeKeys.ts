// Generated from the recipe crate's key registry (crates/recipe/src/ts.rs) by
// `cargo run -p rawtonic-recipe --example ts_type`; do not edit. scripts/check.sh
// fails when this file is stale.
import type {
  ColorCalibration,
  ColorGradingProps,
  ColorRange,
  Crop,
  GrayMixer,
  GuidedPerspective,
  Hsl
} from '../utils/adjustments';

export type WhiteBalanceMode =
  'asShot' | 'daylight' | 'cloudy' | 'shade' | 'tungsten' | 'fluorescent' | 'flash' | 'custom' | 'auto';

export type GrainStructure = 'even' | 'soft' | 'clumped' | 'speckled' | 'gritty';

export type VignetteStyle = 'highlightPriority' | 'colorPriority' | 'paintOverlay';

export type LensBlurShape = 'circle' | 'hexagon' | 'octagon' | 'ring';

/** The global recipe keys of the sections in the recipe crate, each optional as in a saved recipe. */
export interface RecipeKeys {
  // tone
  /** Exposure in stops (EV). */
  exposure?: number;
  contrast?: number;
  highlights?: number;
  shadows?: number;
  whites?: number;
  blacks?: number;
  brightness?: number;
  /** Local contrast around the photograph's centre of interest. */
  centré?: number;

  // colour
  temperature?: number;
  tint?: number;
  vibrance?: number;
  saturation?: number;
  hue?: number;
  /**
   * Absolute white balance of RAWs (Lightroom's Temp/Tint, camera space): unset or `asShot` keeps the camera's; a
   * preset or `custom` with `wbTemperature` (K) and `wbTint`. The relative `temperature`/`tint` apply on top (JPEGs,
   * layers, older edits).
   */
  whiteBalance?: WhiteBalanceMode;
  wbTemperature?: number;
  wbTint?: number;
  /** Black & white conversion (Lightroom's Treatment, Capture One's B&W). */
  blackAndWhite?: boolean;
  grayMixer?: GrayMixer;
  /** Colour editor ranges, applied in order. */
  colorRanges?: ColorRange[];
  hsl?: Hsl;
  colorGrading?: ColorGradingProps;
  colorCalibration?: ColorCalibration;

  // detail
  sharpness?: number;
  sharpnessThreshold?: number;
  /** Lightroom's sharpening Radius / Detail / Masking (photo only). */
  sharpenRadius?: number;
  sharpenDetail?: number;
  sharpenMasking?: number;
  /** Capture One's Sharpness Falloff (0–100): more sharpening towards the corners. */
  sharpnessFalloff?: number;
  lumaNoiseReduction?: number;
  colorNoiseReduction?: number;
  /**
   * Lightroom's noise reduction refinements (0–100): luminance Detail (default 50) and Contrast (0), colour Detail (50)
   * and Smoothness (50).
   */
  lumaNoiseDetail?: number;
  lumaNoiseContrast?: number;
  colorNoiseDetail?: number;
  colorNoiseSmoothness?: number;
  clarity?: number;
  dehaze?: number;
  structure?: number;
  chromaticAberrationRedCyan?: number;
  chromaticAberrationBlueYellow?: number;
  /** Lightroom's Defringe: amounts 0–20, hue ranges 0–100. */
  defringePurpleAmount?: number;
  defringePurpleHueLo?: number;
  defringePurpleHueHi?: number;
  defringeGreenAmount?: number;
  defringeGreenHueLo?: number;
  defringeGreenHueHi?: number;
  /** Capture One's Moiré: amount and pattern size, 0–100. */
  moireAmount?: number;
  moirePattern?: number;
  /** Capture One's diffraction correction: sharpening by the f-number and pixel pitch (diffraction.rs), RAW only. */
  diffractionCorrection?: boolean;

  // geometry
  crop?: Crop | null;
  aspectRatio?: number | null;
  rotation?: number;
  orientationSteps?: number;
  flipHorizontal?: boolean;
  flipVertical?: boolean;
  transformDistortion?: number;
  transformVertical?: number;
  transformHorizontal?: number;
  transformRotate?: number;
  transformAspect?: number;
  transformScale?: number;
  transformXOffset?: number;
  transformYOffset?: number;
  guidedPerspective?: GuidedPerspective;

  // effects
  vignetteAmount?: number;
  vignetteMidpoint?: number;
  vignetteRoundness?: number;
  vignetteFeather?: number;
  vignetteHighlights?: number;
  grainAmount?: number;
  grainSize?: number;
  grainRoughness?: number;
  /**
   * Film grain's colour (grain version 3): 0 one neutral layer like black-and-white film, 100 independent dye layers;
   * missing = 0.
   */
  grainColor?: number;
  glowAmount?: number;
  halationAmount?: number;
  flareAmount?: number;
  lutIntensity?: number;
  lutSize?: number;
  lensBlurAmount?: number;
  lensBlurDiffusion?: number;
  lensBlurMinDepth?: number;
  lensBlurMaxDepth?: number;
  lensBlurMinFade?: number;
  lensBlurMaxFade?: number;
  /** The grain a recipe renders (GRAIN_VERSION); missing = 1. */
  grainVersion?: number;
  /** Grain structure; missing = 'even', the original grain (docs/RECIPE_SCHEMA.md). */
  grainStructure?: GrainStructure;
  /** Lightroom's post-crop vignette style and Highlights (0–100). */
  vignetteStyle?: VignetteStyle;
  lensBlurShape?: LensBlurShape;
  lensBlurEnabled?: boolean;
  lutIsSceneReferred?: boolean;
  lutPath?: string | null;
  lutName?: string | null;
  lutData?: string | null;
  lensBlurDepthMap?: string | null;
}
