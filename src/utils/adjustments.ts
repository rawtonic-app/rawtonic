import { normalizeRelight, type RelightLight, type RelightDepth } from "./relight";
import { Crop } from 'react-image-crop';
import { v4 as uuidv4 } from 'uuid';
import { SubMask, SubMaskMode } from '../components/panel/right/Masks';
import type { AdjustmentLayout, AppSettings } from '../components/ui/AppProperties';

export enum ActiveChannel {
  Blue = 'blue',
  Green = 'green',
  Luma = 'luma',
  Red = 'red',
}

export enum DisplayMode {
  Luma = 'luma',
  Rgb = 'rgb',
  Parade = 'parade',
  Vectorscope = 'vectorscope',
  Histogram = 'histogram',
}

export enum PasteMode {
  Merge = 'merge',
  Replace = 'replace',
}

export interface CopyPasteSettings {
  mode: PasteMode;
  includedAdjustments: Array<string>;
  knownAdjustments: Array<string>;
  autoSync: boolean;
}

export enum BasicAdjustment {
  Blacks = 'blacks',
  Brightness = 'brightness',
  Contrast = 'contrast',
  Exposure = 'exposure',
  Highlights = 'highlights',
  Shadows = 'shadows',
  Whites = 'whites',
}

// Stored brightness uses the engine's exposure-like units, not percentages.
// Keep both editing surfaces in sync without reinterpreting existing recipes.
export const BRIGHTNESS_CONTROL = { min: -5, max: 5, step: 0.01 } as const;
export const MAX_CURVE_POINTS = 16;

export enum ColorAdjustment {
  ColorGrading = 'colorGrading',
  Hsl = 'hsl',
  Hue = 'hue',
  Luminance = 'luminance',
  Saturation = 'saturation',
  Temperature = 'temperature',
  Tint = 'tint',
  Vibrance = 'vibrance',
}

export enum ColorGrading {
  Balance = 'balance',
  Blending = 'blending',
  Global = 'global',
  Highlights = 'highlights',
  Midtones = 'midtones',
  Shadows = 'shadows',
}

export enum DetailsAdjustment {
  Clarity = 'clarity',
  Dehaze = 'dehaze',
  Structure = 'structure',
  Centré = 'centré',
  ColorNoiseReduction = 'colorNoiseReduction',
  LumaNoiseReduction = 'lumaNoiseReduction',
  Sharpness = 'sharpness',
  SharpnessThreshold = 'sharpnessThreshold',
  SharpenRadius = 'sharpenRadius',
  SharpenDetail = 'sharpenDetail',
  SharpenMasking = 'sharpenMasking',
  ChromaticAberrationRedCyan = 'chromaticAberrationRedCyan',
  ChromaticAberrationBlueYellow = 'chromaticAberrationBlueYellow',
}

export enum Effect {
  GrainAmount = 'grainAmount',
  GrainRoughness = 'grainRoughness',
  GrainSize = 'grainSize',
  LutData = 'lutData',
  LutIntensity = 'lutIntensity',
  LutName = 'lutName',
  LutPath = 'lutPath',
  LutSize = 'lutSize',
  LutIsSceneReferred = 'lutIsSceneReferred',
  VignetteAmount = 'vignetteAmount',
  VignetteFeather = 'vignetteFeather',
  VignetteMidpoint = 'vignetteMidpoint',
  VignetteRoundness = 'vignetteRoundness',
  LensBlurEnabled = 'lensBlurEnabled',
  LensBlurAmount = 'lensBlurAmount',
  lensBlurDiffusion = 'lensBlurDiffusion',
  LensBlurShape = 'lensBlurShape',
  LensBlurDepthMap = 'lensBlurDepthMap',
  LensBlurMinDepth = 'lensBlurMinDepth',
  LensBlurMaxDepth = 'lensBlurMaxDepth',
  LensBlurMinFade = 'lensBlurMinFade',
  LensBlurMaxFade = 'lensBlurMaxFade',
}

export enum CreativeAdjustment {
  GlowAmount = 'glowAmount',
  HalationAmount = 'halationAmount',
  FlareAmount = 'flareAmount',
}

enum TransformAdjustment {
  TransformDistortion = 'transformDistortion',
  TransformVertical = 'transformVertical',
  TransformHorizontal = 'transformHorizontal',
  TransformRotate = 'transformRotate',
  TransformAspect = 'transformAspect',
  TransformScale = 'transformScale',
  TransformXOffset = 'transformXOffset',
  TransformYOffset = 'transformYOffset',
}

export enum LensAdjustment {
  LensCorrectionMode = 'lensCorrectionMode',
  LensMaker = 'lensMaker',
  LensModel = 'lensModel',
  LensDistortionAmount = 'lensDistortionAmount',
  LensVignetteAmount = 'lensVignetteAmount',
  LensTcaAmount = 'lensTcaAmount',
  LensDistortionEnabled = 'lensDistortionEnabled',
  LensTcaEnabled = 'lensTcaEnabled',
  LensVignetteEnabled = 'lensVignetteEnabled',
}

export type GuideOrientation = 'vertical' | 'horizontal';

export interface GuideLine {
  id: string;
  type: GuideOrientation;
  p1: Coord;
  p2: Coord;
}

/** Lightroom's Upright modes; "guided" uses drawn lines, the others lines
 * found in the photo (upright.rs). */
export type UprightMode = 'off' | 'auto' | 'level' | 'vertical' | 'full' | 'guided';

export interface GuidedPerspective {
  enabled: boolean;
  lines: GuideLine[];
  autoCrop: boolean;
  upright?: UprightMode;
}

export interface ColorCalibration {
  shadowsTint: number;
  redHue: number;
  redSaturation: number;
  greenHue: number;
  greenSaturation: number;
  blueHue: number;
  blueSaturation: number;
}

export interface ParametricCurveSettings {
  darks: number;
  shadows: number;
  highlights: number;
  lights: number;
  whiteLevel: number;
  blackLevel: number;
  split1: number;
  split2: number;
  split3: number;
}

export interface ParametricCurve {
  [index: string]: ParametricCurveSettings;
  blue: ParametricCurveSettings;
  green: ParametricCurveSettings;
  luma: ParametricCurveSettings;
  red: ParametricCurveSettings;
}

/** Lightroom's B&W mix channels (crs:GrayMixerRed …), −100…100. */
export interface GrayMixer {
  [index: string]: number;
  red: number;
  orange: number;
  yellow: number;
  green: number;
  aqua: number;
  blue: number;
  purple: number;
  magenta: number;
}

export const GRAY_MIXER_CHANNELS = [
  'red',
  'orange',
  'yellow',
  'green',
  'aqua',
  'blue',
  'purple',
  'magenta',
] as const;

/** One channel of Capture One's Levels: levels 0–255, gamma 0.1–10. */
export interface Level {
  inBlack: number;
  inWhite: number;
  gamma: number;
  outBlack: number;
  outWhite: number;
}

export interface Levels {
  [index: string]: Level;
  rgb: Level;
  red: Level;
  green: Level;
  blue: Level;
}

const IDENTITY_LEVEL: Level = { inBlack: 0, inWhite: 255, gamma: 1, outBlack: 0, outWhite: 255 };

export const INITIAL_LEVELS: Levels = {
  rgb: { ...IDENTITY_LEVEL },
  red: { ...IDENTITY_LEVEL },
  green: { ...IDENTITY_LEVEL },
  blue: { ...IDENTITY_LEVEL },
};

/**
 * One range of the colour editor (Capture One's Advanced Color Editor,
 * Lightroom's Point Color): a sampled display colour in Oklab with the
 * range around it and what happens to the colours inside.
 */
export interface ColorRange {
  id: string;
  /** "skin": the colour editor's skin tone range (at most one per recipe or
   * layer; shown in the Skin tone mode). The renderer treats it like any
   * other range. */
  kind?: "skin";
  /** Sampled colour: Oklab hue (degrees), chroma and lightness. */
  hue: number;
  chroma: number;
  lightness: number;
  /** Half-width of the hue range in degrees (5–90). */
  hueRange: number;
  /** Chroma range as a fraction below/above the sample (0–100). */
  chromaRange: number;
  /** Lightness range around the sample (0–100, 100 = all). */
  lightnessRange: number;
  /** Softness of the range edges (0–100). */
  smoothness: number;
  /** Hue shift (−100…100 = ±50°), saturation and lightness (−100…100;
   * lightness scales: ±100 = ×1.4 / ×0.6, so dark tones of the range are
   * lifted less than light ones, as in Lightroom). */
  hueShift: number;
  saturation: number;
  lightnessShift: number;
  /** Capture One's uniformity: pull hue, chroma and lightness of the range
   * towards the sample (0–100), e.g. to even out skin. */
  hueUniformity: number;
  saturationUniformity: number;
  lightnessUniformity: number;
}

export const MAX_COLOR_RANGES = 8;
/** Ranges a mask layer can hold (the renderer keeps four per layer). */
export const MAX_LAYER_COLOR_RANGES = 4;

export function newColorRange(
  hue: number,
  chroma: number,
  lightness: number,
  kind?: "skin",
): ColorRange {
  return {
    id: uuidv4(),
    ...(kind ? { kind } : {}),
    hue,
    chroma,
    lightness,
    hueRange: 25,
    chromaRange: 70,
    lightnessRange: 100,
    smoothness: 50,
    hueShift: 0,
    saturation: 0,
    lightnessShift: 0,
    hueUniformity: 0,
    saturationUniformity: 0,
    lightnessUniformity: 0,
  };
}

/** Lightroom's region tone curve (crs:Parametric*): −100…100, splits 0–100. */
export interface RegionCurve {
  shadows: number;
  darks: number;
  lights: number;
  highlights: number;
  shadowSplit: number;
  midtoneSplit: number;
  highlightSplit: number;
}

export const INITIAL_REGION_CURVE: RegionCurve = {
  shadows: 0,
  darks: 0,
  lights: 0,
  highlights: 0,
  shadowSplit: 25,
  midtoneSplit: 50,
  highlightSplit: 75,
};

export type WhiteBalanceMode =
  | 'asShot'
  | 'auto'
  | 'daylight'
  | 'cloudy'
  | 'shade'
  | 'tungsten'
  | 'fluorescent'
  | 'flash'
  | 'custom';

/** Face retouch sliders (Capture One's Retouch Faces), 0–100; Texture
 * −100–100. A face's own values override those for all faces. */
export interface FaceRetouchSettings {
  impact?: number;
  blemishes?: number;
  evenSkin?: number;
  texture?: number;
  darkCircles?: number;
  shine?: number;
  contouring?: number;
  /** Blemishes, even skin and texture on the neck too. */
  includeNeck?: boolean;
}

/** A blemish found on a face (mask-frame pixels; `score`: prominence). */
export interface RetouchBlemish {
  id: number;
  x: number;
  y: number;
  r: number;
  score: number;
  /** On the neck: healed only with `includeNeck`. */
  neck?: boolean;
}

/** One face of the face retouch, as `analyze_face_retouch` makes it
 * (crates/engine/src/face_retouch.rs reads it). */
export interface RetouchFace {
  id: string;
  crop: number[];
  size: number;
  eyeDistance: number;
  skin: string;
  underEyes: string | null;
  /** Neck skin below the chin (PNG data URL), if visible. */
  neck?: string | null;
  blemishes: RetouchBlemish[];
  /** Blemishes (ids) the user keeps. */
  kept: number[];
  /** Blemishes (ids) the user heals below the slider's reach. */
  added?: number[];
  settings: FaceRetouchSettings;
  enabled: boolean;
  /** Face box (mask frame) and a small JPEG for the panel. */
  box: number[];
  thumbnail?: string;
}

/** A stroke of the face retouch's protection brush: the area it covers
 * keeps its pixels from before the retouch (mask-frame pixels, full
 * resolution, in the geometry of the faces). */
export interface FaceProtectStroke {
  /** The brush centre's path; one point is a dab. */
  points: { x: number; y: number }[];
  radius: number;
  /** Soft edge, 0–100 % of the radius. */
  feather: number;
  /** Takes protection away. */
  erase?: boolean;
}

export interface FaceRetouch {
  settings: FaceRetouchSettings;
  faces: RetouchFace[];
  /** The backend's key of the geometry the faces were found in. */
  frame: string;
  /** The same for the panel: the transform adjustments it analysed with. */
  geometry: string;
  enabled?: boolean;
  /** The protection brush's strokes, in the order painted (for every face). */
  protect?: FaceProtectStroke[];
}

