// Generated from the recipe crate's key registry (crates/recipe/src/ts.rs) by
// `cargo run -p rawtonic-recipe --example ts_type`; do not edit. scripts/check.sh
// fails when this file is stale.
import type {
  ColorCalibration,
  ColorGradingProps,
  ColorRange,
  Crop,
  Curves,
  GrayMixer,
  GuidedPerspective,
  Hsl,
  LensDistortionParams,
  MaskAdjustments,
  MaskContainer,
  ParametricCurve,
  SectionVisibility,
  SubMask
} from '../utils/adjustments';

export type WhiteBalanceMode =
  'asShot' | 'daylight' | 'cloudy' | 'shade' | 'tungsten' | 'fluorescent' | 'flash' | 'custom' | 'auto';

export type GrainStructure = 'even' | 'soft' | 'clumped' | 'speckled' | 'gritty';

export type VignetteStyle = 'highlightPriority' | 'colorPriority' | 'paintOverlay';

export type LensBlurShape = 'circle' | 'hexagon' | 'octagon' | 'ring';

/** 'camera': the RAW's own correction data (Fujifilm, Sony, Panasonic, …). */
export type LensCorrectionMode = 'auto' | 'manual' | 'camera';

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

  // lens
  lensCorrectionMode?: LensCorrectionMode;
  /**
   * Which cameras' CA data camera mode reads (crates/engine/src/embedded_lens.rs `CA_VERSION`): 1 = also Panasonic's;
   * missing = Fujifilm and Sony only (recipes from before it, so saving them again keeps their parameters).
   */
  cameraCaVersion?: number;
  /**
   * How camera mode reads the camera data's geometry (embedded_lens.rs `GEOMETRY_VERSION`): 1 = Sony's distortion as
   * Camera Raw reads it; 2 = also Nikon Z framed as the camera frames it; 3 = also Canon's CTMD distortion; 4 = also
   * Samsung's distortion; 5 = also Panasonic's and Fujifilm's framing as the cameras frame it; 6 = also Sony's knot
   * table warped as it is (lens model 2); 7 = also Fujifilm's (lens model 3); 8 = also Nikon Z corrected about the
   * shot's own optical centre (`centre_x`, `centre_y`); 9 = also Fujifilm's distortion knots on the whole sensor in a
   * crop mode (Sports Finder); 10 = also on normal frames where Fujifilm's knots reach past the frame's corner; missing
   * = as before (saving such recipes again keeps their parameters).
   */
  cameraGeometryVersion?: number;
  lensDistortionAmount?: number;
  lensVignetteAmount?: number;
  lensTcaAmount?: number;
  lensDistortionEnabled?: boolean;
  lensTcaEnabled?: boolean;
  lensVignetteEnabled?: boolean;
  /**
   * Manual lens vignetting (manual_vignette.rs): a radial exposure gain of the uncropped frame, −100…+100 (positive
   * brightens the corners); missing = none. Lightroom's Lens Vignetting.
   */
  lensManualVignetteAmount?: number;
  /** Its midpoint, 0…100 (missing = 50): lower reaches further in. */
  lensManualVignetteMidpoint?: number;
  lensDistortionParams?: LensDistortionParams | null;
  lensMaker?: string | null;
  /**
   * The lens choice `lensDistortionParams` were resolved for; written by the backend's save (edits.rs, LensLookup) and
   * never copied: a save keeps the parameters while it matches the recipe's lens choice.
   */
  lensProfileFor?: string | null;
  lensModel?: string | null;
  /**
   * Lateral chromatic aberration measured on the photo and corrected (crates/engine/src/lateral_ca.rs): true = on
   * (replacing the lens data's own CA correction), false = off; missing = off before `autoCaVersion` 2, from it on
   * wherever the lens correction has no CA data of its own, from 3 also where it has (replacing them only where it
   * measures clearly better).
   */
  autoChromaticAberration?: boolean;
  /**
   * The automatic lateral CA estimate (lateral_ca.rs `VERSION`): 2 = refined at full resolution, applied only when
   * trustworthy, on by default where the lens data has no CA correction; 3 = the same, and where the lens data has one
   * it competes with it; missing = 1 (recipes from before). New RAW edits only.
   */
  autoCaVersion?: number;

  // masks
  masks?: MaskContainer[];
}

/** Mask-local fields, typed by the same keys as the photo; all optional as saved. */
export interface MaskLocalKeys {
  id?: string;
  sectionVisibility?: SectionVisibility;
  defringe?: number;
  moireAmount?: number;
  moirePattern?: number;
  exposure?: number;
  brightness?: number;
  contrast?: number;
  highlights?: number;
  shadows?: number;
  whites?: number;
  blacks?: number;
  temperature?: number;
  tint?: number;
  vibrance?: number;
  saturation?: number;
  hue?: number;
  sharpness?: number;
  sharpnessThreshold?: number;
  lumaNoiseReduction?: number;
  colorNoiseReduction?: number;
  clarity?: number;
  dehaze?: number;
  structure?: number;
  glowAmount?: number;
  halationAmount?: number;
  flareAmount?: number;
  colorGrading?: ColorGradingProps;
  hsl?: Hsl;
  colorRanges?: ColorRange[];
  curves?: Curves;
  pointCurves?: Curves;
  parametricCurve?: ParametricCurve;
  curveMode?: 'point' | 'parametric';
}

/** Mask container fields; all optional as saved. */
export interface MaskContainerKeys {
  id?: string;
  name?: string;
  visible?: boolean;
  invert?: boolean;
  opacity?: number;
  /**
   * 2: 16-bit masks; 3: also Refine edge on both sides of a thin mask; 4: also AI masks grown and feathered around the
   * crop; absent: 8 bits.
   */
  maskVersion?: number;
  /** Refine edge radius in pixels of the full photo (maskVersion 2+). */
  refineEdge?: number;
  adjustments?: MaskAdjustments;
  subMasks?: SubMask[];
}
