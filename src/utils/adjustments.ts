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

export function newColorRange(hue: number, chroma: number, lightness: number): ColorRange {
  return {
    id: uuidv4(),
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

export interface FaceRetouch {
  settings: FaceRetouchSettings;
  faces: RetouchFace[];
  /** The backend's key of the geometry the faces were found in. */
  frame: string;
  /** The same for the panel: the transform adjustments it analysed with. */
  geometry: string;
  enabled?: boolean;
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

/** Writes Even into grain that has none (recipes and presets from before the
 * structure choice), so it keeps rendering Even when copied, pasted, saved as
 * a preset or turned off and on again. */
export const withGrainStructure = <T extends Partial<Adjustments>>(adjustments: T): T =>
  adjustments.grainStructure === undefined && (adjustments.grainAmount ?? 0) > 0
    ? { ...adjustments, grainStructure: 'even' }
    : adjustments;

/** `next` after an edit of `previous`: grain turned on in a recipe without a
 * structure starts as Clumped. */
export const withNewGrainStructure = <T extends Partial<Adjustments>>(previous: Partial<Adjustments>, next: T): T =>
  next.grainStructure === undefined && (previous.grainAmount ?? 0) <= 0 && (next.grainAmount ?? 0) > 0
    ? { ...next, grainStructure: NEW_GRAIN_STRUCTURE }
    : next;

export interface Adjustments {
  [index: string]: unknown;
  aiPatches: Array<AiPatch>;
  /** Spot removal (heal/clone circles), applied in full precision to the
   * base image before every adjustment (crates/engine/src/spot_removal.rs). */
  spots: Array<Spot>;
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
  /** 'camera': the RAW's own correction data (Fujifilm, Sony). */
  lensCorrectionMode: 'auto' | 'manual' | 'camera';
  lensDistortionAmount: number;
  lensVignetteAmount: number;
  lensTcaAmount: number;
  lensDistortionEnabled: boolean;
  lensTcaEnabled: boolean;
  lensVignetteEnabled: boolean;
  /** Capture One's diffraction correction: sharpening by the f-number and
   * pixel pitch (diffraction.rs), RAW only. */
  diffractionCorrection?: boolean;
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
  } | null;
  lensMaker: string | null;
  lensModel: string | null;
  lumaNoiseReduction: number;
  /** Lightroom's noise reduction refinements (0–100): luminance Detail
   * (default 50) and Contrast (0), colour Detail (50) and Smoothness (50). */
  lumaNoiseDetail?: number;
  lumaNoiseContrast?: number;
  colorNoiseDetail?: number;
  colorNoiseSmoothness?: number;
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
  showClipping: boolean;
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
   * (crates/engine/src/lateral_ca.rs). */
  autoChromaticAberration?: boolean;
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
  [index: string]: Array<Coord>;
  blue: Array<Coord>;
  green: Array<Coord>;
  luma: Array<Coord>;
  red: Array<Coord>;
}

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
  name: string;
  opacity: number;
  subMasks: Array<SubMask>;
  visible: boolean;
}

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
});

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

/** Starting adjustments for a photo that has no saved edit. */
export const initialAdjustmentsFor = (isRaw: boolean): Adjustments =>
  isRaw ? { ...INITIAL_ADJUSTMENTS, ...RAW_DETAIL_DEFAULTS } : { ...INITIAL_ADJUSTMENTS };

/** Deep equality of adjustment values regardless of object key order (stored
 * recipes come back with sorted keys, so JSON strings of equal values differ). */
export function sameAdjustmentValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Array.isArray(a) && Array.isArray(b))
    return a.length === b.length && a.every((value, i) => sameAdjustmentValue(value, b[i]));
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
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

  return withGrainStructure({
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
    sectionVisibility: {
      ...INITIAL_ADJUSTMENTS.sectionVisibility,
      ...(loadedAdjustments.sectionVisibility || {}),
    },
    sharpnessThreshold: loadedAdjustments.sharpnessThreshold ?? INITIAL_ADJUSTMENTS.sharpnessThreshold,
    sharpenRadius: loadedAdjustments.sharpenRadius ?? INITIAL_ADJUSTMENTS.sharpenRadius,
    sharpenDetail: loadedAdjustments.sharpenDetail ?? INITIAL_ADJUSTMENTS.sharpenDetail,
    sharpenMasking: loadedAdjustments.sharpenMasking ?? INITIAL_ADJUSTMENTS.sharpenMasking,
  });
};

export interface AdjustmentGroup {
  label: string;
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
    { label: 'modals.copyPaste.groups.presence', keys: [ColorAdjustment.Saturation, ColorAdjustment.Vibrance] },
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
      keys: [
        DetailsAdjustment.Clarity,
        DetailsAdjustment.Structure,
        DetailsAdjustment.Dehaze,
        DetailsAdjustment.Centré,
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
        'aiDenoise',
      ],
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
      ],
    },
  ],
  effects: [
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
      label: 'modals.copyPaste.groups.grain',
      keys: [Effect.GrainAmount, Effect.GrainRoughness, Effect.GrainSize, 'grainStructure'],
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
        'autoChromaticAberration',
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
 * copied: retouching, the depth-based lens blur, derived lens data, tool
 * visibility and the clipping display. Every other key must be copyable. */
export const NON_COPYABLE_ADJUSTMENT_KEYS: string[] = [
  'aiPatches',
  'spots',
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
    CreativeAdjustment.GlowAmount,
    CreativeAdjustment.HalationAmount,
    CreativeAdjustment.FlareAmount,
    Effect.GrainAmount,
    Effect.GrainRoughness,
    Effect.GrainSize,
    'grainStructure',
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