/** Grain structures (shader `grain_structure_noise`, notes: reference/grain.md). */
export type GrainStructure = 'even' | 'soft' | 'clumped' | 'speckled' | 'gritty';

/** The structure grain gets when it is turned on in a recipe without one (user decision 2026-10-12). */
export const NEW_GRAIN_STRUCTURE: GrainStructure = 'clumped';

/** The structure a recipe's grain renders with: recipes without the key
 * render Even (the original grain); grain not yet turned on will start as
 * Clumped. */
export const effectiveGrainStructure = (adjustments: Partial<Adjustments>): GrainStructure =>
  adjustments.grainStructure ?? ((adjustments.grainAmount ?? 0) > 0 ? 'even' : NEW_GRAIN_STRUCTURE);

/** The grain new edits get (`image_processing::GRAIN_VERSION`): 3 is film
 * grain (grains per dye layer in the negative's density, printed; Colour
 * instead of a structure), 2 the portable grain, the same on every GPU; 1
 * (missing key) the earlier one. */
export const GRAIN_VERSION = 3;

/** Film grain (version 3) has no structures; its Colour sets how independent
 * the dye layers are. */
export const isFilmGrain = (adjustments: Partial<Adjustments>): boolean =>
  (adjustments.grainVersion ?? ((adjustments.grainAmount ?? 0) > 0 ? 1 : GRAIN_VERSION)) >= 3;

/** Writes Even into grain that has none (recipes and presets from before the
 * structure choice), so it keeps rendering Even when copied, pasted, saved as
 * a preset or turned off and on again. */
export const withGrainStructure = <T extends Partial<Adjustments>>(adjustments: T): T =>
  adjustments.grainStructure === undefined && (adjustments.grainAmount ?? 0) > 0
    ? { ...adjustments, grainStructure: 'even' }
    : adjustments;

/** Writes grain version 1 into grain that has no version (recipes and
 * presets from before the key), so it keeps its pattern. Not for settings
 * chosen in the Copy/Sync picker: there the version is a processing version
 * and the destination keeps its own unless it is chosen. */
export const withLegacyGrainVersion = <T extends Partial<Adjustments>>(adjustments: T): T =>
  adjustments.grainVersion == null && (adjustments.grainAmount ?? 0) > 0
    ? { ...adjustments, grainVersion: 1 }
    : adjustments;

/** AI Denoise's amount means what its version makes of it, so wherever an
 * amount is copied (paste, tool copy, presets) its version goes along: the
 * source's, else — presets and recipes from before the key — version 1 for an
 * amount above 0 and the current one for 0, which renders the same. */
export const withAiDenoiseVersion = <T extends Partial<Adjustments>>(
  picked: T,
  source?: Partial<Adjustments>,
): T & Pick<Partial<Adjustments>, 'aiDenoiseVersion'> => {
  if (picked.aiDenoise === undefined || (picked.aiDenoiseVersion !== undefined && picked.aiDenoiseVersion !== null)) {
    return picked;
  }
  const version = source?.aiDenoiseVersion ?? ((picked.aiDenoise ?? 0) > 0 ? 1 : AI_DENOISE_VERSION);
  return { ...picked, aiDenoiseVersion: version };
};

/** diffraction.rs VERSION for a request made now. */
export const DIFFRACTION_VERSION = 2;

/** The diffraction correction's crop-factor lookup depends on its version,
 * so wherever the switch is copied (paste, presets) its version goes along:
 * the source's, else — presets and recipes from before the key — version 1
 * for a correction that is on. */
export const withDiffractionVersion = <T extends Partial<Adjustments>>(
  picked: T,
  source?: Partial<Adjustments>,
): T & Pick<Partial<Adjustments>, 'diffractionVersion'> => {
  if (picked.diffractionCorrection !== true || (picked.diffractionVersion !== undefined && picked.diffractionVersion !== null)) {
    return picked;
  }
  return { ...picked, diffractionVersion: source?.diffractionVersion ?? 1 };
};

/** `next` after an edit of `previous`: grain turned on in a recipe without a
 * structure starts as Clumped, without a grain version as the newest. */
export const withNewGrainStructure = <T extends Partial<Adjustments>>(previous: Partial<Adjustments>, next: T): T => {
  if ((previous.grainAmount ?? 0) > 0 || (next.grainAmount ?? 0) <= 0) return next;
  let grain = next;
  if (grain.grainStructure === undefined) grain = { ...grain, grainStructure: NEW_GRAIN_STRUCTURE };
  if (grain.grainVersion == null) grain = { ...grain, grainVersion: GRAIN_VERSION };
  return grain;
};

export interface Adjustments {
  [index: string]: unknown;
  aiPatches: Array<AiPatch>;
  /** Spot removal (heal/clone circles), applied in full precision to the
   * base image before every adjustment (crates/engine/src/spot_removal.rs). */
  spots: Array<Spot>;
  /** Red eye and pet eye corrections, applied to the base image before the
   * spots (crates/engine/src/red_eye.rs). */
  redEyes?: Array<RedEye>;
  /** Face retouch (Capture One's Retouch Faces), applied after the spots. */
  faceRetouch?: FaceRetouch | null;
  aspectRatio: number | null;
  /** Black & white conversion (Lightroom's Treatment, Capture One's B&W). */
  blackAndWhite?: boolean;
  grayMixer?: GrayMixer;
  /** Capture One's Levels (display values, before the curve). */
  levels?: Levels;
  /** Lightroom's region curve, before the point curve. */
  regionCurve?: RegionCurve;
  /** Colour editor ranges, applied in order. */
  colorRanges?: ColorRange[];
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
  /** AI Denoise of the RAW mosaic, 0 … 100 (applied when the photo is developed). */
  aiDenoise?: number;
  /** How AI Denoise reads its amount (crates/engine/src/image_loader.rs
   * `ai_denoise_settings`): 2 = 50 has about Camera Raw Denoise's default
   * effect; 3 = the same on the faster model; missing = 1 (recipes from
   * before it). */
  aiDenoiseVersion?: number;
  blacks: number;
  brightness: number;
  centré: number;
  clarity: number;
  chromaticAberrationBlueYellow: number;
  chromaticAberrationRedCyan: number;
  colorCalibration: ColorCalibration;
  colorGrading: ColorGradingProps;
  colorNoiseReduction: number;
  contrast: number;
  curves: Curves;
  pointCurves?: Curves;
  parametricCurve?: ParametricCurve;
  curveMode?: 'point' | 'parametric';
  crop: Crop | null;
  dehaze: number;
  exposure: number;
  flipHorizontal: boolean;
  flipVertical: boolean;
  flareAmount: number;
  glowAmount: number;
  grainAmount: number;
  grainRoughness: number;
  grainSize: number;
  guidedPerspective: GuidedPerspective;
  halationAmount: number;
  highlights: number;
  hsl: Hsl;
  hue: number;
  lensBlurAmount: number;
  lensBlurDiffusion: number;
  lensBlurShape: 'circle' | 'hexagon' | 'octagon' | 'ring';
  lensBlurDepthMap: string | null;
  lensBlurEnabled: boolean;
  lensBlurMaxDepth: number;
  lensBlurMaxFade: number;
  lensBlurMinDepth: number;
  lensBlurMinFade: number;
  /** 'camera': the RAW's own correction data (Fujifilm, Sony, Panasonic, …). */
  lensCorrectionMode: 'auto' | 'manual' | 'camera';
  /** Which cameras' CA data camera mode reads (crates/engine/src/embedded_lens.rs
   * `CA_VERSION`): 1 = also Panasonic's; missing = Fujifilm and Sony only
   * (recipes from before it, so saving them again keeps their parameters). */
  cameraCaVersion?: number;
  /** How camera mode reads the camera data's geometry (embedded_lens.rs
   * `GEOMETRY_VERSION`): 1 = Sony's distortion as Camera Raw reads it;
   * 2 = also Nikon Z framed as the camera frames it; 3 = also Canon's CTMD
   * distortion; 4 = also Samsung's distortion; 5 = also
   * Panasonic's and Fujifilm's framing as the cameras frame it; 6 = also
   * Sony's knot table warped as it is (lens model 2); 7 = also
   * Fujifilm's (lens model 3); missing = as before (saving such recipes again keeps their parameters). */
  cameraGeometryVersion?: number;
  lensDistortionAmount: number;
  lensVignetteAmount: number;
  lensTcaAmount: number;
  lensDistortionEnabled: boolean;
  lensTcaEnabled: boolean;
  lensVignetteEnabled: boolean;
  /** Capture One's diffraction correction: sharpening by the f-number and
   * pixel pitch (diffraction.rs), RAW only. */
  diffractionCorrection?: boolean;
  /** diffraction.rs VERSION, written when the correction is switched on;
   * missing = 1 (crop factors without the bodies the 2026-10-18 database
   * update added). */
  diffractionVersion?: number;
  lensDistortionParams: {
    k1: number;
    k2: number;
    k3: number;
    model: number;
    tca_vr: number;
    tca_vb: number;
    vig_k1: number;
    vig_k2: number;
    vig_k3: number;
    /** Lens model 2: the distortion factor at uniform knots from the
     * centre to `table_end` (lens_correction.rs `RadialTable`). */
    table?: number[];
    table_end?: number;
    /** Lens model 3: the radii of `table`'s knots, linear in r² between
     * them (lens_correction.rs `RadialTable::knots`). */
    table_radii?: number[];
  } | null;
  lensMaker: string | null;
  /** The lens choice `lensDistortionParams` were resolved for; written by
   * the backend's save (edits.rs, LensLookup) and never copied: a save keeps
   * the parameters while it matches the recipe's lens choice. */
  lensProfileFor?: string | null;
  lensModel: string | null;
  lumaNoiseReduction: number;
  /** Lightroom's noise reduction refinements (0–100): luminance Detail
   * (default 50) and Contrast (0), colour Detail (50) and Smoothness (50). */
  lumaNoiseDetail?: number;
  lumaNoiseContrast?: number;
  colorNoiseDetail?: number;
  colorNoiseSmoothness?: number;
  /** Colour noise reduction filter: 11 = version 10's filter built and
   * measured on the GPU (crates/engine/src/color_noise.rs
   * `Tuning::gpu_pyramid`; faster exports, previews and exports alike);
   * 10 = multi-scale, measured per photo
   * (crates/engine/src/color_noise.rs), each pixel's filtered colour kept
   * within its neighbourhood's plain filtered colours plus about the noise
   * (fewer colour specks at Color 25); 9 = the same filter with the pixel's
   * own edge-aware colour in that range and a wider margin (fewer colour
   * specks along edges); 8 = the same filter without the range, an
   * isolated off-colour pixel's
   * finest detail taken from its 3 × 3 median chroma unless the luminance
   * noise reduction keeps it brighter or darker than its neighbours (fewer
   * isolated colour specks); 7 = the same filter judging that brightness
   * before the luminance noise reduction; 6 = the same filter without the
   * median chroma, the finest detail measured against
   * the same edge-aware expansion as the reconstruction (no halos or
   * blotchy rims along colour edges); 5 = the same filter without that, a
   * pixel whose colour is an outlier among its 3 × 3 neighbours keeps to
   * their side of a colour edge (fewer isolated colour specks); 4 = the
   * same filter without that, its scales
   * built from linear mean colours (dark noisy colours keep their hue); 3 = the same filter on
   * square-root means, keeping more 8–30 px colour texture where those
   * scales are quiet; 2 = the same filter before that; 1 or missing on
   * recipes that set `colorNoiseReduction` = the earlier cross-bilateral
   * filter. */
  colorNoiseVersion?: number;
  /** Tone-slider response (crates/engine/src/image_processing.rs
   * `TONE_VERSION`): 2 = Shadows above +50 reaches further into the
   * quarter-tones and lowered Highlights is refitted; 3 = lowered
   * Highlights also adapts to the image key (reaches further in dark
   * frames); 1 or missing on
   * recipes that used either before it = the earlier response. Photo-wide
   * (layers follow it). */
  toneVersion?: number;
  /** Clarity response (crates/engine/src/image_processing.rs
   * `CLARITY_VERSION`): 2 = the medium-scale band against an edge-aware base
   * (fewer dark halos beside narrow bright edges); 1 or missing on recipes
   * that used Clarity before it = the band against a plain blur. Photo-wide
   * (layers follow it). */
  clarityVersion?: number;
  /** Edge-fringe pass (crates/engine/src/edge_fringe.rs `VERSION`): 1 =
   * colour along strong luminance edges that neither side explains
   * (demosaic false colour, lateral CA rests) is removed after the lens
   * correction; missing = none (recipes from before it). New RAW edits only. */
  fringeVersion?: number;
  /** Defringe's fringe detector (crates/engine/src/image_processing.rs
   * `DEFRINGE_VERSION`): 2 = only colour the edge across it does not explain
   * (beyond the mix of its two sides) moves to that mix, for the photo's and
   * the layers' Defringe; 1 or missing on recipes that used Defringe before
   * it = every colour in range near an edge loses chroma. Photo-wide. */
  defringeVersion?: number;
  /** Sharpening response (crates/engine/src/image_processing.rs
   * `SHARPEN_VERSION`): 6 = 5, and for JPEGs and other non-RAW photos
   * stronger above Amount 40 (closer to Camera Raw's; RAW photos render as 5); 5 = 4 with Masking's ramp after Camera Raw's (wider:
   * more moderate texture, no more flat noise; renders as 4 without Masking); 4 = 3 with Radius and Masking after Camera Raw's
   * (renders as 3 from Radius 1 up without Masking); 3 = 2 weighted by lightness (little sharpening in
   * the shadows, more in the highlights, as Camera Raw's); 2 = halos nearly
   * constant over the amount, gain and Detail tilt after Camera Raw at high
   * amounts; 1 or missing on recipes that sharpened before 3 = the earliest
   * response (1 and 2 are the same at Amount 40 with Radius, Detail and
   * Masking at their defaults). Photo-wide (layers follow it). */
  sharpenVersion?: number;
  /** Luminance noise reduction (crates/engine/src/image_processing.rs
   * `LUMA_NOISE_VERSION`): 2 = noisy photos also lose isolated luminance
   * specks (single pixels darker or brighter than all their neighbours,
   * which the sharpening no longer lifts); 1 or missing on recipes that
   * reduced luminance noise before it = the earlier filter (both are the
   * same without luminance noise reduction). Photo-wide (layers follow it). */
  lumaNoiseVersion?: number;
  /** Render pipeline fixes (crates/engine/src/image_processing.rs
   * `RENDER_VERSION`): 2 = warps and straightening keep negative (wide-gamut)
   * channels, Clarity/Dehaze/Glow's large blur has its whole radius at tile
   * edges, and the Classic/AgX looks' Color Grading and Calibration follow
   * their switches; 3 = a RAW photosite counts as dead only when it is also
   * darker than its adjacent samples of the other colours, so thin dark
   * lines keep their colour; 4 = colours outside the output space keep
   * their own hue when clipped to it; 5 = exports render with the shader
   * variant specialized for the recipe's features; 1 or missing = before.
   * Per photo, never copied. */
  renderVersion?: number;
  /** The frame of the RAW the recipe develops (crates/rawdecode/src/source_frame.rs,
   * ADR 0020): 1 = a CR3's active area holds the crop the camera records
   * (EOS R5 Mark II APS-C: 5088 × 3392 instead of 4970 × 3392); missing =
   * 0, the frame earlier edits were made on. Per photo, never copied, and
   * never changed once the photo is edited (Reset, paste and presets keep
   * it): a photo keeps the size it was edited in (src/utils/sourceFrame.ts). */
  sourceFrameVersion?: number;
  lutData?: string | null;
  lutIntensity?: number;
  lutName?: string | null;
  lutPath?: string | null;
  lutSize?: number;
  lutIsSceneReferred?: boolean;
  masks: Array<MaskContainer>;
  orientationSteps: number;
  rotation: number;
  saturation: number;
  sectionVisibility: SectionVisibility;
  shadows: number;
  sharpness: number;
  sharpnessThreshold: number;
  // Lightroom's sharpening Radius / Detail / Masking (photo only).
  sharpenRadius: number;
  sharpenDetail: number;
  sharpenMasking: number;
  /** Capture One's Sharpness Falloff (0–100): more sharpening towards the corners. */
  sharpnessFalloff?: number;
  /** View state, never saved (`NON_COPYABLE_ADJUSTMENT_KEYS`): the
   * renderer's clipping warnings, `showClippingValue`
   * (utils/clippingWarnings). */
  showClipping: boolean | number;
  structure: number;
  temperature: number;
  tint: number;
  // 'standard' is Capture One's base look (new edits); 'basic' the inherited
  // tone mapper that existing edits keep.
  toneMapper: 'agx' | 'basic' | 'standard';
  // Capture One's curve choice (Base Characteristics) on top of the base
  // look, RAW only; the backend reads it as `baseCurve`.
  baseCurve:
    | 'standard'
    | 'highContrast'
    | 'extraShadow'
    | 'linear';
  /** A DCP camera profile the photographer chose (camera_profile.rs):
   * `profiles/Name.dcp` inside the session, an absolute path outside one;
   * unset = Rawtonic's own colour. */
  cameraProfile?: string;
  /** Absolute white balance of RAWs (Lightroom's Temp/Tint, camera space):
   * unset or `asShot` keeps the camera's; a preset or `custom` with
   * `wbTemperature` (K) and `wbTint`. The relative `temperature`/`tint`
   * apply on top (JPEGs, layers, older edits). */
  whiteBalance?: WhiteBalanceMode;
  wbTemperature?: number;
  wbTint?: number;
  transformDistortion: number;
  /** Lateral chromatic aberration measured on the photo and corrected
   * (crates/engine/src/lateral_ca.rs): true = on (replacing the lens data's
   * own CA correction), false = off; missing = off before `autoCaVersion` 2,
   * from it on wherever the lens correction has no CA data of its own, from
   * 3 also where it has (replacing them only where it measures clearly
   * better). */
  autoChromaticAberration?: boolean;
  /** The automatic lateral CA estimate (lateral_ca.rs `VERSION`): 2 = refined
   * at full resolution, applied only when trustworthy, on by default where
   * the lens data has no CA correction; 3 = the same, and where the lens
   * data has one it competes with it; missing = 1 (recipes from before).
   * New RAW edits only. */
  autoCaVersion?: number;
  transformVertical: number;
  transformHorizontal: number;
  transformRotate: number;
  transformAspect: number;
  transformScale: number;
  transformXOffset: number;
  transformYOffset: number;
  vibrance: number;
  vignetteAmount: number;
  vignetteFeather: number;
  vignetteMidpoint: number;
  vignetteRoundness: number;
  /** Lightroom's post-crop vignette style and Highlights (0–100). */
  vignetteStyle?: 'highlightPriority' | 'colorPriority' | 'paintOverlay';
  /** Grain structure; missing = 'even', the original grain (docs/RECIPE_SCHEMA.md). */
  grainStructure?: GrainStructure;
  /** The grain a recipe renders (`GRAIN_VERSION`); missing = 1. */
  grainVersion?: number;
  /** Film grain's colour (grain version 3): 0 one neutral layer like
   * black-and-white film, 100 independent dye layers; missing = 0. */
  grainColor?: number;
  relightLights?: RelightLight[];
  relightDepth?: RelightDepth;
  relightDepthAmount?: number;
  relightSkinReflections?: number;
  relightOriginalLight?: number;
  vignetteHighlights?: number;
  whites: number;
}

