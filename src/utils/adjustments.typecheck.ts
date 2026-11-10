import type { RelightDepth, RelightLight } from './relight';
// Pinned handwritten interface from 49d13351, before recipe type generation.
// Keep this independent of RecipeKeys and HeldRecipeKey: losing a key or
// changing its required/optional status must fail `npm run typecheck`.
import type {
  Adjustments,
  AiPatch,
  ColorCalibration,
  ColorGradingProps,
  ColorRange,
  Crop,
  Curves,
  FaceRetouch,
  GrayMixer,
  GuidedPerspective,
  Hsl,
  Levels,
  MaskContainer,
  ParametricCurve,
  RedEye,
  RegionCurve,
  SectionVisibility,
  Spot
} from './adjustments';

type GrainStructure = 'even' | 'soft' | 'clumped' | 'speckled' | 'gritty';

type WhiteBalanceMode =
  'asShot' | 'auto' | 'daylight' | 'cloudy' | 'shade' | 'tungsten' | 'fluorescent' | 'flash' | 'custom';

interface OldAdjustments {
  [index: string]: unknown;
  aiPatches: Array<AiPatch>;
  spots: Array<Spot>;
  redEyes?: Array<RedEye>;
  faceRetouch?: FaceRetouch | null;
  aspectRatio: number | null;
  blackAndWhite?: boolean;
  grayMixer?: GrayMixer;
  levels?: Levels;
  regionCurve?: RegionCurve;
  colorRanges?: ColorRange[];
  defringePurpleAmount?: number;
  defringePurpleHueLo?: number;
  defringePurpleHueHi?: number;
  defringeGreenAmount?: number;
  defringeGreenHueLo?: number;
  defringeGreenHueHi?: number;
  moireAmount?: number;
  moirePattern?: number;
  aiDenoise?: number;
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
  lensCorrectionMode: 'auto' | 'manual' | 'camera';
  cameraCaVersion?: number;
  cameraGeometryVersion?: number;
  lensDistortionAmount: number;
  lensVignetteAmount: number;
  lensTcaAmount: number;
  lensDistortionEnabled: boolean;
  lensTcaEnabled: boolean;
  lensVignetteEnabled: boolean;
  lensManualVignetteAmount?: number;
  lensManualVignetteMidpoint?: number;
  diffractionCorrection?: boolean;
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
    table?: number[];
    table_end?: number;
    table_radii?: number[];
    centre_x?: number;
    centre_y?: number;
  } | null;
  lensMaker: string | null;
  lensProfileFor?: string | null;
  lensModel: string | null;
  lumaNoiseReduction: number;
  lumaNoiseDetail?: number;
  lumaNoiseContrast?: number;
  colorNoiseDetail?: number;
  colorNoiseSmoothness?: number;
  colorNoiseVersion?: number;
  toneVersion?: number;
  clarityVersion?: number;
  temperatureVersion?: number;
  tintVersion?: number;
  vibranceVersion?: number;
  structureVersion?: number;
  fringeVersion?: number;
  defringeVersion?: number;
  bwVersion?: number;
  hslVersion?: number;
  sharpenVersion?: number;
  lumaNoiseVersion?: number;
  renderVersion?: number;
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
  sharpenRadius: number;
  sharpenDetail: number;
  sharpenMasking: number;
  sharpnessFalloff?: number;
  showClipping: boolean | number;
  structure: number;
  temperature: number;
  tint: number;
  toneMapper: 'agx' | 'basic' | 'standard';
  baseCurve: 'standard' | 'highContrast' | 'extraShadow' | 'linear';
  cameraProfile?: string;
  whiteBalance?: WhiteBalanceMode;
  wbTemperature?: number;
  wbTint?: number;
  transformDistortion: number;
  autoChromaticAberration?: boolean;
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
  vignetteStyle?: 'highlightPriority' | 'colorPriority' | 'paintOverlay';
  grainStructure?: GrainStructure;
  grainVersion?: number;
  grainColor?: number;
  relightLights?: RelightLight[];
  relightDepth?: RelightDepth;
  relightDepthAmount?: number;
  relightSkinReflections?: number;
  relightOriginalLight?: number;
  vignetteHighlights?: number;
  whites: number;
}

// Remove the open index signature while retaining each named property's modifier.
type Named<T> = { [K in keyof T as string extends K ? never : number extends K ? never : K]: T[K] };
type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
type Assert<T extends true> = T;
export type AdjustmentsStillEqual = Assert<Equal<Named<OldAdjustments>, Named<Adjustments>>>;
export type RejectPartial = Assert<Equal<Equal<Named<OldAdjustments>, Partial<Named<Adjustments>>>, false>>;