/** What `setAdjustments` accepts: a patch merged into the current adjustments, or an updater. */
export type AdjustmentsUpdate = Partial<Adjustments> | ((prev: Adjustments) => Adjustments);

/** Pixels generated for an AI patch (`inpainting.rs`): base64 PNG colour and mask plus placement. */
export interface AiPatchData {
  color: string;
  mask: string;
  offsetX: number;
  offsetY: number;
  width: number;
  height: number;
  isSrgbEncoded: boolean;
  /** The model that guided the fill (`provenance::fill_model`); missing on
   * fills made before it was recorded. */
  model?: 'moebius' | 'mi-gan' | 'external';
}

/** One spot of the spot removal tool, in mask coordinates (the photo after
 * orientation and straightening, before the crop). */
export interface Spot {
  id: string;
  mode: 'heal' | 'clone';
  x: number;
  y: number;
  radius: number;
  sourceX: number;
  sourceY: number;
  /** Soft edge, 0–100 % of the radius. */
  feather: number;
  /** 0–100. */
  opacity: number;
  visible: boolean;
  /** A brush stroke: the path the brush of `radius` followed. `x`/`y` is its
   * first point and `sourceX`/`sourceY` where that point copies from.
   * Absent (or one point): a circle. */
  points?: Array<{ x: number; y: number }>;
  /** Made by Find Dust: where the dust sits on the sensor (normalised in
   * the unrotated raw), which "Sync dust" and later runs use to recognise
   * the same dust (crates/engine/src/dust.rs). */
  dust?: { x: number; y: number };
}

/** A red eye or pet eye correction (Lightroom's Red Eye tool, Capture One's
 * Remove Red Eye): the pupil's ellipse in mask-frame pixels, found by a
 * click (crates/engine/src/red_eye.rs `detect`) and applied to the base
 * image before the spots. */
export interface RedEye {
  id: string;
  /** The render version (`red_eye::VERSION`). */
  version: number;
  kind: 'red' | 'pet';
  x: number;
  y: number;
  radiusX: number;
  radiusY: number;
  /** Degrees from the x axis towards the y axis. */
  angle: number;
  /** 0–100, 50 = the found pupil. */
  pupilSize: number;
  /** 0–100. */
  darken: number;
  /** Pet eye: a catchlight at `catchlightX`/`catchlightY` from the centre,
   * in units of the pupil's mean radius. */
  catchlight: boolean;
  catchlightX: number;
  catchlightY: number;
  visible: boolean;
}

export interface AiPatch {
  id: string;
  isLoading: boolean;
  invert: boolean;
  /** false: no model made the pixels (Fill edges); exports are not marked. */
  generated?: boolean;
  name: string;
  patchData: AiPatchData | null;
  prompt: string;
  subMasks: Array<SubMask>;
  visible: boolean;
}

export interface Color {
  color: string;
  name: string;
}

interface ColorGradingProps {
  [index: string]: number | HueSatLum;
  balance: number;
  blending: number;
  global: HueSatLum;
  highlights: HueSatLum;
  midtones: HueSatLum;
  shadows: HueSatLum;
}

export interface Coord {
  x: number;
  y: number;
}

export interface Curves {
  [index: string]: Array<Coord> | number | undefined;
  blue: Array<Coord>;
  green: Array<Coord>;
  /** The RGB curve: every channel through it (labelled RGB). */
  luma: Array<Coord>;
  red: Array<Coord>;
  /** The Luminance curve, after the others: the colour's luminance
   * through it, its saturation and hue kept (shader
   * `apply_luminance_curve`). Missing on curves that never had one. */
  luminance?: Array<Coord>;
  /** crates/engine/src/point_curve.rs: 2 (`LONG_CURVE_VERSION`) = the
   * photo's curves with more than 16 points render whole; 3
   * (`WIDE_CURVE_VERSION`) = the curves act in the wide working space, as
   * Camera Raw's do. Missing on curves saved before 2, which render their
   * first 16 points on display values. Part of the curves, so it travels
   * wherever they do; a curve edit writes `editedCurvesVersion`. */
  version?: number;
}

/** Point curves render whole beyond 16 points (`Curves.version`). */
export const LONG_CURVE_VERSION = 2;
/** The current curves version: whole curves in the wide working space. */
export const CURVE_VERSION = 3;

export interface HueSatLum {
  hue: number;
  saturation: number;
  luminance: number;
}

interface Hsl {
  [index: string]: HueSatLum;
  aquas: HueSatLum;
  blues: HueSatLum;
  greens: HueSatLum;
  magentas: HueSatLum;
  oranges: HueSatLum;
  purples: HueSatLum;
  reds: HueSatLum;
  yellows: HueSatLum;
}

export interface MaskAdjustments {
  [index: string]: unknown;
  colorRanges?: ColorRange[];
  blacks: number;
  brightness: number;
  clarity: number;
  colorGrading: ColorGradingProps;
  colorNoiseReduction: number;
  contrast: number;
  curves: Curves;
  pointCurves?: Curves;
  parametricCurve?: ParametricCurve;
  curveMode?: 'point' | 'parametric';
  dehaze: number;
  exposure: number;
  flareAmount: number;
  glowAmount: number;
  halationAmount: number;
  highlights: number;
  hsl: Hsl;
  hue: number;
  id?: string;
  lumaNoiseReduction: number;
  saturation: number;
  sectionVisibility: SectionVisibility;
  shadows: number;
  sharpness: number;
  sharpnessThreshold: number;
  structure: number;
  temperature: number;
  tint: number;
  vibrance: number;
  whites: number;
}

export interface MaskContainer {
  adjustments: MaskAdjustments;
  id: string;
  invert: boolean;
  /** 2: the mask is computed at 16 bits; 3: also Refine edge on both sides
   * of a thin mask; 4 (new layers): also AI masks grown and feathered from
   * the photo around the crop; absent: 8 bits. */
  maskVersion?: number;
  name: string;
  opacity: number;
  /** Refine edge radius in pixels of the full photo (`maskVersion` 2+). */
  refineEdge?: number;
  subMasks: Array<SubMask>;
  visible: boolean;
}

/** The mask version new layers get (crates/engine/src/masks.rs). */
export const MASK_VERSION = 4;

export interface Sections {
  [index: string]: Array<string>;
  basic: Array<string>;
  curves: Array<string>;
  color: Array<string>;
  details: Array<string>;
  effects: Array<string>;
}

export interface SectionVisibility {
  [index: string]: boolean;
  basic: boolean;
  curves: boolean;
  color: boolean;
  details: boolean;
  effects: boolean;
}

export const COLOR_LABELS: Array<Color> = [
  { name: 'red', color: '#ef4444' },
  { name: 'yellow', color: '#facc15' },
  { name: 'green', color: '#4ade80' },
  { name: 'blue', color: '#60a5fa' },
  { name: 'purple', color: '#a78bfa' },
];

const INITIAL_COLOR_GRADING: ColorGradingProps = {
  balance: 0,
  blending: 50,
  global: { hue: 0, saturation: 0, luminance: 0 },
  highlights: { hue: 0, saturation: 0, luminance: 0 },
  midtones: { hue: 0, saturation: 0, luminance: 0 },
  shadows: { hue: 0, saturation: 0, luminance: 0 },
};

const INITIAL_COLOR_CALIBRATION: ColorCalibration = {
  shadowsTint: 0,
  redHue: 0,
  redSaturation: 0,
  greenHue: 0,
  greenSaturation: 0,
  blueHue: 0,
  blueSaturation: 0,
};

export const DEFAULT_PARAMETRIC_CURVE_SETTINGS: ParametricCurveSettings = {
  darks: 0,
  shadows: 0,
  highlights: 0,
  lights: 0,
  whiteLevel: 0,
  blackLevel: 0,
  split1: 25,
  split2: 50,
  split3: 75,
};

const getDefaultParametricCurve = (): ParametricCurve => ({
  luma: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS },
  red: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS },
  green: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS },
  blue: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS },
});

export const getDefaultCurves = (): Curves => ({
  blue: [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  green: [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  luma: [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  red: [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  version: CURVE_VERSION,
});

export const DEFAULT_PARAMETRIC_CURVE = getDefaultParametricCurve();

export const INITIAL_MASK_ADJUSTMENTS: MaskAdjustments = {
  blacks: 0,
  brightness: 0,
  clarity: 0,
  colorGrading: { ...INITIAL_COLOR_GRADING },
  colorNoiseReduction: 0,
  contrast: 0,
  curves: getDefaultCurves(),
  pointCurves: getDefaultCurves(),
  parametricCurve: getDefaultParametricCurve(),
  curveMode: 'point',
  dehaze: 0,
  exposure: 0,
  flareAmount: 0,
  glowAmount: 0,
  halationAmount: 0,
  highlights: 0,
  hsl: {
    aquas: { hue: 0, saturation: 0, luminance: 0 },
    blues: { hue: 0, saturation: 0, luminance: 0 },
    greens: { hue: 0, saturation: 0, luminance: 0 },
    magentas: { hue: 0, saturation: 0, luminance: 0 },
    oranges: { hue: 0, saturation: 0, luminance: 0 },
    purples: { hue: 0, saturation: 0, luminance: 0 },
    reds: { hue: 0, saturation: 0, luminance: 0 },
    yellows: { hue: 0, saturation: 0, luminance: 0 },
  },
  hue: 0,
  lumaNoiseReduction: 0,
  moireAmount: 0,
  moirePattern: 30,
  saturation: 0,
  sectionVisibility: {
    basic: true,
    curves: true,
    color: true,
    details: true,
    effects: true,
  },
  shadows: 0,
  sharpness: 0,
  sharpnessThreshold: 15,
  structure: 0,
  temperature: 0,
  tint: 0,
  vibrance: 0,
  whites: 0,
};

export const INITIAL_MASK_CONTAINER: Omit<MaskContainer, 'id'> = {
  adjustments: INITIAL_MASK_ADJUSTMENTS,
  invert: false,
  name: 'New Mask',
  opacity: 100,
  subMasks: [],
  visible: true,
};

export const INITIAL_GRAY_MIXER: GrayMixer = {
  red: 0,
  orange: 0,
  yellow: 0,
  green: 0,
  aqua: 0,
  blue: 0,
  purple: 0,
  magenta: 0,
};

export const INITIAL_ADJUSTMENTS: Adjustments = {
  aiPatches: [],
  spots: [],
  redEyes: [],
  faceRetouch: null,
  aspectRatio: null,
  blackAndWhite: false,
  grayMixer: { ...INITIAL_GRAY_MIXER },
  levels: structuredClone(INITIAL_LEVELS),
  regionCurve: { ...INITIAL_REGION_CURVE },
  colorRanges: [],
  defringePurpleAmount: 0,
  defringePurpleHueLo: 30,
  defringePurpleHueHi: 70,
  defringeGreenAmount: 0,
  defringeGreenHueLo: 40,
  defringeGreenHueHi: 60,
  moireAmount: 0,
  moirePattern: 30,
  blacks: 0,
  brightness: 0,
  centré: 0,
  clarity: 0,
  chromaticAberrationBlueYellow: 0,
  chromaticAberrationRedCyan: 0,
  colorCalibration: { ...INITIAL_COLOR_CALIBRATION },
  colorGrading: { ...INITIAL_COLOR_GRADING },
  colorNoiseReduction: 0,
  aiDenoise: 0,
  contrast: 0,
  crop: null,
  curves: getDefaultCurves(),
  pointCurves: getDefaultCurves(),
  parametricCurve: getDefaultParametricCurve(),
  curveMode: 'point',
  dehaze: 0,
  exposure: 0,
  flipHorizontal: false,
  flipVertical: false,
  flareAmount: 0,
  glowAmount: 0,
  grainAmount: 0,
  grainRoughness: 50,
  grainSize: 25,
  guidedPerspective: { enabled: false, lines: [], autoCrop: true },
  halationAmount: 0,
  highlights: 0,
  hsl: {
    aquas: { hue: 0, saturation: 0, luminance: 0 },
    blues: { hue: 0, saturation: 0, luminance: 0 },
    greens: { hue: 0, saturation: 0, luminance: 0 },
    magentas: { hue: 0, saturation: 0, luminance: 0 },
    oranges: { hue: 0, saturation: 0, luminance: 0 },
    purples: { hue: 0, saturation: 0, luminance: 0 },
    reds: { hue: 0, saturation: 0, luminance: 0 },
    yellows: { hue: 0, saturation: 0, luminance: 0 },
  },
  hue: 0,
  lensBlurAmount: 40,
  lensBlurDiffusion: 0,
  lensBlurShape: 'circle',
  lensBlurDepthMap: null,
  lensBlurEnabled: false,
  lensBlurMaxDepth: 100,
  lensBlurMaxFade: 20,
  lensBlurMinDepth: 20,
  lensBlurMinFade: 20,
  lensCorrectionMode: 'manual',
  lensDistortionAmount: 100,
  lensVignetteAmount: 100,
  lensTcaAmount: 100,
  lensDistortionEnabled: true,
  lensTcaEnabled: true,
  lensVignetteEnabled: true,
  lensDistortionParams: null,
  lensMaker: null,
  lensModel: null,
  lumaNoiseReduction: 0,
  lutData: null,
  lutIntensity: 100,
  lutName: null,
  lutPath: null,
  lutSize: 0,
  lutIsSceneReferred: false,
  masks: [],
  orientationSteps: 0,
  rotation: 0,
  saturation: 0,
  sectionVisibility: {
    basic: true,
    curves: true,
    color: true,
    details: true,
    effects: true,
  },
  shadows: 0,
  sharpness: 0,
  sharpnessThreshold: 15,
  sharpenRadius: 1,
  sharpenDetail: 25,
  sharpenMasking: 0,
  sharpnessFalloff: 0,
  showClipping: false,
  structure: 0,
  temperature: 0,
  tint: 0,
  toneMapper: 'standard',
  baseCurve: 'standard',
  transformDistortion: 0,
  transformVertical: 0,
  transformHorizontal: 0,
  transformRotate: 0,
  transformAspect: 0,
  transformScale: 100,
  transformXOffset: 0,
  transformYOffset: 0,
  vibrance: 0,
  vignetteAmount: 0,
  vignetteFeather: 50,
  vignetteMidpoint: 50,
  vignetteRoundness: 0,
  vignetteStyle: 'highlightPriority',
  vignetteHighlights: 0,
  whites: 0,
};

const deepCloneCurves = (curves: Partial<Curves> | undefined): Curves => ({
  blue: curves?.blue?.map((p: Coord) => ({ ...p })) || [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  green: curves?.green?.map((p: Coord) => ({ ...p })) || [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  luma: curves?.luma?.map((p: Coord) => ({ ...p })) || [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  red: curves?.red?.map((p: Coord) => ({ ...p })) || [
    { x: 0, y: 0 },
    { x: 255, y: 255 },
  ],
  ...(curves?.luminance ? { luminance: curves.luminance.map((p: Coord) => ({ ...p })) } : {}),
  ...(curves?.version !== undefined ? { version: curves.version } : {}),
});

/** A style's settings with its curves as loading fills them in (every
 * channel; the version as it is), so that the record of an applied style
 * still matches the curves after the photo is opened again. */
export function withLoadedCurves<T extends { curves?: Partial<Curves> }>(values: T): T {
  return values.curves ? { ...values, curves: deepCloneCurves(values.curves) } : values;
}

const deepCloneParametric = (pCurve: Partial<ParametricCurve> | undefined): ParametricCurve => ({
  luma: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS, ...(pCurve?.luma || {}) },
  red: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS, ...(pCurve?.red || {}) },
  green: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS, ...(pCurve?.green || {}) },
  blue: { ...DEFAULT_PARAMETRIC_CURVE_SETTINGS, ...(pCurve?.blue || {}) },
});

/** A saved object whose fields `K` may be missing in recipes written by older versions. */
type Stored<T, K extends keyof T> = Omit<T, K> & Partial<Pick<T, K>>;

type StoredSubMask = Stored<SubMask, 'visible' | 'mode' | 'invert' | 'opacity'>;

/**
 * Import defaults for RAW files: Lightroom's capture sharpening and colour
 * noise reduction, plus a light luminance noise reduction (notes: reference/detail.md). The backend applies the same
 * values to RAW recipes without them (`RAW_DEFAULT_*` in image_processing.rs).
 */
export const RAW_DETAIL_DEFAULTS = {
  sharpness: 40,
  colorNoiseReduction: 25,
  // A light luminance noise reduction: about Camera Raw's and Capture One's
  // default noise (their pipelines are cleaner before any slider).
  lumaNoiseReduction: 15,
} as const;

/** The colour noise reduction new edits start with (notes: reference/detail.md,
 * reference/color-noise-reference.md). */
export const COLOR_NOISE_VERSION = 11;

/** The first colour noise version that is the multi-scale filter; versions
 * from it on share its base image (no decode-time chroma clean-up). */
export const MULTISCALE_COLOR_NOISE_VERSION = 2;

/** The tone-slider response new edits start with (notes: reference/highlight-recovery-gfx.md). */
export const TONE_VERSION = 3;

/** The Clarity response new edits start with (notes: parity-gaps/G06-presence.md). */
export const CLARITY_VERSION = 2;

/** The edge-fringe pass new RAW edits start with (notes: reference/edge-fringes.md):
 * 5 = version 4's pass computed on the GPU. */
export const FRINGE_VERSION = 5;

/** The Defringe detector new edits start with (notes: reference/defringe.md). */
export const DEFRINGE_VERSION = 2;

/** The sharpening response new edits start with (notes: reference/detail.md). */
export const SHARPEN_VERSION = 6;

/** The luminance noise reduction new edits start with (notes: reference/luma-noise-reference.md). */
export const LUMA_NOISE_VERSION = 2;

/** The render pipeline fixes new edits start with (`renderVersion`). */
export const RENDER_VERSION = 10;

/** The frame new edits develop (`sourceFrameVersion`,
 * crates/rawdecode/src/source_frame.rs `VERSION`). */
export const SOURCE_FRAME_VERSION = 1;

/** The render version from which a RAW is developed with the second
 * dead-photosite test (crates/engine/src/image_loader.rs
 * `DEAD_TEST_RENDER_VERSION`). */
export const DEAD_PIXEL_RENDER_VERSION = 3;

/** Whether a recipe's RAW is developed with the second dead-photosite test,
 * as the backend decides (`image_loader::decode_variant`; the editor's
 * recipe of a never-edited photo already carries `RENDER_VERSION`). */
export const deadPixelAdjacent = (adjustments: Pick<Partial<Adjustments>, 'renderVersion'>): boolean =>
  (adjustments.renderVersion ?? 1) >= DEAD_PIXEL_RENDER_VERSION;

/** The render version from which a RAW's shadows keep their means through
 * the black clip and the demosaic (crates/engine/src/image_loader.rs
 * `BLACK_RENDER_VERSION`). */
export const UNBIASED_BLACK_RENDER_VERSION = 8;

/** Whether a recipe's RAW is developed with unbiased blacks, as the backend
 * decides (`image_loader::decode_variant`). */
export const unbiasedBlack = (adjustments: Pick<Partial<Adjustments>, 'renderVersion'>): boolean =>
  (adjustments.renderVersion ?? 1) >= UNBIASED_BLACK_RENDER_VERSION;

/** The render version from which clipped colours inside larger partly
 * clipped areas are rebuilt from colour ratios (crates/engine/src/image_loader.rs
 * `HIGHLIGHT_RENDER_VERSION`). */
export const HIGHLIGHT_RATIOS_RENDER_VERSION = 9;

/** Whether a recipe's RAW is developed with the ratio highlight rebuild, as
 * the backend decides (`image_loader::decode_variant`). */
export const highlightRatios = (adjustments: Pick<Partial<Adjustments>, 'renderVersion'>): boolean =>
  (adjustments.renderVersion ?? 1) >= HIGHLIGHT_RATIOS_RENDER_VERSION;

/** The AI Denoise version new edits start with (`aiDenoiseVersion`; notes:
 * benchmark-results/ai-denoise-mac-2026-10-18). */
export const AI_DENOISE_VERSION = 3;

/** The cameras' CA data new edits read in camera lens mode (notes: reference/lateral-ca.md). */
export const CAMERA_CA_VERSION = 1;
/** How new edits read the camera data's geometry in camera lens mode (notes: parity-gaps/G71). */
export const CAMERA_GEOMETRY_VERSION = 7;
/** The automatic lateral CA new RAW edits start with (notes: reference/lateral-ca.md). */
export const AUTO_CA_VERSION = 3;

/** Whether the automatic lateral CA correction is on for a recipe, as the
 * renderer decides (lateral_ca.rs `applies`, `competes`): its switch when
 * set, else on from `autoCaVersion` 2 when the lens data has no CA
 * correction of its own, and from 3 also when it has one at its default
 * amount (the measurement then replaces it only where it leaves clearly
 * fewer fringes). */
export const autoCaOn = (adjustments: Partial<Adjustments>): boolean => {
  if (adjustments.autoChromaticAberration !== undefined && adjustments.autoChromaticAberration !== null) {
    return adjustments.autoChromaticAberration;
  }
  const version = adjustments.autoCaVersion ?? 1;
  if (version < 2) return false;
  const params = adjustments.lensDistortionParams as Record<string, unknown> | null | undefined;
  const value = (key: string, fallback: number) => (typeof params?.[key] === 'number' ? (params[key] as number) : fallback);
  const ownTca =
    adjustments.lensTcaEnabled !== false &&
    (Math.abs(value('tca_vr', 1) - 1) > 1e-6 ||
      Math.abs(value('tca_vb', 1) - 1) > 1e-6 ||
      Math.abs(value('tca_vr2', 0)) > 1e-9 ||
      Math.abs(value('tca_vb2', 0)) > 1e-9);
  if (!ownTca) return true;
  return version >= 3 && Math.abs((adjustments.lensTcaAmount ?? 100) - 100) < 1e-2;
};

/** Starting adjustments for a photo that has no saved edit. */
export const initialAdjustmentsFor = (isRaw: boolean): Adjustments =>
  isRaw
    ? {
        ...INITIAL_ADJUSTMENTS,
        ...RAW_DETAIL_DEFAULTS,
        colorNoiseVersion: COLOR_NOISE_VERSION,
        toneVersion: TONE_VERSION,
        clarityVersion: CLARITY_VERSION,
        fringeVersion: FRINGE_VERSION,
        cameraCaVersion: CAMERA_CA_VERSION,
        cameraGeometryVersion: CAMERA_GEOMETRY_VERSION,
        autoCaVersion: AUTO_CA_VERSION,
        defringeVersion: DEFRINGE_VERSION,
        sharpenVersion: SHARPEN_VERSION,
        lumaNoiseVersion: LUMA_NOISE_VERSION,
        aiDenoiseVersion: AI_DENOISE_VERSION,
        renderVersion: RENDER_VERSION,
        sourceFrameVersion: SOURCE_FRAME_VERSION,
        grainVersion: GRAIN_VERSION,
      }
    : {
        ...INITIAL_ADJUSTMENTS,
        colorNoiseVersion: COLOR_NOISE_VERSION,
        toneVersion: TONE_VERSION,
        clarityVersion: CLARITY_VERSION,
        defringeVersion: DEFRINGE_VERSION,
        sharpenVersion: SHARPEN_VERSION,
        lumaNoiseVersion: LUMA_NOISE_VERSION,
        renderVersion: RENDER_VERSION,
        sourceFrameVersion: SOURCE_FRAME_VERSION,
        grainVersion: GRAIN_VERSION,
      };

/** Whether a saved recipe is new: empty (Library Reset saves `{}`, or only
 * the photo's frame; loading adds only `lensProfileFor`). It renders as a
 * new edit, as the backend does (`image_processing::is_new_recipe`,
 * `embedded_lens::apply_default`). */
export const isNewRecipe = (loaded: Partial<Adjustments>): boolean =>
  Object.keys(loaded).every((key) => key === 'lensProfileFor' || key === 'sourceFrameVersion');

/** The render version of a saved recipe: its own; the current one for a new
 * recipe; else the first (missing key). */
const loadedRenderVersion = (loaded: Partial<Adjustments>): number | undefined => {
  if (loaded.renderVersion !== undefined && loaded.renderVersion !== null) {
    return loaded.renderVersion;
  }
  return isNewRecipe(loaded) ? RENDER_VERSION : undefined;
};

/** The frame a saved recipe develops: its own; the current one for a new
 * recipe; else the first (missing key), as the backend reads it
 * (`source_frame::requested`). */
const loadedSourceFrameVersion = (loaded: Partial<Adjustments>): number | undefined => {
  if (loaded.sourceFrameVersion !== undefined && loaded.sourceFrameVersion !== null) {
    return loaded.sourceFrameVersion;
  }
  return isNewRecipe(loaded) ? SOURCE_FRAME_VERSION : undefined;
};

/** The Defringe detector of a saved recipe: its own; else the current one
 * when the recipe does not use Defringe (no purple or green amount, no layer
 * with a Defringe above 0), the earlier one otherwise (missing key). */
const loadedDefringeVersion = (loaded: Partial<Adjustments>): number | undefined => {
  if (loaded.defringeVersion !== undefined && loaded.defringeVersion !== null) {
    return loaded.defringeVersion;
  }
  const used =
    (loaded.defringePurpleAmount ?? 0) > 0 ||
    (loaded.defringeGreenAmount ?? 0) > 0 ||
    (loaded.masks ?? []).some((mask) => ((mask?.adjustments?.defringe as number | undefined) ?? 0) > 0);
  return used ? undefined : DEFRINGE_VERSION;
};

/** Whether a recipe renders the same with every sharpening response: no
 * sharpening (photo or layer); response 3 differs from 1 at every amount. */
const sharpensAlikeInEveryVersion = (values: Partial<Adjustments>, isRaw: boolean): boolean => {
  // RAW recipes without an amount sharpen at the RAW default (40).
  const amount = values.sharpness ?? (isRaw ? RAW_DETAIL_DEFAULTS.sharpness : 0);
  const layerSharpens = (values.masks ?? []).some(
    (mask) => ((mask?.adjustments?.sharpness as number | undefined) ?? 0) !== 0,
  );
  return amount <= 0 && !layerSharpens;
};

/** The sharpening response of a saved recipe: its own; else the current one
 * when it is new (as the backend's `apply_render_default`) or renders the
 * same with it, the earliest one otherwise (missing key). */
const loadedSharpenVersion = (loaded: Partial<Adjustments>, isRaw: boolean): number | undefined => {
  if (loaded.sharpenVersion !== undefined && loaded.sharpenVersion !== null) {
    return loaded.sharpenVersion;
  }
  return isNewRecipe(loaded) || sharpensAlikeInEveryVersion(loaded, isRaw) ? SHARPEN_VERSION : undefined;
};

/** What a paste applies from the clipboard: the included keys, in Merge mode
 * only those that differ from a new edit's. The sharpening response is not
 * filtered on its own (a new edit has no key, so it would always survive):
 * it goes along only with sharpening values that do (`withSharpenVersion`). */
export const pastedAdjustments = (
  copied: Partial<Adjustments>,
  includedAdjustments: string[],
  mode: PasteMode,
): Partial<Adjustments> => {
  const picked: Partial<Adjustments> = {};
  for (const key of includedAdjustments) {
    if (
      key === 'sharpenVersion' ||
      key === 'lumaNoiseVersion' ||
      key === 'grainVersion' ||
      key === 'clarityVersion' ||
      !Object.prototype.hasOwnProperty.call(copied, key)
    ) {
      continue;
    }
    const value = copied[key as keyof Adjustments];
    if (mode === PasteMode.Merge && sameAdjustmentValue(value, INITIAL_ADJUSTMENTS[key as keyof Adjustments])) continue;
    (picked as Record<string, unknown>)[key] = value;
  }
  return withClarityVersion(withGrainVersion(withLumaNoiseVersion(withSharpenVersion(picked, copied), copied), copied), copied);
};

/** Keys whose values mean what the sharpening response makes of them. */
const SHARPEN_RESPONSE_KEYS = [
  'sharpness',
  'sharpnessThreshold',
  'sharpenRadius',
  'sharpenDetail',
  'sharpenMasking',
  'sharpnessFalloff',
] as const;

/** Sharpening values mean what their response makes of them, so wherever they
 * are copied (paste, tool copy, Edit all selected, presets) the response goes
 * along explicitly — a missing key would vanish in JSON and the target would
 * keep its own: the source's, else 1 (a source or preset from before the key;
 * partial values cannot tell whether the target would render the same).
 * Layers count when their sharpening amount is set. */
export const withSharpenVersion = <T extends Partial<Adjustments>>(
  picked: T,
  source?: Partial<Adjustments>,
): T & Pick<Partial<Adjustments>, 'sharpenVersion'> => {
  // A change back to a missing key (an undo to a recipe from before it) would
  // vanish in JSON too: it is response 1.
  if (Object.prototype.hasOwnProperty.call(picked, 'sharpenVersion') && picked.sharpenVersion == null) {
    return { ...picked, sharpenVersion: 1 };
  }
  const sharpens =
    SHARPEN_RESPONSE_KEYS.some((key) => picked[key] !== undefined) ||
    (picked.masks ?? []).some((mask) => ((mask?.adjustments?.sharpness as number | undefined) ?? 0) !== 0);
  if (!sharpens || (picked.sharpenVersion !== undefined && picked.sharpenVersion !== null)) {
    return picked;
  }
  return { ...picked, sharpenVersion: source?.sharpenVersion ?? 1 };
};

/** The luminance noise reduction of a saved recipe: its own; else the
 * current one when it reduces no luminance noise (photo or layer), the
 * earlier one otherwise (missing key). */
const loadedLumaNoiseVersion = (loaded: Partial<Adjustments>, isRaw: boolean): number | undefined => {
  if (loaded.lumaNoiseVersion !== undefined && loaded.lumaNoiseVersion !== null) {
    return loaded.lumaNoiseVersion;
  }
  // RAW recipes without an amount reduce at the RAW default (15).
  const amount = loaded.lumaNoiseReduction ?? (isRaw ? RAW_DETAIL_DEFAULTS.lumaNoiseReduction : 0);
  const layerReduces = (loaded.masks ?? []).some(
    (mask) => ((mask?.adjustments?.lumaNoiseReduction as number | undefined) ?? 0) !== 0,
  );
  return amount <= 0 && !layerReduces ? LUMA_NOISE_VERSION : undefined;
};

/** Keys whose values mean what the luminance noise reduction version makes of them. */
const LUMA_NOISE_KEYS = ['lumaNoiseReduction', 'lumaNoiseDetail', 'lumaNoiseContrast'] as const;

/** As `withSharpenVersion` for the luminance noise reduction: wherever
 * luminance values are copied (paste, tool copy, Edit all selected, presets)
 * its version goes along explicitly — the source's, else 1 (a source or
 * preset from before the key). Layers count when their luminance amount is
 * set. */
export const withLumaNoiseVersion = <T extends Partial<Adjustments>>(
  picked: T,
  source?: Partial<Adjustments>,
): T & Pick<Partial<Adjustments>, 'lumaNoiseVersion'> => {
  // A change back to a missing key (an undo to a recipe from before it)
  // would vanish in JSON too: it is version 1.
  if (Object.prototype.hasOwnProperty.call(picked, 'lumaNoiseVersion') && picked.lumaNoiseVersion == null) {
    return { ...picked, lumaNoiseVersion: 1 };
  }
  const reduces =
    LUMA_NOISE_KEYS.some((key) => picked[key] !== undefined) ||
    (picked.masks ?? []).some((mask) => ((mask?.adjustments?.lumaNoiseReduction as number | undefined) ?? 0) !== 0);
  if (!reduces || (picked.lumaNoiseVersion !== undefined && picked.lumaNoiseVersion !== null)) {
    return picked;
  }
  return { ...picked, lumaNoiseVersion: source?.lumaNoiseVersion ?? 1 };
};

/** As `withSharpenVersion` for Clarity: wherever Clarity values are copied
 * (paste, tool copy, Edit all selected, presets) the Clarity response goes
 * along explicitly — the source's, else 1 (a source or preset from before
 * the key), never on its own. Layers with Clarity count only when the source
 * recipe is known: an update of layers alone (an AI detection refresh) keeps
 * the destination's response. */
export const withClarityVersion = <T extends Partial<Adjustments>>(
  picked: T,
  source?: Partial<Adjustments>,
): T & Pick<Partial<Adjustments>, 'clarityVersion'> => {
  // A change back to a missing key (an undo to a recipe from before it)
  // would vanish in JSON too: it is version 1.
  if (Object.prototype.hasOwnProperty.call(picked, 'clarityVersion') && picked.clarityVersion == null) {
    return { ...picked, clarityVersion: 1 };
  }
  const clarity =
    picked.clarity !== undefined ||
    (source !== undefined &&
      (picked.masks ?? []).some((mask) => ((mask?.adjustments?.clarity as number | undefined) ?? 0) !== 0));
  if (!clarity || picked.clarityVersion != null) {
    return picked;
  }
  return { ...picked, clarityVersion: source?.clarityVersion ?? 1 };
};

/** A preset's settings with its Clarity response stated: the preset is its
 * own source, so Clarity on its layers alone (a preset from before the key)
 * renders with version 1 wherever it is previewed or applied. */
export const withPresetClarityVersion = <T extends Partial<Adjustments>>(preset: T) =>
  withClarityVersion(preset, preset);

/** Keys whose values render the pattern of the recipe's grain version. */
const GRAIN_KEYS = ['grainAmount', 'grainSize', 'grainRoughness', 'grainStructure', 'grainColor'] as const;

/** As `withSharpenVersion` for the grain: wherever grain values are copied
 * (paste, tool copy, Edit all selected, presets) its version goes along
 * explicitly, so the copy renders the same grain pattern as the source — the
 * source's version, else 1 for a source with grain from before the key and
 * the newest for one without grain. */
export const withGrainVersion = <T extends Partial<Adjustments>>(
  picked: T,
  source?: Partial<Adjustments>,
): T & Pick<Partial<Adjustments>, 'grainVersion'> => {
  // A change back to a missing key (an undo to a recipe from before it)
  // would vanish in JSON too: it is version 1.
  if (Object.prototype.hasOwnProperty.call(picked, 'grainVersion') && picked.grainVersion == null) {
    return { ...picked, grainVersion: 1 };
  }
  let versioned: T & Pick<Partial<Adjustments>, 'grainVersion'> = picked;
  if (GRAIN_KEYS.some((key) => Object.prototype.hasOwnProperty.call(picked, key)) && picked.grainVersion == null) {
    const version = source?.grainVersion ?? ((source?.grainAmount ?? 0) > 0 ? 1 : GRAIN_VERSION);
    versioned = { ...picked, grainVersion: version };
  }
  // Film grain's Colour picked while it is still the missing default (0)
  // would vanish in JSON, and the copy would keep the destination's Colour.
  if (
    Object.prototype.hasOwnProperty.call(picked, 'grainColor') &&
    picked.grainColor == null &&
    (versioned.grainVersion ?? 0) >= 3
  ) {
    versioned = { ...versioned, grainColor: 0 };
  }
  return versioned;
};

/** The AI Denoise version of a saved recipe: its own; else the current one
 * when it does not use AI Denoise (the key only matters above 0), the
 * earlier one otherwise (missing key). */
const loadedAiDenoiseVersion = (loaded: Partial<Adjustments>, isRaw: boolean): number | undefined => {
  if (loaded.aiDenoiseVersion !== undefined && loaded.aiDenoiseVersion !== null) {
    return loaded.aiDenoiseVersion;
  }
  return isRaw && (loaded.aiDenoise ?? 0) <= 0 ? AI_DENOISE_VERSION : undefined;
};

/** The tone-slider response of a saved recipe: its own; else the current
 * one when nothing in it renders differently with it (no Shadows above +50
 * and no lowered Highlights on the photo or a layer), the earlier one
 * otherwise (missing key). */
const loadedToneVersion = (loaded: Partial<Adjustments>): number | undefined => {
  if (loaded.toneVersion !== undefined && loaded.toneVersion !== null) {
    return loaded.toneVersion;
  }
  // Versions 2 and 3 change Shadows above +50 and lowered Highlights only.
  const differs = (values: { shadows?: unknown; highlights?: unknown } | undefined) =>
    ((values?.shadows as number | undefined) ?? 0) > 50 || ((values?.highlights as number | undefined) ?? 0) < 0;
  const rendersDifferently = differs(loaded) || (loaded.masks ?? []).some((mask) => differs(mask?.adjustments));
  return rendersDifferently ? undefined : TONE_VERSION;
};

/** The Clarity response of a saved recipe: its own; else the current one
 * when it uses no Clarity (on the photo or a layer), else the first one,
 * stated (1 renders as the missing key does), so Clarity copied from it
 * keeps its response on a photo already at the current one. */
const loadedClarityVersion = (loaded: Partial<Adjustments>): number => {
  if (loaded.clarityVersion !== undefined && loaded.clarityVersion !== null) {
    return loaded.clarityVersion;
  }
  const uses = (values: { clarity?: unknown } | undefined) => ((values?.clarity as number | undefined) ?? 0) !== 0;
  const rendersDifferently = uses(loaded) || (loaded.masks ?? []).some((mask) => uses(mask?.adjustments));
  return rendersDifferently ? 1 : CLARITY_VERSION;
};

/** The colour noise filter of a saved recipe: its own, else the earlier
 * filter when it sets the colour amount, and the current one when it leaves
 * it to the RAW default (as the backend decides, `color_noise::multiscale`). */
const loadedColorNoiseVersion = (loaded: Partial<Adjustments>, isRaw: boolean): number | undefined =>
  loaded.colorNoiseVersion ?? (isRaw && loaded.colorNoiseReduction === undefined ? COLOR_NOISE_VERSION : undefined);

/** Whether a recipe develops colour noise with the multi-scale filter, as
 * the backend decides (`color_noise::multiscale`); the base image of such a
 * recipe is developed without the decode-time chroma clean-up. */
export const colorNoiseMultiscale = (
  adjustments: Pick<Partial<Adjustments>, 'colorNoiseVersion' | 'colorNoiseReduction'>,
  isRaw: boolean,
): boolean =>
  adjustments.colorNoiseVersion !== undefined && adjustments.colorNoiseVersion !== null
    ? adjustments.colorNoiseVersion >= MULTISCALE_COLOR_NOISE_VERSION
    : isRaw && adjustments.colorNoiseReduction === undefined;

/** Deep equality of adjustment values regardless of object key order (stored
 * recipes come back with sorted keys, so JSON strings of equal values differ). */
const CURVE_CHANNELS = ['luma', 'red', 'green', 'blue'];
const isCurves = (value: Record<string, unknown>) => Array.isArray(value.luma) && 'red' in value;
const changesTone = (points: unknown) =>
  Array.isArray(points) &&
  points.length >= 2 &&
  !(
    (points as Coord[]).every((p) => Math.abs(p.x - p.y) <= 0.5) &&
    Math.abs(points[0].x) < 0.1 &&
    Math.abs(points[0].y) < 0.1 &&
    Math.abs(points[points.length - 1].x - 255) < 0.1 &&
    Math.abs(points[points.length - 1].y - 255) < 0.1
  );
/** How the curves' version renders them: whole beyond 16 points (2), in the
 * wide working space (3, point_curve.rs `curve_space`), or neither. */
const renderedVersion = (curves: Record<string, unknown>) => {
  const version = typeof curves.version === 'number' ? curves.version : 1;
  const long = CURVE_CHANNELS.some((channel) => {
    const points = curves[channel];
    return Array.isArray(points) && points.length > MAX_CURVE_POINTS;
  });
  const wide = CURVE_CHANNELS.some((channel) => changesTone(curves[channel]));
  return `${long && version >= LONG_CURVE_VERSION}${wide && version >= CURVE_VERSION}`;
};

/**
 * The version an edit of `previous` writes: curves that already change the
 * tone keep the space they render in (an old curve does not shift colours
 * when a point moves), at least whole beyond 16 points; neutral or missing
 * curves start in the current one.
 */
export const editedCurvesVersion = (previous: Partial<Curves> | undefined) =>
  previous && CURVE_CHANNELS.some((channel) => changesTone(previous[channel]))
    ? Math.max(previous.version ?? 1, LONG_CURVE_VERSION)
    : CURVE_VERSION;

export function sameAdjustmentValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a) && Array.isArray(b))
    return a.length === b.length && a.every((value, i) => sameAdjustmentValue(value, b[i]));
  let left = a as Record<string, unknown>;
  let right = b as Record<string, unknown>;
  if (isCurves(left) && isCurves(right)) {
    // A neutral Luminance curve renders as none (curves without one).
    if (!changesTone(left.luminance)) left = { ...left, luminance: undefined };
    if (!changesTone(right.luminance)) right = { ...right, luminance: undefined };
    // A curves version matters only where it renders differently
    // (`Curves.version`): neutral curves saved before it equal new ones.
    if (renderedVersion(left) === renderedVersion(right)) {
      left = { ...left, version: undefined };
      right = { ...right, version: undefined };
    }
  }
  const keys = Object.keys(left).filter((key) => left[key] !== undefined);
  return (
    keys.length === Object.keys(right).filter((key) => right[key] !== undefined).length &&
    keys.every((key) => sameAdjustmentValue(left[key], right[key]))
  );
}

/** Default of one adjustment key, honouring the RAW detail defaults. */
export const defaultAdjustmentValue = <K extends keyof Adjustments>(key: K, isRaw: boolean): Adjustments[K] =>
  initialAdjustmentsFor(isRaw)[key];

export const normalizeLoadedAdjustments = (loadedAdjustments: Adjustments, isRaw = false): Adjustments => {
  if (!loadedAdjustments) {
    return initialAdjustmentsFor(isRaw);
  }

  const normalizeSubMasks = (subMasks: StoredSubMask[] | undefined): SubMask[] => {
    return (subMasks || []).map((subMask: StoredSubMask) => ({
      visible: true,
      mode: SubMaskMode.Additive,
      invert: false,
      opacity: 100,
      ...subMask,
    }));
  };

  const normalizedMasks = (loadedAdjustments.masks || []).map((maskContainer: Stored<MaskContainer, 'id'>) => {
    const containerAdjustments = maskContainer.adjustments || {};
    const normalizedSubMasks = normalizeSubMasks(maskContainer.subMasks);

    return {
      ...INITIAL_MASK_CONTAINER,
      id: maskContainer.id || uuidv4(),
      ...maskContainer,
      adjustments: {
        ...INITIAL_MASK_ADJUSTMENTS,
        ...containerAdjustments,
        flareAmount: containerAdjustments.flareAmount ?? INITIAL_MASK_ADJUSTMENTS.flareAmount,
        glowAmount: containerAdjustments.glowAmount ?? INITIAL_MASK_ADJUSTMENTS.glowAmount,
        halationAmount: containerAdjustments.halationAmount ?? INITIAL_MASK_ADJUSTMENTS.halationAmount,
        hue: containerAdjustments.hue ?? INITIAL_MASK_ADJUSTMENTS.hue,
        colorGrading: { ...INITIAL_MASK_ADJUSTMENTS.colorGrading, ...(containerAdjustments.colorGrading || {}) },
        hsl: { ...INITIAL_MASK_ADJUSTMENTS.hsl, ...(containerAdjustments.hsl || {}) },
        curves: containerAdjustments.curves ? deepCloneCurves(containerAdjustments.curves) : getDefaultCurves(),
        pointCurves: containerAdjustments.pointCurves
          ? deepCloneCurves(containerAdjustments.pointCurves)
          : getDefaultCurves(),
        parametricCurve: containerAdjustments.parametricCurve
          ? deepCloneParametric(containerAdjustments.parametricCurve)
          : getDefaultParametricCurve(),
        curveMode: containerAdjustments.curveMode || INITIAL_MASK_ADJUSTMENTS.curveMode,
        sectionVisibility: {
          ...INITIAL_MASK_ADJUSTMENTS.sectionVisibility,
          ...(containerAdjustments.sectionVisibility || {}),
        },
        sharpnessThreshold: containerAdjustments.sharpnessThreshold ?? INITIAL_MASK_ADJUSTMENTS.sharpnessThreshold,
      },
      subMasks: normalizedSubMasks,
    };
  });

  const normalizedAiPatches = (loadedAdjustments.aiPatches || []).map((patch: Stored<AiPatch, 'visible'>) => ({
    visible: true,
    ...patch,
    subMasks: normalizeSubMasks(patch.subMasks),
  }));

  return withLegacyGrainVersion(withGrainStructure({
    ...initialAdjustmentsFor(isRaw),
    ...loadedAdjustments,
    guidedPerspective: {
      enabled: loadedAdjustments.guidedPerspective?.enabled ?? false,
      lines: (() => {
        const clamp01 = (n: number) => Math.min(1, Math.max(0, n));
        const raw = (loadedAdjustments.guidedPerspective?.lines || []).map((l: Partial<GuideLine>) => ({
          id: l.id || uuidv4(),
          type: (l.type === 'horizontal' ? 'horizontal' : 'vertical') as 'vertical' | 'horizontal',
          p1: { x: clamp01(l.p1?.x ?? 0), y: clamp01(l.p1?.y ?? 0) },
          p2: { x: clamp01(l.p2?.x ?? 1), y: clamp01(l.p2?.y ?? 1) },
        }));
        const verts = raw.filter((l) => l.type === 'vertical').slice(0, 2);
        const hors = raw.filter((l) => l.type === 'horizontal').slice(0, 2);
        return [...verts, ...hors];
      })(),
      autoCrop: loadedAdjustments.guidedPerspective?.autoCrop ?? true,
      ...(loadedAdjustments.guidedPerspective?.upright ? { upright: loadedAdjustments.guidedPerspective.upright } : {}),
    },
    lutIsSceneReferred: loadedAdjustments.lutIsSceneReferred ?? false,
    colorNoiseVersion: loadedColorNoiseVersion(loadedAdjustments, isRaw),
    toneVersion: loadedToneVersion(loadedAdjustments),
    clarityVersion: loadedClarityVersion(loadedAdjustments),
    defringeVersion: loadedDefringeVersion(loadedAdjustments),
    sharpenVersion: loadedSharpenVersion(loadedAdjustments, isRaw),
    // A saved recipe's own grain; without the key 1 where it has grain, else
    // the newest (a new edit's, as `initialAdjustmentsFor`).
    grainVersion: loadedAdjustments.grainVersion ?? ((loadedAdjustments.grainAmount ?? 0) > 0 ? 1 : GRAIN_VERSION),
    lumaNoiseVersion: loadedLumaNoiseVersion(loadedAdjustments, isRaw),
    aiDenoiseVersion: loadedAiDenoiseVersion(loadedAdjustments, isRaw),
    // Saved recipes keep rendering as they were made: no pass unless they
    // carry the key (never-edited RAWs get it in useImageLoader).
    fringeVersion: loadedAdjustments.fringeVersion,
    cameraCaVersion: loadedAdjustments.cameraCaVersion,
    cameraGeometryVersion: loadedAdjustments.cameraGeometryVersion,
    autoCaVersion: loadedAdjustments.autoCaVersion,
    renderVersion: loadedRenderVersion(loadedAdjustments),
    sourceFrameVersion: loadedSourceFrameVersion(loadedAdjustments),
    ...normalizeRelight(loadedAdjustments.relightLights, loadedAdjustments.relightDepth),
    relightDepthAmount: loadedAdjustments.relightDepthAmount,
    relightSkinReflections: loadedAdjustments.relightSkinReflections,
    flareAmount: loadedAdjustments.flareAmount ?? INITIAL_ADJUSTMENTS.flareAmount,
    glowAmount: loadedAdjustments.glowAmount ?? INITIAL_ADJUSTMENTS.glowAmount,
    halationAmount: loadedAdjustments.halationAmount ?? INITIAL_ADJUSTMENTS.halationAmount,
    lensBlurAmount: loadedAdjustments.lensBlurAmount ?? INITIAL_ADJUSTMENTS.lensBlurAmount,
    lensBlurDiffusion: loadedAdjustments.lensBlurDiffusion ?? INITIAL_ADJUSTMENTS.lensBlurDiffusion,
    lensBlurShape: loadedAdjustments.lensBlurShape ?? INITIAL_ADJUSTMENTS.lensBlurShape,
    lensBlurDepthMap: loadedAdjustments.lensBlurDepthMap ?? INITIAL_ADJUSTMENTS.lensBlurDepthMap,
    lensBlurEnabled: loadedAdjustments.lensBlurEnabled ?? INITIAL_ADJUSTMENTS.lensBlurEnabled,
    lensBlurMaxDepth: loadedAdjustments.lensBlurMaxDepth ?? INITIAL_ADJUSTMENTS.lensBlurMaxDepth,
    lensBlurMaxFade: loadedAdjustments.lensBlurMaxFade ?? INITIAL_ADJUSTMENTS.lensBlurMaxFade,
    lensBlurMinDepth: loadedAdjustments.lensBlurMinDepth ?? INITIAL_ADJUSTMENTS.lensBlurMinDepth,
    lensBlurMinFade: loadedAdjustments.lensBlurMinFade ?? INITIAL_ADJUSTMENTS.lensBlurMinFade,
    lensCorrectionMode: loadedAdjustments.lensCorrectionMode || 'manual',
    lensMaker: loadedAdjustments.lensMaker ?? INITIAL_ADJUSTMENTS.lensMaker,
    lensModel: loadedAdjustments.lensModel ?? INITIAL_ADJUSTMENTS.lensModel,
    lensDistortionAmount: loadedAdjustments.lensDistortionAmount ?? INITIAL_ADJUSTMENTS.lensDistortionAmount,
    lensVignetteAmount: loadedAdjustments.lensVignetteAmount ?? INITIAL_ADJUSTMENTS.lensVignetteAmount,
    lensTcaAmount: loadedAdjustments.lensTcaAmount ?? INITIAL_ADJUSTMENTS.lensTcaAmount,
    lensDistortionEnabled: loadedAdjustments.lensDistortionEnabled ?? INITIAL_ADJUSTMENTS.lensDistortionEnabled,
    lensTcaEnabled: loadedAdjustments.lensTcaEnabled ?? INITIAL_ADJUSTMENTS.lensTcaEnabled,
    lensVignetteEnabled: loadedAdjustments.lensVignetteEnabled ?? INITIAL_ADJUSTMENTS.lensVignetteEnabled,
    lensDistortionParams: loadedAdjustments.lensDistortionParams ?? INITIAL_ADJUSTMENTS.lensDistortionParams,
    transformDistortion: loadedAdjustments.transformDistortion ?? INITIAL_ADJUSTMENTS.transformDistortion,
    transformVertical: loadedAdjustments.transformVertical ?? INITIAL_ADJUSTMENTS.transformVertical,
    transformHorizontal: loadedAdjustments.transformHorizontal ?? INITIAL_ADJUSTMENTS.transformHorizontal,
    transformRotate: loadedAdjustments.transformRotate ?? INITIAL_ADJUSTMENTS.transformRotate,
    transformAspect: loadedAdjustments.transformAspect ?? INITIAL_ADJUSTMENTS.transformAspect,
    transformScale: loadedAdjustments.transformScale ?? INITIAL_ADJUSTMENTS.transformScale,
    transformXOffset: loadedAdjustments.transformXOffset ?? INITIAL_ADJUSTMENTS.transformXOffset,
    transformYOffset: loadedAdjustments.transformYOffset ?? INITIAL_ADJUSTMENTS.transformYOffset,
    colorCalibration: { ...INITIAL_ADJUSTMENTS.colorCalibration, ...(loadedAdjustments.colorCalibration || {}) },
    colorGrading: { ...INITIAL_ADJUSTMENTS.colorGrading, ...(loadedAdjustments.colorGrading || {}) },
    blackAndWhite: loadedAdjustments.blackAndWhite ?? false,
    grayMixer: { ...INITIAL_GRAY_MIXER, ...(loadedAdjustments.grayMixer || {}) },
    colorRanges: (loadedAdjustments.colorRanges || []).map((range: ColorRange) => ({ ...range })),
    regionCurve: { ...INITIAL_REGION_CURVE, ...(loadedAdjustments.regionCurve || {}) },
    levels: Object.fromEntries(
      Object.entries(INITIAL_LEVELS).map(([channel, level]) => [
        channel,
        { ...level, ...(loadedAdjustments.levels?.[channel] || {}) },
      ]),
    ) as Levels,
    hsl: { ...INITIAL_ADJUSTMENTS.hsl, ...(loadedAdjustments.hsl || {}) },
    curves: loadedAdjustments.curves ? deepCloneCurves(loadedAdjustments.curves) : getDefaultCurves(),
    pointCurves: loadedAdjustments.pointCurves ? deepCloneCurves(loadedAdjustments.pointCurves) : getDefaultCurves(),
    parametricCurve: loadedAdjustments.parametricCurve
      ? deepCloneParametric(loadedAdjustments.parametricCurve)
      : getDefaultParametricCurve(),
    curveMode: loadedAdjustments.curveMode || INITIAL_ADJUSTMENTS.curveMode,
    masks: normalizedMasks,
    aiPatches: normalizedAiPatches,
    spots: Array.isArray(loadedAdjustments.spots) ? loadedAdjustments.spots.map((spot: Spot) => ({ ...spot })) : [],
    redEyes: Array.isArray(loadedAdjustments.redEyes)
      ? loadedAdjustments.redEyes.map((eye: RedEye) => ({ ...eye }))
      : [],
    sectionVisibility: {
      ...INITIAL_ADJUSTMENTS.sectionVisibility,
      ...(loadedAdjustments.sectionVisibility || {}),
    },
    sharpnessThreshold: loadedAdjustments.sharpnessThreshold ?? INITIAL_ADJUSTMENTS.sharpnessThreshold,
    sharpenRadius: loadedAdjustments.sharpenRadius ?? INITIAL_ADJUSTMENTS.sharpenRadius,
    sharpenDetail: loadedAdjustments.sharpenDetail ?? INITIAL_ADJUSTMENTS.sharpenDetail,
    sharpenMasking: loadedAdjustments.sharpenMasking ?? INITIAL_ADJUSTMENTS.sharpenMasking,
  }));
};

export interface AdjustmentGroup {
  label: string;
  /** Override the category in the Copy/Sync picker when the panel groups these controls elsewhere. */
  clipboardSection?: string;
  keys: string[];
}

export const ADJUSTMENT_GROUPS: Record<string, AdjustmentGroup[]> = {
  basic: [
    {
      label: 'modals.copyPaste.groups.exposureToneMapper',
      keys: [BasicAdjustment.Exposure, 'toneMapper', 'baseCurve', 'cameraProfile'],
    },
    {
      label: 'modals.copyPaste.groups.tone',
      keys: [
        BasicAdjustment.Brightness,
        BasicAdjustment.Contrast,
        BasicAdjustment.Highlights,
        BasicAdjustment.Shadows,
        BasicAdjustment.Whites,
        BasicAdjustment.Blacks,
        'toneVersion',
      ],
    },
    {
      label: 'modals.copyPaste.groups.curves',
      keys: ['curves', 'pointCurves', 'parametricCurve', 'curveMode', 'levels', 'regionCurve'],
    },
  ],
  color: [
    {
      label: 'modals.copyPaste.groups.whiteBalance',
      keys: [ColorAdjustment.Temperature, ColorAdjustment.Tint, 'whiteBalance', 'wbTemperature', 'wbTint'],
    },
    { label: 'modals.copyPaste.groups.presence', clipboardSection: 'basic', keys: [ColorAdjustment.Saturation, ColorAdjustment.Vibrance] },
    {
      label: 'modals.copyPaste.groups.hueShift',
      keys: [ColorAdjustment.Hue],
    },
    { label: 'modals.copyPaste.groups.colorGrading', keys: [ColorAdjustment.ColorGrading] },
    { label: 'modals.copyPaste.groups.colorMixer', keys: [ColorAdjustment.Hsl, 'colorRanges'] },
    { label: 'modals.copyPaste.groups.colorCalibration', keys: ['colorCalibration'] },
    { label: 'modals.copyPaste.groups.blackAndWhite', keys: ['blackAndWhite', 'grayMixer'] },
  ],
  details: [
    {
      label: 'modals.copyPaste.groups.clarityDehaze',
      clipboardSection: 'basic',
      keys: [
        DetailsAdjustment.Clarity,
        DetailsAdjustment.Structure,
        DetailsAdjustment.Dehaze,
        DetailsAdjustment.Centré,
        'clarityVersion',
      ],
    },
    {
      label: 'modals.copyPaste.groups.sharpness',
      keys: [
        DetailsAdjustment.Sharpness,
        DetailsAdjustment.SharpnessThreshold,
        'sharpnessFalloff',
        'sharpenRadius',
        'sharpenDetail',
        'sharpenMasking',
        'sharpenVersion',
      ],
    },
    {
      label: 'modals.copyPaste.groups.noiseReduction',
      keys: [
        DetailsAdjustment.LumaNoiseReduction,
        DetailsAdjustment.ColorNoiseReduction,
        'lumaNoiseDetail',
        'lumaNoiseContrast',
        'colorNoiseDetail',
        'colorNoiseSmoothness',
        'colorNoiseVersion',
        'lumaNoiseVersion',
        'aiDenoise',
        'aiDenoiseVersion',
      ],
    },
    {
      label: 'modals.copyPaste.groups.grain',
      keys: [Effect.GrainAmount, Effect.GrainRoughness, Effect.GrainSize, 'grainStructure', 'grainColor', 'grainVersion'],
    },
    {
      label: 'modals.copyPaste.groups.chromaticAberration',
      keys: [
        DetailsAdjustment.ChromaticAberrationRedCyan,
        DetailsAdjustment.ChromaticAberrationBlueYellow,
        'defringePurpleAmount',
        'defringePurpleHueLo',
        'defringePurpleHueHi',
        'defringeGreenAmount',
        'defringeGreenHueLo',
        'defringeGreenHueHi',
        'moireAmount',
        'moirePattern',
        'fringeVersion',
        'defringeVersion',
      ],
    },
  ],
  effects: [
    { label: 'studio.relight.title', keys: ['relightLights', 'relightDepthAmount', 'relightSkinReflections', 'relightOriginalLight'] },
    {
      label: 'modals.copyPaste.groups.vignette',
      keys: [
        Effect.VignetteAmount,
        Effect.VignetteFeather,
        Effect.VignetteMidpoint,
        Effect.VignetteRoundness,
        'vignetteStyle',
        'vignetteHighlights',
      ],
    },
    {
      label: 'modals.copyPaste.groups.halationGlow',
      keys: [CreativeAdjustment.GlowAmount, CreativeAdjustment.HalationAmount, CreativeAdjustment.FlareAmount],
    },
    {
      label: 'modals.copyPaste.groups.lut',
      keys: [
        Effect.LutIntensity,
        Effect.LutName,
        Effect.LutPath,
        Effect.LutSize,
        Effect.LutData,
        Effect.LutIsSceneReferred,
      ],
    },
  ],
  geometry: [
    { label: 'modals.copyPaste.groups.cropAspectRatio', keys: ['crop', 'aspectRatio'] },
    {
      label: 'modals.copyPaste.groups.transformRotation',
      keys: [
        'rotation',
        'flipHorizontal',
        'flipVertical',
        'orientationSteps',
        TransformAdjustment.TransformDistortion,
        TransformAdjustment.TransformVertical,
        TransformAdjustment.TransformHorizontal,
        TransformAdjustment.TransformRotate,
        TransformAdjustment.TransformAspect,
        TransformAdjustment.TransformScale,
        TransformAdjustment.TransformXOffset,
        TransformAdjustment.TransformYOffset,
      ],
    },
    {
      label: 'modals.copyPaste.groups.lensCorrection',
      keys: [
        LensAdjustment.LensCorrectionMode,
        LensAdjustment.LensMaker,
        LensAdjustment.LensModel,
        LensAdjustment.LensDistortionAmount,
        LensAdjustment.LensVignetteAmount,
        LensAdjustment.LensTcaAmount,
        LensAdjustment.LensDistortionEnabled,
        LensAdjustment.LensTcaEnabled,
        LensAdjustment.LensVignetteEnabled,
        'diffractionCorrection',
        'diffractionVersion',
        'autoChromaticAberration',
        'cameraCaVersion',
        'cameraGeometryVersion',
        'autoCaVersion',
      ],
    },
    {
      label: 'modals.copyPaste.groups.guidedPerspective',
      keys: ['guidedPerspective'],
    },
  ],
  masks: [{ label: 'modals.copyPaste.groups.masks', keys: ['masks'] }],
};

export const COPYABLE_ADJUSTMENT_KEYS: string[] = Object.values(ADJUSTMENT_GROUPS)
  .flat()
  .flatMap((group) => group.keys);

/** Adjustments that belong to one photograph or to the view and are never
 * copied: retouching, the depth-based lens blur, derived lens data, the
 * render pipeline version, the frame of the RAW developed, tool
 * visibility, the clipping display and the style last applied (with this
 * photograph's settings before it, src/utils/appliedStyle.ts). Every other
 * key must be copyable. */
export const NON_COPYABLE_ADJUSTMENT_KEYS: string[] = [
  'appliedStyle',
  'relightDepth',
  'aiPatches',
  'spots',
  'redEyes',
  'faceRetouch',
  'lensBlurAmount',
  'lensBlurDiffusion',
  'lensBlurShape',
  'lensBlurDepthMap',
  'lensBlurEnabled',
  'lensBlurMaxDepth',
  'lensBlurMaxFade',
  'lensBlurMinDepth',
  'lensBlurMinFade',
  'lensDistortionParams',
  'lensProfileFor',
  'renderVersion',
  'sourceFrameVersion',
  'sectionVisibility',
  'showClipping',
];

export const ADJUSTMENT_SECTIONS: Sections = {
  basic: [
    BasicAdjustment.Brightness,
    BasicAdjustment.Contrast,
    BasicAdjustment.Highlights,
    BasicAdjustment.Shadows,
    BasicAdjustment.Whites,
    BasicAdjustment.Blacks,
    BasicAdjustment.Exposure,
    'toneMapper',
    'baseCurve',
    'cameraProfile',
  ],
  curves: ['curves', 'pointCurves', 'parametricCurve', 'curveMode', 'levels', 'regionCurve'],
  color: [
    ColorAdjustment.Saturation,
    ColorAdjustment.Temperature,
    ColorAdjustment.Tint,
    'whiteBalance',
    'wbTemperature',
    'wbTint',
    ColorAdjustment.Vibrance,
    ColorAdjustment.Hsl,
    ColorAdjustment.ColorGrading,
    'colorCalibration',
    ColorAdjustment.Hue,
    'blackAndWhite',
    'grayMixer',
    'colorRanges',
  ],
  details: [
    DetailsAdjustment.Clarity,
    DetailsAdjustment.Dehaze,
    DetailsAdjustment.Structure,
    DetailsAdjustment.Centré,
    DetailsAdjustment.Sharpness,
    DetailsAdjustment.SharpnessThreshold,
    'sharpnessFalloff',
    DetailsAdjustment.LumaNoiseReduction,
    DetailsAdjustment.ColorNoiseReduction,
    DetailsAdjustment.ChromaticAberrationRedCyan,
    DetailsAdjustment.ChromaticAberrationBlueYellow,
    'defringePurpleAmount',
    'defringePurpleHueLo',
    'defringePurpleHueHi',
    'defringeGreenAmount',
    'defringeGreenHueLo',
    'defringeGreenHueHi',
    'moireAmount',
    'moirePattern',
  ],
  effects: [
    'relightLights',
    'relightDepthAmount',
    'relightSkinReflections',
    'relightOriginalLight',
    CreativeAdjustment.GlowAmount,
    CreativeAdjustment.HalationAmount,
    CreativeAdjustment.FlareAmount,
    Effect.GrainAmount,
    Effect.GrainRoughness,
    Effect.GrainSize,
    'grainStructure',
    'grainColor',
    Effect.LutIntensity,
    Effect.LutName,
    Effect.LutPath,
    Effect.LutSize,
    Effect.LutIsSceneReferred,
    Effect.VignetteAmount,
    Effect.VignetteFeather,
    Effect.VignetteMidpoint,
    Effect.VignetteRoundness,
    'vignetteStyle',
    'vignetteHighlights',
    Effect.LensBlurEnabled,
    Effect.LensBlurAmount,
    Effect.lensBlurDiffusion,
    Effect.LensBlurShape,
    Effect.LensBlurDepthMap,
    Effect.LensBlurMinDepth,
    Effect.LensBlurMaxDepth,
    Effect.LensBlurMinFade,
    Effect.LensBlurMaxFade,
  ],
};

const reconcileOrder = (defaultOrder: string[], order: string[] = []): string[] => {
  const savedOrder = order.filter((id, index) => defaultOrder.includes(id) && order.indexOf(id) === index);
  return [...savedOrder, ...defaultOrder.filter((id) => !savedOrder.includes(id))];
};

export const getAdjustmentSectionOrder = (order?: string[]): string[] =>
  reconcileOrder(Object.keys(ADJUSTMENT_SECTIONS), order);

export const getVisibleAdjustmentSections = (layout?: AdjustmentLayout): string[] =>
  getAdjustmentSectionOrder(layout?.sectionOrder).filter((section) => !layout?.hiddenSections?.includes(section));

export const DEFAULT_HIDDEN_ADJUSTMENT_TOOLS = ['chromaticAberration', 'colorCalibration'];

export const getHiddenAdjustmentTools = (layout?: AdjustmentLayout): string[] =>
  layout?.hiddenTools ?? DEFAULT_HIDDEN_ADJUSTMENT_TOOLS;

export const withAdjustmentLayout = (settings: AppSettings, changes: Partial<AdjustmentLayout>): AppSettings => ({
  ...settings,
  adjustmentLayout: { ...settings.adjustmentLayout, ...changes },
});

export interface AdjustmentSectionTool {
  id: string;
  label: string;
}

export const ADJUSTMENT_SECTION_TOOLS: Record<string, Array<AdjustmentSectionTool>> = {
  color: [
    { id: 'whiteBalance', label: 'adjustments.color.whiteBalance' },
    { id: 'colorPresence', label: 'adjustments.color.presence' },
    { id: 'hue', label: 'adjustments.color.hue' },
    { id: 'colorGrading', label: 'adjustments.color.colorGrading' },
    { id: 'colorMixer', label: 'adjustments.color.colorMixer' },
    { id: 'colorCalibration', label: 'adjustments.color.calibration.title' },
  ],
  details: [
    { id: 'sharpening', label: 'adjustments.details.sharpening' },
    { id: 'presence', label: 'adjustments.details.presence' },
    { id: 'noiseReduction', label: 'adjustments.details.noiseReduction' },
    { id: 'chromaticAberration', label: 'adjustments.details.chromaticAberration' },
  ],
  effects: [
    { id: 'creative', label: 'adjustments.effects.creative' },
    { id: 'lensBlur', label: 'adjustments.effects.lensBlur' },
    { id: 'lut', label: 'adjustments.effects.lut' },
    { id: 'vignette', label: 'adjustments.effects.vignette' },
    { id: 'grain', label: 'adjustments.effects.grain' },
  ],
};

export const getAdjustmentSectionToolIds = (section: string): string[] =>
  (ADJUSTMENT_SECTION_TOOLS[section] ?? []).map((tool) => tool.id);

export const getAdjustmentToolOrder = (section: string, toolOrder?: Record<string, string[]>): string[] =>
  reconcileOrder(getAdjustmentSectionToolIds(section), toolOrder?.[section]);
